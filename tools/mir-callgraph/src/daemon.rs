use std::env;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use crate::extract;
use crate::types::RustcArgs;

const MAX_REQUESTS: usize = 100;
const IDLE_TIMEOUT_MS: u32 = 300_000;
const WORKER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
const MAX_CONSECUTIVE_FAILURES: u32 = 3;

pub fn run(args: &[String]) {
    let pipe_name = args.iter()
        .skip_while(|a| *a != "--pipe").skip(1).next()
        .cloned().unwrap_or_else(|| default_pipe_name());
    let event_name = args.iter()
        .skip_while(|a| *a != "--event").skip(1).next()
        .cloned();

    eprintln!("[supervisor] starting on {pipe_name}");

    let pipe = match pipe::create(&pipe_name) {
        Ok(p) => p,
        Err(e) => { eprintln!("[supervisor] pipe create failed: {e}"); return; }
    };

    if let Some(ref ev) = event_name {
        signal_event(ev);
    }
    eprintln!("[supervisor] ready, spawning worker");

    let exe = std::env::current_exe().unwrap();
    let mut worker: Option<Worker> = spawn_worker(&exe).ok();

    let result = supervisor_loop(&pipe, &exe, &mut worker);
    // Cleanup: kill worker on exit
    if let Some(mut w) = worker {
        let _ = w.child.kill();
        let _ = w.child.wait();
    }
    if let Err(e) = result {
        eprintln!("[supervisor] exit: {e}");
    }
}

fn supervisor_loop(
    pipe: &pipe::PipeHandle, exe: &Path, worker: &mut Option<Worker>,
) -> Result<(), String> {
    let mut consecutive_failures: u32 = 0;
    loop {
        match pipe::wait_connect(pipe, IDLE_TIMEOUT_MS) {
            Ok(false) => { eprintln!("[supervisor] idle timeout, exiting"); return Ok(()); }
            Err(e) => { eprintln!("[supervisor] connect error: {e}"); continue; }
            Ok(true) => {}
        }
        let request = match pipe::read_line(pipe) {
            Ok(s) if s.is_empty() => { pipe::disconnect(pipe); continue; }
            Ok(s) => s,
            Err(e) => { eprintln!("[supervisor] read error: {e}"); pipe::disconnect(pipe); continue; }
        };
        ensure_worker(worker, exe, &mut consecutive_failures);
        let response = match worker.as_mut() {
            Some(w) => match w.send_with_timeout(&request, WORKER_TIMEOUT) {
                Ok(r) => { consecutive_failures = 0; r }
                Err(e) => {
                    eprintln!("[supervisor] worker error: {e}, respawning");
                    kill_worker(worker);
                    consecutive_failures += 1;
                    if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                        eprintln!("[supervisor] {consecutive_failures} consecutive failures, giving up");
                        error_json("worker repeatedly crashed")
                    } else {
                        *worker = spawn_worker(exe).ok();
                        match worker.as_mut() {
                            Some(w2) => w2.send_with_timeout(&request, WORKER_TIMEOUT)
                                .unwrap_or_else(|e2| error_json(&format!("retry failed: {e2}"))),
                            None => error_json("failed to spawn worker"),
                        }
                    }
                }
            },
            None => error_json("no worker"),
        };
        let _ = pipe::write(pipe, &response);
        pipe::flush_and_disconnect(pipe);
    }
}

fn ensure_worker(worker: &mut Option<Worker>, exe: &Path, failures: &mut u32) {
    if worker.as_mut().map_or(true, |w| !w.is_alive()) {
        if *failures >= MAX_CONSECUTIVE_FAILURES { return; }
        eprintln!("[supervisor] worker dead, respawning");
        kill_worker(worker);
        *worker = spawn_worker(exe).ok();
        if worker.is_none() { *failures += 1; }
    }
}

fn kill_worker(worker: &mut Option<Worker>) {
    if let Some(mut w) = worker.take() {
        let _ = w.child.kill();
        let _ = w.child.wait();
    }
}

fn default_pipe_name() -> String {
    #[cfg(windows)]
    { r"\\.\pipe\rude-mir-default".to_owned() }
    #[cfg(not(windows))]
    { "/tmp/rude-mir-default.sock".to_owned() }
}

fn error_json(msg: &str) -> String {
    format!("{{\"ok\":false,\"error\":\"{msg}\"}}\n")
}

struct Worker {
    stdin: BufWriter<std::process::ChildStdin>,
    stdout: BufReader<std::process::ChildStdout>,
    child: std::process::Child,
}

fn spawn_worker(exe: &Path) -> Result<Worker, String> {
    let mut child = Command::new(exe)
        .arg("--worker")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("spawn: {e}"))?;
    let stdin = BufWriter::new(child.stdin.take().unwrap());
    let stdout = BufReader::new(child.stdout.take().unwrap());
    eprintln!("[supervisor] worker spawned (pid {})", child.id());
    Ok(Worker { stdin, stdout, child })
}

impl Worker {
    fn send_with_timeout(&mut self, request: &str, timeout: std::time::Duration) -> Result<String, String> {
        let req = if request.ends_with('\n') { request.to_owned() } else { format!("{request}\n") };
        self.stdin.write_all(req.as_bytes()).map_err(|e| format!("write: {e}"))?;
        self.stdin.flush().map_err(|e| format!("flush: {e}"))?;
        // Watchdog: kill worker if it doesn't respond within timeout.
        // When worker dies, its stdout closes → read_line returns Ok(0) or Err.
        let child_id = self.child.id();
        let watchdog = std::thread::spawn(move || {
            std::thread::sleep(timeout);
            #[cfg(windows)]
            {
                #[link(name = "kernel32")]
                unsafe extern "system" {
                    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut std::ffi::c_void;
                    fn TerminateProcess(proc: *mut std::ffi::c_void, code: u32) -> i32;
                    fn CloseHandle(h: *mut std::ffi::c_void) -> i32;
                }
                unsafe {
                    let h = OpenProcess(0x0001, 0, child_id);
                    if !h.is_null() { TerminateProcess(h, 1); CloseHandle(h); }
                }
            }
            #[cfg(unix)]
            {
                // kill -9 via Command to avoid libc dependency
                let _ = Command::new("kill").arg("-9").arg(child_id.to_string()).status();
            }
        });
        let mut response = String::new();
        let result = match self.stdout.read_line(&mut response) {
            Ok(0) => Err("worker closed stdout".into()),
            Ok(_) => Ok(response),
            Err(e) => Err(format!("read: {e}")),
        };
        drop(watchdog); // watchdog thread continues but is harmless if worker already responded
        result
    }
    fn is_alive(&mut self) -> bool {
        self.child.try_wait().ok().flatten().is_none()
    }
}

pub fn worker_run() {
    eprintln!("[worker] starting (pid {})", std::process::id());
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    let mut count = 0usize;
    for line in stdin.lock().lines() {
        let request = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if request.is_empty() { continue; }
        let response = match std::thread::spawn(move || process(&request)).join() {
            Ok(r) => r,
            Err(_) => error_json("panic"),
        };
        let resp = response.trim_end();
        if writeln!(stdout, "{resp}").is_err() { break; }
        if stdout.flush().is_err() { break; }
        count += 1;
        if count >= MAX_REQUESTS {
            eprintln!("[worker] max requests ({MAX_REQUESTS}), exiting");
            break;
        }
    }
    eprintln!("[worker] exiting after {count} requests");
}

#[cfg(windows)]
fn signal_event(name: &str) {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateEventW(attrs: *const std::ffi::c_void, manual: i32, initial: i32, name: *const u16) -> *mut std::ffi::c_void;
        fn SetEvent(handle: *mut std::ffi::c_void) -> i32;
        fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
    }
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let handle = unsafe { CreateEventW(std::ptr::null(), 1, 0, wide.as_ptr()) };
    if !handle.is_null() {
        unsafe { SetEvent(handle); CloseHandle(handle); }
    }
}
#[cfg(not(windows))]
fn signal_event(_name: &str) {}

fn process(request: &str) -> String {
    #[derive(serde::Deserialize)]
    #[allow(dead_code)]
    struct Req { args_file: String, out_dir: String, db: String }

    let req: Req = match serde_json::from_str(request) {
        Ok(r) => r,
        Err(e) => return error_json(&format!("parse: {e}")),
    };

    let cached: RustcArgs = match crate::types::RustcArgs::load(&req.args_file) {
        Ok(c) => c,
        Err(e) => return error_json(&format!("{e}")),
    };

    // SAFETY: worker processes requests sequentially, one thread at a time.
    // CARGO_* vars must be set before rustc_public::run! for env!() macro expansion.
    for (k, v) in &cached.env { unsafe { env::set_var(k, v); } }

    let is_test = cached.args.iter().any(|a| a == "--test");
    let db_path = Some(req.db.clone());
    let crate_name = cached.crate_name.clone();

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rustc_public::run!(&cached.args, || extract::extract_all(is_test, true, &db_path))
    }));

    match result {
        Ok(Ok(_)) => format!("{{\"ok\":true,\"crate\":\"{crate_name}\"}}\n"),
        Ok(Err(e)) => error_json(&format!("run: {e:?}")),
        Err(_) => error_json("panic"),
    }
}

#[cfg(windows)]
#[allow(dead_code)]
pub mod pipe {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateNamedPipeW(
            name: *const u16, open_mode: u32, pipe_mode: u32,
            max_instances: u32, out_buf: u32, in_buf: u32, timeout: u32,
            attrs: *const std::ffi::c_void,
        ) -> *mut std::ffi::c_void;
        fn CreateEventW(attrs: *const std::ffi::c_void, manual: i32, initial: i32, name: *const u16) -> *mut std::ffi::c_void;
        fn ConnectNamedPipe(pipe: *mut std::ffi::c_void, overlapped: *mut std::ffi::c_void) -> i32;
        fn WaitForSingleObject(handle: *mut std::ffi::c_void, timeout_ms: u32) -> u32;
        fn CancelIo(handle: *mut std::ffi::c_void) -> i32;
        fn SetEvent(handle: *mut std::ffi::c_void) -> i32;
        fn ResetEvent(handle: *mut std::ffi::c_void) -> i32;
        fn GetOverlappedResult(
            file: *mut std::ffi::c_void, overlapped: *mut std::ffi::c_void,
            transferred: *mut u32, wait: i32,
        ) -> i32;
        fn DisconnectNamedPipe(pipe: *mut std::ffi::c_void) -> i32;
        fn ReadFile(
            file: *mut std::ffi::c_void, buf: *mut u8, len: u32,
            read: *mut u32, overlapped: *mut std::ffi::c_void,
        ) -> i32;
        fn WriteFile(
            file: *mut std::ffi::c_void, buf: *const u8, len: u32,
            written: *mut u32, overlapped: *mut std::ffi::c_void,
        ) -> i32;
        fn FlushFileBuffers(file: *mut std::ffi::c_void) -> i32;
        fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
        fn GetLastError() -> u32;
    }

    const PIPE_ACCESS_DUPLEX: u32 = 0x03;
    const FILE_FLAG_FIRST_PIPE_INSTANCE: u32 = 0x00080000;
    const FILE_FLAG_OVERLAPPED: u32 = 0x40000000;
    const PIPE_TYPE_BYTE: u32 = 0x00;
    const PIPE_READMODE_BYTE: u32 = 0x00;
    const PIPE_WAIT: u32 = 0x00;
    const INVALID_HANDLE: *mut std::ffi::c_void = -1isize as *mut _;
    const ERROR_PIPE_CONNECTED: u32 = 535;
    const ERROR_IO_PENDING: u32 = 997;
    const WAIT_OBJECT_0: u32 = 0;
    const WAIT_TIMEOUT: u32 = 258;

    #[repr(C)]
    struct Overlapped {
        internal: usize,
        internal_high: usize,
        offset: u32,
        offset_high: u32,
        event: *mut std::ffi::c_void,
    }

    pub struct PipeHandle(pub *mut std::ffi::c_void, pub *mut std::ffi::c_void);
    unsafe impl Send for PipeHandle {}
    unsafe impl Sync for PipeHandle {}
    impl Drop for PipeHandle {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
                if !self.1.is_null() { CloseHandle(self.1); }
            }
        }
    }

    pub fn create(name: &str) -> Result<PipeHandle, String> {
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        let open_mode = PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE | FILE_FLAG_OVERLAPPED;
        let handle = unsafe {
            CreateNamedPipeW(
                wide.as_ptr(), open_mode,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                1, 65536, 65536, 0, std::ptr::null(),
            )
        };
        if handle == INVALID_HANDLE {
            return Err(format!("CreateNamedPipe failed: {}", unsafe { GetLastError() }));
        }
        let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if event.is_null() {
            unsafe { CloseHandle(handle); }
            return Err(format!("CreateEventW failed: {}", unsafe { GetLastError() }));
        }
        Ok(PipeHandle(handle, event))
    }

    pub fn wait_connect(pipe: &PipeHandle, timeout_ms: u32) -> Result<bool, String> {
        let mut ov = Overlapped {
            internal: 0, internal_high: 0, offset: 0, offset_high: 0, event: pipe.1,
        };
        let ok = unsafe { ConnectNamedPipe(pipe.0, &mut ov as *mut Overlapped as *mut std::ffi::c_void) };
        let err = unsafe { GetLastError() };
        if ok != 0 || err == ERROR_PIPE_CONNECTED { return Ok(true); }
        if err != ERROR_IO_PENDING { return Err(format!("ConnectNamedPipe failed: {err}")); }
        let wait = unsafe { WaitForSingleObject(pipe.1, timeout_ms) };
        if wait == WAIT_TIMEOUT { unsafe { CancelIo(pipe.0); } return Ok(false); }
        if wait == WAIT_OBJECT_0 { return Ok(true); }
        Err(format!("WaitForSingleObject failed: {}", unsafe { GetLastError() }))
    }

    pub fn disconnect(pipe: &PipeHandle) {
        unsafe { DisconnectNamedPipe(pipe.0); }
    }

    fn overlapped_io(pipe: &PipeHandle) -> Overlapped {
        unsafe { ResetEvent(pipe.1); }
        Overlapped { internal: 0, internal_high: 0, offset: 0, offset_high: 0, event: pipe.1 }
    }
    fn wait_io(pipe: &PipeHandle, ov: &mut Overlapped, bytes: &mut u32) -> bool {
        let err = unsafe { GetLastError() };
        if err == ERROR_IO_PENDING {
            unsafe { WaitForSingleObject(pipe.1, 30_000); }
            unsafe { GetOverlappedResult(pipe.0, ov as *mut Overlapped as *mut std::ffi::c_void, bytes, 0) != 0 }
        } else {
            false
        }
    }

    pub fn read_line(pipe: &PipeHandle) -> Result<String, String> {
        let mut buf = vec![0u8; 65536];
        let mut total = 0usize;
        loop {
            let mut read = 0u32;
            let mut ov = overlapped_io(pipe);
            let ok = unsafe {
                ReadFile(pipe.0, buf[total..].as_mut_ptr(), (buf.len() - total) as u32, &mut read, &mut ov as *mut Overlapped as *mut std::ffi::c_void)
            };
            if ok == 0 && !wait_io(pipe, &mut ov, &mut read) { break; }
            total += read as usize;
            if total > 0 && buf[total - 1] == b'\n' { break; }
        }
        String::from_utf8(buf[..total].to_vec()).map_err(|e| format!("utf8: {e}"))
    }

    pub fn write(pipe: &PipeHandle, data: &str) -> Result<(), String> {
        let bytes = data.as_bytes();
        let mut written = 0u32;
        let mut ov = overlapped_io(pipe);
        let ok = unsafe { WriteFile(pipe.0, bytes.as_ptr(), bytes.len() as u32, &mut written, &mut ov as *mut Overlapped as *mut std::ffi::c_void) };
        if ok == 0 && !wait_io(pipe, &mut ov, &mut written) {
            Err(format!("write failed: {}", unsafe { GetLastError() }))
        } else {
            Ok(())
        }
    }

    pub fn flush_and_disconnect(pipe: &PipeHandle) {
        unsafe { FlushFileBuffers(pipe.0); DisconnectNamedPipe(pipe.0); }
    }
}

#[cfg(unix)]
pub mod pipe {
    use std::cell::RefCell;
    use std::io::{Write, BufRead, BufReader};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::time::{Duration, Instant};

    pub struct PipeHandle {
        listener: UnixListener,
        conn: RefCell<Option<UnixStream>>,
        path: String,
    }
    impl Drop for PipeHandle {
        fn drop(&mut self) { let _ = std::fs::remove_file(&self.path); }
    }

    pub fn create(name: &str) -> Result<PipeHandle, String> {
        let _ = std::fs::remove_file(name);
        let listener = UnixListener::bind(name).map_err(|e| format!("bind: {e}"))?;
        Ok(PipeHandle { listener, conn: RefCell::new(None), path: name.to_owned() })
    }

    pub fn wait_connect(pipe: &PipeHandle, timeout_ms: u32) -> Result<bool, String> {
        pipe.listener.set_nonblocking(true).map_err(|e| format!("nonblock: {e}"))?;
        let start = Instant::now();
        let timeout = Duration::from_millis(timeout_ms as u64);
        loop {
            match pipe.listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(false).ok();
                    *pipe.conn.borrow_mut() = Some(stream);
                    return Ok(true);
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if start.elapsed() >= timeout { return Ok(false); }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => return Err(format!("accept: {e}")),
            }
        }
    }

    pub fn disconnect(pipe: &PipeHandle) {
        *pipe.conn.borrow_mut() = None;
    }

    pub fn read_line(pipe: &PipeHandle) -> Result<String, String> {
        let conn = pipe.conn.borrow();
        let stream = conn.as_ref().ok_or("no connection")?;
        let mut response = String::new();
        BufReader::new(stream).read_line(&mut response).map_err(|e| format!("read: {e}"))?;
        Ok(response)
    }

    pub fn write(pipe: &PipeHandle, data: &str) -> Result<(), String> {
        let conn = pipe.conn.borrow();
        let mut stream = conn.as_ref().ok_or("no connection")?;
        stream.write_all(data.as_bytes()).map_err(|e| format!("write: {e}"))?;
        Ok(())
    }

    pub fn flush_and_disconnect(pipe: &PipeHandle) {
        if let Some(stream) = pipe.conn.borrow().as_ref() {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
        *pipe.conn.borrow_mut() = None;
    }
}

#[cfg(not(any(windows, unix)))]
pub mod pipe {
    pub struct PipeHandle;
    pub fn create(_: &str) -> Result<PipeHandle, String> { Err("not supported".into()) }
    pub fn wait_connect(_: &PipeHandle, _timeout_ms: u32) -> Result<bool, String> { Err("not supported".into()) }
    pub fn disconnect(_: &PipeHandle) {}
    pub fn read_line(_: &PipeHandle) -> Result<String, String> { Err("not supported".into()) }
    pub fn write(_: &PipeHandle, _: &str) -> Result<(), String> { Err("not supported".into()) }
    pub fn flush_and_disconnect(_: &PipeHandle) {}
}
