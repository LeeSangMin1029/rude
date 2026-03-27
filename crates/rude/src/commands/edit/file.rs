use std::fs::File;
use std::path::{Path, PathBuf};
use anyhow::{Result, bail};
use fs2::FileExt;

pub(crate) fn locked_edit<F: FnOnce(&str) -> Result<String>>(path: &Path, f: F) -> Result<()> {
    let lock_path = path.with_extension("lock");
    let lock = File::create(&lock_path)?;
    lock.lock_exclusive()?;
    let _cleanup = LockCleanup(&lock, &lock_path);
    let content = std::fs::read_to_string(path)?;
    let new = f(&content)?;
    std::fs::write(path, new)?;
    Ok(())
}

struct LockCleanup<'a>(&'a File, &'a Path);
impl Drop for LockCleanup<'_> {
    fn drop(&mut self) {
        let _ = self.0.unlock();
        let _ = std::fs::remove_file(self.1);
    }
}

pub(crate) fn splice_file(path: &Path, f: impl FnOnce(&mut Vec<String>)) -> Result<()> {
    locked_edit(path, |content| {
        let mut lines: Vec<String> = content.lines().map(String::from).collect();
        let trailing = content.ends_with('\n');
        f(&mut lines);
        let mut out = lines.join("\n");
        if trailing { out.push('\n'); }
        Ok(out)
    })
}

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
