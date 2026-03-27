use std::path::{Path, PathBuf};
use anyhow::{Result, bail};

pub(crate) fn resolve_abs_path(db: &Path, file: &str) -> Result<PathBuf> {
    let cwd = std::env::current_dir().unwrap_or_default();
    let db_dir = db.parent().unwrap_or(Path::new("."));
    let p = PathBuf::from(file);
    if p.is_absolute() { return Ok(rude_db::strip_unc_prefix_path(&p)); }
    let try_cwd = cwd.join(file);
    if try_cwd.exists() { return Ok(try_cwd); }
    let try_db = db_dir.canonicalize().unwrap_or(db_dir.to_path_buf()).join(file);
    Ok(rude_db::strip_unc_prefix_path(&try_db))
}

pub(crate) fn resolve_path(db: &Path, file: &str) -> Result<(PathBuf, String)> {
    let abs = resolve_abs_path(db, file)?;
    if !abs.exists() { bail!("File not found: {}", abs.display()); }
    let rel = relative_display(db, file);
    Ok((abs, rel))
}

pub(crate) fn relative_display(db: &Path, file: &str) -> String {
    let cwd = std::env::current_dir().unwrap_or_default();
    let root = if cwd.join(file).exists() { cwd } else {
        db.parent().unwrap_or(Path::new(".")).canonicalize().unwrap_or_default()
    };
    let norm = file.replace('\\', "/");
    let root_s = rude_db::strip_unc_prefix(&root.to_string_lossy()).replace('\\', "/");
    norm.strip_prefix(&format!("{root_s}/")).unwrap_or(&norm).to_string()
}

pub(crate) fn check_line(line: usize) -> Result<()> {
    if line == 0 { bail!("--line must be >= 1"); }
    Ok(())
}

pub(crate) fn check_range(start: usize, end: usize) -> Result<()> {
    if start == 0 || end == 0 { bail!("--start/--end must be >= 1"); }
    if start > end { bail!("--start ({start}) > --end ({end})"); }
    Ok(())
}
