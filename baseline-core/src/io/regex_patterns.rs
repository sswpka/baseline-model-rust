use regex::Regex;
use std::sync::LazyLock;

pub static WHITESPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());
pub static E225_HEADER: LazyLock<Regex> = LazyLock::new(|| Regex::new("E225[0-9A-F]+").unwrap());
