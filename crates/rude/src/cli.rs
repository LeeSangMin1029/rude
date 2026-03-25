//! rude CLI definition — code intelligence commands only.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// rude: Code intelligence CLI.
#[derive(Parser)]
#[command(name = "rude")]
#[command(author, version, about = "Code intelligence: structural analysis, clone detection, and reasoning")]
pub struct Cli {
    /// Path to the database directory (auto-detected if omitted).
    #[arg(long, global = true)]
    pub db: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
#[expect(clippy::large_enum_variant, reason = "clap derive enums are parsed once, size is irrelevant")]
pub enum Commands {
    /// Print global path alias mapping (stable across all commands).
    Aliases,
    /// List symbols in the database (functions, structs, enums, impls).
    Symbols {
        /// Filter by symbol name (substring match).
        #[arg(short, long)]
        name: Option<String>,
        /// Filter by kind (function, struct, enum, impl, trait, etc.).
        #[arg(short, long)]
        kind: Option<String>,
        /// Include test symbols in results (excluded by default).
        #[arg(long)]
        include_tests: bool,
        /// Max number of symbols to show.
        #[arg(short, long)]
        limit: Option<usize>,
        /// Compact output: name and location only (no signatures).
        #[arg(long)]
        compact: bool,
    },
    /// Unified context: definition + callers + callees + types + tests.
    #[command(visible_alias = "ctx")]
    Context {
        /// Symbol name to look up.
        symbol: String,
        /// Max BFS depth (default: 1).
        #[arg(long, default_value = "1")]
        depth: u32,
        /// Include source code inline for each entry.
        #[arg(long, short = 's')]
        source: bool,
        /// Show individual test entries (default: summary count only).
        #[arg(long)]
        include_tests: bool,
        /// Filter results to symbols whose file path contains this prefix.
        #[arg(long)]
        scope: Option<String>,
        /// Show DFS callee tree instead of grouped context.
        #[arg(long)]
        tree: bool,
        /// Show blast radius (callers only with depth tags + summary).
        #[arg(long)]
        blast: bool,
    },
    /// Find shortest call path between two symbols.
    #[command(visible_alias = "tr")]
    Trace {
        /// Source symbol name.
        from: String,
        /// Target symbol name.
        to: String,
    },
    /// Find duplicate code (token Jaccard default, --ast structural).
    #[command(visible_alias = "dup")]
    Dupes {
        /// Similarity threshold (Jaccard, 0.0-1.0).
        #[arg(long, default_value = "0.5")]
        threshold: f32,
        /// Exclude test functions from comparison.
        #[arg(long)]
        exclude_tests: bool,
        /// Max number of results to show.
        #[arg(short, long, default_value = "50")]
        k: usize,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
        /// Use AST structural hash (Type-1/2, ignores identifier names).
        #[arg(long)]
        ast: bool,
        /// Unified pipeline: Filter (AST+MinHash) → Verify (all signals).
        #[arg(long)]
        all: bool,
        /// Skip functions shorter than N lines.
        #[arg(long, default_value = "5")]
        min_lines: usize,
        /// Minimum sub-block size (lines) for intra-function clone detection.
        #[arg(long, default_value = "5")]
        min_sub_lines: usize,
        /// Analyze duplicate pairs: callee/caller match, blast radius, merge safety.
        #[arg(long)]
        analyze: bool,
    },
    /// Show per-crate code statistics (functions, structs, enums).
    Stats,
    /// Per-crate test coverage with per-function test counts.
    #[command(visible_alias = "cov")]
    Coverage {
        /// BFS depth from test functions (0 = unlimited).
        #[arg(long, default_value = "0")]
        depth: u32,
        /// Filter by file path suffix (e.g. "add.rs" or "commands/intel").
        #[arg(long)]
        file: Option<String>,
        /// Force re-run llvm-cov (ignore cache).
        #[arg(long)]
        refresh: bool,
    },
    /// Find dead code: functions with no callers (unreachable).
    Dead {
        /// Include pub functions (excluded by default — may be API entry points).
        #[arg(long)]
        include_pub: bool,
        /// Filter by file path suffix.
        #[arg(long)]
        file: Option<String>,
    },
    /// Add/update code files in the database (auto-incremental).
    Add {
        /// Path to code folder or single file.
        input: PathBuf,
        /// Glob patterns to exclude from scanning.
        #[arg(short, long)]
        exclude: Vec<String>,
    },
    /// Replace a symbol's body with new content.
    #[command(visible_alias = "rep")]
    Replace {
        /// Symbol name to replace.
        symbol: String,
        /// Restrict to file (suffix match).
        #[arg(long)]
        file: Option<String>,
        /// New body content (reads from stdin if omitted).
        #[arg(long, conflicts_with = "body_file")]
        body: Option<String>,
        /// Read body from file (avoids bash quoting issues).
        #[arg(long)]
        body_file: Option<PathBuf>,
    },
    /// Insert content after a symbol.
    InsertAfter {
        /// Symbol name to insert after.
        symbol: String,
        /// Restrict to file (suffix match).
        #[arg(long)]
        file: Option<String>,
        /// Content to insert (reads from stdin if omitted).
        #[arg(long, conflicts_with = "body_file")]
        body: Option<String>,
        /// Read body from file (avoids bash quoting issues).
        #[arg(long)]
        body_file: Option<PathBuf>,
    },
    /// Insert content before a symbol.
    InsertBefore {
        /// Symbol name to insert before.
        symbol: String,
        /// Restrict to file (suffix match).
        #[arg(long)]
        file: Option<String>,
        /// Content to insert (reads from stdin if omitted).
        #[arg(long, conflicts_with = "body_file")]
        body: Option<String>,
        /// Read body from file (avoids bash quoting issues).
        #[arg(long)]
        body_file: Option<PathBuf>,
    },
    /// Delete a symbol from its source file.
    #[command(visible_alias = "del")]
    DeleteSymbol {
        /// Symbol name to delete.
        symbol: String,
        /// Restrict to file (suffix match).
        #[arg(long)]
        file: Option<String>,
    },
    /// Insert content at a specific line number (before that line).
    #[command(visible_alias = "ia")]
    InsertAt {
        /// File path relative to project root.
        file: String,
        /// 1-based line number to insert before.
        #[arg(long)]
        line: usize,
        /// Content to insert (reads from stdin if omitted).
        #[arg(long, conflicts_with = "body_file")]
        body: Option<String>,
        /// Read body from file (avoids bash quoting issues).
        #[arg(long)]
        body_file: Option<PathBuf>,
    },
    /// Delete a range of lines from a file.
    #[command(visible_alias = "dl")]
    DeleteLines {
        /// File path relative to project root.
        file: String,
        /// 1-based start line (inclusive).
        #[arg(long)]
        start: usize,
        /// 1-based end line (inclusive).
        #[arg(long)]
        end: usize,
    },
    /// Replace a range of lines with new content.
    #[command(visible_alias = "rl")]
    ReplaceLines {
        /// File path relative to project root.
        file: String,
        /// 1-based start line (inclusive).
        #[arg(long)]
        start: usize,
        /// 1-based end line (inclusive).
        #[arg(long)]
        end: usize,
        /// Replacement content (reads from stdin if omitted).
        #[arg(long, conflicts_with = "body_file")]
        body: Option<String>,
        /// Read body from file (avoids bash quoting issues).
        #[arg(long)]
        body_file: Option<PathBuf>,
    },
    /// Analyze independent function clusters in a file
    Cluster {
        /// File path suffix to filter (e.g. "src/dag_runner.rs").
        #[arg(long)]
        file: String,
        /// Minimum total lines for a cluster to be a split candidate.
        #[arg(long, default_value = "50")]
        min_lines: usize,
    },
    /// Watch for file changes and auto-update the code database
    Watch {
        /// Input directory to watch
        input: PathBuf,
    },
    /// Create a new file at a project-relative path.
    #[command(visible_alias = "cf")]
    CreateFile {
        /// File path relative to project root.
        file: String,
        /// File content (reads from stdin if omitted).
        #[arg(long, conflicts_with = "body_file")]
        body: Option<String>,
        /// Read body from file (avoids bash quoting issues).
        #[arg(long)]
        body_file: Option<PathBuf>,
    },
    /// Split symbols into a new module file.
    Split {
        /// Comma-separated symbol names to extract.
        #[arg(long)]
        symbols: String,
        /// Target file path (relative to project root).
        #[arg(long)]
        to: String,
        /// Preview changes without modifying files.
        #[arg(long)]
        dry_run: bool,
    },
}
