//! Reasoning storage — design decisions and history for code symbols.
//!
//! Stores `ReasonEntry` as JSON files inside `<db>/reasons/<hash>.json`.
//! Human-editable, no separate indexing needed.
//!
//! ## HOW TO EXTEND
//! - Add new fields to `ReasonEntry` (serde will handle missing fields via `default`).
//! - Add query helpers (e.g., search by constraint keyword).

use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Deserializer, Serialize};

/// A reasoning entry attached to a code symbol.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReasonEntry {
    /// Fully qualified symbol name (e.g., `DagState::update_status`).
    pub symbol: String,
    /// Design decision summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<String>,
    /// Why this decision was made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    /// Active constraints.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraints: Vec<String>,
    /// Rejected alternatives (structured with reason and condition).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[serde(deserialize_with = "deserialize_rejected")]
    pub rejected: Vec<RejectedAlternative>,
    /// Chronological history of changes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<HistoryItem>,
    /// Source file path for location-based fallback lookup (#1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
    /// Line range `(start, end)` for location-based fallback lookup (#1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_range: Option<(usize, usize)>,
    /// Related symbols for cross-referencing (#6).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related_symbols: Vec<String>,
}

/// A single history entry recording an action on the symbol.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryItem {
    /// Action type: create, modify, note, etc.
    pub action: String,
    /// Date string (YYYY-MM-DD).
    pub date: String,
    /// Free-form note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// What failed (for modify actions).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    /// How it was fixed (for modify actions).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
    /// Root cause symbol name (for attributing failure to another symbol).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_cause: Option<String>,
    /// Whether this failure has been resolved.
    #[serde(default, skip_serializing_if = "is_false")]
    pub resolved: bool,
    /// Git commit hash at the time of this history entry (#5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

/// A rejected alternative with structured reason and optional condition.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RejectedAlternative {
    /// The approach that was rejected.
    pub approach: String,
    /// Why it was rejected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Under what condition it fails (e.g., "only in async context").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
}

impl std::fmt::Display for RejectedAlternative {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.approach)?;
        if let Some(ref reason) = self.reason {
            write!(f, " ({reason})")?;
        }
        if let Some(ref condition) = self.condition {
            write!(f, " [when: {condition}]")?;
        }
        Ok(())
    }
}

fn is_false(v: &bool) -> bool {
    !v
}

/// Deserialize `rejected` from either `Vec<String>` (legacy) or
/// `Vec<RejectedAlternative>` (new format).
fn deserialize_rejected<'de, D>(deserializer: D) -> std::result::Result<Vec<RejectedAlternative>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Item {
        Structured(RejectedAlternative),
        Plain(String),
    }

    let items: Vec<Item> = Vec::deserialize(deserializer)?;
    Ok(items
        .into_iter()
        .map(|item| match item {
            Item::Structured(r) => r,
            Item::Plain(s) => RejectedAlternative {
                approach: s,
                reason: None,
                condition: None,
            },
        })
        .collect())
}

// ── Path helpers ──────────────────────────────────────────────────────────

fn reasons_dir(db: &Path) -> PathBuf {
    db.join("reasons")
}

fn symbol_hash(symbol: &str) -> String {
    let mut hasher = DefaultHasher::new();
    symbol.hash(&mut hasher);
    format!("{:x}", hasher.finish())
}

fn reason_path(db: &Path, symbol: &str) -> PathBuf {
    reasons_dir(db).join(format!("{}.json", symbol_hash(symbol)))
}

// ── Public API ────────────────────────────────────────────────────────────

/// Save a reason entry to disk with file locking for concurrent safety.
///
/// Uses a `.lock` file alongside the JSON file. Retries up to 3 times
/// with 1-second waits on lock contention. Reads are lock-free (stale
/// reads are acceptable).
///
/// Also maintains a location index (`reasons/_loc_index.json`) mapping
/// `file:line_start` to symbol names for fallback lookup (#1).
pub fn save_reason(db: &Path, entry: &ReasonEntry) -> Result<()> {
    let dir = reasons_dir(db);
    fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create reasons dir: {}", dir.display()))?;

    let path = reason_path(db, &entry.symbol);
    let lock_path = path.with_extension("json.lock");

    let json = serde_json::to_string_pretty(entry)
        .context("failed to serialize reason entry")?;

    // Acquire lock with retry
    let _guard = acquire_lock(&lock_path)?;

    fs::write(&path, &json)
        .with_context(|| format!("failed to write reason file: {}", path.display()))?;

    // Update location index if file_path is set
    if entry.file_path.is_some() {
        let _ = update_location_index(db, entry);
    }

    Ok(())
}

/// Simple file-lock guard. Removes the lock file on drop.
struct LockGuard {
    path: PathBuf,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Try to acquire an exclusive lock via atomic file creation.
/// Retries up to 3 times with 1-second delay.
fn acquire_lock(lock_path: &Path) -> Result<LockGuard> {
    const MAX_RETRIES: u32 = 3;
    const RETRY_DELAY: Duration = Duration::from_secs(1);

    for attempt in 0..MAX_RETRIES {
        // Try to create lock file exclusively
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(lock_path)
        {
            Ok(_file) => {
                return Ok(LockGuard {
                    path: lock_path.to_path_buf(),
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // Check if the lock file is stale (older than 30 seconds)
                if let Ok(metadata) = fs::metadata(lock_path)
                    && let Ok(modified) = metadata.modified()
                    && let Ok(age) = modified.elapsed()
                    && age > Duration::from_secs(30)
                {
                    // Stale lock — remove and retry immediately
                    let _ = fs::remove_file(lock_path);
                    continue;
                }

                if attempt < MAX_RETRIES - 1 {
                    thread::sleep(RETRY_DELAY);
                }
            }
            Err(e) => {
                return Err(e)
                    .with_context(|| format!("failed to create lock file: {}", lock_path.display()));
            }
        }
    }
    bail!(
        "failed to acquire lock after {MAX_RETRIES} attempts: {}",
        lock_path.display()
    )
}

/// Load a reason entry by symbol name. Returns `None` if not found.
pub fn load_reason(db: &Path, symbol: &str) -> Result<Option<ReasonEntry>> {
    let path = reason_path(db, symbol);
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(&path)
        .with_context(|| format!("failed to read reason file: {}", path.display()))?;
    let entry: ReasonEntry = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse reason file: {}", path.display()))?;
    Ok(Some(entry))
}

/// Load a reason entry with fallback: try symbol name first, then
/// fall back to `file:line` location index (#1 rename resilience).
pub fn load_reason_with_fallback(
    db: &Path,
    symbol: &str,
    file_path: Option<&str>,
    line: Option<usize>,
) -> Result<Option<ReasonEntry>> {
    // Primary: by symbol name
    if let Some(entry) = load_reason(db, symbol)? {
        return Ok(Some(entry));
    }
    // Fallback: by file:line location index
    if let (Some(fp), Some(ln)) = (file_path, line) {
        let key = location_key(fp, ln);
        let index = load_location_index(db)?;
        if let Some(old_symbol) = index.get(&key)
            && let Some(entry) = load_reason(db, old_symbol)?
        {
            return Ok(Some(entry));
        }
    }
    Ok(None)
}

// ── Location index helpers (#1) ─────────────────────────────────────

/// File path for the location index mapping.
fn location_index_path(db: &Path) -> PathBuf {
    reasons_dir(db).join("_loc_index.json")
}

/// Build a location key from file path and line number.
fn location_key(file_path: &str, line_start: usize) -> String {
    format!("{file_path}:{line_start}")
}

/// Load the location index (file:line -> symbol name).
fn load_location_index(db: &Path) -> Result<std::collections::HashMap<String, String>> {
    let path = location_index_path(db);
    if !path.exists() {
        return Ok(std::collections::HashMap::new());
    }
    let content = fs::read_to_string(&path)
        .with_context(|| format!("failed to read location index: {}", path.display()))?;
    let index = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse location index: {}", path.display()))?;
    Ok(index)
}

/// Update the location index with the entry's file:line info.
fn update_location_index(db: &Path, entry: &ReasonEntry) -> Result<()> {
    let Some(ref fp) = entry.file_path else {
        return Ok(());
    };
    let line_start = entry.line_range.map_or(0, |(start, _)| start);
    let key = location_key(fp, line_start);

    let mut index = load_location_index(db)?;
    index.insert(key, entry.symbol.clone());

    let path = location_index_path(db);
    let json = serde_json::to_string_pretty(&index)
        .context("failed to serialize location index")?;
    fs::write(&path, json)
        .with_context(|| format!("failed to write location index: {}", path.display()))?;
    Ok(())
}

/// Delete a reason entry by symbol name. Returns true if deleted.
pub fn delete_reason(db: &Path, symbol: &str) -> Result<bool> {
    let path = reason_path(db, symbol);
    if path.exists() {
        fs::remove_file(&path)
            .with_context(|| format!("failed to delete reason file: {}", path.display()))?;
        Ok(true)
    } else {
        Ok(false)
    }
}

/// List all reason entries in the database.
pub fn list_reasons(db: &Path) -> Result<Vec<ReasonEntry>> {
    let dir = reasons_dir(db);
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut entries = Vec::new();
    for item in fs::read_dir(&dir)
        .with_context(|| format!("failed to read reasons dir: {}", dir.display()))?
    {
        let item = item?;
        let path = item.path();
        if path.extension().is_some_and(|ext| ext == "json")
            && let Ok(content) = fs::read_to_string(&path)
            && let Ok(entry) = serde_json::from_str::<ReasonEntry>(&content)
        {
            entries.push(entry);
        }
    }

    entries.sort_by(|a, b| a.symbol.cmp(&b.symbol));
    Ok(entries)
}

/// Get today's date as YYYY-MM-DD string.
pub fn today() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Simple date calculation (no chrono dependency needed)
    let days = now / 86400;
    let (year, month, day) = days_to_ymd(days);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Convert days since Unix epoch to (year, month, day).
fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    // Algorithm from http://howardhinnant.github.io/date_algorithms.html
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// Mark the last unresolved failure in the history as resolved.
/// Returns `true` if a failure was found and marked.
pub fn resolve_last_failure(entry: &mut ReasonEntry) -> bool {
    for item in entry.history.iter_mut().rev() {
        if item.failure.is_some() && !item.resolved {
            item.resolved = true;
            return true;
        }
    }
    false
}

/// Invalidate the last unresolved failure: mark resolved and prepend
/// `[INVALIDATED: reason]` to the note field.
/// Returns `true` if a failure was found and invalidated.
pub fn invalidate_last_failure(entry: &mut ReasonEntry, reason: &str) -> bool {
    for item in entry.history.iter_mut().rev() {
        if item.failure.is_some() && !item.resolved {
            item.resolved = true;
            let tag = format!("[INVALIDATED: {reason}]");
            item.note = Some(match item.note.take() {
                Some(existing) => format!("{tag} {existing}"),
                None => tag,
            });
            return true;
        }
    }
    false
}

/// Get the current git HEAD short commit hash (#5).
/// Returns `None` if not in a git repo or git is unavailable.
pub fn get_git_head() -> Option<String> {
    std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                let s = String::from_utf8_lossy(&output.stdout).trim().to_owned();
                if s.is_empty() { None } else { Some(s) }
            } else {
                None
            }
        })
}

/// Find all reason entries that reference the given symbol in their
/// `related_symbols` list (#6 bidirectional cross-reference).
pub fn find_related_reasons(db: &Path, symbol: &str) -> Result<Vec<ReasonEntry>> {
    let all = list_reasons(db)?;
    Ok(all
        .into_iter()
        .filter(|e| e.related_symbols.iter().any(|s| s == symbol))
        .collect())
}

/// Format a one-line summary of a reason entry for inline display.
pub fn one_line_summary(entry: &ReasonEntry) -> String {
    if let Some(ref decision) = entry.decision {
        if let Some(ref why) = entry.why {
            format!("{decision} -- {why}")
        } else {
            decision.clone()
        }
    } else if let Some(ref why) = entry.why {
        why.clone()
    } else if !entry.constraints.is_empty() {
        format!("constraints: {}", entry.constraints.join("; "))
    } else {
        "reason recorded".to_owned()
    }
}
