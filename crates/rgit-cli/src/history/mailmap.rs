// SPDX-License-Identifier: MIT
//! `.mailmap` (Documentation/gitmailmap.adoc, config/mailmap.adoc): map
//! author and committer identities to canonical ones for display.

use std::collections::HashMap;

use rgit_object::ObjectKind;
use rgit_revwalk::revision::ObjectLookup;

use super::Repo;
use crate::messages::rev_list as msg;
pub use crate::revisions::BlobLookup;

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
            // A line starting with `#` is a comment; elsewhere `#` is
            // text (an email may contain one) and anything after the
            // last `<...>` is ignored anyway.
            if line.trim_ascii_start().starts_with(b"#") {
                continue;
            }
            let Some((name1, email1, rest)) = name_email(line) else {
                continue;
            };
            let (key_name, key_email, new_email) = match name_email(rest) {
                // An empty replacement email makes the line void.
                Some(_) if email1.is_empty() => continue,
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

    /// The header identities of a commit (`author`, `committer`) or tag
    /// (`tagger`) object mapped (`cat-file --use-mailmap`); other objects
    /// unchanged.
    pub fn rewrite_object(&self, kind: ObjectKind, data: &[u8]) -> Vec<u8> {
        let keys: &[&[u8]] = match kind {
            ObjectKind::Commit => &[b"author ", b"committer "],
            ObjectKind::Tag => &[b"tagger "],
            _ => return data.to_vec(),
        };
        let mut out = Vec::with_capacity(data.len());
        let mut rest = data;
        while !rest.is_empty() {
            let end = rest
                .iter()
                .position(|&b| b == b'\n')
                .map_or(rest.len(), |i| i + 1);
            let line = &rest[..end];
            rest = &rest[end..];
            if line == b"\n" {
                // The message follows unchanged.
                out.extend_from_slice(line);
                out.extend_from_slice(rest);
                break;
            }
            let found = keys
                .iter()
                .find(|k| line.starts_with(k))
                .and_then(|k| Some((k, name_email(&line[k.len()..])?)));
            match found {
                Some((k, (name, email, after))) if self.map(name, email) != (name, email) => {
                    let (name, email) = self.map(name, email);
                    out.extend_from_slice(k);
                    out.extend_from_slice(name);
                    out.extend_from_slice(b" <");
                    out.extend_from_slice(email);
                    out.push(b'>');
                    out.extend_from_slice(after);
                }
                _ => out.extend_from_slice(line),
            }
        }
        out
    }

    /// Load the repository's mailmap: `.mailmap` in the work tree, then
    /// `mailmap.blob` (default `HEAD:.mailmap` in a bare repository), then
    /// `mailmap.file`; later entries win.
    pub fn load(repo: &Repo<'_>) -> Mailmap {
        Mailmap::load_lookup(repo.setup, repo.config, &|spec| {
            let Ok(id) = repo.resolver.resolve(spec) else {
                return BlobLookup::Unresolved;
            };
            match repo.objects.read(&id) {
                Ok(Some((ObjectKind::Blob, data))) => BlobLookup::Blob(data),
                Ok(Some(_)) => BlobLookup::NotBlob,
                _ => BlobLookup::Unresolved,
            }
        })
    }

    /// The mailmap of the repository of the current directory (for
    /// commands without a walk of their own).
    pub fn load_for(ctx: &crate::Context) -> Result<Mailmap, crate::Die> {
        let Some(setup) = crate::repo::setup_gently(ctx)? else {
            return Ok(Mailmap::default());
        };
        let config = crate::repo::load_config(ctx, &setup)?;
        Ok(Mailmap::load_lookup(&setup, &config, &|spec| {
            crate::revisions::read_blob(ctx, spec, "")
        }))
    }

    /// [`Mailmap::load`] for callers with their own object access:
    /// `read_blob` returns the blob a revision (`HEAD:.mailmap`) names.
    pub fn load_with(
        setup: &rgit_repo::Setup,
        config: &rgit_config::ConfigSet,
        read_blob: &dyn Fn(&str) -> Option<Vec<u8>>,
    ) -> Mailmap {
        Mailmap::load_lookup(setup, config, &|spec| match read_blob(spec) {
            Some(data) => BlobLookup::Blob(data),
            None => BlobLookup::Unresolved,
        })
    }

    /// [`Mailmap::load_with`] that can tell a name of something other
    /// than a blob (reported as an error) from a missing one (ignored).
    pub fn load_lookup(
        setup: &rgit_repo::Setup,
        config: &rgit_config::ConfigSet,
        read_blob: &dyn Fn(&str) -> BlobLookup,
    ) -> Mailmap {
        let mut mm = Mailmap::default();
        if let Some(top) = &setup.work_tree {
            let path = setup.cwd.join(top).join(".mailmap");
            if let Ok(meta) = path.symlink_metadata()
                && meta.is_file()
                && let Ok(text) = std::fs::read(&path)
            {
                mm.add(&text);
            }
        }
        let blob = match config.string("mailmap.blob") {
            Ok(Some(v)) => Some(v.to_string()),
            _ if setup.work_tree.is_none() => Some("HEAD:.mailmap".to_string()),
            _ => None,
        };
        if let Some(spec) = blob {
            match read_blob(&spec) {
                BlobLookup::Blob(data) => mm.add(&data),
                BlobLookup::NotBlob => {
                    eprintln!("error: {}", msg::not_a_blob(&spec));
                }
                BlobLookup::Unresolved => {}
            }
        }
        if let Ok(Some(file)) = config.string("mailmap.file") {
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

    #[test]
    fn hashes_and_empty_emails() {
        let mut mm = Mailmap::default();
        mm.add(b"  # Comment <c@x>\nHash <h@x> <#@x>\nVoid <> <v@x>\nEmpty <e@x> <>\n");
        assert_eq!(mm.map(b"a", b"c@x"), (&b"a"[..], &b"c@x"[..]));
        assert_eq!(mm.map(b"a", b"#@x"), (&b"Hash"[..], &b"h@x"[..]));
        assert_eq!(mm.map(b"a", b"v@x"), (&b"a"[..], &b"v@x"[..]));
        assert_eq!(mm.map(b"a", b""), (&b"Empty"[..], &b"e@x"[..]));
        let raw =
            b"tree t\nauthor Someone <#@x> 1 +0000\ncommitter C <c@x> 1 +0000\n\nauthor x <#@x>\n";
        assert_eq!(
            mm.rewrite_object(rgit_object::ObjectKind::Commit, raw),
            b"tree t\nauthor Hash <h@x> 1 +0000\ncommitter C <c@x> 1 +0000\n\nauthor x <#@x>\n"
        );
    }
}
