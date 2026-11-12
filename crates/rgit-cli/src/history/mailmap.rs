// SPDX-License-Identifier: MIT
//! `.mailmap` (Documentation/gitmailmap.adoc, config/mailmap.adoc): map
//! author and committer identities to canonical ones for display.

use std::collections::HashMap;

use rgit_object::ObjectKind;
use rgit_revwalk::revision::ObjectLookup;

use super::Repo;

/// Replacement name and email; `None` keeps the original.
#[derive(Debug, Default, Clone)]
struct Replacement {
    name: Option<Vec<u8>>,
    email: Option<Vec<u8>>,
}

impl Replacement {
    fn merge(&mut self, name: Option<Vec<u8>>, email: Option<Vec<u8>>) {
        if name.is_some() {
            self.name = name;
        }
        if email.is_some() {
            self.email = email;
        }
    }
}

#[derive(Debug, Default)]
struct ByEmail {
    /// Entries without a commit name.
    any: Option<Replacement>,
    /// Entries for a commit name (lower case).
    by_name: HashMap<Vec<u8>, Replacement>,
}

/// A loaded mailmap.
#[derive(Debug, Default)]
pub struct Mailmap {
    /// Keyed by the commit email in lower case.
    map: HashMap<Vec<u8>, ByEmail>,
}

fn trim(s: &[u8]) -> &[u8] {
    let start = s
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(s.len());
    let end = s
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .map_or(start, |i| i + 1);
    &s[start..end.max(start)]
}

/// `Name <email>` at the start of `s`: the name (possibly empty), the
/// email and the rest after `>`.
fn name_email(s: &[u8]) -> Option<(&[u8], &[u8], &[u8])> {
    let lt = s.iter().position(|&b| b == b'<')?;
    let gt = lt + 1 + s[lt + 1..].iter().position(|&b| b == b'>')?;
    Some((trim(&s[..lt]), &s[lt + 1..gt], &s[gt + 1..]))
}

fn non_empty(s: &[u8]) -> Option<Vec<u8>> {
    (!s.is_empty()).then(|| s.to_vec())
}

impl Mailmap {
    /// Add the entries of a mailmap file.
    pub fn add(&mut self, text: &[u8]) {
        for line in text.split(|&b| b == b'\n') {
            let line = match line.iter().position(|&b| b == b'#') {
                Some(i) => &line[..i],
                None => line,
            };
            let Some((name1, email1, rest)) = name_email(line) else {
                continue;
            };
            let (key_name, key_email, new_email) = match name_email(rest) {
                Some((name2, email2, _)) => (non_empty(name2), email2, non_empty(email1)),
                None => (None, email1, None),
            };
            let entry = self.map.entry(key_email.to_ascii_lowercase()).or_default();
            let target = match key_name {
                Some(n) => entry.by_name.entry(n.to_ascii_lowercase()).or_default(),
                None => entry.any.get_or_insert_with(Replacement::default),
            };
            target.merge(non_empty(name1), new_email);
        }
    }

    /// The canonical identity for `name <email>`.
    pub fn map<'a>(&'a self, name: &'a [u8], email: &'a [u8]) -> (&'a [u8], &'a [u8]) {
        let Some(entry) = self.map.get(&email.to_ascii_lowercase()) else {
            return (name, email);
        };
        let r = entry
            .by_name
            .get(&name.to_ascii_lowercase())
            .or(entry.any.as_ref());
        match r {
            Some(r) => (
                r.name.as_deref().unwrap_or(name),
                r.email.as_deref().unwrap_or(email),
            ),
            None => (name, email),
        }
    }

    /// Load the repository's mailmap: `.mailmap` in the work tree, then
    /// `mailmap.blob` (default `HEAD:.mailmap` in a bare repository), then
    /// `mailmap.file`; later entries win.
    pub fn load(repo: &Repo<'_>) -> Mailmap {
        let mut mm = Mailmap::default();
        if let Some(top) = &repo.setup.work_tree {
            let path = repo.setup.cwd.join(top).join(".mailmap");
            if let Ok(meta) = path.symlink_metadata()
                && meta.is_file()
                && let Ok(text) = std::fs::read(&path)
            {
                mm.add(&text);
            }
        }
        let blob = match repo.config.string("mailmap.blob") {
            Ok(Some(v)) => Some(v.to_string()),
            _ if repo.setup.work_tree.is_none() => Some("HEAD:.mailmap".to_string()),
            _ => None,
        };
        if let Some(spec) = blob
            && let Ok(id) = repo.resolver.resolve(&spec)
            && let Ok(Some((ObjectKind::Blob, data))) = repo.objects.read(&id)
        {
            mm.add(&data);
        }
        if let Ok(Some(file)) = repo.config.string("mailmap.file") {
            let file = file.to_string();
            let path = match file.strip_prefix("~/") {
                Some(rest) => std::env::var_os("HOME")
                    .map(|h| std::path::PathBuf::from(h).join(rest))
                    .unwrap_or_else(|| file.clone().into()),
                None => file.into(),
            };
            if let Ok(text) = std::fs::read(path) {
                mm.add(&text);
            }
        }
        mm
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forms() {
        let mut mm = Mailmap::default();
        mm.add(
            b"# comment\n\
              Proper Name <commit@x>\n\
              <proper@x> <old@x>\n\
              Both Name <both@x> <both-old@x>\n\
              Exact Name <exact@x> Commit Name <shared@x>  # trailing\n",
        );
        assert_eq!(
            mm.map(b"a", b"COMMIT@x"),
            (&b"Proper Name"[..], &b"COMMIT@x"[..])
        );
        assert_eq!(mm.map(b"a", b"old@x"), (&b"a"[..], &b"proper@x"[..]));
        assert_eq!(
            mm.map(b"a", b"both-old@x"),
            (&b"Both Name"[..], &b"both@x"[..])
        );
        assert_eq!(
            mm.map(b"commit NAME", b"shared@x"),
            (&b"Exact Name"[..], &b"exact@x"[..])
        );
        assert_eq!(
            mm.map(b"Other", b"shared@x"),
            (&b"Other"[..], &b"shared@x"[..])
        );
    }
}
