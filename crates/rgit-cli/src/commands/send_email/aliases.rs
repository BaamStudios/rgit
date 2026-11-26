// SPDX-License-Identifier: MIT
//! Alias files (`sendemail.aliasesFile`, `sendemail.aliasFileType`):
//! mutt, mailrc, pine, elm, gnus and sendmail formats, each read the way
//! its program documents it, with the limits listed in
//! Documentation/config/sendemail.adoc for sendmail.

use std::collections::BTreeMap;

use super::address;
use crate::messages::send_email as msg;

#[derive(Debug, Default)]
pub struct Aliases {
    map: BTreeMap<String, Vec<String>>,
}

impl Aliases {
    /// Read `files` of `kind`. An unknown kind reads nothing.
    pub fn load(files: &[String], kind: Option<&str>) -> Result<Aliases, String> {
        let mut a = Aliases::default();
        let Some(kind) = kind else {
            return Ok(a);
        };
        let kind = kind.to_ascii_lowercase();
        for f in files {
            let path = expand_home(f);
            let text = std::fs::read(&path).map_err(|e| {
                let err = match e.kind() {
                    std::io::ErrorKind::NotFound => "No such file or directory".to_string(),
                    _ => e.to_string(),
                };
                msg::opening(f, &err)
            })?;
            let text = String::from_utf8_lossy(&text);
            match kind.as_str() {
                "mutt" => a.parse_mutt(&text),
                "mailrc" => a.parse_mailrc(&text),
                "pine" => a.parse_pine(&text),
                "elm" => a.parse_elm(&text),
                "gnus" => a.parse_gnus(&text),
                "sendmail" => a.parse_sendmail(&text),
                _ => {}
            }
        }
        Ok(a)
    }

    fn add(&mut self, name: &str, targets: Vec<String>) {
        if name.is_empty() {
            return;
        }
        self.map.insert(name.to_string(), targets);
    }

    /// `alias [-group name]... <alias> <address>[, <address>...]`
    fn parse_mutt(&mut self, text: &str) {
        for line in text.lines() {
            let line = address::trim(line);
            let Some(rest) = line.strip_prefix("alias") else {
                continue;
            };
            if !rest.starts_with(char::is_whitespace) {
                continue;
            }
            let mut words = rest.trim_start();
            while let Some(r) = words.strip_prefix("-group") {
                let r = r.trim_start();
                words = r.split_once(char::is_whitespace).map_or("", |x| x.1).trim_start();
            }
            let Some((name, value)) = words.split_once(char::is_whitespace) else {
                continue;
            };
            let value = value.replace("\\\"", "\"");
            self.add(name, address::split_list(&value));
        }
    }

    /// `alias <name> <address> [<address>...]`, addresses may be quoted.
    fn parse_mailrc(&mut self, text: &str) {
        for line in text.lines() {
            let line = address::trim(line);
            let Some(rest) = line
                .strip_prefix("alias")
                .or_else(|| line.strip_prefix("group"))
            else {
                continue;
            };
            if !rest.starts_with(char::is_whitespace) {
                continue;
            }
            let words = shell_words(rest);
            if words.len() < 2 {
                continue;
            }
            self.add(&words[0], words[1..].to_vec());
        }
    }

    /// Tab-separated: nickname, full name, address (or a parenthesized
    /// list), fcc, comment.
    fn parse_pine(&mut self, text: &str) {
        // Lines starting with whitespace continue the previous one.
        let mut joined: Vec<String> = Vec::new();
        for line in text.lines() {
            if line.starts_with([' ']) && !line.trim().is_empty() {
                if let Some(last) = joined.last_mut() {
                    last.push_str(line.trim_start());
                    continue;
                }
            }
            joined.push(line.to_string());
        }
        for line in joined {
            let fields: Vec<&str> = line.split('\t').collect();
            if fields.len() < 3 {
                continue;
            }
            let addr = address::trim(fields[2]);
            let targets = if let Some(list) = addr.strip_prefix('(').and_then(|a| a.strip_suffix(')')) {
                address::split_list(list)
            } else {
                vec![addr.to_string()]
            };
            self.add(address::trim(fields[0]), targets);
        }
    }

    /// `<alias> = <full name> = <address>[, <address>...]`
    fn parse_elm(&mut self, text: &str) {
        for line in text.lines() {
            let line = address::trim(line);
            if line.starts_with('#') {
                continue;
            }
            let f: Vec<&str> = line.splitn(3, '=').collect();
            if f.len() != 3 {
                continue;
            }
            let name = address::trim(f[1]);
            let list = address::split_list(f[2]);
            let targets = if list.len() == 1 && !name.is_empty() && !list[0].contains('<') {
                vec![format!("{name} <{}>", list[0])]
            } else {
                list
            };
            for alias in f[0].split(',') {
                self.add(address::trim(alias), targets.clone());
            }
        }
    }

    /// `(define-mail-alias "name" "address")`
    fn parse_gnus(&mut self, text: &str) {
        for line in text.lines() {
            let line = address::trim(line);
            let Some(rest) = line.strip_prefix("(define-mail-alias") else {
                continue;
            };
            let words = shell_words(rest.trim_end_matches(')'));
            if words.len() >= 2 {
                self.add(&words[0], address::split_list(&words[1]));
            }
        }
    }

    /// `name: address, address` with continuation lines (a trailing
    /// backslash, or leading whitespace on the next line).
    fn parse_sendmail(&mut self, text: &str) {
        let mut logical: Vec<String> = Vec::new();
        let mut continues = false;
        for raw in text.lines() {
            if raw.trim_start().starts_with('#') {
                continue;
            }
            let (line, backslash) = match raw.strip_suffix('\\') {
                Some(l) => (l, true),
                None => (raw, false),
            };
            let indented = line.starts_with([' ', '\t']);
            if (continues || indented)
                && let Some(last) = logical.last_mut()
            {
                if indented && !continues {
                    last.push(' ');
                }
                last.push_str(if continues { line } else { line.trim_start() });
            } else {
                logical.push(line.trim_start().to_string());
            }
            continues = backslash;
        }
        for line in logical {
            let line = address::trim(&line).to_string();
            if line.is_empty() {
                continue;
            }
            if line.contains('"') {
                eprintln!("{}", msg::alias_quotes(&line));
                continue;
            }
            if line.starts_with(":include:") {
                eprintln!("{}", msg::alias_include(&line));
                continue;
            }
            let Some((name, value)) = line.split_once(':') else {
                eprintln!("{}", msg::alias_unknown(&line));
                continue;
            };
            let name = address::trim(name);
            if name.is_empty() || name.contains(char::is_whitespace) {
                eprintln!("{}", msg::alias_unknown(&line));
                continue;
            }
            if value.contains([':', '/', '|']) && address::trim(value).starts_with(['/', '|']) {
                eprintln!("{}", msg::alias_redirection(&line));
                continue;
            }
            self.add(name, address::split_list(value));
        }
    }

    /// The alias names in alphabetical order.
    pub fn names(&self) -> impl Iterator<Item = &String> {
        self.map.keys()
    }

    /// Expand `addr` (recursively); an address that is no alias stays.
    pub fn expand(&self, addr: &str) -> Vec<String> {
        let mut out = Vec::new();
        self.expand_into(address::trim(addr), &mut Vec::new(), &mut out);
        out
    }

    fn expand_into(&self, addr: &str, seen: &mut Vec<String>, out: &mut Vec<String>) {
        match self.map.get(addr) {
            Some(targets) if !seen.iter().any(|s| s == addr) => {
                seen.push(addr.to_string());
                for t in targets {
                    self.expand_into(address::trim(t), seen, out);
                }
                seen.pop();
            }
            _ => out.push(addr.to_string()),
        }
    }
}

/// `~/` at the start of a path means the home directory.
pub fn expand_home(path: &str) -> std::path::PathBuf {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return std::path::Path::new(&home).join(rest);
    }
    std::path::PathBuf::from(path)
}

/// Split on whitespace, with double-quoted words.
fn shell_words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut have = false;
    for c in s.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                have = true;
            }
            c if c.is_whitespace() && !quoted => {
                if have {
                    out.push(std::mem::take(&mut cur));
                    have = false;
                }
            }
            c => {
                cur.push(c);
                have = true;
            }
        }
    }
    if have {
        out.push(cur);
    }
    out
}

/// Split a line of `--translate-aliases` input into addresses: a list
/// with commas, or words when no entry has a mailbox in angle brackets.
pub fn split_input_line(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    for entry in address::split_list(line) {
        if entry.contains('<') || entry.contains('"') {
            out.push(entry);
        } else {
            out.extend(entry.split_whitespace().map(str::to_string));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn from(kind: &str, text: &str) -> Aliases {
        let mut a = Aliases::default();
        match kind {
            "mutt" => a.parse_mutt(text),
            "mailrc" => a.parse_mailrc(text),
            "pine" => a.parse_pine(text),
            "gnus" => a.parse_gnus(text),
            "sendmail" => a.parse_sendmail(text),
            "elm" => a.parse_elm(text),
            _ => unreachable!(),
        }
        a
    }

    #[test]
    fn sendmail_groups_expand() {
        let a = from(
            "sendmail",
            "alice: Alice <a@x>\nbob: b@x\n# c\n  # c2\ngrp: alice, bob, Other <o@x>\n",
        );
        assert_eq!(a.expand("grp"), vec!["Alice <a@x>", "b@x", "Other <o@x>"]);
        assert_eq!(a.names().collect::<Vec<_>>(), vec!["alice", "bob", "grp"]);
    }

    #[test]
    fn sendmail_folding() {
        let a = from(
            "sendmail",
            "bob: bob1,\\\nbob2\nchuck: chuck1,\n    chuck2\nfred: fred1,\\\n    fred2\n",
        );
        assert_eq!(a.expand("bob"), vec!["bob1", "bob2"]);
        assert_eq!(a.expand("chuck"), vec!["chuck1", "chuck2"]);
        assert_eq!(a.expand("fred"), vec!["fred1", "fred2"]);
    }

    #[test]
    fn other_formats() {
        assert_eq!(
            from("mutt", "alias sbd \\\"Dot U. Sir\\\" <s@x>\n").expand("sbd"),
            vec!["\"Dot U. Sir\" <s@x>"]
        );
        assert_eq!(
            from("mailrc", "alias alice   \"Alice W <a@x>\"\nalias c  c@x\n").expand("alice"),
            vec!["Alice W <a@x>"]
        );
        let pine = from("pine", "eve\tEve\teve@x\nbcgrp\t\t(eve, Other <o@x>)\n");
        assert_eq!(pine.expand("bcgrp"), vec!["eve@x", "Other <o@x>"]);
        assert_eq!(
            from("gnus", "(define-mail-alias \"alice\" \"a@x\")\n").expand("alice"),
            vec!["a@x"]
        );
        assert_eq!(
            from("elm", "al = Al Bundy = al@x\n").expand("al"),
            vec!["Al Bundy <al@x>"]
        );
    }

    #[test]
    fn input_lines() {
        assert_eq!(split_input_line("alice bcgrp"), vec!["alice", "bcgrp"]);
        assert_eq!(split_input_line("Other <o@x>"), vec!["Other <o@x>"]);
        assert_eq!(split_input_line("a, b"), vec!["a", "b"]);
    }
}
