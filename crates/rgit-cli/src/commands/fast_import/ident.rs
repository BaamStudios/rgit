// SPDX-License-Identifier: MIT
//! Identities of `author`, `committer` and `tagger` lines and their dates
//! in the formats of `--date-format` (Documentation/git-fast-import.adoc,
//! "Date Formats").

use crate::messages::fast_import as msg;

/// `--date-format`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateFormat {
    Raw,
    RawPermissive,
    Rfc2822,
    Now,
}

impl DateFormat {
    pub fn parse(s: &str) -> Option<DateFormat> {
        Some(match s {
            "raw" => DateFormat::Raw,
            "raw-permissive" => DateFormat::RawPermissive,
            "rfc2822" => DateFormat::Rfc2822,
            "now" => DateFormat::Now,
            _ => return None,
        })
    }
}

fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

/// `<time> SP <offutc>`: the time as an unsigned decimal (C `strtoumax`
/// rules, so blanks and a sign may lead), the offset as a sign and
/// digits; strictly at most 14 hours.
fn valid_raw_date(s: &[u8], strict: bool) -> bool {
    let mut i = 0;
    while i < s.len() && is_space(s[i]) {
        i += 1;
    }
    if i < s.len() && (s[i] == b'+' || s[i] == b'-') {
        i += 1;
    }
    let start = i;
    let mut value: u64 = 0;
    while i < s.len() && s[i].is_ascii_digit() {
        match value
            .checked_mul(10)
            .and_then(|v| v.checked_add(u64::from(s[i] - b'0')))
        {
            Some(v) => value = v,
            None => return false,
        }
        i += 1;
    }
    if i == start || s.get(i) != Some(&b' ') {
        return false;
    }
    i += 1;
    if !matches!(s.get(i), Some(b'+') | Some(b'-')) {
        return false;
    }
    i += 1;
    let digits = &s[i..];
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return false;
    }
    if strict {
        let mut tz: u64 = 0;
        for &d in digits {
            tz = tz.saturating_mul(10).saturating_add(u64::from(d - b'0'));
        }
        if tz > 1400 {
            return false;
        }
    }
    true
}

/// The current time as `<time> <offset>`.
fn now_stamp() -> Vec<u8> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let ctx = rgit_date::DateContext::from_env();
    rgit_date::format_raw(now as u64, ctx.local_tz(now)).into_bytes()
}

/// Parse the identity after `author `, `committer ` or `tagger ` and return
/// it as stored in the object.
pub fn parse_ident(buf: &[u8], fmt: DateFormat) -> Result<Vec<u8>, String> {
    let text = || String::from_utf8_lossy(buf).into_owned();
    let lt = buf
        .iter()
        .position(|&c| c == b'<' || c == b'>')
        .filter(|&p| buf[p] == b'<')
        .ok_or_else(|| msg::missing_lt(&text()))?;
    if lt > 0 && buf[lt - 1] != b' ' {
        return Err(msg::missing_space_before_lt(&text()));
    }
    let gt = buf[lt + 1..]
        .iter()
        .position(|&c| c == b'<' || c == b'>')
        .map(|p| p + lt + 1)
        .filter(|&p| buf[p] == b'>')
        .ok_or_else(|| msg::missing_gt(&text()))?;
    if buf.get(gt + 1) != Some(&b' ') {
        return Err(msg::missing_space_after_gt(&text()));
    }
    let date = &buf[gt + 2..];
    let date_text = || String::from_utf8_lossy(date).into_owned();
    let mut out = Vec::with_capacity(buf.len() + 1);
    if lt == 0 {
        out.push(b' ');
    }
    out.extend_from_slice(&buf[..gt + 2]);
    match fmt {
        DateFormat::Raw | DateFormat::RawPermissive => {
            if !valid_raw_date(date, fmt == DateFormat::Raw) {
                return Err(msg::invalid_raw_date(&date_text(), &text()));
            }
            out.extend_from_slice(date);
        }
        DateFormat::Rfc2822 => {
            let ctx = rgit_date::DateContext::from_env();
            let (t, tz) = rgit_date::parse_date(&date_text(), &ctx)
                .ok_or_else(|| msg::invalid_rfc2822_date(&date_text(), &text()))?;
            out.extend_from_slice(rgit_date::format_raw(t, tz).as_bytes());
        }
        DateFormat::Now => {
            if date != b"now" {
                return Err(msg::date_must_be_now(&text()));
            }
            out.extend_from_slice(&now_stamp());
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_dates() {
        for ok in [
            "1 +0000", "-1 +0000", "01 +0000", "1 +000", "1 +00000", " 1 +0000", "+1 +0000",
            "1 -1400", "1 +0099", "18446744073709551615 +0000",
        ] {
            assert!(valid_raw_date(ok.as_bytes(), true), "{ok}");
        }
        for bad in [
            "1 +1401", "1 -9999", "1 0000", "1 +", "1  +0000", "1 +0000 ", "1 +0a00",
            "18446744073709551616 +0000", "x +0000", "1",
        ] {
            assert!(!valid_raw_date(bad.as_bytes(), true), "{bad}");
        }
        assert!(valid_raw_date(b"1 +051800", false));
        assert!(!valid_raw_date(b"1 0000", false));
    }

    #[test]
    fn idents() {
        assert_eq!(
            parse_ident(b"<a@b.com> 1 +0000", DateFormat::Raw).unwrap(),
            b" <a@b.com> 1 +0000"
        );
        assert_eq!(
            parse_ident(b"N <e> 1 +0000", DateFormat::Raw).unwrap(),
            b"N <e> 1 +0000"
        );
        assert!(parse_ident(b"Name email> 1 +0000", DateFormat::Raw).unwrap_err().starts_with("missing <"));
        assert!(parse_ident(b"Name <e<mail> 1 +0000", DateFormat::Raw).unwrap_err().starts_with("missing >"));
        assert!(parse_ident(b"Name <email>> 1 +0000", DateFormat::Raw).unwrap_err().starts_with("missing space after"));
        assert!(parse_ident(b"Name<email> 1 +0000", DateFormat::Raw).unwrap_err().starts_with("missing space before"));
    }
}
