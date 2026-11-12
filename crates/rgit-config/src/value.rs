// SPDX-License-Identifier: MIT
//! Typed interpretation of configuration values.
//!
//! The functions here work on raw values; [`crate::Entry`] wraps them and
//! adds git's error messages with variable name and origin.

use std::path::PathBuf;

use bstr::{BStr, BString, ByteSlice};

use crate::env::Environment;
use crate::origin::path_bytes;

/// Why an integer could not be parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntError {
    /// Not a number, or an unknown unit suffix.
    InvalidUnit,
    /// The number does not fit the requested width.
    OutOfRange,
}

impl IntError {
    /// git's wording (`invalid unit`, `out of range`).
    pub fn reason(self) -> &'static str {
        match self {
            IntError::InvalidUnit => crate::messages::INVALID_UNIT,
            IntError::OutOfRange => crate::messages::OUT_OF_RANGE,
        }
    }
}

/// Width of an integer value. git uses 32 bits for `int`-typed options read
/// with `bool`/`bool-or-int` semantics and 64 bits for `git config --type=int`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntWidth {
    /// `int`
    I32,
    /// `int64_t`
    I64,
    /// `unsigned long` (64-bit on the platforms rgit supports)
    U64,
}

/// Boolean spelled as a word: `true`/`yes`/`on`, `false`/`no`/`off` (any
/// case) and the empty string (`false`). A missing value (`None`) is `true`.
/// Numbers are not accepted here.
pub fn parse_bool_text(value: Option<&[u8]>) -> Option<bool> {
    let Some(v) = value else { return Some(true) };
    if v.is_empty() {
        return Some(false);
    }
    let eq = |w: &str| v.eq_ignore_ascii_case(w.as_bytes());
    if eq("true") || eq("yes") || eq("on") {
        Some(true)
    } else if eq("false") || eq("no") || eq("off") {
        Some(false)
    } else {
        None
    }
}

/// Boolean as git's `--type=bool`: a word, or a 32-bit integer (non-zero is
/// true, unit suffixes allowed).
pub fn parse_bool(value: Option<&[u8]>) -> Option<bool> {
    parse_bool_text(value).or_else(|| {
        value
            .and_then(|v| parse_int(v, IntWidth::I32).ok())
            .map(|n| n != 0)
    })
}

/// Result of `bool-or-int`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoolOrInt {
    /// A boolean word (or a missing value).
    Bool(bool),
    /// A number.
    Int(i64),
}

/// `--type=bool-or-int`: a boolean word, else a 32-bit integer.
pub fn parse_bool_or_int(value: Option<&[u8]>) -> Result<BoolOrInt, IntError> {
    if let Some(b) = parse_bool_text(value) {
        return Ok(BoolOrInt::Bool(b));
    }
    parse_int(value.unwrap_or_default(), IntWidth::I32).map(BoolOrInt::Int)
}

/// C `isspace` in the "C" locale.
fn c_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// Parse a number like C's `strtoimax(s, &end, 0)`: leading whitespace,
/// optional sign, `0x` hex, leading-zero octal or decimal. Returns the value
/// (saturating on overflow), whether it overflowed, and the rest.
fn strtoimax(s: &[u8]) -> Option<(i128, bool, &[u8])> {
    let mut i = 0;
    while i < s.len() && c_space(s[i]) {
        i += 1;
    }
    let mut neg = false;
    if i < s.len() && (s[i] == b'+' || s[i] == b'-') {
        neg = s[i] == b'-';
        i += 1;
    }
    let (radix, start) = if s.len() > i + 2
        && s[i] == b'0'
        && (s[i + 1] | 0x20) == b'x'
        && s[i + 2].is_ascii_hexdigit()
    {
        (16, i + 2)
    } else if i < s.len() && s[i] == b'0' {
        (8, i)
    } else {
        (10, i)
    };
    let mut j = start;
    let mut acc: i128 = 0;
    let mut overflow = false;
    while j < s.len() {
        let Some(d) = (s[j] as char).to_digit(radix) else {
            break;
        };
        acc = acc * radix as i128 + d as i128;
        if acc > u64::MAX as i128 {
            overflow = true;
            acc = u64::MAX as i128;
        }
        j += 1;
    }
    if j == start {
        return None;
    }
    if acc > i64::MAX as i128 + i128::from(neg) {
        overflow = true;
    }
    let v = if neg { -acc } else { acc };
    Some((v, overflow, &s[j..]))
}

/// Integer with optional unit suffix `k`, `m`, `g` (any case), as git's
/// `git_parse_int`/`git_parse_int64`/`git_parse_ulong`.
pub fn parse_int(value: &[u8], width: IntWidth) -> Result<i64, IntError> {
    if width == IntWidth::U64 && value.contains(&b'-') {
        // Negative values are rejected for unsigned options.
        return Err(IntError::InvalidUnit);
    }
    let Some((n, overflow, rest)) = strtoimax(value) else {
        return Err(IntError::InvalidUnit);
    };
    if overflow {
        return Err(IntError::OutOfRange);
    }
    let factor: i128 = match rest {
        b"" => 1,
        [c] => match c.to_ascii_lowercase() {
            b'k' => 1 << 10,
            b'm' => 1 << 20,
            b'g' => 1 << 30,
            _ => return Err(IntError::InvalidUnit),
        },
        _ => return Err(IntError::InvalidUnit),
    };
    let v = n * factor;
    let (min, max): (i128, i128) = match width {
        IntWidth::I32 => (i32::MIN.into(), i32::MAX.into()),
        IntWidth::I64 => (i64::MIN.into(), i64::MAX.into()),
        IntWidth::U64 => (0, i64::MAX.into()),
    };
    if v < min || v > max {
        return Err(IntError::OutOfRange);
    }
    Ok(v as i64)
}

/// Why a path value could not be expanded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathError {
    /// `~` or `~user` could not be resolved.
    UserDir(BString),
}

/// Prefix marking a path whose absence makes the variable count as unset.
pub const OPTIONAL_PREFIX: &str = ":(optional)";

/// Expand a `pathname` value: `~/`, `~user/` and `%(prefix)/`. A value with
/// the `:(optional)` prefix yields `Ok(None)` when the path does not exist.
pub fn interpolate_path(value: &[u8], env: &dyn Environment) -> Result<Option<PathBuf>, PathError> {
    let (optional, v) = match value.strip_prefix(OPTIONAL_PREFIX.as_bytes()) {
        Some(rest) => (true, rest),
        None => (false, value),
    };
    let path = expand_user_path(v, env)?;
    if optional && std::fs::symlink_metadata(&path).is_err() {
        return Ok(None);
    }
    Ok(Some(path))
}

/// Tilde and `%(prefix)` expansion without the `:(optional)` handling.
pub fn expand_user_path(v: &[u8], env: &dyn Environment) -> Result<PathBuf, PathError> {
    let fail = || PathError::UserDir(v.into());
    if let Some(rest) = v.strip_prefix(b"%(prefix)/") {
        let mut out = env
            .runtime_prefix()
            .map(|p| path_bytes(&p))
            .unwrap_or_default();
        out.push(b'/');
        out.extend_from_slice(rest);
        return Ok(bytes_to_path(&out));
    }
    let Some(after) = v.strip_prefix(b"~") else {
        return Ok(bytes_to_path(v));
    };
    let (user, rest) = match after.find_byte(b'/') {
        Some(i) => (&after[..i], &after[i..]),
        None => (after, &b""[..]),
    };
    let home = if user.is_empty() {
        env.var_bytes("HOME")
            .filter(|h| !h.is_empty())
            .ok_or_else(fail)?
    } else {
        let user = std::str::from_utf8(user).map_err(|_| fail())?;
        path_bytes(&env.user_home(user).ok_or_else(fail)?)
    };
    let mut out = home;
    out.extend_from_slice(rest);
    Ok(bytes_to_path(&out))
}

pub(crate) fn bytes_to_path(b: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        PathBuf::from(std::ffi::OsStr::from_bytes(b))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(b.to_str_lossy().into_owned())
    }
}

/// Canonical output of `git config --type=bool`.
pub fn format_bool(b: bool) -> &'static BStr {
    if b {
        b"true".as_bstr()
    } else {
        b"false".as_bstr()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::MapEnv;

    #[test]
    fn ints() {
        use IntError::*;
        let i64 = |s: &str| parse_int(s.as_bytes(), IntWidth::I64);
        let i32 = |s: &str| parse_int(s.as_bytes(), IntWidth::I32);
        assert_eq!(i64("10k"), Ok(10240));
        assert_eq!(i64("0x10"), Ok(16));
        assert_eq!(i64("-0x10"), Ok(-16));
        assert_eq!(i64("010"), Ok(8));
        assert_eq!(i64("09"), Err(InvalidUnit));
        assert_eq!(i64("0x"), Err(InvalidUnit));
        assert_eq!(i64(" 5"), Ok(5));
        assert_eq!(i64("5 "), Err(InvalidUnit));
        assert_eq!(i64("+5"), Ok(5));
        assert_eq!(i64("+-1"), Err(InvalidUnit));
        assert_eq!(i64("1G"), Ok(1 << 30));
        assert_eq!(i64("1t"), Err(InvalidUnit));
        assert_eq!(i64("k"), Err(InvalidUnit));
        assert_eq!(i64(""), Err(InvalidUnit));
        assert_eq!(i64("9223372036854775807"), Ok(i64::MAX));
        assert_eq!(i64("9223372036854775808"), Err(OutOfRange));
        assert_eq!(i64("-9223372036854775808"), Ok(i64::MIN));
        assert_eq!(i64("8589934592k"), Ok(8796093022208));
        assert_eq!(i32("2147483648"), Err(OutOfRange));
        assert_eq!(i32("-2147483648"), Ok(i32::MIN as i64));
        assert_eq!(i32("8g"), Err(OutOfRange));
    }

    #[test]
    fn bools() {
        assert_eq!(parse_bool(None), Some(true));
        assert_eq!(parse_bool(Some(b"")), Some(false));
        assert_eq!(parse_bool(Some(b"YeS")), Some(true));
        assert_eq!(parse_bool(Some(b"nO")), Some(false));
        assert_eq!(parse_bool(Some(b"-1k")), Some(true));
        assert_eq!(parse_bool(Some(b"00")), Some(false));
        assert_eq!(parse_bool(Some(b"8g")), None);
        assert_eq!(parse_bool(Some(b"1true")), None);
        assert_eq!(parse_bool_or_int(Some(b"1")), Ok(BoolOrInt::Int(1)));
        assert_eq!(parse_bool_or_int(Some(b"on")), Ok(BoolOrInt::Bool(true)));
        assert_eq!(parse_bool_or_int(Some(b"xx")), Err(IntError::InvalidUnit));
    }

    #[test]
    fn paths() {
        let env = MapEnv::new([("HOME", "/home/u")]).with_user("root", "/root");
        let p = |s: &str| expand_user_path(s.as_bytes(), &env);
        assert_eq!(p("~").unwrap(), PathBuf::from("/home/u"));
        assert_eq!(p("~/x").unwrap(), PathBuf::from("/home/u/x"));
        assert_eq!(p("~root/x").unwrap(), PathBuf::from("/root/x"));
        assert_eq!(p("a/~/b").unwrap(), PathBuf::from("a/~/b"));
        assert_eq!(p("%(prefix)").unwrap(), PathBuf::from("%(prefix)"));
        assert_eq!(p("~nosuch/x"), Err(PathError::UserDir("~nosuch/x".into())));
        let no_home = MapEnv::new([("HOME", "")]);
        assert!(expand_user_path(b"~/x", &no_home).is_err());
        assert_eq!(
            interpolate_path(b":(optional)/nonexistent/rgit-test", &env),
            Ok(None)
        );
        assert_eq!(
            interpolate_path(b":(optional)/", &env),
            Ok(Some(PathBuf::from("/")))
        );
    }
}
