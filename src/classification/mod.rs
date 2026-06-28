use std::sync::OnceLock;

use ::regex::Regex;

pub mod chain;
pub mod fewshot;
pub mod llm;
pub mod regex;
pub mod types;

pub(crate) fn code_block_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?s)```[^`]*```").expect("code_block_re regex must be valid"))
}
