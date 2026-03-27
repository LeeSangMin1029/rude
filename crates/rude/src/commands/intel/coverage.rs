
use anyhow::Result;

use rude_util::extract_crate_name;

pub fn run_coverage(
    _file_filter: Option<String>,
    refresh: bool,
) -> Result<()> {
    use std::collections::BTreeMap;

    let llvm_cov_result = run_llvm_cov(refresh);

    let Some(cov) = llvm_cov_result else {
        println!("cargo llvm-cov not available. Install with: cargo install cargo-llvm-cov");
        return Ok(());
    };

    println!("=== test coverage (cargo llvm-cov) ===\n");

    let mut crate_cov: BTreeMap<String, (usize, usize, usize, usize)> = BTreeMap::new();
    for fc in &cov.files {
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
    println!("{}", "-".repeat(84));

    for (name, (fn_t, fn_c, ln_t, ln_c)) in &crate_cov {
        let fn_pct = if *fn_t > 0 {
            format!("{:.1}%", *fn_c as f64 / *fn_t as f64 * 100.0)
        } else {
            "N/A".to_owned()
        };
        let ln_pct = if *ln_t > 0 {
            format!("{:.1}%", *ln_c as f64 / *ln_t as f64 * 100.0)
        } else {
            "N/A".to_owned()
        };
        println!(
            "{:<28} {:>8} {:>8} {:>10} {:>8} {:>8} {:>10}",
            name, fn_t, fn_c, fn_pct, ln_t, ln_c, ln_pct
        );
    }

    println!("{}", "-".repeat(84));
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

    Ok(())
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

fn run_llvm_cov(refresh: bool) -> Option<LlvmCovResult> {
    let db = crate::db();
    let engine = rude_db::StorageEngine::open(db).ok();

    if !refresh {
        if let Some(ref eng) = engine {
            if let Ok(Some(bytes)) = eng.get_cache("llvm_cov") {
                eprintln!("  [coverage] using cached");
                let json: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
                return parse_llvm_cov_json(&json);
            }
        }
    }

    let project_root = db.parent()?;
    eprintln!("  [coverage] running cargo llvm-cov --json ...");
    let output = std::process::Command::new("cargo")
        .args(["llvm-cov", "--json", "--ignore-run-fail"])
        .current_dir(project_root)
        .output()
        .ok()?;
    if !output.status.success() { return None; }

    if let Some(ref eng) = engine {
        let _ = eng.set_cache("llvm_cov", &output.stdout);
    }

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    parse_llvm_cov_json(&json)
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
