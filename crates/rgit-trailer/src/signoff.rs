// SPDX-License-Identifier: MIT
//! `--signoff`: the committer's `Signed-off-by` line at the end of a
//! message (Documentation/signoff-option.adoc). Placement was checked
//! against the `git` binary: the line goes after the last trailer block,
//! before trailing empty and comment lines, and is left out when the
//! message already ends with the same line.

use crate::config::Config;
use crate::parse::{ParseOptions, message_end, parse};

/// Add `line` (a complete `Signed-off-by: ...` line without newline) to
/// `msg`. `comment` is the comment string; `---` does not end the message.
pub fn append_signoff(msg: &[u8], line: &[u8], conf: &Config, comment: &[u8]) -> Vec<u8> {
    let end = message_end(msg, comment);
    let body = &msg[..end];
    let last = body
        .strip_suffix(b"\n")
        .unwrap_or(body)
        .rsplit(|&b| b == b'\n')
        .next()
        .unwrap_or_default();
    if last == line {
        return msg.to_vec();
    }
    let info = parse(
        body,
        conf,
        &ParseOptions {
            comment,
            divider: false,
        },
    );
    let mut out = Vec::with_capacity(msg.len() + line.len() + 3);
    out.extend_from_slice(body);
    if !out.ends_with(b"\n") {
        out.push(b'\n');
    }
    if !info.found {
        out.push(b'\n');
    }
    out.extend_from_slice(line);
    out.push(b'\n');
    out.extend_from_slice(&msg[end..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOB: &str = "Signed-off-by: C O Mitter <c@e>";

    fn sob(msg: &str) -> String {
        let out = append_signoff(msg.as_bytes(), SOB.as_bytes(), &Config::default(), b"#");
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn placement() {
        let s = SOB;
        assert_eq!(sob(""), format!("\n\n{s}\n"));
        assert_eq!(sob("\n"), format!("\n\n{s}\n"));
        assert_eq!(sob("\n\n"), format!("\n\n{s}\n\n"));
        assert_eq!(sob("msg"), format!("msg\n\n{s}\n"));
        assert_eq!(sob("msg\n"), format!("msg\n\n{s}\n"));
        assert_eq!(sob("x\n\nFoo: bar"), format!("x\n\nFoo: bar\n{s}\n"));
        assert_eq!(
            sob("x\n\nFoo: bar\n# c\n"),
            format!("x\n\nFoo: bar\n{s}\n# c\n")
        );
        assert_eq!(sob("x\nFoo: bar\n"), format!("x\nFoo: bar\n\n{s}\n"));
        assert_eq!(sob("Foo: bar\n"), format!("Foo: bar\n\n{s}\n"));
        assert_eq!(sob(&format!("{s}\n")), format!("{s}\n"));
        assert_eq!(sob(&format!("x\n\n{s}\n\n")), format!("x\n\n{s}\n\n"));
        assert_eq!(
            sob(&format!("x\n\n{s}\nFoo: bar\n")),
            format!("x\n\n{s}\nFoo: bar\n{s}\n")
        );
        assert_eq!(sob("x\n---\ny\n"), format!("x\n---\ny\n\n{s}\n"));
        assert_eq!(sob("# only\n"), format!("# only\n\n{s}\n"));
        assert_eq!(
            sob("msg\n\n# comment\n\n"),
            format!("msg\n\n{s}\n\n# comment\n\n")
        );
        assert_eq!(
            sob("x\n\nnot trailer\nSigned-off-by: Q <q>\n"),
            format!("x\n\nnot trailer\nSigned-off-by: Q <q>\n{s}\n")
        );
    }
}
