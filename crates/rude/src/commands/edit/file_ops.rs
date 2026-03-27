use std::fs::File;
use std::path::Path;
use anyhow::Result;
use fs2::FileExt;

pub(crate) fn splice_file(path: &Path, f: impl FnOnce(&mut Vec<String>)) -> Result<()> {
    locked_edit(path, |content| {
        let mut lines: Vec<String> = content.lines().map(String::from).collect();
        let trailing = content.ends_with('\n');
        f(&mut lines);
        let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        Ok(join_lines(&refs, trailing))
    })
}

pub(crate) fn op_to_splice<'a>(start: usize, end: usize, op: &'a super::Op, len: usize) -> (std::ops::Range<usize>, Vec<&'a str>) {
    match op {
        super::Op::Replace(b) => (start..(end + 1).min(len), b.trim_end().lines().collect()),
        super::Op::Before(b) => { let mut r: Vec<&str> = b.trim_end().lines().collect(); r.push(""); (start..start, r) }
        super::Op::After(b) => { let pos = (end + 1).min(len); let mut r = vec![""]; r.extend(b.trim_end().lines()); (pos..pos, r) }
        super::Op::Delete => (start..(end + 1).min(len), vec![]),
    }
}

pub(crate) fn op_label(op: &super::Op, start: usize, end: usize) -> String {
    match op {
        super::Op::Replace(_) => format!("Replaced (L{}-{})", start + 1, end + 1),
        super::Op::Before(_) => format!("Inserted before (L{})", start + 1),
        super::Op::After(_) => format!("Inserted after (L{})", end + 1),
        super::Op::Delete => format!("Deleted (L{}-{})", start + 1, end + 1),
    }
}

pub(crate) fn locked_edit<F: FnOnce(&str) -> Result<String>>(path: &Path, f: F) -> Result<()> {
    let lock_path = path.with_extension("lock");
    let lock = File::create(&lock_path)?;
    lock.lock_exclusive()?;
    let content = std::fs::read_to_string(path)?;
    let new = f(&content)?;
    std::fs::write(path, new)?;
    lock.unlock()?;
    let _ = std::fs::remove_file(&lock_path);
    Ok(())
}

pub(crate) fn join_lines(lines: &[&str], trailing_nl: bool) -> String {
    if trailing_nl { lines.join("\n") + "\n" } else { lines.join("\n") }
}
