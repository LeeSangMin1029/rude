#![expect(clippy::expect_used)]

mod types;
mod output;
mod analyze;
mod run;

pub use run::run;

pub struct DupesConfig {
    pub threshold: f32,
    pub exclude_tests: bool,
    pub k: usize,
    pub json: bool,
    pub ast_mode: bool,
    pub all_mode: bool,
    pub min_lines: usize,
    pub min_sub_lines: usize,
    pub analyze: bool,
}
