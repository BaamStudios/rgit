// SPDX-License-Identifier: MIT
//! The walk behind `git bundle create`: the commits `rev-list --boundary
//! <args>` lists, its boundary (the bundle's prerequisites), and the refs
//! the positive arguments name (`Documentation/git-bundle.adoc`,
//! "SPECIFYING REFERENCES").

use rgit_hash::ObjectId;
use rgit_revwalk::revision::{self, RefLookup, Resolver};
use rgit_revwalk::walk::Walk;

use super::options::{Options, Tip};
use super::{Command, Commits, Repo, msg, tips};
use crate::repo::{self as repo_mod, Mode};
use crate::revisions::{self, Config, IndexFile, Objects, Refs, WorkTree};
use crate::{Context, Die};

/// A ref a positive argument names.
#[derive(Debug, Clone)]
pub struct NamedRef {
    /// The name as written into the bundle (the full name; the name as
    /// given for a symbolic ref such as `HEAD`).
    pub name: String,
    /// The name as given (for messages).
    pub given: String,
    /// The value of the ref (a tag object for an annotated tag).
    pub id: ObjectId,
}

/// What [`bundle_walk`] found.
#[derive(Debug, Default)]
pub struct BundleWalk {
    /// The commits listed, in walk order.
    pub shown: Vec<ObjectId>,
    /// The boundary commits, in walk order.
    pub boundary: Vec<ObjectId>,
    /// The commits of negative arguments.
    pub negative: Vec<ObjectId>,
    /// The refs named by positive arguments (ambiguous and unknown names
    /// are left out), in argument order.
    pub refs: Vec<NamedRef>,
    /// `--since` / `--until`.
    pub max_age: Option<i64>,
    pub min_age: Option<i64>,
}

/// The names a positive revision argument stands for: the right side of
/// `A..B`, both sides of `A...B` (an empty side is `HEAD`), `X` of `X^!`
/// and `X^-<n>`, nothing for `X^@`.
fn arg_names(arg: &str, negate: bool) -> Vec<String> {
    let (arg, negative) = match arg.strip_prefix('^') {
        Some(a) => (a, !negate),
        None => (arg, negate),
    };
    let or_head = |s: &str| {
        if s.is_empty() {
            "HEAD".to_owned()
        } else {
            s.to_owned()
        }
    };
    if let Some((l, r)) = arg.split_once("...") {
        return if negative {
            Vec::new()
        } else {
            vec![or_head(l), or_head(r)]
        };
    }
    if let Some((_, r)) = arg.split_once("..") {
        return if negative {
            Vec::new()
        } else {
            vec![or_head(r)]
        };
    }
    if negative || arg.ends_with("^@") {
        return Vec::new();
    }
    if let Some(base) = arg.strip_suffix("^!") {
        return vec![base.to_owned()];
    }
    if let Some(pos) = arg.rfind("^-") {
        let rest = &arg[pos + 2..];
        if rest.bytes().all(|b| b.is_ascii_digit()) {
            return vec![arg[..pos].to_owned()];
        }
    }
    vec![arg.to_owned()]
}

/// Whether `name` (as given) resolves through a symbolic ref first.
fn is_symref(refs: &Refs, name: &str) -> bool {
    for rule in [
        "{}",
        "refs/{}",
        "refs/tags/{}",
        "refs/heads/{}",
        "refs/remotes/{}",
        "refs/remotes/{}/HEAD",
    ] {
        let full = rule.replace("{}", name);
        if let Ok(Some(v)) = refs.resolve(&full) {
            if v.oid.is_some() {
                return v.name != full;
            }
        }
    }
    false
}

/// Walk `rev-list --boundary <args>` (with `stdin` as the `--stdin`
/// input) and resolve the refs the positive arguments name.
pub fn bundle_walk(
    ctx: &Context,
    args: &[String],
    stdin: Option<&[u8]>,
) -> Result<BundleWalk, Die> {
    // Parsed like `log` arguments (rev-list would print its usage); an
    // unknown option is an `error:` for bundle.
    let mut opts = Options::parse(Command::Log, args).map_err(|e| match e {
        Die::Fatal(m) if m.starts_with("unrecognized argument") => Die::Error(m, 1),
        e => e,
    })?;
    let setup = repo_mod::setup(ctx, Mode::Required)?;
    let kind = setup.format.object_format;
    let config = revisions::load_config(ctx, &setup)?;
    let rev_opts = revisions::options(ctx, &config, kind)?;
    let shallow = setup.common_dir.join("shallow");
    let mut objects = Objects::new(revisions::objects_dir(&setup), kind, &shallow);
    let refs = Refs::new(&setup, kind);
    objects.use_replace_refs(&refs, &config, ctx.no_replace_objects);
    let cfg = Config(&config);
    let index = IndexFile::new(&setup, kind);
    let work_tree = setup.work_tree.clone().map(WorkTree);
    let rev_repo = revision::Repo {
        objects: &objects,
        refs: &refs,
        config: &cfg,
        index: Some(&index),
        work_tree: work_tree
            .as_ref()
            .map(|w| w as &dyn revision::WorkTreeLookup),
        work_tree_info: revisions::work_tree_info(&setup),
    };
    let resolver = Resolver::new(rev_repo, rev_opts);
    let repo = Repo {
        setup: &setup,
        config: &config,
        objects: &objects,
        refs: &refs,
        resolver: &resolver,
    };
    if let Some(input) = stdin {
        opts.read_stdin(input)?;
    }
    let resolved = tips::resolve_with(&repo, &opts, false)?;

    // The refs: positive arguments in order, duplicates once.
    let mut out = BundleWalk {
        max_age: opts.walk.max_age,
        min_age: opts.walk.min_age,
        ..BundleWalk::default()
    };
    let add = |out: &mut BundleWalk, name: String, given: String, id: ObjectId| {
        if !out.refs.iter().any(|r| r.name == name) {
            out.refs.push(NamedRef { name, given, id });
        }
    };
    for tip in &opts.tips {
        match tip {
            Tip::Rev { arg, negate } => {
                for name in arg_names(arg, *negate) {
                    let Ok(found) = resolver.dwim_ref(&name) else {
                        continue;
                    };
                    // An ambiguous name (warned about by the lookup) is
                    // left out.
                    let Some(id) = found.oid.filter(|_| found.count == 1) else {
                        continue;
                    };
                    let shown = if is_symref(&refs, &name) {
                        name.clone()
                    } else {
                        found.full_name.clone()
                    };
                    add(&mut out, shown, name, id);
                }
            }
            Tip::Refs {
                set,
                negative: false,
                exclude,
                hidden,
            } => {
                let hidden = hidden.as_deref().map(|s| tips::Hidden::new(&repo, s));
                for (name, id) in tips::ref_set(&repo, &opts, set, exclude, hidden.as_ref())? {
                    // Refs that resolve through a symbolic ref (`HEAD`)
                    // keep their name; read the value they point to.
                    add(&mut out, name.clone(), name, id);
                }
            }
            Tip::Refs { .. } => {}
        }
    }

    let mut walk_opts = opts.walk.clone();
    walk_opts.boundary = true;
    let mut walk = Walk::new(
        Commits {
            repo: &repo,
            grep: None,
            patch_ids: None,
            paths: None,
            mailmap: None,
            diff_limit: None,
            line_log: None,
        },
        walk_opts,
    );
    let walk_error = |id: ObjectId| Die::Fatal(msg::bad_object(&id.to_hex()));
    for (id, negative, side) in &resolved.commits {
        if *negative {
            out.negative.push(*id);
        }
        walk.push_side(id, *negative, *side).map_err(walk_error)?;
    }
    while let Some(s) = walk.next_shown().map_err(walk_error)? {
        if s.boundary {
            out.boundary.push(s.id);
        } else {
            out.shown.push(s.id);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::arg_names;

    #[test]
    fn names() {
        assert_eq!(arg_names("main", false), ["main"]);
        assert_eq!(arg_names("^main", false), Vec::<String>::new());
        assert_eq!(arg_names("main", true), Vec::<String>::new());
        assert_eq!(arg_names("v1..v2", false), ["v2"]);
        assert_eq!(arg_names("HEAD^..", false), ["HEAD"]);
        assert_eq!(arg_names("a...b", false), ["a", "b"]);
        assert_eq!(arg_names("main^!", false), ["main"]);
        assert_eq!(arg_names("main^@", false), Vec::<String>::new());
    }
}
