fn main() {
    // Compute hash of graph-related source files for cache invalidation.
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for file in ["src/graph.rs", "src/edge_resolve.rs", "src/parse.rs"] {
        if let Ok(content) = std::fs::read_to_string(file) {
            std::hash::Hash::hash(&content, &mut hasher);
        }
    }
    let hash = std::hash::Hasher::finish(&hasher);
    println!("cargo::rustc-env=GRAPH_SOURCE_HASH={hash:016x}");
}
