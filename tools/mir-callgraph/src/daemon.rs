use std::env;
use crate::extract;
use crate::types::RustcArgs;

pub fn run(args: &[String]) {
    let pipe_name = args.iter()
        .skip_while(|a| *a != "--pipe").skip(1).next()
        .cloned().unwrap_or_else(|| r"\\.\pipe\rude-mir-default".to_owned());

    eprintln!("[daemon] starting on {pipe_name}");
    eprintln!("[daemon] rustc_driver loaded, ready for requests");

    loop {
        let pipe = match pipe::create_and_wait(&pipe_name) {
            Ok(p) => p,
            Err(e) => { eprintln!("[daemon] pipe error: {e}"); continue; }
        };
        let request = match pipe::read_line(&pipe) {
            Ok(s) => s,
            Err(e) => { eprintln!("[daemon] read error: {e}"); continue; }
        };
        let response = process(&request);
        let _ = pipe::write(&pipe, &response);
        pipe::flush_and_disconnect(&pipe);
    }
}

fn process(request: &str) -> String {
    #[derive(serde::Deserialize)]
    struct Req { args_file: String, out_dir: String, db: String }

    let req: Req = match serde_json::from_str(request) {
        Ok(r) => r,
        Err(e) => return format!("{{\"ok\":false,\"error\":\"parse: {e}\"}}\n"),
    };

    let cached: RustcArgs = match crate::load_cached_args(&req.args_file) {
        Ok(c) => c,
        Err(e) => return format!("{{\"ok\":false,\"error\":\"{e}\"}}\n"),
    };

    unsafe {
        env::set_var("MIR_CALLGRAPH_OUT", &req.out_dir);
        env::set_var("MIR_CALLGRAPH_DB", &req.db);
        env::set_var("MIR_CALLGRAPH_JSON", "1");
    }
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
pub mod pipe {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateNamedPipeW(
            name: *const u16, open_mode: u32, pipe_mode: u32,
            max_instances: u32, out_buf: u32, in_buf: u32, timeout: u32,
            attrs: *const std::ffi::c_void,
        ) -> *mut std::ffi::c_void;
        fn ConnectNamedPipe(pipe: *mut std::ffi::c_void, overlapped: *const std::ffi::c_void) -> i32;
        fn DisconnectNamedPipe(pipe: *mut std::ffi::c_void) -> i32;
        fn ReadFile(
            file: *mut std::ffi::c_void, buf: *mut u8, len: u32,
            read: *mut u32, overlapped: *const std::ffi::c_void,
        ) -> i32;
        fn WriteFile(
            file: *mut std::ffi::c_void, buf: *const u8, len: u32,
            written: *mut u32, overlapped: *const std::ffi::c_void,
        ) -> i32;
        fn FlushFileBuffers(file: *mut std::ffi::c_void) -> i32;
        fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
        fn GetLastError() -> u32;
    }

    const PIPE_ACCESS_DUPLEX: u32 = 0x03;
    const PIPE_TYPE_BYTE: u32 = 0x00;
    const PIPE_READMODE_BYTE: u32 = 0x00;
    const PIPE_WAIT: u32 = 0x00;
    const INVALID_HANDLE: *mut std::ffi::c_void = -1isize as *mut _;
    const ERROR_PIPE_CONNECTED: u32 = 535;

    pub struct PipeHandle(pub *mut std::ffi::c_void);
    impl Drop for PipeHandle { fn drop(&mut self) { unsafe { CloseHandle(self.0); } } }

    pub fn create_and_wait(name: &str) -> Result<PipeHandle, String> {
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        let handle = unsafe {
            CreateNamedPipeW(
                wide.as_ptr(), PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                1, 65536, 65536, 0, std::ptr::null(),
            )
        };
        if handle == INVALID_HANDLE {
            return Err(format!("CreateNamedPipe failed: {}", unsafe { GetLastError() }));
        }
        let pipe = PipeHandle(handle);
        let ok = unsafe { ConnectNamedPipe(pipe.0, std::ptr::null()) };
        if ok == 0 && unsafe { GetLastError() } != ERROR_PIPE_CONNECTED {
            return Err(format!("ConnectNamedPipe failed: {}", unsafe { GetLastError() }));
        }
        Ok(pipe)
    }

    pub fn read_line(pipe: &PipeHandle) -> Result<String, String> {
        let mut buf = vec![0u8; 65536];
        let mut total = 0usize;
        loop {
            let mut read = 0u32;
            let ok = unsafe {
                ReadFile(pipe.0, buf[total..].as_mut_ptr(), (buf.len() - total) as u32, &mut read, std::ptr::null())
            };
            if ok == 0 { break; }
            total += read as usize;
            if total > 0 && buf[total - 1] == b'\n' { break; }
        }
        String::from_utf8(buf[..total].to_vec()).map_err(|e| format!("utf8: {e}"))
    }

    pub fn write(pipe: &PipeHandle, data: &str) -> Result<(), String> {
        let bytes = data.as_bytes();
        let mut written = 0u32;
        let ok = unsafe { WriteFile(pipe.0, bytes.as_ptr(), bytes.len() as u32, &mut written, std::ptr::null()) };
        if ok == 0 { Err(format!("write failed: {}", unsafe { GetLastError() })) } else { Ok(()) }
    }

    pub fn flush_and_disconnect(pipe: &PipeHandle) {
        unsafe { FlushFileBuffers(pipe.0); DisconnectNamedPipe(pipe.0); }
    }
}

#[cfg(not(windows))]
pub mod pipe {
    pub struct PipeHandle;
    pub fn create_and_wait(_: &str) -> Result<PipeHandle, String> { Err("not supported".into()) }
    pub fn read_line(_: &PipeHandle) -> Result<String, String> { Err("not supported".into()) }
    pub fn write(_: &PipeHandle, _: &str) -> Result<(), String> { Err("not supported".into()) }
    pub fn flush_and_disconnect(_: &PipeHandle) {}
}
