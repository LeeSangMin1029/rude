use std::env;
use crate::extract;
use crate::types::RustcArgs;

const MAX_REQUESTS: usize = 100;
const IDLE_TIMEOUT_MS: u32 = 300_000;

pub fn run(args: &[String]) {
    let pipe_name = args.iter()
        .skip_while(|a| *a != "--pipe").skip(1).next()
        .cloned().unwrap_or_else(|| r"\\.\pipe\rude-mir-default".to_owned());
    let event_name = args.iter()
        .skip_while(|a| *a != "--event").skip(1).next()
        .cloned();

    eprintln!("[daemon] starting on {pipe_name}");

    let pipe = match pipe::create(&pipe_name) {
        Ok(p) => p,
        Err(e) => { eprintln!("[daemon] pipe create failed: {e}"); return; }
    };

    if let Some(ref ev) = event_name {
        signal_event(ev);
    }
    eprintln!("[daemon] ready for requests");

    let mut request_count = 0usize;
    loop {
        match pipe::wait_connect(&pipe, IDLE_TIMEOUT_MS) {
            Ok(false) => {
                eprintln!("[daemon] idle timeout, exiting");
                break;
            }
            Err(e) => { eprintln!("[daemon] connect error: {e}"); continue; }
            Ok(true) => {}
        }
        let request = match pipe::read_line(&pipe) {
            Ok(s) => s,
            Err(e) => { eprintln!("[daemon] read error: {e}"); pipe::disconnect(&pipe); continue; }
        };

        let response = match std::thread::spawn(move || process(&request)).join() {
            Ok(r) => r,
            Err(_) => "{\"ok\":false,\"error\":\"panic\"}\n".to_owned(),
        };

        let _ = pipe::write(&pipe, &response);
        pipe::flush_and_disconnect(&pipe);

        request_count += 1;
        if request_count >= MAX_REQUESTS {
            eprintln!("[daemon] max requests ({MAX_REQUESTS}) reached, exiting");
            break;
        }
    }
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
        Err(e) => return format!("{{\"ok\":false,\"error\":\"parse: {e}\"}}\n"),
    };

    let cached: RustcArgs = match crate::types::RustcArgs::load(&req.args_file) {
        Ok(c) => c,
        Err(e) => return format!("{{\"ok\":false,\"error\":\"{e}\"}}\n"),
    };

    // SAFETY: daemon processes requests sequentially, one thread at a time.
    // CARGO_* vars must be set before rustc_public::run! for env!() macro expansion.
    // rustc internal threads haven't started yet at this point.
    for (k, v) in &cached.env { unsafe { env::set_var(k, v); } }

    let is_test = cached.args.iter().any(|a| a == "--test");
    let db_path = Some(req.db.clone());
    let crate_name = cached.crate_name.clone();

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rustc_public::run!(&cached.args, || extract::extract_all(is_test, true, &db_path))
    }));

    match result {
        Ok(Ok(_)) => format!("{{\"ok\":true,\"crate\":\"{crate_name}\"}}\n"),
        Ok(Err(e)) => format!("{{\"ok\":false,\"error\":\"run: {e:?}\"}}\n"),
        Err(_) => format!("{{\"ok\":false,\"error\":\"panic\"}}\n"),
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
            internal: 0,
            internal_high: 0,
            offset: 0,
            offset_high: 0,
            event: pipe.1,
        };
        let ok = unsafe { ConnectNamedPipe(pipe.0, &mut ov as *mut Overlapped as *mut std::ffi::c_void) };
        let err = unsafe { GetLastError() };
        if ok != 0 || err == ERROR_PIPE_CONNECTED {
            return Ok(true);
        }
        if err != ERROR_IO_PENDING {
            return Err(format!("ConnectNamedPipe failed: {err}"));
        }
        let wait = unsafe { WaitForSingleObject(pipe.1, timeout_ms) };
        if wait == WAIT_TIMEOUT {
            unsafe { CancelIo(pipe.0); }
            return Ok(false);
        }
        if wait == WAIT_OBJECT_0 {
            return Ok(true);
        }
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

#[cfg(not(windows))]
pub mod pipe {
    pub struct PipeHandle;
    pub fn create(_: &str) -> Result<PipeHandle, String> { Err("not supported".into()) }
    pub fn wait_connect(_: &PipeHandle, _timeout_ms: u32) -> Result<bool, String> { Err("not supported".into()) }
    pub fn disconnect(_: &PipeHandle) {}
    pub fn read_line(_: &PipeHandle) -> Result<String, String> { Err("not supported".into()) }
    pub fn write(_: &PipeHandle, _: &str) -> Result<(), String> { Err("not supported".into()) }
    pub fn flush_and_disconnect(_: &PipeHandle) {}
}
