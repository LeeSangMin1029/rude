
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
    let mut cmd_args = vec!["+nightly"];
    cmd_args.extend(args);
    Command::new("rustc").args(&cmd_args).output().ok()
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
        cmd.env("PATH", format!("{current};{nightly_bin}"));
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
        .args(["+nightly", "install", "--git", REPO_URL,
               "mir-callgraph", "--root", &base.to_string_lossy(), "--force"])
        .status()
        .context("failed to run cargo +nightly install")?;

    if !status.success() {
        bail!("mir-callgraph install failed (exit code: {status})");
    }

    std::fs::write(&version_file, &nightly_ver)?;
    eprintln!("  [mir] mir-callgraph ready: {}", cached_bin.display());
    Ok(cached_bin)
}

fn find_mir_callgraph_bin(override_path: Option<&Path>) -> Result<PathBuf> {
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

    // Try daemon first (DLL already loaded → fast)
    if let Some(()) = try_daemon_all(project_root, &lib_files, &out_dir, &mir_db) {
        return Ok(());
    }
    // Daemon not running → start it, retry once
    if start_daemon(project_root, mir_callgraph_bin).is_ok() {
        if let Some(()) = try_daemon_all(project_root, &lib_files, &out_dir, &mir_db) {
            return Ok(());
        }
    }
    // Fallback: subprocess (slow DLL loading)
    let mut had_error = false;
    {
        let mut lib_children: Vec<(PathBuf, std::process::Child)> = Vec::new();
        for args_file in &lib_files {
            let _span = tracing::info_span!("lib_spawn").entered();
            let mut cmd = Command::new(&bin);
            add_nightly_path(&mut cmd);
            cmd.current_dir(project_root)
                .arg("--direct")
                .arg("--args-file").arg(args_file)
                .env("MIR_CALLGRAPH_OUT", &out_dir)
                .env("MIR_CALLGRAPH_DB", &mir_db)
                .env("MIR_CALLGRAPH_JSON", "1");
            match cmd.spawn() {
                Ok(child) => lib_children.push((args_file.clone(), child)),
                Err(e) => { eprintln!("  [mir] failed to spawn direct (lib): {e}"); had_error = true; }
            }
        }
        for (path, mut child) in lib_children {
            let _span = tracing::info_span!("lib_wait").entered();
            if let Ok(status) = child.wait() {
                if !status.success() { eprintln!("  [mir] direct failed for {}: {status}", path.display()); had_error = true; }
            }
        }
    }
    if had_error {
        eprintln!("  [mir] some lib builds failed, refreshing via cargo...");
        run_mir_callgraph_for(project_root, mir_callgraph_bin, crates, _rust_only)?;
        return Ok(());
    }

    // Test builds skipped in direct mode — lib edges are sufficient.
    Ok(())
}

fn try_daemon_all(project_root: &Path, lib_files: &[PathBuf], out_dir: &Path, mir_db: &Path) -> Option<()> {
    for args_file in lib_files {
        try_daemon(project_root, args_file, out_dir, mir_db)?;
    }
    Some(())
}

fn daemon_pipe_name(project_root: &Path) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    project_root.canonicalize().unwrap_or(project_root.to_path_buf()).hash(&mut h);
    format!(r"\\.\pipe\rude-mir-{:016x}", h.finish())
}

fn try_daemon(
    project_root: &Path, args_file: &Path, out_dir: &Path, mir_db: &Path,
) -> Option<()> {
    let pipe_name = daemon_pipe_name(project_root);
    let request = format!(
        "{{\"args_file\":\"{}\",\"out_dir\":\"{}\",\"db\":\"{}\"}}\n",
        args_file.to_string_lossy().replace('\\', "\\\\"),
        out_dir.to_string_lossy().replace('\\', "\\\\"),
        mir_db.to_string_lossy().replace('\\', "\\\\"),
    );

    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn CreateFileW(
                name: *const u16, access: u32, share: u32, attrs: *const std::ffi::c_void,
                disposition: u32, flags: u32, template: *const std::ffi::c_void,
            ) -> *mut std::ffi::c_void;
            fn ReadFile(
                file: *mut std::ffi::c_void, buf: *mut u8, len: u32,
                read: *mut u32, overlapped: *const std::ffi::c_void,
            ) -> i32;
            fn WriteFile(
                file: *mut std::ffi::c_void, buf: *const u8, len: u32,
                written: *mut u32, overlapped: *const std::ffi::c_void,
            ) -> i32;
            fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
        }
        const GENERIC_READ: u32 = 0x80000000;
        const GENERIC_WRITE: u32 = 0x40000000;
        const OPEN_EXISTING: u32 = 3;
        const INVALID_HANDLE: *mut std::ffi::c_void = -1isize as *mut _;

        let wide: Vec<u16> = pipe_name.encode_utf16().chain(std::iter::once(0)).collect();
        let handle = unsafe {
            CreateFileW(wide.as_ptr(), GENERIC_READ | GENERIC_WRITE, 0, std::ptr::null(), OPEN_EXISTING, 0, std::ptr::null())
        };
        if handle == INVALID_HANDLE { return None; } // daemon not running

        let _span = tracing::info_span!("daemon_rpc").entered();
        let bytes = request.as_bytes();
        let mut written = 0u32;
        unsafe { WriteFile(handle, bytes.as_ptr(), bytes.len() as u32, &mut written, std::ptr::null()); }

        let mut buf = vec![0u8; 65536];
        let mut read = 0u32;
        unsafe { ReadFile(handle, buf.as_mut_ptr(), buf.len() as u32, &mut read, std::ptr::null()); }
        unsafe { CloseHandle(handle); }

        let response = String::from_utf8_lossy(&buf[..read as usize]);
        if response.contains("\"ok\":true") {
            eprintln!("  [mir] daemon: {}", response.trim());
            return Some(());
        }
        eprintln!("  [mir] daemon error: {}", response.trim());
    }
    None
}

pub fn start_daemon(project_root: &Path, mir_callgraph_bin: Option<&Path>) -> Result<()> {
    let bin = find_mir_callgraph_bin(mir_callgraph_bin)?;
    let pipe_name = daemon_pipe_name(project_root);
    let out_dir = project_root.join("target").join("mir-edges");
    let mut cmd = Command::new(&bin);
    add_nightly_path(&mut cmd);
    cmd.arg("--daemon").arg("--pipe").arg(&pipe_name)
        .env("MIR_CALLGRAPH_OUT", &out_dir)
        .env("MIR_CALLGRAPH_DB", super::sqlite::mir_db_path(project_root))
        .env("MIR_CALLGRAPH_JSON", "1")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit());
    cmd.spawn().context("failed to start daemon")?;
    // daemon이 pipe를 생성할 때까지 연결 재시도 (이벤트 기반, sleep 없음)
    for _ in 0..50 {
        if try_daemon(project_root, Path::new(""), Path::new(""), Path::new("")).is_some()
            || pipe_exists(&pipe_name)
        {
            return Ok(());
        }
        std::thread::yield_now();
    }
    Ok(())
}

#[cfg(windows)]
fn pipe_exists(name: &str) -> bool {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn WaitNamedPipeW(name: *const u16, timeout: u32) -> i32;
    }
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe { WaitNamedPipeW(wide.as_ptr(), 100) != 0 }
}

#[cfg(not(windows))]
fn pipe_exists(_: &str) -> bool { false }
