// SPDX-License-Identifier: MIT

//! `GIT_ALLOC_LIMIT`: the largest single buffer an object may be read
//! into.
//!
//! git's test suite sets `GIT_ALLOC_LIMIT` to prove that large objects are
//! streamed instead of being held in memory: any attempt to allocate more
//! than the limit is fatal. rgit checks the limit where whole objects are
//! read into memory (loose objects, pack entries, objects arriving in a
//! pack stream). Paths that stream never ask.
//!
//! The value takes the unit suffixes `k`, `m` and `g`; `0`, an empty
//! value or an unset variable mean no limit.

use std::sync::OnceLock;

use crate::messages;

/// The environment variable.
pub const ALLOC_LIMIT_ENV: &str = "GIT_ALLOC_LIMIT";

/// A buffer would exceed `GIT_ALLOC_LIMIT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{}", messages::attempting_to_allocate(.size, .limit))]
pub struct AllocLimitError {
    /// The size asked for.
    pub size: u64,
    /// The limit.
    pub limit: u64,
}

/// Parse a `GIT_ALLOC_LIMIT` value: a number with an optional `k`, `m` or
/// `g` suffix (any case). `None` for no limit (empty, `0`) and for values
/// that do not parse.
pub fn parse_limit(value: &str) -> Option<u64> {
    let value = value.trim();
    let (num, mult) = match value.as_bytes().last() {
        Some(b'k' | b'K') => (&value[..value.len() - 1], 1u64 << 10),
        Some(b'm' | b'M') => (&value[..value.len() - 1], 1 << 20),
        Some(b'g' | b'G') => (&value[..value.len() - 1], 1 << 30),
        _ => (value, 1),
    };
    let n = num.parse::<u64>().ok()?.checked_mul(mult)?;
    (n > 0).then_some(n)
}

/// The limit from the environment (read once per process).
pub fn limit() -> Option<u64> {
    static LIMIT: OnceLock<Option<u64>> = OnceLock::new();
    *LIMIT.get_or_init(|| {
        std::env::var(ALLOC_LIMIT_ENV)
            .ok()
            .and_then(|v| parse_limit(&v))
    })
}

/// Check that a buffer of `size` bytes may be allocated.
pub fn check(size: u64) -> Result<(), AllocLimitError> {
    match limit() {
        Some(limit) if size > limit => Err(AllocLimitError { size, limit }),
        _ => Ok(()),
    }
}

/// Check the buffer for an object's content of `size` bytes. git keeps a
/// terminating NUL after the content, so the buffer is one byte larger
/// (and that is the size its message names).
pub fn check_object(size: u64) -> Result<(), AllocLimitError> {
    check(size.saturating_add(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits() {
        assert_eq!(parse_limit("1500k"), Some(1_536_000));
        assert_eq!(parse_limit("1m"), Some(1 << 20));
        assert_eq!(parse_limit("2G"), Some(2 << 30));
        assert_eq!(parse_limit("0"), None);
        assert_eq!(parse_limit(""), None);
        assert_eq!(parse_limit("x"), None);
        let e = AllocLimitError {
            size: 1_500_001,
            limit: 1 << 20,
        };
        assert_eq!(
            e.to_string(),
            "attempting to allocate 1500001 over limit 1048576"
        );
    }
}
