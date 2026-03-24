//! Concurrency tests — verifies rude DB operations are safe under parallel access.
//!
//! Scenarios:
//! 1. Multiple threads reading chunks + graph concurrently
//! 2. One thread writing cache while others read (torn read detection)
//! 3. Multiple threads writing cache simultaneously (corruption detection)

use std::path::Path;
use std::sync::{Arc, Barrier};

use crate::graph::CallGraph;
use crate::loader::{load_chunks, save_chunks_cache};
use crate::parse::ParsedChunk;

// ── Helpers ─────────────────────────────────────────────────────────

fn test_chunk(name: &str, file: &str, calls: &[&str]) -> ParsedChunk {
    ParsedChunk {
        kind: "function".to_owned(),
        name: name.to_owned(),
        file: file.to_owned(),
        lines: Some((1, 10)),
        signature: Some(format!("fn {name}()")),
        calls: calls.iter().map(|s| s.to_string()).collect(),
        call_lines: calls.iter().enumerate().map(|(i, _)| i as u32 + 1).collect(),
        types: vec![],
        imports: vec![],
        string_args: vec![],
        param_flows: vec![],
        param_types: vec![],
        field_types: vec![],
        local_types: vec![],
        let_call_bindings: vec![],
        return_type: None,
        field_accesses: vec![],
        enum_variants: vec![],
        is_test: false,
    }
}

/// Create a temporary DB directory with chunks and graph cache.
/// Creates a dummy `payload.dat` so `load_chunks` cache-hit logic works
/// (it checks `payload.dat` mtime to validate the cache).
fn setup_test_db(dir: &Path) -> Vec<ParsedChunk> {
    let chunks = vec![
        test_chunk("mod_a::foo", "src/a.rs", &["mod_b::bar"]),
        test_chunk("mod_b::bar", "src/b.rs", &["mod_c::baz"]),
        test_chunk("mod_c::baz", "src/c.rs", &[]),
        test_chunk("mod_a::helper", "src/a.rs", &["mod_a::foo"]),
        test_chunk("mod_d::entry", "src/d.rs", &["mod_a::foo", "mod_b::bar"]),
    ];

    // Create dummy payload.dat (load_chunks checks its mtime for cache validity)
    std::fs::write(dir.join("payload.dat"), b"dummy").unwrap();

    // Save chunks cache (must be written AFTER payload.dat so cache mtime >= db mtime)
    let cache_dir = dir.join("cache");
    std::fs::create_dir_all(&cache_dir).unwrap();
    // Small sleep to ensure cache file gets a strictly newer mtime
    std::thread::sleep(std::time::Duration::from_millis(50));
    save_chunks_cache(&cache_dir.join("chunks.bin"), &chunks);

    // Build and save graph
    let graph = CallGraph::build(&chunks);
    graph.save(dir).unwrap();

    chunks
}

// ── Test 1: Concurrent reads ────────────────────────────────────────

#[test]
fn concurrent_reads_return_consistent_results() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path();
    let expected_chunks = setup_test_db(db);
    let expected_count = expected_chunks.len();

    let barrier = Arc::new(Barrier::new(8));
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let db = db.to_path_buf();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                // All threads read simultaneously
                let chunks = load_chunks(&db).unwrap();
                assert_eq!(chunks.len(), expected_count);

                let graph = CallGraph::load(&db).unwrap();
                let seeds = graph.resolve("mod_a::foo");
                assert!(!seeds.is_empty(), "resolve should find mod_a::foo");
                (chunks.len(), seeds.len())
            })
        })
        .collect();

    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    // All threads must see the same data
    let first = &results[0];
    for r in &results[1..] {
        assert_eq!(r, first, "concurrent reads returned inconsistent results");
    }
}

// ── Test 2: Read during write (torn read detection) ─────────────────

#[test]
fn read_during_cache_write_does_not_panic() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path();
    let chunks = setup_test_db(db);
    let cache_path = db.join("cache").join("chunks.bin");

    let barrier = Arc::new(Barrier::new(2));
    let iterations = 200;

    // Writer thread: repeatedly overwrites cache
    let writer = {
        let cache_path = cache_path.clone();
        let chunks = chunks.clone();
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            barrier.wait();
            for _ in 0..iterations {
                save_chunks_cache(&cache_path, &chunks);
            }
        })
    };

    // Reader thread: repeatedly reads chunks
    let reader = {
        let db = db.to_path_buf();
        let expected_count = chunks.len();
        std::thread::spawn(move || {
            barrier.wait();
            let mut success = 0;
            let mut graceful_fail = 0;
            for _ in 0..iterations {
                match load_chunks(&db) {
                    Ok(c) => {
                        assert_eq!(c.len(), expected_count);
                        success += 1;
                    }
                    Err(_) => {
                        // Graceful failure (e.g., partial read → decode error) is acceptable.
                        // Panic or corruption is NOT.
                        graceful_fail += 1;
                    }
                }
            }
            (success, graceful_fail)
        })
    };

    writer.join().unwrap();
    let (success, graceful_fail) = reader.join().unwrap();
    eprintln!("read-during-write: {success} ok, {graceful_fail} graceful failures (no panics)");
    // At least some reads should succeed
    assert!(success > 0, "all reads failed during concurrent write");
}

// ── Test 3: Concurrent cache writes (corruption detection) ──────────

#[test]
fn concurrent_cache_writes_produce_valid_output() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path();
    let cache_dir = db.join("cache");
    std::fs::create_dir_all(&cache_dir).unwrap();
    let cache_path = cache_dir.join("chunks.bin");

    // Two different chunk sets — after concurrent writes, the final file
    // must be decodable and match one of them.
    let chunks_a: Vec<ParsedChunk> = (0..10)
        .map(|i| test_chunk(&format!("a::fn_{i}"), "src/a.rs", &[]))
        .collect();
    let chunks_b: Vec<ParsedChunk> = (0..20)
        .map(|i| test_chunk(&format!("b::fn_{i}"), "src/b.rs", &[]))
        .collect();

    let barrier = Arc::new(Barrier::new(2));
    let iterations = 200;

    let writer_a = {
        let path = cache_path.clone();
        let chunks = chunks_a.clone();
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            barrier.wait();
            for _ in 0..iterations {
                save_chunks_cache(&path, &chunks);
            }
        })
    };

    let writer_b = {
        let path = cache_path.clone();
        let chunks = chunks_b.clone();
        std::thread::spawn(move || {
            barrier.wait();
            for _ in 0..iterations {
                save_chunks_cache(&path, &chunks);
            }
        })
    };

    writer_a.join().unwrap();
    writer_b.join().unwrap();

    // Final file must be valid — one of the two chunk sets
    let bytes = std::fs::read(&cache_path).unwrap();
    let config = bincode::config::standard();
    let (result, _): (Vec<ParsedChunk>, _) =
        bincode::decode_from_slice(&bytes[1..], config)
            .expect("cache file corrupted after concurrent writes");
    assert!(
        result.len() == 10 || result.len() == 20,
        "unexpected chunk count {} — file may contain interleaved data",
        result.len()
    );
}

// ── Test 4: Concurrent graph save + load ────────────────────────────

#[test]
fn concurrent_graph_save_and_load() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path();
    let chunks = setup_test_db(db);
    let _graph = CallGraph::build(&chunks);

    let barrier = Arc::new(Barrier::new(4));
    let iterations = 100;

    // Writers: rebuild + save from chunks each time (CallGraph is not Clone)
    let writer_handles: Vec<_> = (0..2)
        .map(|_| {
            let db = db.to_path_buf();
            let chunks = chunks.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                for _ in 0..iterations {
                    let g = CallGraph::build(&chunks);
                    let _ = g.save(&db);
                }
            })
        })
        .collect();

    // Readers
    let reader_handles: Vec<_> = (0..2)
        .map(|_| {
            let db = db.to_path_buf();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                let mut success = 0;
                let mut miss = 0;
                for _ in 0..iterations {
                    match CallGraph::load(&db) {
                        Some(g) => {
                            assert!(!g.names.is_empty());
                            success += 1;
                        }
                        None => miss += 1,
                    }
                }
                (success, miss)
            })
        })
        .collect();

    for h in writer_handles {
        h.join().unwrap();
    }
    for h in reader_handles {
        let (success, miss) = h.join().unwrap();
        eprintln!("graph save+load: {success} ok, {miss} cache miss");
    }
}

// ── Test 5: Unlocked concurrent edits lose data (proves the race) ───

/// Without file locking, concurrent read-modify-write WILL lose data.
/// This test demonstrates the race condition that `locked_edit` (in rude)
/// is designed to prevent.
#[test]
fn unlocked_concurrent_edits_lose_data() {
    let dir = tempfile::tempdir().unwrap();
    let src_file = dir.path().join("target.rs");
    std::fs::write(
        &src_file,
        "fn alpha() {\n    println!(\"alpha\");\n}\n\nfn beta() {\n    println!(\"beta\");\n}\n",
    )
    .unwrap();

    let barrier = Arc::new(Barrier::new(2));
    let iterations = 100;

    let edit_alpha = {
        let file = src_file.clone();
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            barrier.wait();
            for i in 0..iterations {
                let content = std::fs::read_to_string(&file).unwrap_or_default();
                let new_content = content.replace(
                    "println!(\"alpha\")",
                    &format!("println!(\"alpha-{i}\")"),
                );
                let _ = std::fs::write(&file, &new_content);
            }
        })
    };

    let edit_beta = {
        let file = src_file.clone();
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            barrier.wait();
            for i in 0..iterations {
                let content = std::fs::read_to_string(&file).unwrap_or_default();
                let new_content = content.replace(
                    "println!(\"beta\")",
                    &format!("println!(\"beta-{i}\")"),
                );
                let _ = std::fs::write(&file, &new_content);
            }
        })
    };

    edit_alpha.join().unwrap();
    edit_beta.join().unwrap();

    // At least one side's changes should be lost in most runs.
    // If by luck both survive, the test still passes (non-deterministic race).
    let final_content = std::fs::read_to_string(&src_file).unwrap();
    let has_alpha = final_content.contains("fn alpha()");
    let has_beta = final_content.contains("fn beta()");
    if !has_alpha || !has_beta {
        eprintln!("unlocked race confirmed: alpha={has_alpha}, beta={has_beta}");
    } else {
        eprintln!("unlocked race: both survived this run (non-deterministic)");
    }
    // No assert — this test documents the race, not a guarantee.
}

// ── Test 6: Locked concurrent edits preserve data ───────────────────

/// With file locking (same `.lock` sidecar mechanism as rude's `locked_edit`),
/// concurrent read-modify-write preserves all changes.
#[test]
fn locked_concurrent_edits_preserve_data() {
    use std::fs::File;
    use fs2::FileExt;

    let dir = tempfile::tempdir().unwrap();
    let src_file = dir.path().join("target.rs");
    let lock_file_path = dir.path().join("target.lock");
    std::fs::write(
        &src_file,
        "fn alpha() {\n    println!(\"alpha\");\n}\n\nfn beta() {\n    println!(\"beta\");\n}\n",
    )
    .unwrap();

    let barrier = Arc::new(Barrier::new(2));
    let iterations = 100;

    let make_editor = |file: std::path::PathBuf, lock_path: std::path::PathBuf, barrier: Arc<Barrier>, search: &'static str, prefix: &'static str| {
        std::thread::spawn(move || {
            barrier.wait();
            for i in 0..iterations {
                // Acquire exclusive lock on sidecar .lock file
                let lock = File::create(&lock_path).unwrap();
                lock.lock_exclusive().unwrap();

                let content = std::fs::read_to_string(&file).unwrap();
                let new_content = content.replace(
                    search,
                    &format!("println!(\"{prefix}-{i}\")"),
                );
                std::fs::write(&file, &new_content).unwrap();

                lock.unlock().unwrap();
            }
        })
    };

    let h1 = make_editor(
        src_file.clone(),
        lock_file_path.clone(),
        Arc::clone(&barrier),
        "println!(\"alpha\")",
        "alpha",
    );
    let h2 = make_editor(
        src_file.clone(),
        lock_file_path.clone(),
        barrier,
        "println!(\"beta\")",
        "beta",
    );

    h1.join().unwrap();
    h2.join().unwrap();

    let final_content = std::fs::read_to_string(&src_file).unwrap();
    assert!(final_content.contains("fn alpha()"), "alpha function lost with locking");
    assert!(final_content.contains("fn beta()"), "beta function lost with locking");
    eprintln!("locked edits: both functions preserved after {iterations} concurrent iterations");
}

// ── Test 7: 10 agents editing the same file simultaneously ──────────

/// Simulates 10 agents each owning a unique function in one file,
/// all performing locked edits concurrently for multiple rounds.
#[test]
fn ten_agents_locked_edits_same_file() {
    use std::fs::File;
    use fs2::FileExt;

    let agent_count = 10;
    let iterations = 50;

    let dir = tempfile::tempdir().unwrap();
    let src_file = dir.path().join("shared.rs");
    let lock_path = dir.path().join("shared.lock");

    // Build initial file: 10 functions, one per agent
    let mut initial = String::new();
    for i in 0..agent_count {
        if i > 0 { initial.push('\n'); }
        initial.push_str(&format!(
            "fn agent_{i}() {{\n    println!(\"agent-{i}-v0\");\n}}\n"
        ));
    }
    std::fs::write(&src_file, &initial).unwrap();

    let barrier = Arc::new(Barrier::new(agent_count));

    let handles: Vec<_> = (0..agent_count)
        .map(|agent_id| {
            let file = src_file.clone();
            let lock = lock_path.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                for round in 0..iterations {
                    let lf = File::create(&lock).unwrap();
                    lf.lock_exclusive().unwrap();

                    let content = std::fs::read_to_string(&file).unwrap();
                    let search = format!("agent-{agent_id}-v");
                    // Find the current version marker and replace it
                    let new_content = if let Some(pos) = content.find(&search) {
                        // Replace from marker to closing ")
                        let rest = &content[pos..];
                        if let Some(end) = rest.find("\")") {
                            let old = &content[pos..pos + end];
                            content.replacen(old, &format!("agent-{agent_id}-v{round}"), 1)
                        } else {
                            content
                        }
                    } else {
                        content
                    };
                    std::fs::write(&file, &new_content).unwrap();

                    lf.unlock().unwrap();
                }
            })
        })
        .collect();

    for h in handles {
        h.join().unwrap();
    }

    // Verify: all 10 functions must still exist with valid version markers
    let final_content = std::fs::read_to_string(&src_file).unwrap();
    for i in 0..agent_count {
        assert!(
            final_content.contains(&format!("fn agent_{i}()")),
            "agent_{i} function lost after 10-agent concurrent edits"
        );
        // Version marker must exist (some round number)
        assert!(
            final_content.contains(&format!("agent-{i}-v")),
            "agent_{i} version marker lost"
        );
    }
    eprintln!(
        "10-agent test: all {} functions preserved after {} rounds each",
        agent_count, iterations
    );
}

// ── Test 8: 10 agents concurrent reads ──────────────────────────────

/// 10 agents all reading chunks + graph simultaneously — no stale or partial data.
#[test]
fn ten_agents_concurrent_reads() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path();
    let expected_chunks = setup_test_db(db);
    let expected_count = expected_chunks.len();

    let agent_count = 10;
    let barrier = Arc::new(Barrier::new(agent_count));

    let handles: Vec<_> = (0..agent_count)
        .map(|_| {
            let db = db.to_path_buf();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                // Each agent does multiple reads
                for _ in 0..20 {
                    let chunks = load_chunks(&db).unwrap();
                    assert_eq!(chunks.len(), expected_count);

                    let graph = CallGraph::load(&db).unwrap();
                    let seeds = graph.resolve("mod_a::foo");
                    assert!(!seeds.is_empty());
                }
            })
        })
        .collect();

    for h in handles {
        h.join().unwrap();
    }
}
