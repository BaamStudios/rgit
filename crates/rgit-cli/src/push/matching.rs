// SPDX-License-Identifier: MIT
//! Which remote refs a push updates with what: the expansion of push
//! refspecs against the local refs and the server's advertisement
//! (`Documentation/git-push.adoc`, `<refspec>...`).
//!
//! - `<src>` is a local ref (git's lookup rules; `refs/heads/` and
//!   `refs/tags/` matches win over others) or any object name;
//! - `<dst>` is a remote ref of that name (same rules), else, when it does
//!   not start with `refs/`, the namespace of a `refs/heads/` or
//!   `refs/tags/` source is prepended;
//! - a missing `<dst>` is the source's ref name (or what
//!   `remote.<name>.push` maps it to);
//! - an empty `<src>` deletes, `:` pushes matching branches, `*` patterns
//!   expand over the local refs, `^` refspecs exclude.
//!
//! The result lists existing remote refs in advertisement order, then new
//! refs in the order they were matched, as git prints them.

use rgit_hash::ObjectId;
use rgit_object::ObjectKind;
use rgit_protocol::push::AdvertisedRef;
use rgit_remote::refspec::refname_match;
use rgit_remote::{Refspec, RefspecSet};

use super::{PushRef, Status};
use crate::messages::push as msg;

/// The local side.
pub struct Local<'a> {
    /// Every ref (`refs/...`) with its value, sorted, plus `HEAD`.
    pub refs: &'a [(String, ObjectId)],
    /// The branch `HEAD` points to.
    pub head: Option<&'a str>,
    /// Resolve an object name (`HEAD~1`, an id).
    pub resolve: &'a dyn Fn(&str) -> Option<ObjectId>,
    /// The type of an object.
    pub kind: &'a dyn Fn(&ObjectId) -> Option<ObjectKind>,
}

/// One problem found while matching: the `error:` text and an optional
/// `hint:` text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchError {
    pub error: String,
    pub hint: Option<String>,
}

impl MatchError {
    fn new(error: String) -> Self {
        MatchError { error, hint: None }
    }
}

/// Find `name` among `names` with git's lookup rules. `Err` if it is
/// ambiguous.
fn dwim<'a>(name: &str, names: impl Iterator<Item = &'a str>) -> Result<Option<&'a str>, ()> {
    let mut strong = Vec::new();
    let mut weak = Vec::new();
    for full in names {
        if full == name {
            return Ok(Some(full));
        }
        if refname_match(name.as_bytes(), full.as_bytes()) {
            if full.starts_with("refs/heads/") || full.starts_with("refs/tags/") {
                strong.push(full);
            } else {
                weak.push(full);
            }
        }
    }
    let pick = if strong.is_empty() { weak } else { strong };
    match pick.len() {
        0 => Ok(None),
        1 => Ok(Some(pick[0])),
        _ => Err(()),
    }
}

/// Expand `specs` (command line, `--all`/`--tags`, `remote.<name>.push` or
/// `push.default`) into the list of updates. `remote_push` maps sources of
/// refspecs without destination (`remote.<name>.push`, only for refspecs
/// from the command line).
pub fn match_refs(
    local: &Local<'_>,
    remote: &[AdvertisedRef],
    specs: &RefspecSet,
    remote_push: Option<&RefspecSet>,
    force_all: bool,
) -> (Vec<PushRef>, Vec<MatchError>) {
    let mut out: Vec<PushRef> = Vec::new();
    let mut errors = Vec::new();
    let remote_oid = |name: &str| remote.iter().find(|r| r.name == name).map(|r| r.oid);
    let add = |out: &mut Vec<PushRef>,
               src: Option<String>,
               new: Option<ObjectId>,
               dst: String,
               force: bool| {
        out.push(PushRef {
            old: remote_oid(&dst),
            dst,
            new,
            src,
            force: force || force_all,
            status: Status::None,
            forced_update: false,
            remote_msg: None,
        });
    };
    for spec in specs.positive() {
        if spec.matching {
            for (name, oid) in local.refs {
                if name.starts_with("refs/heads/")
                    && remote_oid(name).is_some()
                    && !specs.is_excluded(name.as_bytes())
                {
                    add(
                        &mut out,
                        Some(name.clone()),
                        Some(*oid),
                        name.clone(),
                        spec.force,
                    );
                }
            }
        } else if spec.pattern {
            for (name, oid) in local.refs {
                if name == "HEAD" || specs.is_excluded(name.as_bytes()) {
                    continue;
                }
                if let Some(dst) = spec.map_src(name.as_bytes()) {
                    add(
                        &mut out,
                        Some(name.clone()),
                        Some(*oid),
                        dst.to_string(),
                        spec.force,
                    );
                }
            }
        } else {
            match explicit(local, remote, spec, remote_push) {
                Ok((src, new, dst)) => add(&mut out, src, new, dst, spec.force),
                Err(e) => errors.push(e),
            }
        }
    }
    // A destination may be updated from one source only.
    let mut seen: Vec<&str> = Vec::new();
    for (i, r) in out.iter().enumerate() {
        if out[..i].iter().any(|o| o.dst == r.dst) && !seen.contains(&r.dst.as_str()) {
            seen.push(&r.dst);
            errors.push(MatchError::new(msg::dst_multiple_src(&r.dst)));
        }
    }
    // Existing remote refs in advertisement order, then the new ones.
    let pos = |r: &PushRef| {
        remote
            .iter()
            .position(|a| a.name == r.dst)
            .unwrap_or(usize::MAX)
    };
    out.sort_by_key(pos);
    (out, errors)
}

/// One refspec without pattern: `(src display name, new value, dst)`.
fn explicit(
    local: &Local<'_>,
    remote: &[AdvertisedRef],
    spec: &Refspec,
    remote_push: Option<&RefspecSet>,
) -> Result<(Option<String>, Option<ObjectId>, String), MatchError> {
    let src = spec.src.to_string();
    let remote_names = || remote.iter().map(|r| r.name.as_str());
    if src.is_empty() {
        // Deletion.
        let dst = spec.dst.as_ref().map(|d| d.to_string()).unwrap_or_default();
        return match dwim(&dst, remote_names()) {
            Ok(Some(full)) => Ok((None, None, full.to_owned())),
            Ok(None) => Err(MatchError::new(msg::unable_to_delete(&dst))),
            Err(()) => Err(MatchError::new(msg::dst_ambiguous(&dst))),
        };
    }
    let local_names = || local.refs.iter().map(|(n, _)| n.as_str());
    let (src_ref, oid) = match dwim(&src, local_names()) {
        Err(()) => return Err(MatchError::new(msg::src_ambiguous(&src))),
        Ok(Some(full)) => {
            let oid = local.refs.iter().find(|(n, _)| n == full).map(|(_, o)| *o);
            (Some(full.to_owned()), oid)
        }
        Ok(None) => (None, (local.resolve)(&src)),
    };
    let Some(oid) = oid else {
        return Err(MatchError::new(msg::src_no_match(&src)));
    };
    // The ref the source stands for (HEAD: the branch it points to).
    let base = match src_ref.as_deref() {
        Some("HEAD") => local.head.map(str::to_owned),
        other => other.map(str::to_owned),
    };
    let display = src_ref.clone().unwrap_or_else(|| src.clone());
    let dst_text = match &spec.dst {
        Some(d) => d.to_string(),
        None => {
            let mapped = base
                .as_deref()
                .and_then(|b| remote_push.and_then(|p| p.map_src(b.as_bytes())));
            match (mapped, &base) {
                (Some(m), _) => m.to_string(),
                (None, Some(b)) => b.clone(),
                (None, None) => display.clone(),
            }
        }
    };
    if dst_text.starts_with("refs/") {
        return Ok((Some(display), Some(oid), dst_text));
    }
    match dwim(&dst_text, remote_names()) {
        Err(()) => return Err(MatchError::new(msg::dst_ambiguous(&dst_text))),
        Ok(Some(full)) => return Ok((Some(display), Some(oid), full.to_owned())),
        Ok(None) => {}
    }
    for prefix in ["refs/heads/", "refs/tags/"] {
        if base.as_deref().is_some_and(|b| b.starts_with(prefix)) {
            return Ok((Some(display), Some(oid), format!("{prefix}{dst_text}")));
        }
    }
    let hint =
        (local.kind)(&oid).and_then(|k| msg::unqualified_dst_hint(&k.to_string(), &src, &dst_text));
    Err(MatchError {
        error: msg::unqualified_dst(&dst_text, &src),
        hint,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgit_hash::HashKind;
    use rgit_remote::Direction;

    fn oid(n: u8) -> ObjectId {
        let mut hex = String::new();
        for _ in 0..20 {
            hex.push_str(&format!("{n:02x}"));
        }
        ObjectId::from_hex(HashKind::Sha1, hex.as_bytes()).unwrap()
    }

    fn run(
        specs: &[&str],
        remote_refs: &[(&str, u8)],
    ) -> (Vec<(Option<String>, String)>, Vec<String>) {
        let refs = vec![
            ("HEAD".to_string(), oid(1)),
            ("refs/heads/main".to_string(), oid(1)),
            ("refs/heads/topic".to_string(), oid(2)),
            ("refs/tags/v1".to_string(), oid(3)),
        ];
        let resolve = |s: &str| (s == "HEAD~1").then(|| oid(9));
        let kind = |_: &ObjectId| Some(ObjectKind::Commit);
        let local = Local {
            refs: &refs,
            head: Some("refs/heads/main"),
            resolve: &resolve,
            kind: &kind,
        };
        let remote: Vec<AdvertisedRef> = remote_refs
            .iter()
            .map(|(n, o)| AdvertisedRef {
                name: n.to_string(),
                oid: oid(*o),
            })
            .collect();
        let set = RefspecSet::parse(specs, Direction::Push).unwrap();
        let (out, errs) = match_refs(&local, &remote, &set, None, false);
        (
            out.into_iter().map(|r| (r.src, r.dst)).collect(),
            errs.into_iter().map(|e| e.error).collect(),
        )
    }

    fn pair(src: &str, dst: &str) -> (Option<String>, String) {
        (Some(src.to_string()), dst.to_string())
    }

    #[test]
    fn plain_and_qualified() {
        let (out, errs) = run(&["main", "topic:other", "HEAD~1:refs/heads/x"], &[]);
        assert!(errs.is_empty());
        assert_eq!(
            out,
            vec![
                pair("refs/heads/main", "refs/heads/main"),
                pair("refs/heads/topic", "refs/heads/other"),
                pair("HEAD~1", "refs/heads/x"),
            ]
        );
        let (out, _) = run(&["HEAD", "v1:v2"], &[]);
        assert_eq!(
            out,
            vec![
                pair("HEAD", "refs/heads/main"),
                pair("refs/tags/v1", "refs/tags/v2")
            ]
        );
    }

    #[test]
    fn order_and_remote_lookup() {
        let (out, _) = run(
            &["topic:new", "main:z"],
            &[("refs/heads/a", 1), ("refs/heads/z", 5)],
        );
        assert_eq!(out[0].1, "refs/heads/z");
        assert_eq!(out[1].1, "refs/heads/new");
        let (out, _) = run(&["HEAD~1:z"], &[("refs/heads/z", 5)]);
        assert_eq!(out, vec![pair("HEAD~1", "refs/heads/z")]);
    }

    #[test]
    fn errors() {
        let (_, errs) = run(&["nope", ":gone", "main:x", "topic:x", "HEAD~1:foo"], &[]);
        assert_eq!(errs[0], msg::src_no_match("nope"));
        assert_eq!(errs[1], msg::unable_to_delete("gone"));
        assert!(errs[2].starts_with("The destination you provided"));
        assert_eq!(errs[3], msg::dst_multiple_src("refs/heads/x"));
    }

    #[test]
    fn matching_and_patterns() {
        let (out, _) = run(&[":"], &[("refs/heads/topic", 7)]);
        assert_eq!(out, vec![pair("refs/heads/topic", "refs/heads/topic")]);
        let (out, _) = run(&["refs/heads/*", "^refs/heads/topic"], &[]);
        assert_eq!(out, vec![pair("refs/heads/main", "refs/heads/main")]);
        let (out, _) = run(&[":topic"], &[("refs/heads/topic", 7)]);
        assert_eq!(out, vec![(None, "refs/heads/topic".to_string())]);
    }
}
