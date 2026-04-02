use std::path::{Path, PathBuf};
use std::process::Command;
use anyhow::{Result, Context, bail};
use serde::Deserialize;

#[derive(Deserialize)]
pub struct ExtCallEdge {
    pub caller: String,
    pub callee: String,
    pub file: String,
    pub line: usize,
    #[serde(default)]
    pub caller_file: String,
    #[serde(default)]
    pub caller_start: usize,
    #[serde(default)]
    pub caller_end: usize,
}

#[derive(Deserialize)]
pub struct ExtChunk {
    pub name: String,
    pub file: String,
    pub kind: String,
    #[serde(alias = "start_line")]
    #[serde(default)]
    pub start: usize,
    #[serde(alias = "end_line")]
    #[serde(default)]
    pub end: usize,
    #[serde(default)]
    pub signature: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub crate_name: String,
}

#[derive(Deserialize)]
pub struct TsOutput {
    pub edges: Vec<ExtCallEdge>,
    pub chunks: Vec<ExtChunk>,
}

pub enum ProjectLang {
    Rust,
    Go,
    TypeScript,
    Unknown,
}

pub fn detect_lang(project_dir: &Path) -> ProjectLang {
    if project_dir.join("Cargo.toml").exists() { return ProjectLang::Rust; }
    if project_dir.join("go.mod").exists() { return ProjectLang::Go; }
    if project_dir.join("tsconfig.json").exists() || project_dir.join("package.json").exists() {
        return ProjectLang::TypeScript;
    }
    ProjectLang::Unknown
}

pub fn run_go_callgraph(project_dir: &Path) -> Result<(Vec<ExtCallEdge>, Vec<ExtChunk>)> {
    let bin = find_go_callgraph_bin()?;
    eprintln!("  [go] running go-callgraph...");
    let mut cmd = Command::new(&bin);
    cmd.arg("./...").current_dir(project_dir);
    augment_go_path(&mut cmd);
    let output = cmd.output().context("failed to run go-callgraph")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("go-callgraph failed: {stderr}");
    }
    let go_out: TsOutput = serde_json::from_slice(&output.stdout)
        .context("failed to parse go-callgraph JSON")?;
    Ok((go_out.edges, go_out.chunks))
}

pub fn run_ts_callgraph(project_dir: &Path) -> Result<(Vec<ExtCallEdge>, Vec<ExtChunk>)> {
    let script = find_ts_callgraph_script()?;
    eprintln!("  [ts] running ts-callgraph...");
    let output = Command::new("node")
        .arg(&script)
        .arg(project_dir)
        .output()
        .context("failed to run ts-callgraph")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("ts-callgraph failed: {stderr}");
    }
    let ts_out: TsOutput = serde_json::from_slice(&output.stdout)
        .context("failed to parse ts-callgraph JSON")?;
    Ok((ts_out.edges, ts_out.chunks))
}

fn find_go_callgraph_bin() -> Result<PathBuf> {
    let name = if cfg!(windows) { "go-callgraph.exe" } else { "go-callgraph" };
    if let Ok(exe) = std::env::current_exe() {
        let sibling = exe.with_file_name(name);
        if sibling.exists() { return Ok(sibling); }
        // dev: exe is target/release/rude.exe → project root is ../../
        if let Some(project_root) = exe.parent().and_then(|p| p.parent()).and_then(|p| p.parent()) {
            let dev = project_root.join("tools").join("go-callgraph").join(name);
            if dev.exists() { return Ok(dev); }
        }
    }
    let home = rude_util::home_dir().context("no home dir")?;
    let cached = home.join(".rude").join("bin").join(name);
    if cached.exists() { return Ok(cached); }
    let dev = Path::new("tools/go-callgraph").join(name);
    if dev.exists() { return Ok(dev); }
    bail!("go-callgraph not found. Build it: cd tools/go-callgraph && go build .")
}

fn find_ts_callgraph_script() -> Result<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(project_root) = exe.parent().and_then(|p| p.parent()).and_then(|p| p.parent()) {
            let dev = project_root.join("tools").join("ts-callgraph").join("dist").join("index.js");
            if dev.exists() { return Ok(dev); }
        }
    }
    let home = rude_util::home_dir().context("no home dir")?;
    let cached = home.join(".rude").join("lib").join("ts-callgraph").join("dist").join("index.js");
    if cached.exists() { return Ok(cached); }
    let dev = PathBuf::from("tools/ts-callgraph/dist/index.js");
    if dev.exists() { return Ok(dev); }
    bail!("ts-callgraph not found. Build it: cd tools/ts-callgraph && npm install && npx tsc")
}

fn chunks_from_edges(edges: &[ExtCallEdge]) -> Vec<ExtChunk> {
    use std::collections::HashMap;
    let mut seen: HashMap<String, ExtChunk> = HashMap::new();
    for e in edges {
        let key = format!("{}:{}", e.caller_file, e.caller);
        seen.entry(key).or_insert_with(|| ExtChunk {
            name: e.caller.clone(),
            file: e.caller_file.clone(),
            kind: "function".to_string(),
            start: e.caller_start,
            end: e.caller_end,
            signature: String::new(),
            text: String::new(),
            crate_name: String::new(),
        });
    }
    seen.into_values().collect()
}

fn augment_go_path(cmd: &mut Command) {
    let candidates: &[&str] = if cfg!(windows) {
        &["C:\\go\\bin", "C:\\Program Files\\Go\\bin"]
    } else {
        &["/usr/local/go/bin"]
    };
    let path_var = std::env::var("PATH").unwrap_or_default();
    let sep = if cfg!(windows) { ";" } else { ":" };
    for c in candidates {
        if !path_var.contains(c) && Path::new(c).exists() {
            cmd.env("PATH", format!("{}{}{}", c, sep, path_var));
            return;
        }
    }
}
