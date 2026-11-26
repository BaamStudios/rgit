// SPDX-License-Identifier: MIT
//! E-mail addresses (RFC 5322 section 3.4) as send-email handles them:
//! splitting lists, putting display names into a form that can go into a
//! header (quoted or RFC 2047 encoded), and comparing addresses.

/// Characters that need a quoted display name (RFC 5322 "specials").
fn is_special(c: char) -> bool {
    matches!(
        c,
        '(' | ')' | '<' | '>' | '[' | ']' | ':' | ';' | '@' | '\\' | ',' | '.' | '"'
    )
}

/// Split a comma-separated list, keeping commas inside quotes, angle
/// brackets and comments; entries are trimmed, empty ones dropped.
pub fn split_list(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut quoted, mut angle, mut paren) = (false, false, 0usize);
    let mut escaped = false;
    for c in s.chars() {
        if escaped {
            cur.push(c);
            escaped = false;
            continue;
        }
        match c {
            '\\' if quoted => {
                escaped = true;
                cur.push(c);
            }
            '"' => {
                quoted = !quoted;
                cur.push(c);
            }
            '<' if !quoted => {
                angle = true;
                cur.push(c);
            }
            '>' if !quoted => {
                angle = false;
                cur.push(c);
            }
            '(' if !quoted => {
                paren += 1;
                cur.push(c);
            }
            ')' if !quoted => {
                paren = paren.saturating_sub(1);
                cur.push(c);
            }
            ',' if !quoted && !angle && paren == 0 => {
                out.push(std::mem::take(&mut cur));
            }
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out.into_iter()
        .map(|s| trim(&s).to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Trim whitespace, including carriage returns and NULs.
pub fn trim(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || c == '\0')
}

/// Split an address into display name and mailbox. Without angle brackets
/// the whole text is the mailbox.
pub fn parts(addr: &str) -> (String, String) {
    let addr = trim(addr);
    if let Some(lt) = find_unquoted(addr, '<') {
        let rest = &addr[lt + 1..];
        let email = match rest.find('>') {
            Some(gt) => &rest[..gt],
            None => rest,
        };
        (trim(&addr[..lt]).to_string(), trim(email).to_string())
    } else {
        (String::new(), addr.to_string())
    }
}

fn find_unquoted(s: &str, needle: char) -> Option<usize> {
    let mut quoted = false;
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            c if c == needle && !quoted => return Some(i),
            _ => {}
        }
    }
    None
}

/// The mailbox part of an address (`a@b` of `Name <a@b>`).
pub fn email(addr: &str) -> String {
    parts(addr).1
}

/// Encode `text` as one RFC 2047 "Q" encoded word in `charset`, with the
/// characters RFC 2047 section 5 (3) allows in a phrase left as they are.
pub fn encode_word(text: &[u8], charset: &str) -> String {
    let mut out = format!("=?{charset}?q?");
    for &b in text {
        if b.is_ascii_alphanumeric() || matches!(b, b'!' | b'*' | b'+' | b'-' | b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("={b:02X}"));
        }
    }
    out.push_str("?=");
    out
}

fn hex(c: u8) -> Option<u8> {
    (c as char).to_digit(16).map(|d| d as u8)
}

/// Decode the RFC 2047 encoded words in `text` (Q and B encodings; the
/// bytes are taken as they are, which is right for UTF-8).
pub fn decode_words(text: &str) -> String {
    let mut out: Vec<u8> = Vec::with_capacity(text.len());
    let mut rest = text;
    let mut last_was_word = false;
    while let Some(start) = rest.find("=?") {
        let before = &rest[..start];
        let tail = &rest[start + 2..];
        let decoded = (|| {
            let (_charset, tail) = tail.split_once('?')?;
            let (enc, tail) = tail.split_once('?')?;
            let end = tail.find("?=")?;
            let data = &tail[..end];
            let bytes = match enc {
                "q" | "Q" => {
                    let mut v = Vec::new();
                    let b = data.as_bytes();
                    let mut i = 0;
                    while i < b.len() {
                        match b[i] {
                            b'_' => v.push(b' '),
                            b'=' => {
                                match (b.get(i + 1).and_then(|&c| hex(c)), b.get(i + 2).and_then(|&c| hex(c))) {
                                    (Some(h), Some(l)) => {
                                        v.push(h << 4 | l);
                                        i += 2;
                                    }
                                    _ => v.push(b'='),
                                }
                            }
                            c => v.push(c),
                        }
                        i += 1;
                    }
                    v
                }
                "b" | "B" => rgit_mailnet::encoding::base64_decode(data.as_bytes())?,
                _ => return None,
            };
            Some((bytes, &tail[end + 2..]))
        })();
        match decoded {
            Some((bytes, after)) => {
                // Whitespace between two encoded words is dropped.
                if !(last_was_word && before.trim().is_empty()) {
                    out.extend_from_slice(before.as_bytes());
                }
                out.extend_from_slice(&bytes);
                rest = after;
                last_was_word = true;
            }
            None => {
                out.extend_from_slice(&rest.as_bytes()[..start + 2]);
                rest = &rest[start + 2..];
                last_was_word = false;
            }
        }
    }
    out.extend_from_slice(rest.as_bytes());
    String::from_utf8_lossy(&out).into_owned()
}

/// Remove the quotes (and backslash escapes) of a quoted display name.
pub fn unquote(name: &str) -> String {
    let t = trim(name);
    if t.len() >= 2 && t.starts_with('"') && t.ends_with('"') {
        let mut out = String::new();
        let mut escaped = false;
        for c in t[1..t.len() - 1].chars() {
            if escaped {
                out.push(c);
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else {
                out.push(c);
            }
        }
        out
    } else {
        t.to_string()
    }
}

/// Put an address into the form send-email puts into headers: a display
/// name with non-ASCII characters is RFC 2047 encoded, one with specials
/// is quoted; an address without a name is the bare mailbox.
pub fn sanitize(addr: &str) -> String {
    let addr = trim(addr);
    if find_unquoted(addr, '<').is_none() {
        return addr.to_string();
    }
    let (name, email) = parts(addr);
    if name.is_empty() {
        return email;
    }
    let display = if !name.is_ascii() {
        encode_word(unquote(&name).as_bytes(), "UTF-8")
    } else if (name.starts_with('"') && name.ends_with('"') && name.len() >= 2)
        || name.contains("=?")
    {
        name
    } else if name.chars().any(is_special) {
        let mut q = String::from("\"");
        for c in name.chars() {
            if c == '"' || c == '\\' {
                q.push('\\');
            }
            q.push(c);
        }
        q.push('"');
        q
    } else {
        name
    };
    format!("{display} <{email}>")
}

/// A form for comparing addresses: decoded, unquoted name and the mailbox.
pub fn canonical(addr: &str) -> (String, String) {
    let (name, email) = parts(addr);
    (unquote(&decode_words(&name)), email)
}

/// The address as a human reads it: name decoded, not quoted.
pub fn display(addr: &str) -> String {
    let addr = trim(addr);
    if find_unquoted(addr, '<').is_none() {
        return addr.to_string();
    }
    let (name, email) = canonical(addr);
    if name.is_empty() {
        format!("<{email}>")
    } else {
        format!("{name} <{email}>")
    }
}

/// The first address of a trailer value: up to the closing angle bracket
/// if there is one, else the first word; comments after it are dropped.
pub fn first_of_trailer(value: &str) -> String {
    let v = trim(value);
    if let Some(lt) = find_unquoted(v, '<') {
        match v[lt..].find('>') {
            Some(gt) => v[..lt + gt + 1].to_string(),
            None => v.to_string(),
        }
    } else {
        v.split(|c: char| c.is_whitespace() || c == ',')
            .next()
            .unwrap_or("")
            .to_string()
    }
}

/// Strip a trailing parenthesized comment (`get_maintainer.pl` output).
pub fn strip_comment(s: &str) -> String {
    let t = trim(s);
    if let Some(gt) = t.rfind('>')
        && find_unquoted(t, '<').is_some()
    {
        return t[..=gt].to_string();
    }
    if let Some(p) = find_unquoted(t, '(') {
        return trim(&t[..p]).to_string();
    }
    t.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitting() {
        assert_eq!(
            split_list("To 1 <to1@example.com>, to2@example.com"),
            vec!["To 1 <to1@example.com>", "to2@example.com"]
        );
        assert_eq!(
            split_list("\"Person, One\" <one@x>,  b@x , "),
            vec!["\"Person, One\" <one@x>", "b@x"]
        );
    }

    #[test]
    fn sanitizing() {
        assert_eq!(sanitize("A U. Thor <a@x>"), "\"A U. Thor\" <a@x>");
        assert_eq!(sanitize("\"A U. Thor\" <a@x>"), "\"A U. Thor\" <a@x>");
        assert_eq!(sanitize("Ronnie O'Sullivan <os@x>"), "Ronnie O'Sullivan <os@x>");
        assert_eq!(sanitize("<a@x>"), "a@x");
        assert_eq!(sanitize("  a@x "), "a@x");
        assert_eq!(
            sanitize("Füñný Nâmé <odd_?=mail@example.com>"),
            "=?UTF-8?q?F=C3=BC=C3=B1n=C3=BD=20N=C3=A2m=C3=A9?= <odd_?=mail@example.com>"
        );
        assert_eq!(
            sanitize("Person, One <one@example.com>"),
            "\"Person, One\" <one@example.com>"
        );
    }

    #[test]
    fn decoding() {
        assert_eq!(
            decode_words("=?UTF-8?q?F=C3=BC=C3=B1n=C3=BD=20N=C3=A2m=C3=A9?="),
            "Füñný Nâmé"
        );
        assert_eq!(decode_words("=?UTF-8?q?a?= =?UTF-8?q?b?="), "ab");
        assert_eq!(decode_words("plain"), "plain");
    }

    #[test]
    fn canonical_matches_variants() {
        assert_eq!(
            canonical("\"A U. Thor\" <a@x>"),
            canonical("A U. Thor <a@x>")
        );
        assert_eq!(
            canonical(&sanitize("Füñný Nâmé <o@x>")),
            canonical("Füñný Nâmé <o@x>")
        );
    }

    #[test]
    fn trailers() {
        assert_eq!(first_of_trailer("<two@x> # comment"), "<two@x>");
        assert_eq!(first_of_trailer("five@x # not.six@x"), "five@x");
        assert_eq!(first_of_trailer("six@x, not@x"), "six@x");
        assert_eq!(
            first_of_trailer("\"Some # Body\" <four@x> [ <c@x> ]"),
            "\"Some # Body\" <four@x>"
        );
        assert_eq!(
            strip_comment("One Person <one@x> (supporter:THIS (FOO/bar))"),
            "One Person <one@x>"
        );
        assert_eq!(strip_comment("five@x (open list:FOR THING)"), "five@x");
    }
}
