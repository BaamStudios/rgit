// SPDX-License-Identifier: MIT
//! A minimal writer for the one-line JSON objects of the event format.

use std::fmt::Write;

/// Fields of a JSON object, written in insertion order.
#[derive(Default)]
pub(crate) struct Json {
    body: String,
}

fn escape(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

impl Json {
    fn key(&mut self, k: &str) {
        if !self.body.is_empty() {
            self.body.push(',');
        }
        escape(&mut self.body, k);
        self.body.push(':');
    }

    pub(crate) fn str(&mut self, k: &str, v: &str) -> &mut Json {
        self.key(k);
        escape(&mut self.body, v);
        self
    }

    pub(crate) fn int(&mut self, k: &str, v: i64) -> &mut Json {
        self.key(k);
        let _ = write!(self.body, "{v}");
        self
    }

    pub(crate) fn uint(&mut self, k: &str, v: u64) -> &mut Json {
        self.key(k);
        let _ = write!(self.body, "{v}");
        self
    }

    pub(crate) fn float(&mut self, k: &str, v: f64) -> &mut Json {
        self.key(k);
        let _ = write!(self.body, "{v:.6}");
        self
    }

    pub(crate) fn bool(&mut self, k: &str, v: bool) -> &mut Json {
        self.key(k);
        self.body.push_str(if v { "true" } else { "false" });
        self
    }

    /// A value that is JSON text already.
    pub(crate) fn raw(&mut self, k: &str, json: &str) -> &mut Json {
        self.key(k);
        self.body.push_str(json);
        self
    }

    pub(crate) fn arr<S: AsRef<str>>(&mut self, k: &str, v: &[S]) -> &mut Json {
        self.key(k);
        self.body.push('[');
        for (i, s) in v.iter().enumerate() {
            if i > 0 {
                self.body.push(',');
            }
            escape(&mut self.body, s.as_ref());
        }
        self.body.push(']');
        self
    }

    /// Append the fields of `other`.
    pub(crate) fn append(&mut self, other: &Json) -> &mut Json {
        if !other.body.is_empty() {
            if !self.body.is_empty() {
                self.body.push(',');
            }
            self.body.push_str(&other.body);
        }
        self
    }

    pub(crate) fn finish(&self) -> String {
        format!("{{{}}}", self.body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object() {
        let mut j = Json::default();
        j.str("a", "x\"y\n")
            .int("n", -1)
            .float("t", 0.5)
            .bool("b", false);
        j.arr("v", &["p", "q r"]);
        assert_eq!(
            j.finish(),
            r#"{"a":"x\"y\n","n":-1,"t":0.500000,"b":false,"v":["p","q r"]}"#
        );
    }
}
