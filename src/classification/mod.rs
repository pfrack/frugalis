use std::sync::OnceLock;

use ::regex::Regex;

pub(crate) mod chain;
pub(crate) mod fewshot;
pub(crate) mod llm;
pub(crate) mod regex;
pub(crate) mod types;

pub(crate) fn code_block_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?s)```[^`]*```").expect("code_block_re regex must be valid"))
}
