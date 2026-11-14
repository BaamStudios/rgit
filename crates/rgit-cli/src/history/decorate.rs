// SPDX-License-Identifier: MIT
//! Ref names shown next to commits (`%d`, `%D`, `%(decorate)`;
//! `--decorate-refs`, `--decorate-refs-exclude`, `--clear-decorations`,
//! `log.excludeDecoration`, `log.initialDecorationSet`, `--decorate=full`;
//! Documentation/git-log.adoc and pretty-formats.adoc).
//!
//! Details from the reference `git`: names of one commit are listed in
//! reverse ref name order, except that `HEAD` comes first and takes the
//! branch it points to along (`HEAD -> main`); annotated tags (also chains
//! of them) decorate the commit they point to; `refs/replace/` refs never
//! decorate.

use std::collections::HashMap;

use rgit_config::ConfigSet;
use rgit_hash::ObjectId;
use rgit_object::{ObjectKind, Tag};
use rgit_pathspec::wildmatch::{WildmatchFlags, wildmatch};
use rgit_revwalk::revision::{ObjectLookup, RefLookup};

use super::Repo;
use crate::Die;

/// What kind of ref a name is (it decides the colour and `tag: `).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Head,
    Branch,
    Remote,
    Tag,
    Stash,
    Other,
}

/// One name of a commit.
#[derive(Debug, Clone)]
pub struct Name {
    pub kind: Kind,
    /// As shown (short or full).
    pub shown: String,
    /// The full ref name.
    pub refname: String,
}

/// The `--decorate-refs*` settings.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    /// `--clear-decorations`: all refs are candidates.
    pub clear: bool,
    /// `--decorate=full`.
    pub full: Option<bool>,
}

/// The parts of a decoration (`%(decorate:...)` options).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parts {
    pub prefix: Vec<u8>,
    pub suffix: Vec<u8>,
    pub separator: Vec<u8>,
    pub pointer: Vec<u8>,
    pub tag: Vec<u8>,
}

impl Default for Parts {
    fn default() -> Self {
        Parts {
            prefix: b" (".to_vec(),
            suffix: b")".to_vec(),
            separator: b", ".to_vec(),
            pointer: b" -> ".to_vec(),
            tag: b"tag: ".to_vec(),
        }
    }
}

/// Colours of the kinds of names (`color.decorate.<slot>`).
#[derive(Debug, Clone)]
pub struct Colors {
    pub commit: String,
    head: String,
    branch: String,
    remote: String,
    tag: String,
    stash: String,
}

impl Colors {
    pub fn load(config: &ConfigSet, commit: String) -> Result<Colors, Die> {
        let slot = |name: &str, default: &str| {
            crate::color::slot(config, &format!("color.decorate.{name}"), default)
        };
        Ok(Colors {
            commit,
            head: slot("HEAD", "\x1b[1;36m")?,
            branch: slot("branch", "\x1b[1;32m")?,
            remote: slot("remoteBranch", "\x1b[1;31m")?,
            tag: slot("tag", "\x1b[1;33m")?,
            stash: slot("stash", "\x1b[1;35m")?,
        })
    }

    fn of(&self, kind: Kind) -> &str {
        match kind {
            Kind::Head => &self.head,
            Kind::Branch => &self.branch,
            Kind::Remote => &self.remote,
            Kind::Tag => &self.tag,
            Kind::Stash => &self.stash,
            Kind::Other => "",
        }
    }
}

/// `pattern` matches `name`: a glob, or a ref name or prefix of whole
/// path components. A pattern other than `HEAD` that does not start with
/// `refs/` is taken relative to `refs/` (`heads/main`, `tags/v*`), and a
/// glob's `*` also matches `/` (both as the reference `git` does).
fn matches(pattern: &str, name: &str) -> bool {
    let normalized;
    let pattern = if pattern == "HEAD" || pattern.starts_with("refs/") {
        pattern
    } else {
        normalized = format!("refs/{pattern}");
        normalized.as_str()
    };
    if pattern.contains(['*', '?', '[']) {
        return wildmatch(pattern.as_bytes(), name.as_bytes(), WildmatchFlags::NONE);
    }
    let p = pattern.trim_end_matches('/');
    name == p || name.strip_prefix(p).is_some_and(|r| r.starts_with('/'))
}

const DEFAULT_SET: [&str; 5] = [
    "HEAD",
    "refs/heads",
    "refs/remotes",
    "refs/stash",
    "refs/tags",
];

/// The names of every decorated commit.
pub struct Decorations {
    map: HashMap<ObjectId, Vec<Name>>,
    /// The branch `HEAD` points to.
    head_target: Option<String>,
}

impl Decorations {
    /// Read the refs of `repo` through `filter` and the configuration.
    pub fn load(repo: &Repo<'_>, filter: &Filter) -> Decorations {
        let config = repo.config;
        let mut config_exclude: Vec<String> = Vec::new();
        for e in config.get_all("log.excludeDecoration") {
            match e.value.as_ref() {
                Some(v) => config_exclude.push(v.to_string()),
                None => eprintln!(
                    "error: {}",
                    crate::messages::comment::missing_value("log.excludeDecoration")
                ),
            }
        }
        let all = filter.clear
            || config
                .string("log.initialDecorationSet")
                .ok()
                .flatten()
                .is_some_and(|v| v == "all");
        let full = filter.full.unwrap_or_else(|| {
            config
                .string("log.decorate")
                .ok()
                .flatten()
                .is_some_and(|v| v == "full")
        });
        let custom =
            !filter.include.is_empty() || !filter.exclude.is_empty() || !config_exclude.is_empty();
        let wanted = |name: &str| -> bool {
            let included = filter.include.iter().any(|p| matches(p, name));
            if !filter.include.is_empty() && !included {
                return false;
            }
            if filter.exclude.iter().any(|p| matches(p, name)) {
                return false;
            }
            if !included && config_exclude.iter().any(|p| matches(p, name)) {
                return false;
            }
            all || custom || DEFAULT_SET.iter().any(|p| matches(p, name))
        };
        let mut d = Decorations {
            map: HashMap::new(),
            head_target: None,
        };
        let mut refs: Vec<(String, ObjectId)> = Vec::new();
        let _ = repo.refs.for_each_ref("refs/", &mut |name, id| {
            refs.push((name.to_string(), *id));
        });
        refs.sort();
        for (name, id) in refs {
            if name.starts_with("refs/replace/") || !wanted(&name) {
                continue;
            }
            let (kind, short) = if let Some(s) = name.strip_prefix("refs/heads/") {
                (Kind::Branch, s)
            } else if let Some(s) = name.strip_prefix("refs/remotes/") {
                (Kind::Remote, s)
            } else if let Some(s) = name.strip_prefix("refs/tags/") {
                (Kind::Tag, s)
            } else if name == "refs/stash" {
                (Kind::Stash, name.as_str())
            } else {
                (Kind::Other, name.as_str())
            };
            let shown = if full {
                name.clone()
            } else {
                short.to_string()
            };
            let target = peel(repo, id);
            d.map.entry(target).or_default().push(Name {
                kind,
                shown,
                refname: name,
            });
        }
        for names in d.map.values_mut() {
            names.reverse();
        }
        if wanted("HEAD")
            && let Ok(Some(head)) = repo.refs.resolve("HEAD")
            && let Some(id) = head.oid
        {
            if head.name != "HEAD" {
                d.head_target = Some(head.name);
            }
            d.map.entry(id).or_default().insert(
                0,
                Name {
                    kind: Kind::Head,
                    shown: "HEAD".into(),
                    refname: "HEAD".into(),
                },
            );
        }
        d
    }

    /// The decoration of `id`, empty when it has none. `colors` when
    /// colouring.
    pub fn format(&self, id: &ObjectId, parts: &Parts, colors: Option<&Colors>) -> Vec<u8> {
        let Some(names) = self.map.get(id).filter(|n| !n.is_empty()) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let paint = |out: &mut Vec<u8>, color: &str, text: &[u8]| {
            if text.is_empty() {
                return;
            }
            match colors {
                Some(_) => {
                    out.extend_from_slice(color.as_bytes());
                    out.extend_from_slice(text);
                    out.extend_from_slice(crate::color::RESET.as_bytes());
                }
                None => out.extend_from_slice(text),
            }
        };
        let commit_color = colors.map_or("", |c| c.commit.as_str());
        let kind_color = |k: Kind| colors.map_or("", |c| c.of(k));
        paint(&mut out, commit_color, &parts.prefix);
        // The branch shown after `HEAD -> `.
        let pointed = names
            .first()
            .filter(|n| n.kind == Kind::Head)
            .and_then(|_| {
                let target = self.head_target.as_deref()?;
                names.iter().position(|n| n.refname == target)
            });
        let mut first = true;
        for (i, n) in names.iter().enumerate() {
            if Some(i) == pointed {
                continue;
            }
            if !first {
                paint(&mut out, commit_color, &parts.separator);
            }
            first = false;
            if n.kind == Kind::Tag {
                paint(&mut out, kind_color(Kind::Tag), &parts.tag);
            }
            paint(&mut out, kind_color(n.kind), n.shown.as_bytes());
            if n.kind == Kind::Head
                && let Some(p) = pointed
            {
                paint(&mut out, commit_color, &parts.pointer);
                paint(
                    &mut out,
                    kind_color(names[p].kind),
                    names[p].shown.as_bytes(),
                );
            }
        }
        paint(&mut out, commit_color, &parts.suffix);
        out
    }
}

/// Follow annotated tags to the object they finally point to.
fn peel(repo: &Repo<'_>, mut id: ObjectId) -> ObjectId {
    for _ in 0..64 {
        match repo.objects.read(&id) {
            Ok(Some((ObjectKind::Tag, data))) => {
                match Tag::parse(repo.objects.hash_kind(), &data).and_then(|t| t.object()) {
                    Ok(next) => id = next,
                    Err(_) => break,
                }
            }
            _ => break,
        }
    }
    id
}

#[cfg(test)]
mod tests {
    use super::matches;

    #[test]
    fn patterns() {
        assert!(matches("refs/heads", "refs/heads/main"));
        assert!(matches("refs/heads/", "refs/heads/main"));
        assert!(!matches("refs/head", "refs/heads/main"));
        assert!(matches("refs/stash", "refs/stash"));
        assert!(matches("refs/tags/v*", "refs/tags/v1"));
        assert!(matches("HEAD", "HEAD"));
        assert!(matches("tags/x", "refs/tags/x/side-2"));
        assert!(matches("heads/octopus*", "refs/heads/octopus-b"));
        assert!(matches("*side-2", "refs/tags/x/side-2"));
        assert!(!matches("tags/fo", "refs/tags/foo"));
        assert!(!matches("*", "HEAD"));
    }
}
