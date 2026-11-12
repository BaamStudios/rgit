// SPDX-License-Identifier: MIT
//! Remotes: `remote.<name>.*` (`git-config(1)`), the legacy
//! `$GIT_DIR/remotes/<name>` and `$GIT_DIR/branches/<name>` files
//! (`git-fetch(1)` "REMOTES"), a URL given in place of a remote name, and
//! the default remote.

use std::path::Path;

use bstr::{BStr, BString, ByteSlice};
use rgit_config::{ConfigSet, key::split_canonical};

use crate::error::{ConfigError, string_value};
use crate::messages;
use crate::refspec::{Direction, InvalidRefspec, RefspecSet};
use crate::rewrite::UrlRewrites;

/// `remote.<name>.tagOpt`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TagOpt {
    /// Follow tags that point into fetched history.
    #[default]
    Auto,
    /// `--no-tags`.
    NoTags,
    /// `--tags`.
    All,
}

/// Where a remote's definition came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteSource {
    /// `remote.<name>.*` with at least one URL.
    Config,
    /// `$GIT_DIR/remotes/<name>`.
    RemotesFile,
    /// `$GIT_DIR/branches/<name>`.
    BranchesFile,
    /// Not configured: the name is used as the URL.
    Url,
}

/// One remote with its URLs after `insteadOf` rewriting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remote {
    /// The name (or the URL for an unconfigured one).
    pub name: BString,
    /// Fetch URLs (`remote.<name>.url`, rewritten); the first one is used
    /// for fetching.
    pub url: Vec<BString>,
    /// The URLs a push goes to: every `pushurl` with `insteadOf`; without
    /// `pushurl`, the URLs that `pushInsteadOf` rewrites, or (when it
    /// rewrites none) the fetch URLs.
    pub push_url: Vec<BString>,
    /// Whether `remote.<name>.pushurl` is set.
    pub has_pushurl: bool,
    /// `remote.<name>.fetch`.
    pub fetch: RefspecSet,
    /// `remote.<name>.push`.
    pub push: RefspecSet,
    /// `remote.<name>.tagOpt`.
    pub tag_opt: TagOpt,
    /// `remote.<name>.prune`.
    pub prune: Option<bool>,
    /// `remote.<name>.pruneTags`.
    pub prune_tags: Option<bool>,
    /// `remote.<name>.mirror`.
    pub mirror: bool,
    /// `remote.<name>.skipFetchAll` / `skipDefaultUpdate`.
    pub skip_fetch_all: bool,
    /// `remote.<name>.uploadpack`.
    pub upload_pack: Option<BString>,
    /// `remote.<name>.receivepack`.
    pub receive_pack: Option<BString>,
    /// `remote.<name>.vcs`.
    pub vcs: Option<BString>,
    /// Where the definition came from.
    pub source: RemoteSource,
    /// Warnings to print (`warning: ` prefix) when the remote is used.
    pub warnings: Vec<String>,
}

impl Remote {
    /// The URL used for fetching.
    pub fn fetch_url(&self) -> Option<&BStr> {
        self.url.first().map(|u| u.as_bstr())
    }

    /// Whether the remote is defined (config or legacy file) rather than a
    /// bare URL.
    pub fn is_configured(&self) -> bool {
        self.source != RemoteSource::Url
    }
}

/// Errors while reading remotes.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RemoteError {
    /// A bad configuration value.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// A configured refspec is invalid.
    #[error(transparent)]
    Refspec(#[from] InvalidRefspec),
}

impl From<rgit_config::ValueError> for RemoteError {
    fn from(e: rgit_config::ValueError) -> Self {
        RemoteError::Config(e.into())
    }
}

/// The raw configuration of one remote, before resolution.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct RawRemote {
    name: BString,
    url: Vec<BString>,
    pushurl: Vec<BString>,
    fetch: RefspecSet,
    push: RefspecSet,
    tag_opt: TagOpt,
    prune: Option<bool>,
    prune_tags: Option<bool>,
    mirror: bool,
    skip_fetch_all: bool,
    upload_pack: Option<BString>,
    receive_pack: Option<BString>,
    vcs: Option<BString>,
}

/// All remotes of a configuration plus the URL rewrite rules.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Remotes {
    raw: Vec<RawRemote>,
    rewrites: UrlRewrites,
    push_default: Option<BString>,
}

impl Remotes {
    /// Read every `remote.*` and `url.*` entry. Like git, any invalid
    /// entry of any remote is an error, also for commands that use another
    /// remote.
    pub fn from_config(config: &ConfigSet) -> Result<Remotes, RemoteError> {
        let rewrites = UrlRewrites::from_config(config)?;
        let mut raw: Vec<RawRemote> = Vec::new();
        let mut fetch: Vec<Vec<BString>> = Vec::new();
        let mut push: Vec<Vec<BString>> = Vec::new();
        let mut push_default = None;
        for e in config.entries() {
            let (section, sub, key) = split_canonical(e.name.as_bstr());
            if section != "remote" {
                continue;
            }
            let Some(name) = sub else {
                if key == "pushdefault" {
                    push_default = Some(string_value(e)?.to_owned());
                }
                continue;
            };
            let idx = match raw.iter().position(|r| r.name == name) {
                Some(i) => i,
                None => {
                    raw.push(RawRemote {
                        name: name.to_owned(),
                        ..RawRemote::default()
                    });
                    fetch.push(Vec::new());
                    push.push(Vec::new());
                    raw.len() - 1
                }
            };
            let r = &mut raw[idx];
            match key.as_bytes() {
                b"url" | b"pushurl" => {
                    let v = string_value(e)?;
                    let list = if key == "url" {
                        &mut r.url
                    } else {
                        &mut r.pushurl
                    };
                    if v.is_empty() {
                        list.clear();
                    } else {
                        list.push(v.to_owned());
                    }
                }
                b"fetch" => fetch[idx].push(string_value(e)?.to_owned()),
                b"push" => push[idx].push(string_value(e)?.to_owned()),
                b"tagopt" => match string_value(e)?.as_bytes() {
                    b"--no-tags" => r.tag_opt = TagOpt::NoTags,
                    b"--tags" => r.tag_opt = TagOpt::All,
                    _ => {}
                },
                b"prune" => r.prune = Some(e.bool()?),
                b"prunetags" => r.prune_tags = Some(e.bool()?),
                b"mirror" => r.mirror = e.bool()?,
                b"skipdefaultupdate" | b"skipfetchall" => r.skip_fetch_all = e.bool()?,
                b"uploadpack" => r.upload_pack = Some(string_value(e)?.to_owned()),
                b"receivepack" => r.receive_pack = Some(string_value(e)?.to_owned()),
                b"vcs" => r.vcs = Some(string_value(e)?.to_owned()),
                _ => {}
            }
        }
        for (i, r) in raw.iter_mut().enumerate() {
            r.fetch = RefspecSet::parse(&fetch[i], Direction::Fetch)?;
            r.push = RefspecSet::parse(&push[i], Direction::Push)?;
        }
        Ok(Remotes {
            raw,
            rewrites,
            push_default,
        })
    }

    /// The URL rewrite rules.
    pub fn rewrites(&self) -> &UrlRewrites {
        &self.rewrites
    }

    /// `remote.pushDefault`.
    pub fn push_default(&self) -> Option<&BStr> {
        self.push_default.as_ref().map(|s| s.as_bstr())
    }

    /// The names of the remotes in the configuration, in first-seen order.
    pub fn names(&self) -> impl Iterator<Item = &BStr> {
        self.raw.iter().map(|r| r.name.as_bstr())
    }

    /// Resolve `name` like git does for a `<repository>` argument: a
    /// configured remote, else a legacy file in `git_dir`, else the name is
    /// used as a URL. A configured remote without URL also gets the name as
    /// its URL.
    pub fn get(&self, name: &[u8], git_dir: Option<&Path>) -> Result<Remote, RemoteError> {
        let configured = self.raw.iter().find(|r| r.name == name);
        let mut source = RemoteSource::Config;
        let mut warnings = Vec::new();
        let mut raw = configured.cloned().unwrap_or_else(|| RawRemote {
            name: name.into(),
            ..RawRemote::default()
        });
        if raw.url.is_empty() {
            match git_dir.and_then(|d| read_legacy(d, name)) {
                Some(legacy) => {
                    source = legacy.source;
                    warnings.push(messages::legacy_remote_warning(
                        if legacy.source == RemoteSource::RemotesFile {
                            "remotes"
                        } else {
                            "branches"
                        },
                        name.as_bstr(),
                    ));
                    raw.url = legacy.url;
                    raw.fetch = RefspecSet::parse(&legacy.fetch, Direction::Fetch)?;
                    raw.push = RefspecSet::parse(&legacy.push, Direction::Push)?;
                }
                None => {
                    if configured.is_none() {
                        source = RemoteSource::Url;
                    }
                    raw.url = vec![name.into()];
                }
            }
        }
        let url: Vec<BString> = raw.url.iter().map(|u| self.rewrites.rewrite(u)).collect();
        let has_pushurl = !raw.pushurl.is_empty();
        let push_url = if has_pushurl {
            raw.pushurl
                .iter()
                .map(|u| self.rewrites.rewrite(u))
                .collect()
        } else {
            // Like git: when `pushInsteadOf` rewrites any URL, only the
            // rewritten URLs are pushed to; otherwise all fetch URLs are.
            let rewritten: Vec<BString> = raw
                .url
                .iter()
                .filter_map(|u| self.rewrites.rewrite_push(u))
                .collect();
            if rewritten.is_empty() {
                url.clone()
            } else {
                rewritten
            }
        };
        Ok(Remote {
            name: raw.name,
            url,
            push_url,
            has_pushurl,
            fetch: raw.fetch,
            push: raw.push,
            tag_opt: raw.tag_opt,
            prune: raw.prune,
            prune_tags: raw.prune_tags,
            mirror: raw.mirror,
            skip_fetch_all: raw.skip_fetch_all,
            upload_pack: raw.upload_pack,
            receive_pack: raw.receive_pack,
            vcs: raw.vcs,
            source,
            warnings,
        })
    }

    /// The remote to fetch from: `branch.<name>.remote` of the current
    /// branch, else the only configured remote, else `origin`.
    pub fn default_remote_name(&self, branch_remote: Option<&BStr>) -> BString {
        if let Some(r) = branch_remote {
            return r.to_owned();
        }
        self.single_or_origin()
    }

    /// The remote to push to: `branch.<name>.pushRemote`, else
    /// `remote.pushDefault`, else the fetch default.
    pub fn push_remote_name(
        &self,
        branch_push_remote: Option<&BStr>,
        branch_remote: Option<&BStr>,
    ) -> BString {
        branch_push_remote
            .or(self.push_default())
            .map(|r| r.to_owned())
            .unwrap_or_else(|| self.default_remote_name(branch_remote))
    }

    fn single_or_origin(&self) -> BString {
        match self.raw.as_slice() {
            [only] => only.name.clone(),
            _ => "origin".into(),
        }
    }
}

struct Legacy {
    source: RemoteSource,
    url: Vec<BString>,
    fetch: Vec<BString>,
    push: Vec<BString>,
}

/// Read `$GIT_DIR/remotes/<name>` or `$GIT_DIR/branches/<name>`.
fn read_legacy(git_dir: &Path, name: &[u8]) -> Option<Legacy> {
    let file = name.to_str().ok()?;
    if file.is_empty() || file.contains('/') || file == "." || file == ".." {
        return None;
    }
    if let Ok(data) = std::fs::read(git_dir.join("remotes").join(file)) {
        return Some(parse_remotes_file(&data));
    }
    let data = std::fs::read(git_dir.join("branches").join(file)).ok()?;
    Some(parse_branches_file(&data, name))
}

/// Parse a `$GIT_DIR/remotes/<name>` file (`URL:`, `Push:`, `Pull:`
/// lines).
fn parse_remotes_file(data: &[u8]) -> Legacy {
    let mut out = Legacy {
        source: RemoteSource::RemotesFile,
        url: Vec::new(),
        fetch: Vec::new(),
        push: Vec::new(),
    };
    for line in data.lines() {
        let (list, rest) = if let Some(r) = line.strip_prefix(b"URL:") {
            (&mut out.url, r)
        } else if let Some(r) = line.strip_prefix(b"Push:") {
            (&mut out.push, r)
        } else if let Some(r) = line.strip_prefix(b"Pull:") {
            (&mut out.fetch, r)
        } else {
            continue;
        };
        let v = rest.trim_with(|c| c.is_ascii_whitespace());
        if !v.is_empty() {
            list.push(v.into());
        }
    }
    out
}

/// Parse a `$GIT_DIR/branches/<name>` file: `<url>[#<head>]`.
fn parse_branches_file(data: &[u8], name: &[u8]) -> Legacy {
    let line = data
        .lines()
        .next()
        .unwrap_or_default()
        .trim_with(|c| c.is_ascii_whitespace());
    let (url, head) = match line.find_byte(b'#') {
        Some(i) => (&line[..i], &line[i + 1..]),
        None => (line, &b"master"[..]),
    };
    let name = name.as_bstr();
    let head = head.as_bstr();
    Legacy {
        source: RemoteSource::BranchesFile,
        url: if url.is_empty() {
            Vec::new()
        } else {
            vec![url.into()]
        },
        fetch: vec![format!("refs/heads/{head}:refs/heads/{name}").into()],
        push: vec![format!("HEAD:refs/heads/{head}").into()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgit_config::{Origin, Scope, parse_bytes};

    fn remotes(text: &str) -> Result<Remotes, RemoteError> {
        let cfg = parse_bytes(
            text.as_bytes(),
            Origin::File(".git/config".into()),
            Scope::Local,
        )
        .unwrap();
        Remotes::from_config(&cfg)
    }

    #[test]
    fn urls_and_rewrites() {
        let r = remotes(
            "[url \"A\"]\n insteadOf = x\n[url \"C\"]\n insteadOf = xy\n\
             [url \"P\"]\n pushInsteadOf = x\n[url \"Q\"]\n pushInsteadOf = xyz\n\
             [remote \"r1\"]\n url = xfoo\n\
             [remote \"r2\"]\n url = xyzfoo\n pushurl = xyzbar\n\
             [remote \"r3\"]\n url = xyzfoo\n url = xyq\n url = yq\n\
             [remote \"r4\"]\n url = a\n url =\n url = later\n\
             [remote \"t\"]\n url = xx\n pushurl = p\n pushurl =\n",
        )
        .unwrap();
        let get = |n: &str| r.get(n.as_bytes(), None).unwrap();
        assert_eq!(get("r1").url, ["Afoo"]);
        assert_eq!(get("r1").push_url, ["Pfoo"]);
        assert_eq!(get("r2").push_url, ["Czbar"]);
        assert_eq!(get("r3").url, ["Czfoo", "Cq", "yq"]);
        assert_eq!(get("r3").push_url, ["Qfoo", "Pyq"]);
        assert_eq!(get("r4").url, ["later"]);
        assert_eq!(get("t").push_url, ["Px"]);
        let u = get("xelse");
        assert_eq!(u.url, ["Aelse"]);
        assert_eq!(u.source, RemoteSource::Url);
    }

    #[test]
    fn remote_without_url_uses_name() {
        let r = remotes("[remote \"r4\"]\n fetch = refs/heads/*:refs/remotes/r4/*\n").unwrap();
        let r4 = r.get(b"r4", None).unwrap();
        assert_eq!(r4.url, ["r4"]);
        assert!(r4.is_configured());
    }

    #[test]
    fn options() {
        let r = remotes(
            "[remote \"o\"]\n url = u\n tagOpt = --no-tags\n prune\n mirror = false\n\
             fetch = +refs/heads/*:refs/remotes/o/*\n push = HEAD\n uploadpack = up\n",
        )
        .unwrap();
        let o = r.get(b"o", None).unwrap();
        assert_eq!(o.tag_opt, TagOpt::NoTags);
        assert_eq!(o.prune, Some(true));
        assert!(!o.mirror);
        assert_eq!(o.fetch.items.len(), 1);
        assert_eq!(o.push.items[0].src, "HEAD");
        assert_eq!(o.upload_pack, Some("up".into()));
    }

    #[test]
    fn errors() {
        let e = remotes("[remote \"x\"]\n url = .\n[remote \"y\"]\n fetch = ::\n").unwrap_err();
        assert_eq!(e.to_string(), "invalid refspec '::'");
        let e = remotes("[remote \"y\"]\n\turl = .\n\turl\n").unwrap_err();
        let RemoteError::Config(c) = e else { panic!() };
        let lines: Vec<String> = c
            .report()
            .into_iter()
            .map(|(l, m)| format!("{} {m}", l.prefix()))
            .collect();
        assert_eq!(
            lines,
            [
                "error: missing value for 'remote.y.url'",
                "fatal: bad config variable 'remote.y.url' in file '.git/config' at line 3"
            ]
        );
        let e = remotes("[remote \"x\"]\n prune = bogus\n").unwrap_err();
        assert_eq!(
            e.to_string(),
            "bad boolean config value 'bogus' for 'remote.x.prune'"
        );
    }

    #[test]
    fn default_remote() {
        let one = remotes("[remote \"foo\"]\n url = F\n").unwrap();
        assert_eq!(one.default_remote_name(None), "foo");
        let two = remotes("[remote \"foo\"]\n url = F\n[remote \"bar\"]\n url = B\n[remote]\n pushDefault = foo\n").unwrap();
        assert_eq!(two.default_remote_name(None), "origin");
        assert_eq!(two.default_remote_name(Some(b"bar".as_bstr())), "bar");
        assert_eq!(two.push_remote_name(None, Some(b"bar".as_bstr())), "foo");
        assert_eq!(
            two.push_remote_name(Some(b"x".as_bstr()), Some(b"bar".as_bstr())),
            "x"
        );
    }

    #[test]
    fn legacy_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("remotes")).unwrap();
        std::fs::create_dir_all(dir.path().join("branches")).unwrap();
        std::fs::write(
            dir.path().join("remotes/rf"),
            "URL: remurl\nPull: refs/heads/a:refs/heads/b\nPush: refs/heads/c\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("branches/bf"), "brurl#head\n").unwrap();
        std::fs::write(dir.path().join("branches/bf2"), "brurl2\n").unwrap();
        let r = remotes("[remote \"conf\"]\n url = c\n").unwrap();
        let rf = r.get(b"rf", Some(dir.path())).unwrap();
        assert_eq!(rf.url, ["remurl"]);
        assert_eq!(rf.source, RemoteSource::RemotesFile);
        assert_eq!(rf.fetch.items[0].dst, Some("refs/heads/b".into()));
        assert!(
            rf.warnings[0]
                .starts_with("reading remote from \"remotes/rf\", which is nominated for removal.")
        );
        let bf = r.get(b"bf", Some(dir.path())).unwrap();
        assert_eq!(bf.url, ["brurl"]);
        assert_eq!(bf.fetch.items[0].as_bstr(), "refs/heads/head:refs/heads/bf");
        assert_eq!(bf.push.items[0].as_bstr(), "HEAD:refs/heads/head");
        let bf2 = r.get(b"bf2", Some(dir.path())).unwrap();
        assert_eq!(
            bf2.fetch.items[0].as_bstr(),
            "refs/heads/master:refs/heads/bf2"
        );
    }
}
