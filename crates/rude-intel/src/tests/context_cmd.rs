//! Unit tests for `context_cmd` — is_noise filter.

use crate::context_cmd::is_noise;

// ── is_noise ─────────────────────────────────────────────────────────

#[test]
fn noise_self_calls() {
    assert!(is_noise("self.process"));
    assert!(is_noise("self.engine.start"));
}

#[test]
fn noise_common_std_methods() {
    assert!(is_noise("clone"));
    assert!(is_noise("to_string"));
    assert!(is_noise("to_owned"));
    assert!(is_noise("unwrap"));
    assert!(is_noise("expect"));
    assert!(is_noise("collect"));
    assert!(is_noise("iter"));
    assert!(is_noise("push"));
    assert!(is_noise("len"));
    assert!(is_noise("is_empty"));
    assert!(is_noise("fmt"));
}

#[test]
fn noise_receiver_std_methods() {
    assert!(is_noise("x.clone"));
    assert!(is_noise("data.iter"));
    assert!(is_noise("result.unwrap"));
}

#[test]
fn noise_std_constructors() {
    assert!(is_noise("Ok"));
    assert!(is_noise("Err"));
    assert!(is_noise("Some"));
    assert!(is_noise("None"));
    assert!(is_noise("format"));
    assert!(is_noise("println"));
    assert!(is_noise("vec"));
}

#[test]
fn not_noise_project_calls() {
    assert!(!is_noise("process"));
    assert!(!is_noise("Config::load"));
    assert!(!is_noise("db.query"));
    assert!(!is_noise("engine.start"));
}
