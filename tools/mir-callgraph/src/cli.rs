use std::process::Command;

pub fn run(args: &[String]) {
    let json = args.iter().any(|a| a == "--json");
    let keep_going = args.iter().any(|a| a == "--keep-going");
    let exe = std::env::current_exe().unwrap_or_default();
    let extra: Vec<&String> = args.iter().skip(1)
        .filter(|a| *a != "--json" && *a != "--keep-going").collect();
    let has_package_flag = extra.iter().any(|a| *a == "-p" || a.starts_with("--package"));
    let packages: Vec<String> = if has_package_flag { Vec::new() } else { local_workspace_packages() };

    let tc_hash = Command::new("rustc").arg("+nightly").arg("--version")
        .output().ok().and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|v| {
            let v = v.trim().trim_end_matches(')').trim();
            let date = v.rsplit_once(' ')?.1;
            Some(date.to_owned())
        })
        .unwrap_or_else(|| "unknown".into());
    let target_dir = format!("target/mir-check-{tc_hash}");
    let mut cmd = Command::new("cargo");
    cmd.arg("check").arg("--tests")
        .arg("--target-dir").arg(&target_dir)
        .env("RUSTC_WRAPPER", &exe)
        .env("RUSTUP_TOOLCHAIN", "nightly");
    if keep_going { cmd.arg("--keep-going"); }
    if json { cmd.env("MIR_CALLGRAPH_JSON", "1"); }
    for arg in &extra { cmd.arg(arg); }
    if !has_package_flag { for pkg in &packages { cmd.arg("-p").arg(pkg); } }
    let status = cmd.status().expect("failed to run cargo check");
    std::process::exit(status.code().unwrap_or(1));
}

fn local_workspace_packages() -> Vec<String> {
    let output = Command::new("cargo").args(["metadata", "--no-deps", "--format-version", "1"])
        .output().ok();
    let Some(out) = output.filter(|o| o.status.success()) else { return Vec::new() };
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else { return Vec::new() };
    json.get("packages").and_then(|p| p.as_array()).map(|pkgs| {
        pkgs.iter().filter_map(|p| {
            let id = p.get("id")?.as_str()?;
            if id.starts_with("path+") { Some(id.to_owned()) } else { None }
        }).collect()
    }).unwrap_or_default()
}
