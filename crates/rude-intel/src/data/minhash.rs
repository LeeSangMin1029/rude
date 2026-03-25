use std::hash::{Hash, Hasher};

pub const MINHASH_K: usize = 64;

pub fn code_tokens(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut in_block_comment = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if in_block_comment {
            if let Some(pos) = trimmed.find("*/") {
                in_block_comment = false;
                tokenize_line(&trimmed[pos + 2..], &mut tokens);
            }
            continue;
        }
        if trimmed.starts_with("/*") {
            if !trimmed.contains("*/") { in_block_comment = true; }
            continue;
        }
        if trimmed.starts_with("//") || trimmed.starts_with('#') { continue; }
        let code = trimmed.find("//").map_or(trimmed, |pos| &trimmed[..pos]);
        tokenize_line(code, &mut tokens);
    }
    tokens
}

fn tokenize_line(code: &str, tokens: &mut Vec<String>) {
    for word in code.split(|c: char| !c.is_alphanumeric() && c != '_') {
        if word.is_empty() { continue; }
        if word.chars().all(|c| c.is_ascii_digit()) {
            tokens.push("$N".to_owned());
        } else {
            tokens.push(word.to_owned());
        }
    }
}

pub fn minhash_signature(tokens: &[String], k: usize) -> Vec<u64> {
    let bigrams: Vec<String> = tokens.windows(2).map(|w| format!("{}_{}", w[0], w[1])).collect();
    let mut features: Vec<&str> = tokens.iter().map(|t| t.as_str()).collect();
    for b in &bigrams { features.push(b.as_str()); }
    (0..k).map(|seed| {
        features.iter().map(|feature| {
            let mut hasher = std::hash::DefaultHasher::new();
            (seed as u64).hash(&mut hasher);
            feature.hash(&mut hasher);
            hasher.finish()
        }).min().unwrap_or(u64::MAX)
    }).collect()
}

pub fn jaccard_from_minhash(a: &[u64], b: &[u64]) -> f64 {
    if a.len() != b.len() || a.is_empty() { return 0.0; }
    let matches = a.iter().zip(b.iter()).filter(|(x, y)| x == y).count();
    matches as f64 / a.len() as f64
}

pub fn minhash_to_hex(sig: &[u64]) -> String {
    let mut hex = String::with_capacity(sig.len() * 16);
    for h in sig { use std::fmt::Write; let _ = write!(hex, "{h:016x}"); }
    hex
}

pub fn minhash_from_hex(hex: &str) -> Option<Vec<u64>> {
    if hex.len() % 16 != 0 { return None; }
    let k = hex.len() / 16;
    let mut sig = Vec::with_capacity(k);
    for i in 0..k {
        sig.push(u64::from_str_radix(&hex[i * 16..(i + 1) * 16], 16).ok()?);
    }
    Some(sig)
}
