//! Unit tests for `context_cmd` — is_noise / is_derived_noise filters.

use crate::context_cmd::{is_noise, is_derived_noise};

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

#[test]
fn derived_noise_partial_eq() {
    assert!(is_derived_noise("<dag::TaskMode as std::cmp::PartialEq>::eq"));
    assert!(is_derived_noise("<discovery::TraitImpl as std::cmp::PartialEq>::eq"));
    assert!(is_derived_noise("<dag::Status as core::cmp::PartialEq>::eq"));
}

#[test]
fn derived_noise_other_traits() {
    assert!(is_derived_noise("<Foo as std::clone::Clone>::clone"));
    assert!(is_derived_noise("<Foo as std::fmt::Debug>::fmt"));
    assert!(is_derived_noise("<Foo as core::hash::Hash>::hash"));
    assert!(is_derived_noise("<Foo as std::default::Default>::default"));
    assert!(is_derived_noise("<Foo as std::cmp::Ord>::cmp"));
    assert!(is_derived_noise("<Foo as std::cmp::PartialOrd>::partial_cmp"));
    assert!(is_derived_noise("<Foo as serde::ser::Serialize>::serialize"));
    assert!(is_derived_noise("<Foo as serde::de::Deserialize>::deserialize"));
}

#[test]
fn not_derived_noise() {
    assert!(!is_derived_noise("Config::load"));
    assert!(!is_derived_noise("graph.resolve"));
    assert!(!is_derived_noise("<Foo as MyTrait>::do_stuff"));
    assert!(!is_derived_noise("PartialEq::eq"));
}
