// SPDX-License-Identifier: MIT
//! Configuration from the command line and the environment: `-c`,
//! `--config-env`, `GIT_CONFIG_PARAMETERS` and
//! `GIT_CONFIG_COUNT`/`GIT_CONFIG_KEY_<n>`/`GIT_CONFIG_VALUE_<n>`.
//!
//! git passes `-c` settings to child processes through
//! `GIT_CONFIG_PARAMETERS`; [`CommandLine::to_parameters`] produces that
//! encoding (shell single quotes, `'key'='value'`, `'key'=` for a value-less
//! key).

use bstr::{BStr, BString, ByteSlice};

use crate::env::Environment;
use crate::error::Error;
use crate::messages;

/// One raw `key[=value]` pair, not yet validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parameter {
    /// The key as given.
    pub key: BString,
    /// The value; `None` for the implicit `true`.
    pub value: Option<BString>,
}

/// Settings given with `-c` and `--config-env`, in command-line order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandLine {
    /// The parameters.
    pub parameters: Vec<Parameter>,
}

impl CommandLine {
    /// Add `-c <name>[=<value>]`: split at the first `=`.
    pub fn push_c(&mut self, arg: impl AsRef<[u8]>) {
        let arg = arg.as_ref();
        let (key, value) = match arg.find_byte(b'=') {
            Some(i) => (&arg[..i], Some(BString::from(&arg[i + 1..]))),
            None => (arg, None),
        };
        self.parameters.push(Parameter {
            key: key.into(),
            value,
        });
    }

    /// Add `--config-env=<name>=<envvar>`: split at the last `=`, read the
    /// value from the environment variable. Errors are fatal in git.
    pub fn push_config_env(&mut self, arg: &str, env: &dyn Environment) -> Result<(), Error> {
        if arg.is_empty() {
            return Err(Error::ConfigEnv(messages::no_config_env_key().into()));
        }
        let Some(i) = arg.rfind('=') else {
            return Err(Error::ConfigEnv(messages::invalid_config_format(arg)));
        };
        let (key, var) = (&arg[..i], &arg[i + 1..]);
        if var.is_empty() {
            return Err(Error::ConfigEnv(messages::missing_env_name(key)));
        }
        let Some(value) = env.var_bytes(var) else {
            return Err(Error::ConfigEnv(messages::missing_env_var(var, key)));
        };
        self.parameters.push(Parameter {
            key: key.into(),
            value: Some(value),
        });
        Ok(())
    }

    /// The value of `GIT_CONFIG_PARAMETERS` for child processes: `existing`
    /// (the inherited value) followed by this command line's settings.
    pub fn to_parameters(&self, existing: Option<&[u8]>) -> BString {
        let mut out = BString::from(existing.unwrap_or_default());
        for p in &self.parameters {
            if !out.is_empty() {
                out.push(b' ');
            }
            sq_quote(&mut out, &p.key);
            out.push(b'=');
            if let Some(v) = &p.value {
                sq_quote(&mut out, v);
            }
        }
        out
    }
}

/// Append `s` in shell single quotes (`'` and `!` are escaped outside the
/// quotes, as git does).
fn sq_quote(out: &mut BString, s: &[u8]) {
    out.push(b'\'');
    for &c in s {
        if c == b'\'' || c == b'!' {
            out.extend_from_slice(b"'\\");
            out.push(c);
            out.push(b'\'');
        } else {
            out.push(c);
        }
    }
    out.push(b'\'');
}

fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r')
}

/// Read one single-quoted word starting at `*pos`.
fn sq_dequote(s: &[u8], pos: &mut usize) -> Option<BString> {
    if s.get(*pos) != Some(&b'\'') {
        return None;
    }
    *pos += 1;
    let mut out = BString::default();
    loop {
        let c = *s.get(*pos)?;
        *pos += 1;
        if c != b'\'' {
            out.push(c);
            continue;
        }
        // Closing quote: either the word ends or `\'`/`\!` follows,
        // after which the quote must reopen.
        if s.get(*pos) != Some(&b'\\') {
            return Some(out);
        }
        let e = *s.get(*pos + 1)?;
        if e != b'\'' && e != b'!' {
            return None;
        }
        out.push(e);
        if s.get(*pos + 2) != Some(&b'\'') {
            return None;
        }
        *pos += 3;
    }
}

/// Parse `GIT_CONFIG_PARAMETERS`. Old-style entries (`'key=value'`) are split
/// at the first `=`; new-style entries are `'key'='value'` or `'key'=`.
pub fn parse_parameters(s: &[u8]) -> Result<Vec<Parameter>, Error> {
    let bogus = || Error::CommandLine(messages::BOGUS_PARAMETERS.into());
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < s.len() {
        let word = sq_dequote(s, &mut pos).ok_or_else(bogus)?;
        if s.get(pos) == Some(&b'=') {
            pos += 1;
            let value = if pos >= s.len() || is_space(s[pos]) {
                None
            } else {
                Some(sq_dequote(s, &mut pos).ok_or_else(bogus)?)
            };
            out.push(Parameter { key: word, value });
        } else {
            let (key, value) = match word.find_byte(b'=') {
                Some(i) => (
                    BString::from(&word[..i]),
                    Some(BString::from(&word[i + 1..])),
                ),
                None => (word, None),
            };
            out.push(Parameter { key, value });
        }
        if pos < s.len() {
            if !is_space(s[pos]) {
                return Err(bogus());
            }
            while pos < s.len() && is_space(s[pos]) {
                pos += 1;
            }
        }
    }
    Ok(out)
}

/// Read `GIT_CONFIG_COUNT`, `GIT_CONFIG_KEY_<n>`, `GIT_CONFIG_VALUE_<n>`.
pub fn env_count_parameters(env: &dyn Environment) -> Result<Vec<Parameter>, Error> {
    let err = |m: String| Error::CommandLine(m);
    let Some(count) = env.var_bytes("GIT_CONFIG_COUNT") else {
        return Ok(Vec::new());
    };
    let count = parse_count(&count).map_err(|m| err(m.into()))?;
    let mut out = Vec::with_capacity(count);
    for n in 0..count {
        let key = env
            .var_bytes(&format!("GIT_CONFIG_KEY_{n}"))
            .ok_or_else(|| err(messages::missing_env_key(n)))?;
        let value = env
            .var_bytes(&format!("GIT_CONFIG_VALUE_{n}"))
            .ok_or_else(|| err(messages::missing_env_value(n)))?;
        out.push(Parameter {
            key,
            value: Some(value),
        });
    }
    Ok(out)
}

/// `strtoul`-like parse of the count; empty means 0.
fn parse_count(s: &[u8]) -> Result<usize, &'static str> {
    if s.is_empty() {
        return Ok(0);
    }
    let mut i = 0;
    while i < s.len() && matches!(s[i], b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r') {
        i += 1;
    }
    let mut neg = false;
    if i < s.len() && (s[i] == b'+' || s[i] == b'-') {
        neg = s[i] == b'-';
        i += 1;
    }
    let digits = &s[i..];
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return Err(messages::BOGUS_COUNT);
    }
    let mut n: u128 = 0;
    for d in digits {
        n = (n * 10 + u128::from(d - b'0')).min(u128::from(u64::MAX) + 1);
    }
    if neg && n != 0 || n > i32::MAX as u128 {
        return Err(messages::TOO_MANY_ENTRIES);
    }
    Ok(n as usize)
}

/// Validate a command-line key and canonicalize it.
pub(crate) fn canonical_key(key: &BStr) -> Result<BString, Error> {
    if key.is_empty() {
        return Err(Error::CommandLine(messages::EMPTY_KEY.into()));
    }
    crate::key::canonicalize(key).map_err(|e| Error::CommandLine(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::MapEnv;

    fn p(k: &str, v: Option<&str>) -> Parameter {
        Parameter {
            key: k.into(),
            value: v.map(Into::into),
        }
    }

    #[test]
    fn encode_roundtrip() {
        let mut cl = CommandLine::default();
        cl.push_c("alias.e=!env");
        cl.push_c("x.Y.z=it's");
        cl.push_c("flag.on");
        cl.push_c("empty.v=");
        cl.push_c("a.b=c=d");
        let enc = cl.to_parameters(Some(b"'q.r=1'"));
        assert_eq!(
            enc,
            "'q.r=1' 'alias.e'=''\\!'env' 'x.Y.z'='it'\\''s' 'flag.on'= 'empty.v'='' 'a.b'='c=d'"
        );
        let back = parse_parameters(&enc).unwrap();
        assert_eq!(back[0], p("q.r", Some("1")));
        assert_eq!(back[1..], cl.parameters[..]);
    }

    #[test]
    fn parameters() {
        let ok = |s: &str| parse_parameters(s.as_bytes()).unwrap();
        assert_eq!(ok(""), vec![]);
        assert_eq!(
            ok("'key.one=foo'  'key.ambiguous=section.whatever=value'"),
            vec![
                p("key.one", Some("foo")),
                p("key.ambiguous", Some("section.whatever=value"))
            ]
        );
        assert_eq!(
            ok("'key.with=equals.oldbool' 'key.with=equals.newbool'="),
            vec![
                p("key.with", Some("equals.oldbool")),
                p("key.with=equals.newbool", None)
            ]
        );
        assert_eq!(
            ok("'env.one=one'\\''' 'env.two=two'")[0],
            p("env.one", Some("one'"))
        );
        for bad in [
            "'env.one=one'\\' 'env.two=two'",
            "  ",
            "'a.b=c",
            "a.b=c",
            "'a.b'='c'x",
        ] {
            assert!(parse_parameters(bad.as_bytes()).is_err(), "{bad}");
        }
    }

    #[test]
    fn count() {
        assert_eq!(parse_count(b""), Ok(0));
        assert_eq!(parse_count(b" 1"), Ok(1));
        assert_eq!(parse_count(b"10a"), Err(messages::BOGUS_COUNT));
        assert_eq!(parse_count(b"0x1"), Err(messages::BOGUS_COUNT));
        assert_eq!(parse_count(b"-1"), Err(messages::TOO_MANY_ENTRIES));
        assert_eq!(
            parse_count(b"9999999999999999"),
            Err(messages::TOO_MANY_ENTRIES)
        );
        let env = MapEnv::new([("GIT_CONFIG_COUNT", "1"), ("GIT_CONFIG_KEY_0", "a.b")]);
        let e = env_count_parameters(&env).unwrap_err();
        assert_eq!(e.report()[0].1, "missing config value GIT_CONFIG_VALUE_0");
    }

    #[test]
    fn config_env() {
        let env = MapEnv::new([("ENVV", "hello")]);
        let mut cl = CommandLine::default();
        cl.push_config_env("s.k=ENVV", &env).unwrap();
        assert_eq!(cl.parameters, vec![p("s.k", Some("hello"))]);
        let msg = |a: &str| cl.clone().push_config_env(a, &env).unwrap_err().to_string();
        assert_eq!(msg("foo.flag"), "invalid config format: foo.flag");
        assert_eq!(
            msg("foo.flag="),
            "missing environment variable name for configuration 'foo.flag'"
        );
        assert_eq!(
            msg("foo.flag=NONEX"),
            "missing environment variable 'NONEX' for configuration 'foo.flag'"
        );
    }
}
