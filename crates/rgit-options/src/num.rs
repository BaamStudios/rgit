// SPDX-License-Identifier: MIT
//! Integer arguments: C `strtol`-style syntax (leading whitespace, sign,
//! `0x` hex, leading-zero octal) followed by an optional `k`/`m`/`g` suffix
//! that scales by 1024, 1024^2 or 1024^3.

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum NumError {
    /// The argument is the empty string.
    Empty,
    /// Not a number (or a negative number where none is allowed).
    Invalid,
    /// Syntactically fine but outside the allowed range.
    Range,
}

/// Parse `s` and check it against `[min, max]`.
pub(crate) fn parse_scaled(
    s: &str,
    min: i128,
    max: i128,
    allow_negative: bool,
) -> Result<i128, NumError> {
    if s.is_empty() {
        return Err(NumError::Empty);
    }
    if !allow_negative && s.contains('-') {
        return Err(NumError::Invalid);
    }
    let t = s.trim_start_matches([' ', '\t', '\n', '\x0b', '\x0c', '\r']);
    let (neg, t) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    let bytes = t.as_bytes();
    let (radix, digits_start) = if bytes.len() >= 2
        && bytes[0] == b'0'
        && (bytes[1] == b'x' || bytes[1] == b'X')
        && bytes.get(2).is_some_and(u8::is_ascii_hexdigit)
    {
        (16, 2)
    } else if bytes.first() == Some(&b'0') {
        (8, 0)
    } else {
        (10, 0)
    };
    let mut value: i128 = 0;
    let mut overflow = false;
    let mut end = digits_start;
    while end < bytes.len() {
        let Some(d) = (bytes[end] as char).to_digit(radix) else {
            break;
        };
        value = match value
            .checked_mul(radix as i128)
            .and_then(|v| v.checked_add(d as i128))
        {
            Some(v) => v,
            None => {
                overflow = true;
                value
            }
        };
        end += 1;
    }
    if end == digits_start {
        return Err(NumError::Invalid);
    }
    let factor: i128 = match &t[end..] {
        "" => 1,
        "k" | "K" => 1 << 10,
        "m" | "M" => 1 << 20,
        "g" | "G" => 1 << 30,
        _ => return Err(NumError::Invalid),
    };
    if overflow {
        return Err(NumError::Range);
    }
    let value = if neg { -value } else { value };
    let value = value.checked_mul(factor).ok_or(NumError::Range)?;
    if value < min || value > max {
        return Err(NumError::Range);
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    const I32: (i128, i128) = (i32::MIN as i128, i32::MAX as i128);

    fn signed(s: &str) -> Result<i128, NumError> {
        parse_scaled(s, I32.0, I32.1, true)
    }

    #[test]
    fn plain_and_scaled() {
        assert_eq!(signed("1729"), Ok(1729));
        assert_eq!(signed("-2345"), Ok(-2345));
        assert_eq!(signed("234k"), Ok(239616));
        assert_eq!(signed("-234K"), Ok(-239616));
        assert_eq!(signed("1g"), Ok(1 << 30));
        assert_eq!(signed("2g"), Err(NumError::Range));
        assert_eq!(signed("-2g"), Ok(-(1 << 31)));
        assert_eq!(signed("-3g"), Err(NumError::Range));
    }

    #[test]
    fn c_syntax() {
        assert_eq!(signed(" 5"), Ok(5));
        assert_eq!(signed("+5"), Ok(5));
        assert_eq!(signed("0x10"), Ok(16));
        assert_eq!(signed("010"), Ok(8));
        assert_eq!(signed("0"), Ok(0));
        assert_eq!(signed("08"), Err(NumError::Invalid));
    }

    #[test]
    fn errors() {
        assert_eq!(signed(""), Err(NumError::Empty));
        assert_eq!(signed("abc"), Err(NumError::Invalid));
        assert_eq!(signed("1kb"), Err(NumError::Invalid));
        assert_eq!(signed("99999999999"), Err(NumError::Range));
        assert_eq!(
            signed("999999999999999999999999999999999999999999"),
            Err(NumError::Range)
        );
        assert_eq!(
            parse_scaled("-1", 0, u64::MAX as i128, false),
            Err(NumError::Invalid)
        );
        assert_eq!(
            parse_scaled("m", 0, u64::MAX as i128, false),
            Err(NumError::Invalid)
        );
        assert_eq!(parse_scaled("3g", 0, u64::MAX as i128, false), Ok(3 << 30));
    }
}
