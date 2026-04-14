use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};

use rude_util::extract_crate_name;

const DAEMON_ENV: &str = "RUDE_COVERAGE_DAEMON";
const RUNNING_FILE: &str = "coverage.running";
const OUTPUT_FILE: &str = "coverage.output";
const STDERR_FILE: &str = "coverage.stderr";

pub fn run_coverage(
    file_filter: Option<String>,
    refresh: bool,
    wait: bool,
) -> Result<()> {
    if std::env::var_os(DAEMON_ENV).is_some() {
        return run_daemon();
    }

    if let Some(ref filter) = file_filter {
        if let Ok(graph) = super::query::load_or_build_graph() {
            let any_match = graph.chunks.iter().any(|c| c.file.contains(filter.as_str()));
            if !any_match {
                println!(
                    "No files match filter \"{filter}\". (tip: use a substring of the project-relative path)"
                );
                return Ok(());
            }
        }
    }

    let db = crate::db();

    if !refresh {
        if let Some(cov) = try_cached_result(db) {
            print_coverage(&cov, file_filter.as_deref());
            return Ok(());
        }
    }

    if let Some(cov) = try_ingest_completed_output(db)? {
        print_coverage(&cov, file_filter.as_deref());
        return Ok(());
    }

    if let Some(elapsed) = running_elapsed(db) {
        if wait {
            wait_for_completion(db, Some(elapsed))?;
            if let Some(cov) = try_ingest_completed_output(db)? {
                print_coverage(&cov, file_filter.as_deref());
                return Ok(());
            }
            println!("cargo llvm-cov finished but produced no parseable output.");
            emit_stderr_tail(db);
            return Ok(());
        }
        let secs = elapsed.as_secs();
        println!(
            "cargo llvm-cov still running ({secs}s elapsed). Rerun `rude coverage` in ~1-3 min, or pass `--wait` to block."
        );
        return Ok(());
    }

    spawn_daemon(db, refresh)?;
    if wait {
        wait_for_completion(db, None)?;
        if let Some(cov) = try_ingest_completed_output(db)? {
            print_coverage(&cov, file_filter.as_deref());
            return Ok(());
        }
        println!("cargo llvm-cov finished but produced no parseable output.");
        emit_stderr_tail(db);
        return Ok(());
    }
    println!(
        "cargo llvm-cov started in background. Rerun `rude coverage` in ~1-3 min to see results."
    );
    Ok(())
}

fn run_daemon() -> Result<()> {
    let db = crate::db().to_path_buf();
    let running = db.join(RUNNING_FILE);
    std::fs::write(&running, std::process::id().to_string())
        .with_context(|| format!("failed to write {}", running.display()))?;
    let _guard = DaemonGuard(running.clone());

    let project_root = db.parent().unwrap_or(Path::new("."));
    let output = std::process::Command::new("cargo")
        .args(["llvm-cov", "--json", "--ignore-run-fail"])
        .current_dir(project_root)
        .output()
        .context("failed to spawn cargo llvm-cov (is cargo-llvm-cov installed?)")?;

    if output.status.success() {
        let tmp = db.join(format!("{OUTPUT_FILE}.tmp"));
        std::fs::write(&tmp, &output.stdout)?;
        std::fs::rename(&tmp, db.join(OUTPUT_FILE))?;
    } else {
        std::fs::write(db.join(STDERR_FILE), &output.stderr)?;
    }
    Ok(())
}

struct DaemonGuard(PathBuf);
impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn spawn_daemon(db: &Path, refresh: bool) -> Result<()> {
    if refresh {
        let _ = std::fs::remove_file(db.join(OUTPUT_FILE));
        let _ = std::fs::remove_file(db.join(STDERR_FILE));
    }
    let exe = std::env::current_exe().context("cannot resolve current rude executable")?;
    let log_dir = db.to_path_buf();
    let stdout_log = std::fs::File::create(log_dir.join("coverage.daemon.log"))?;
    let stderr_log = stdout_log.try_clone()?;
    std::process::Command::new(&exe)
        .arg("coverage")
        .arg("--wait")
        .env(DAEMON_ENV, "1")
        .stdin(std::process::Stdio::null())
        .stdout(stdout_log)
        .stderr(stderr_log)
        .spawn()
        .with_context(|| format!("failed to spawn background daemon from {}", exe.display()))?;
    Ok(())
}

fn wait_for_completion(db: &Path, already_elapsed: Option<Duration>) -> Result<()> {
    let running = db.join(RUNNING_FILE);
    let start_elapsed = already_elapsed.unwrap_or(Duration::from_secs(0));
    let start = SystemTime::now();
    eprintln!(
        "  [coverage] waiting for cargo llvm-cov (already {}s elapsed) ...",
        start_elapsed.as_secs()
    );
    let max_wait = Duration::from_secs(60 * 20);
    loop {
        if !running.exists() { return Ok(()); }
        if db.join(OUTPUT_FILE).exists() { return Ok(()); }
        if start.elapsed().unwrap_or_default() > max_wait {
            anyhow::bail!("timeout waiting for cargo llvm-cov (>20 min)");
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

fn running_elapsed(db: &Path) -> Option<Duration> {
    let running = db.join(RUNNING_FILE);
    let meta = std::fs::metadata(&running).ok()?;
    let mtime = meta.modified().ok()?;
    SystemTime::now().duration_since(mtime).ok()
}

fn try_cached_result(db: &Path) -> Option<LlvmCovResult> {
    let engine = rude_db::StorageEngine::open(db).ok()?;
    let bytes = engine.get_cache("llvm_cov").ok().flatten()?;
    let json: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    eprintln!("  [coverage] using cached");
    parse_llvm_cov_json(&json)
}

fn try_ingest_completed_output(db: &Path) -> Result<Option<LlvmCovResult>> {
    let output_path = db.join(OUTPUT_FILE);
    if !output_path.exists() { return Ok(None); }
    let bytes = std::fs::read(&output_path)
        .with_context(|| format!("failed to read {}", output_path.display()))?;
    let json: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => {
            let _ = std::fs::remove_file(&output_path);
            return Ok(None);
        }
    };
    let parsed = parse_llvm_cov_json(&json);
    if let Ok(engine) = rude_db::StorageEngine::open(db) {
        let _ = engine.set_cache("llvm_cov", &bytes);
    }
    let _ = std::fs::remove_file(&output_path);
    let _ = std::fs::remove_file(db.join(STDERR_FILE));
    Ok(parsed)
}

fn emit_stderr_tail(db: &Path) {
    let path = db.join(STDERR_FILE);
    let Ok(content) = std::fs::read_to_string(&path) else { return };
    let tail: Vec<&str> = content.lines().rev().take(5).collect();
    eprintln!("  [coverage] last stderr lines:");
    for line in tail.iter().rev() {
        eprintln!("    {line}");
    }
}

fn print_coverage(cov: &LlvmCovResult, file_filter: Option<&str>) {
    use std::collections::BTreeMap;
    println!("=== test coverage (cargo llvm-cov) ===\n");
    let filtered_files: Vec<&LlvmFileCov> = cov.files.iter()
        .filter(|fc| file_filter.is_none_or(|f| fc.filename.contains(f)))
        .collect();
    let mut crate_cov: BTreeMap<String, (usize, usize, usize, usize)> = BTreeMap::new();
    for fc in &filtered_files {
        let crate_name = extract_crate_name(&fc.filename);
        let entry = crate_cov.entry(crate_name).or_default();
        entry.0 += fc.fn_total;
        entry.1 += fc.fn_covered;
        entry.2 += fc.line_total;
        entry.3 += fc.line_covered;
    }
    println!(
        "{:<28} {:>8} {:>8} {:>10} {:>8} {:>8} {:>10}",
        "crate", "prod_fn", "covered", "fn_cov", "lines", "ln_cov", "ln_%"
    );
    println!("{}", "-".repeat(72));
    for (name, (fn_t, fn_c, ln_t, ln_c)) in &crate_cov {
        let fn_pct = if *fn_t > 0 {
            format!("{:.1}%", *fn_c as f64 / *fn_t as f64 * 100.0)
        } else { "N/A".to_owned() };
        let ln_pct = if *ln_t > 0 {
            format!("{:.1}%", *ln_c as f64 / *ln_t as f64 * 100.0)
        } else { "N/A".to_owned() };
        println!(
            "{:<28} {:>8} {:>8} {:>10} {:>8} {:>8} {:>10}",
            name, fn_t, fn_c, fn_pct, ln_t, ln_c, ln_pct
        );
    }
    println!("{}", "-".repeat(72));
    println!(
        "{:<28} {:>8} {:>8} {:>10} {:>8} {:>8} {:>10}",
        "total",
        cov.fn_total,
        cov.fn_covered,
        format!("{:.1}%", cov.fn_percent),
        cov.line_total,
        cov.line_covered,
        format!("{:.1}%", cov.line_percent),
    );
    println!();
}

struct LlvmCovResult {
    fn_total: usize,
    fn_covered: usize,
    fn_percent: f64,
    line_total: usize,
    line_covered: usize,
    line_percent: f64,
    files: Vec<LlvmFileCov>,
}

struct LlvmFileCov {
    filename: String,
    fn_total: usize,
    fn_covered: usize,
    line_total: usize,
    line_covered: usize,
}

fn parse_llvm_cov_json(json: &serde_json::Value) -> Option<LlvmCovResult> {
    let data = json.get("data")?.get(0)?;
    let totals = data.get("totals")?;
    let functions = totals.get("functions")?;
    let lines = totals.get("lines")?;
    let mut files = Vec::new();
    if let Some(file_array) = data.get("files").and_then(|f| f.as_array()) {
        for entry in file_array {
            let filename = entry.get("filename")?.as_str()?.to_owned();
            let summary = entry.get("summary")?;
            let f = summary.get("functions")?;
            let l = summary.get("lines")?;
            files.push(LlvmFileCov {
                filename,
                fn_total: f.get("count")?.as_u64()? as usize,
                fn_covered: f.get("covered")?.as_u64()? as usize,
                line_total: l.get("count")?.as_u64()? as usize,
                line_covered: l.get("covered")?.as_u64()? as usize,
            });
        }
    }
    Some(LlvmCovResult {
        fn_total: functions.get("count")?.as_u64()? as usize,
        fn_covered: functions.get("covered")?.as_u64()? as usize,
        fn_percent: functions.get("percent")?.as_f64()?,
        line_total: lines.get("count")?.as_u64()? as usize,
        line_covered: lines.get("covered")?.as_u64()? as usize,
        line_percent: lines.get("percent")?.as_f64()?,
        files,
    })
}
