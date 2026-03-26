
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use super::sqlite::mir_db_path;
use super::workspace::{all_extern_paths_valid, is_args_cache_stale};

const REPO_URL: &str = "https://github.com/LeeSangMin1029/rude";



fn mir_callgraph_bin_name() -> &'static str {
    if cfg!(windows) { "mir-callgraph.exe" } else { "mir-callgraph" }
}

fn rude_home() -> Result<PathBuf> {
    let home = rude_db::home_dir()
        .context("cannot determine home directory")?;
    Ok(home.join(".rude"))
}

fn run_nightly_rustc(args: &[&str]) -> Option<String> {
    Command::new("rustup").arg("run").arg("nightly").arg("rustc").args(args)
        .output().ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
}

pub(super) fn nightly_rustc_version() -> Option<String> {
    run_nightly_rustc(&["--version"])
}

fn nightly_sysroot_bin() -> Option<String> {
    use std::sync::OnceLock;
    static CACHE: OnceLock<Option<String>> = OnceLock::new();
    CACHE.get_or_init(|| run_nightly_rustc(&["--print", "sysroot"]).map(|s| format!("{s}/bin"))).clone()
}

pub(super) fn add_nightly_path(cmd: &mut Command) {
    if let Some(nightly_bin) = nightly_sysroot_bin() {
        let current = std::env::var("PATH").unwrap_or_default();
        let sep = if cfg!(windows) { ";" } else { ":" };
        cmd.env("PATH", format!("{nightly_bin}{sep}{current}"));
    }
}

fn install_mir_callgraph() -> Result<PathBuf> {
    let nightly_ver = nightly_rustc_version().ok_or_else(|| {
        anyhow::anyhow!(
            "nightly Rust toolchain required for rude add.\n\
             Run: rustup toolchain install nightly --component rust-src rustc-dev llvm-tools-preview"
        )
    })?;

    let base = rude_home()?;
    let bin_dir = base.join("bin");
    let cached_bin = bin_dir.join(mir_callgraph_bin_name());
    let version_file = bin_dir.join(".nightly-version");

    if cached_bin.exists() {
        if let Ok(saved_ver) = std::fs::read_to_string(&version_file) {
            if saved_ver.trim() == nightly_ver {
                return Ok(cached_bin);
            }
            eprintln!("  [mir] nightly version changed, reinstalling mir-callgraph...");
        }
    } else {
        eprintln!("  [mir] installing mir-callgraph (first run)...");
    }

    std::fs::create_dir_all(&bin_dir)?;
    let status = Command::new("cargo")
        .args(["install", "--git", REPO_URL,
               "mir-callgraph", "--root", &base.to_string_lossy(), "--force"])
        .env("RUSTUP_TOOLCHAIN", "nightly")
        .status()
        .context("failed to run cargo install mir-callgraph")?;

    if !status.success() {
        bail!("mir-callgraph install failed (exit code: {status})");
    }

    std::fs::write(&version_file, &nightly_ver)?;
    eprintln!("  [mir] mir-callgraph ready: {}", cached_bin.display());
    Ok(cached_bin)
}

pub fn find_mir_callgraph_bin(override_path: Option<&Path>) -> Result<PathBuf> {
    if let Some(p) = override_path { return Ok(p.to_path_buf()); }
    if let Ok(exe) = std::env::current_exe() {
        let sibling = exe.with_file_name(mir_callgraph_bin_name());
        if sibling.exists() { return Ok(sibling); }
    }
    // Cached binary — skip nightly version check for speed
    let base = rude_home()?;
    let cached = base.join("bin").join(mir_callgraph_bin_name());
    if cached.exists() { return Ok(cached); }
    install_mir_callgraph()
}

pub fn run_mir_callgraph(project_root: &Path, mir_callgraph_bin: Option<&Path>) -> Result<()> {
    run_mir_callgraph_for(project_root, mir_callgraph_bin, &[], false)
}

#[tracing::instrument(skip_all)]
pub fn run_mir_callgraph_for(
    project_root: &Path,
    mir_callgraph_bin: Option<&Path>,
    crates: &[&str],
    _rust_only: bool,
) -> Result<()> {
    let out_dir = project_root.join("target").join("mir-edges");
    // No pre-deletion: RUSTC_WRAPPER now truncates on lib build and appends on test.
    // Crates that cargo skips (already built) keep their existing edge files intact.
    std::fs::create_dir_all(&out_dir)
        .with_context(|| format!("failed to create MIR edge dir: {}", out_dir.display()))?;

    let bin = find_mir_callgraph_bin(mir_callgraph_bin)?;
    let _t0 = std::time::Instant::now();

    let mir_db = mir_db_path(project_root);

    let mut cmd = Command::new(&bin);
    add_nightly_path(&mut cmd);
    cmd.current_dir(project_root)
        .arg("--keep-going")
        .env("MIR_CALLGRAPH_OUT", &out_dir)
        .env("MIR_CALLGRAPH_DB", &mir_db)
        .env("MIR_CALLGRAPH_JSON", "1");

    for krate in crates {
        cmd.arg("-p").arg(krate);
    }

    let status = cmd.status()
        .with_context(|| format!("failed to run mir-callgraph: {}", bin.display()))?;

    if !status.success() {
        eprintln!("  [mir] mir-callgraph exited with {status} (partial results may be available)");
    }

    // Run language-specific extractors (Python, TypeScript)

    // Ensure rustc-args nightly version is saved so next run isn't stale.
    let args_dir = out_dir.join("rustc-args");
    if args_dir.exists() {
        let ver_file = args_dir.join(".nightly-version");
        if !ver_file.exists() {
            if let Some(ver) = nightly_rustc_version() {
                let _ = std::fs::write(&ver_file, &ver);
            }
        }
    }

    Ok(())
}

fn kill_previous_test_bg(out_dir: &Path) {
    let pid_file = out_dir.join(".test-bg.pid");
    if !pid_file.exists() { return; }
    let content = match std::fs::read_to_string(&pid_file) {
        Ok(c) => c,
        Err(_) => return,
    };
    let _ = std::fs::remove_file(&pid_file);

    for line in content.lines() {
        if let Ok(pid) = line.trim().parse::<u32>() {
            kill_process_by_pid(pid);
        }
    }
}

#[cfg(windows)]
fn kill_process_by_pid(pid: u32) {
    let _ = Command::new("taskkill")
        .args([
            "/F",
            "/PID", &pid.to_string(),
            "/FI", "IMAGENAME eq mir-callgraph.exe",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

#[cfg(not(windows))]
fn kill_process_by_pid(pid: u32) {
    // Guard: only kill if the process is actually mir-callgraph.
    // If /proc is unavailable (e.g., macOS), fall through and kill anyway
    // since macOS PID reuse is less aggressive and the risk is lower.
    let comm_path = format!("/proc/{pid}/comm");
    if let Ok(name) = std::fs::read_to_string(&comm_path) {
        if !name.trim().starts_with("mir-callgraph") {
            return; // PID reused by a different process — do not kill
        }
    }
    let _ = Command::new("kill")
        .args(["-9", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}


#[tracing::instrument(skip_all)]
pub fn run_mir_direct(
    project_root: &Path,
    mir_callgraph_bin: Option<&Path>,
    crates: &[&str],
    _rust_only: bool,
) -> Result<()> {
    let out_dir = project_root.join("target").join("mir-edges");
    let args_dir = out_dir.join("rustc-args");

    let mut lib_files = Vec::new();
    let mut test_files = Vec::new();
    for krate in crates {
        let u = krate.replace('-', "_");
        let lib = args_dir.join(format!("{u}.lib.rustc-args.json"));
        let test = args_dir.join(format!("{u}.test.rustc-args.json"));
        if lib.exists() { lib_files.push(lib); }
        if test.exists() { test_files.push(test); }
    }

    let mir_db = mir_db_path(project_root);

    // No cached args for these crates → need cargo to generate them
    if lib_files.is_empty() && test_files.is_empty() {
        let needs_refresh = !args_dir.exists() || is_args_cache_stale(project_root, &args_dir);
        if needs_refresh || !all_extern_paths_valid(crates, &args_dir) {
            run_mir_callgraph_for(project_root, mir_callgraph_bin, crates, _rust_only)?;
        }
        // Re-check after cargo run
        for krate in crates {
            let u = krate.replace('-', "_");
            let f = args_dir.join(format!("{u}.lib.rustc-args.json"));
            if f.exists() { lib_files.push(f); }
            let t = args_dir.join(format!("{u}.test.rustc-args.json"));
            if t.exists() { test_files.push(t); }
        }
        if lib_files.is_empty() && test_files.is_empty() {
            return Ok(());
        }
    }

    let bin = {
        let _span = tracing::info_span!("find_bin").entered();
        find_mir_callgraph_bin(mir_callgraph_bin)?
    };
    kill_previous_test_bg(&out_dir);

    let all_files = lib_files;
    if let Some(()) = try_daemon_all(project_root, &all_files, &out_dir, &mir_db) {
        return Ok(());
    }
    start_daemon(project_root, mir_callgraph_bin).ok();
    if let Some(()) = try_daemon_all(project_root, &all_files, &out_dir, &mir_db) {
        return Ok(());
    }
    // Fallback: subprocess
    let mut had_error = false;
    {
        let mut children: Vec<(PathBuf, std::process::Child)> = Vec::new();
        for args_file in &all_files {
            let mut cmd = Command::new(&bin);
            add_nightly_path(&mut cmd);
            cmd.current_dir(project_root)
                .arg("--direct")
                .arg("--args-file").arg(args_file)
                .env("MIR_CALLGRAPH_OUT", &out_dir)
                .env("MIR_CALLGRAPH_DB", &mir_db)
                .env("MIR_CALLGRAPH_JSON", "1");
            match cmd.spawn() {
                Ok(child) => children.push((args_file.clone(), child)),
                Err(e) => { eprintln!("  [mir] failed to spawn direct: {e}"); had_error = true; }
            }
        }
        for (path, mut child) in children {
            if let Ok(status) = child.wait() {
                if !status.success() { eprintln!("  [mir] direct failed for {}: {status}", path.display()); had_error = true; }
            }
        }
    }
    if had_error {
        eprintln!("  [mir] some builds failed, refreshing via cargo...");
        run_mir_callgraph_for(project_root, mir_callgraph_bin, crates, _rust_only)?;
    }
    Ok(())
}

fn try_daemon_all(project_root: &Path, lib_files: &[PathBuf], out_dir: &Path, mir_db: &Path) -> Option<()> {
    for args_file in lib_files {
        try_daemon(project_root, args_file, out_dir, mir_db)?;
    }
    Some(())
}


fn daemon_pipe_name(project_root: &Path) -> String {
    let path = project_root.canonicalize().unwrap_or(project_root.to_path_buf());
    let bytes = path.as_os_str().as_encoded_bytes();
    let hash = xxhash_rust::xxh64::xxh64(bytes, 0);
    #[cfg(windows)]
    { format!(r"\\.\pipe\rude-mir-{hash:016x}") }
    #[cfg(not(windows))]
    { format!("/tmp/rude-mir-{hash:016x}.sock") }
}

const DAEMON_RPC_TIMEOUT_MS: u32 = 180_000; // 3 minutes per request

fn try_daemon(
    project_root: &Path, args_file: &Path, out_dir: &Path, mir_db: &Path,
) -> Option<()> {
    let pipe_name = daemon_pipe_name(project_root);
    #[derive(serde::Serialize)]
    struct Req<'a> { args_file: &'a str, out_dir: &'a str, db: &'a str }
    let req = Req {
        args_file: &args_file.to_string_lossy(),
        out_dir: &out_dir.to_string_lossy(),
        db: &mir_db.to_string_lossy(),
    };
    let mut request = serde_json::to_string(&req).ok()?;
    request.push('\n');

    let response = daemon_rpc(&pipe_name, &request)?;
    if response.contains("\"ok\":true") {
        eprintln!("  [mir] daemon: {}", response.trim());
        Some(())
    } else {
        eprintln!("  [mir] daemon error: {}", response.trim());
        None
    }
}

#[cfg(windows)]
fn daemon_rpc(pipe_name: &str, request: &str) -> Option<String> {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateFileW(name: *const u16, access: u32, share: u32, attrs: *const std::ffi::c_void,
            disposition: u32, flags: u32, template: *const std::ffi::c_void) -> *mut std::ffi::c_void;
        fn CreateEventW(attrs: *const std::ffi::c_void, manual: i32, initial: i32, name: *const u16) -> *mut std::ffi::c_void;
        fn ReadFile(file: *mut std::ffi::c_void, buf: *mut u8, len: u32,
            read: *mut u32, overlapped: *mut std::ffi::c_void) -> i32;
        fn WriteFile(file: *mut std::ffi::c_void, buf: *const u8, len: u32,
            written: *mut u32, overlapped: *const std::ffi::c_void) -> i32;
        fn GetOverlappedResult(file: *mut std::ffi::c_void, overlapped: *mut std::ffi::c_void,
            transferred: *mut u32, wait: i32) -> i32;
        fn WaitForSingleObject(handle: *mut std::ffi::c_void, ms: u32) -> u32;
        fn WaitNamedPipeW(name: *const u16, timeout: u32) -> i32;
        fn ResetEvent(handle: *mut std::ffi::c_void) -> i32;
        fn CancelIo(handle: *mut std::ffi::c_void) -> i32;
        fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
        fn GetLastError() -> u32;
    }
    const GENERIC_RW: u32 = 0xC0000000;
    const OPEN_EXISTING: u32 = 3;
    const FILE_FLAG_OVERLAPPED: u32 = 0x40000000;
    const INVALID: *mut std::ffi::c_void = -1isize as *mut _;
    const ERROR_PIPE_BUSY: u32 = 231;
    const ERROR_IO_PENDING: u32 = 997;

    let wide: Vec<u16> = pipe_name.encode_utf16().chain(std::iter::once(0)).collect();
    // MS standard pattern: CreateFile → ERROR_PIPE_BUSY → WaitNamedPipe → retry
    let mut handle = unsafe {
        CreateFileW(wide.as_ptr(), GENERIC_RW, 0, std::ptr::null(), OPEN_EXISTING, FILE_FLAG_OVERLAPPED, std::ptr::null())
    };
    if handle == INVALID {
        if unsafe { GetLastError() } != ERROR_PIPE_BUSY { return None; }
        unsafe { WaitNamedPipeW(wide.as_ptr(), 3000); }
        handle = unsafe {
            CreateFileW(wide.as_ptr(), GENERIC_RW, 0, std::ptr::null(), OPEN_EXISTING, FILE_FLAG_OVERLAPPED, std::ptr::null())
        };
        if handle == INVALID { return None; }
    }
    let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
    if event.is_null() { unsafe { CloseHandle(handle); } return None; }

    let _span = tracing::info_span!("daemon_rpc").entered();
    #[repr(C)]
    struct Ov { _pad: [usize; 2], _off: [u32; 2], event: *mut std::ffi::c_void }
    let ov_new = |e| Ov { _pad: [0; 2], _off: [0; 2], event: e };

    let wait_ov = |h: *mut _, ov: &mut Ov, bytes: &mut u32, timeout_ms: u32| -> bool {
        if unsafe { GetLastError() } != ERROR_IO_PENDING { return false; }
        if unsafe { WaitForSingleObject(ov.event, timeout_ms) } != 0 {
            unsafe { CancelIo(h); }
            return false;
        }
        unsafe { GetOverlappedResult(h, ov as *mut Ov as *mut _, bytes, 0) != 0 }
    };

    // Write
    let bytes = request.as_bytes();
    let mut written = 0u32;
    let mut ov_w = ov_new(event);
    let ok = unsafe { WriteFile(handle, bytes.as_ptr(), bytes.len() as u32, &mut written, &mut ov_w as *mut Ov as *mut _) };
    if ok == 0 && !wait_ov(handle, &mut ov_w, &mut written, 10_000) {
        unsafe { CloseHandle(event); CloseHandle(handle); }
        return None;
    }

    // Read
    unsafe { ResetEvent(event); }
    let mut buf = vec![0u8; 65536];
    let mut read = 0u32;
    let mut ov_r = ov_new(event);
    let ok = unsafe { ReadFile(handle, buf.as_mut_ptr(), buf.len() as u32, &mut read, &mut ov_r as *mut Ov as *mut _) };
    if ok == 0 && !wait_ov(handle, &mut ov_r, &mut read, DAEMON_RPC_TIMEOUT_MS) {
        unsafe { CloseHandle(event); CloseHandle(handle); }
        return None;
    }
    unsafe { CloseHandle(event); CloseHandle(handle); }
    Some(String::from_utf8_lossy(&buf[..read as usize]).into_owned())
}

#[cfg(unix)]
fn daemon_rpc(pipe_name: &str, request: &str) -> Option<String> {
    use std::os::unix::net::UnixStream;
    use std::io::{Write, BufRead, BufReader};
    let mut stream = UnixStream::connect(pipe_name).ok()?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(180))).ok();
    let _span = tracing::info_span!("daemon_rpc").entered();
    stream.write_all(request.as_bytes()).ok()?;
    let mut response = String::new();
    BufReader::new(&stream).read_line(&mut response).ok()?;
    Some(response)
}

#[cfg(not(any(windows, unix)))]
fn daemon_rpc(_: &str, _: &str) -> Option<String> { None }

pub fn start_daemon(project_root: &Path, mir_callgraph_bin: Option<&Path>) -> Result<()> {
    let bin = find_mir_callgraph_bin(mir_callgraph_bin)?;
    let pipe_name = daemon_pipe_name(project_root);
    #[cfg(windows)]
    let event_name = pipe_name.replace("rude-mir-", "rude-mir-ready-");
    #[cfg(not(windows))]
    let event_name = pipe_name.clone(); // Unix: check socket file existence
    let out_dir = project_root.join("target").join("mir-edges");
    let mut cmd = Command::new(&bin);
    add_nightly_path(&mut cmd);
    cmd.arg("--daemon").arg("--pipe").arg(&pipe_name);
    #[cfg(windows)]
    cmd.arg("--event").arg(&event_name);
    cmd.env("MIR_CALLGRAPH_OUT", &out_dir)
        .env("MIR_CALLGRAPH_DB", super::sqlite::mir_db_path(project_root))
        .env("MIR_CALLGRAPH_JSON", "1")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit());
    cmd.spawn().context("failed to start daemon")?;
    wait_for_event(&event_name, 6000);
    Ok(())
}

#[cfg(windows)]
fn wait_for_event(name: &str, timeout_ms: u32) -> bool {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn OpenEventW(access: u32, inherit: i32, name: *const u16) -> *mut std::ffi::c_void;
        fn WaitForSingleObject(handle: *mut std::ffi::c_void, ms: u32) -> u32;
        fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
    }
    const SYNCHRONIZE: u32 = 0x00100000;
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let handle = unsafe { OpenEventW(SYNCHRONIZE, 0, wide.as_ptr()) };
    if handle.is_null() {
        std::thread::sleep(std::time::Duration::from_millis(100));
        let handle = unsafe { OpenEventW(SYNCHRONIZE, 0, wide.as_ptr()) };
        if handle.is_null() { return false; }
        let r = unsafe { WaitForSingleObject(handle, timeout_ms) };
        unsafe { CloseHandle(handle); }
        return r == 0;
    }
    let r = unsafe { WaitForSingleObject(handle, timeout_ms) };
    unsafe { CloseHandle(handle); }
    r == 0
}
#[cfg(not(windows))]
fn wait_for_event(name: &str, timeout_ms: u32) -> bool {
    let start = std::time::Instant::now();
    let timeout = std::time::Duration::from_millis(timeout_ms as u64);
    while start.elapsed() < timeout {
        if std::path::Path::new(name).exists() { return true; }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    false
}
