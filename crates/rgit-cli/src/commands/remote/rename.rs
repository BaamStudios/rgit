// SPDX-License-Identifier: MIT
//! `git remote rename` and `git remote remove`.

use bstr::ByteSlice;
use rgit_config::Scope;
use rgit_hash::ObjectId;
use rgit_refs::{IterItem, IterOptions, RefStore, RefUpdate, Target, Transaction};
use rgit_remote::{Remote, Remotes};

use super::common::{self, Repo};
use crate::messages::remote as msg;
use crate::prelude::*;
use crate::refs_support::{Objects, UpdateHooks};

/// Every reference below `prefix` with its stored value (symbolic
/// references included, also dangling ones).
fn raw_refs(repo: &Repo, prefix: &str) -> Vec<(String, Target)> {
    repo.store
        .iter(&IterOptions::prefix(prefix))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|i| match i {
            IterItem::Ref(r) => Some((r.name, r.target)),
            _ => None,
        })
        .collect()
}

pub fn rename(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let progress = Cell::new(-1);
    let mut parser = Parser::new(
        msg::USAGE_RENAME,
        vec![Opt::bool(None, "progress", &progress, msg::HELP_PROGRESS)],
    );
    let args = parser.parse(argv)?.args;
    if args.len() != 2 {
        return Err(parser.usage_error().into());
    }
    drop(parser);
    let (old, new) = (args[0].as_str(), args[1].as_str());

    let repo = Repo::open(ctx)?;
    let remotes = repo.remotes()?;
    if !repo.exists(old) {
        return Err(Die::Error(msg::no_such_remote_colon(old), 2));
    }
    let legacy = (!repo.configured_in_repo(old))
        .then(|| repo.legacy_file(old))
        .flatten();
    if (old != new || legacy.is_none()) && repo.exists(new) {
        return Err(Die::Error(msg::already_exists(new), 3));
    }
    if !common::valid_name(new) {
        return Err(Die::Fatal(msg::invalid_name(new)));
    }
    let remote = repo.remote(&remotes, old)?;

    if let Some(file) = legacy {
        migrate(&repo, &remote, new)?;
        let _ = std::fs::remove_file(file);
        if old == new {
            return Ok(0);
        }
    } else {
        repo.rename_section(&format!("remote.{old}"), Some(&format!("remote.{new}")))?;
    }

    // The remote-tracking refs move only when the fetch refspecs store
    // into `refs/remotes/<old>/`; such refspecs follow the new name.
    let specs = repo.repo_values(&format!("remote.{old}.fetch"));
    let from = format!(":refs/remotes/{old}/");
    let to = format!(":refs/remotes/{new}/");
    if specs.iter().any(|s| s.contains(&from)) {
        rename_refs(&repo, old, new)?;
    }
    if !specs.is_empty() {
        let key = format!("remote.{new}.fetch");
        repo.unset_all(&key)?;
        for spec in specs {
            let updated = if spec.contains(&from) {
                spec.replacen(&from, &to, 1)
            } else {
                eprintln!("warning: {}", msg::not_updating_refspec(&spec));
                spec
            };
            repo.add(&key, &updated)?;
        }
    }

    for b in common::read_branches(&repo.config) {
        if b.remote.as_deref() == Some(old) {
            repo.set(&format!("branch.{}.remote", b.name), new)?;
        }
        if b.push_remote.as_deref() == Some(old) {
            repo.set(&format!("branch.{}.pushRemote", b.name), new)?;
        }
    }
    push_default(&repo, old, Some(new))?;
    Ok(0)
}

/// Write a legacy remote (`remotes/` or `branches/` file) as
/// `remote.<name>.*`.
fn migrate(repo: &Repo, remote: &Remote, name: &str) -> Result<(), Die> {
    for url in &remote.url {
        repo.add(&format!("remote.{name}.url"), &url.to_str_lossy())?;
    }
    for spec in &remote.push.items {
        repo.add(&format!("remote.{name}.push"), &spec.as_bstr().to_str_lossy())?;
    }
    for spec in &remote.fetch.items {
        repo.add(&format!("remote.{name}.fetch"), &spec.as_bstr().to_str_lossy())?;
    }
    Ok(())
}

/// Move `refs/remotes/<old>/*` to `refs/remotes/<new>/*` with their
/// reflogs; symbolic references keep pointing into the renamed space.
fn rename_refs(repo: &Repo, old: &str, new: &str) -> Result<(), Die> {
    let old_prefix = format!("refs/remotes/{old}/");
    let new_prefix = format!("refs/remotes/{new}/");
    let moved = |name: &str| format!("{new_prefix}{}", &name[old_prefix.len()..]);
    let refs = raw_refs(repo, &old_prefix);
    if refs.is_empty() {
        return Ok(());
    }
    let hash = repo.setup.format.object_format;
    let mut tx = Transaction::new(&repo.store);
    let mut renamed: Vec<(String, String, Option<ObjectId>)> = Vec::new();
    let mut result = Ok(());
    for (name, target) in &refs {
        let to = moved(name);
        let (del, add, oid) = match target {
            Target::Object(oid) => (
                RefUpdate::delete(name.as_str(), hash, Some(*oid)),
                RefUpdate::create(to.as_str(), *oid),
                Some(*oid),
            ),
            Target::Symbolic(t) => {
                let t = match t.strip_prefix(&old_prefix) {
                    Some(rest) => format!("{new_prefix}{rest}"),
                    None => t.clone(),
                };
                (
                    RefUpdate::symref_delete(name.as_str(), hash, None),
                    RefUpdate::symref_update(to.as_str(), t, None),
                    None,
                )
            }
        };
        result = result
            .and_then(|()| tx.push(del.no_deref(true)))
            .and_then(|()| tx.push(add.no_deref(true)));
        renamed.push((name.clone(), to, oid));
    }
    let objects = Objects::open(&repo.setup);
    let mut quiet = UpdateHooks::new(&repo.setup, &repo.config, &objects).without_reflogs();
    if let Err(e) = result.and_then(|()| tx.commit(&mut quiet)) {
        eprintln!("error: {}", msg::rename_refs_failed(&e.to_string()));
        for line in msg::RENAME_CONFLICT_HINT.lines() {
            if line.is_empty() {
                eprintln!("hint:");
            } else {
                eprintln!("hint: {line}");
            }
        }
        return Err(Die::Exit(128));
    }
    let hooks = UpdateHooks::new(&repo.setup, &repo.config, &objects);
    if let Some(logs) = hooks.reflogs() {
        for (from, to, oid) in &renamed {
            if !logs.exists(from) {
                continue;
            }
            if logs.rename(from, to).is_err() {
                continue;
            }
            let id = oid.unwrap_or_else(|| ObjectId::null(hash));
            hooks.log(to, id, id, &msg::renamed_reflog(from, to), false);
        }
    }
    Ok(())
}

/// `remote.pushDefault` naming `old`: renamed (`new = Some`) or removed
/// when the repository configuration sets it; a system or global setting
/// only gets a warning.
fn push_default(repo: &Repo, old: &str, new: Option<&str>) -> Result<(), Die> {
    let Some(e) = repo.config.get("remote.pushdefault") else {
        return Ok(());
    };
    if e.value.as_ref().is_none_or(|v| v != old.as_bytes()) {
        return Ok(());
    }
    if common::in_repo(e) {
        match new {
            Some(n) => repo.set("remote.pushDefault", n)?,
            None => {
                repo.unset_all("remote.pushDefault")?;
            }
        }
    } else if matches!(e.source.scope, Scope::System | Scope::Global) {
        eprintln!(
            "warning: {}",
            msg::push_default_dangling(e.source.scope.as_str(), &format!("file:{}", e.line), old)
        );
    }
    Ok(())
}

pub fn remove(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let mut parser = Parser::new(msg::USAGE_REMOVE, vec![]);
    let args = parser.parse(argv)?.args;
    if args.len() != 1 {
        return Err(parser.usage_error().into());
    }
    drop(parser);
    let name = args[0].as_str();
    let repo = Repo::open(ctx)?;
    let remotes = repo.remotes()?;
    if !repo.exists(name) {
        return Err(Die::Error(msg::no_such_remote_colon(name), 2));
    }
    let remote = repo.remote(&remotes, name)?;

    for b in common::read_branches(&repo.config) {
        if b.remote.as_deref() == Some(name) {
            repo.unset_all(&format!("branch.{}.remote", b.name))?;
            repo.unset_all(&format!("branch.{}.merge", b.name))?;
        }
        if b.push_remote.as_deref() == Some(name) {
            repo.unset_all(&format!("branch.{}.pushRemote", b.name))?;
        }
    }

    let kept = delete_refs(&repo, &remotes, &remote)?;

    match repo.legacy_file(name) {
        Some(file) if !repo.configured_in_repo(name) => {
            let _ = std::fs::remove_file(file);
        }
        _ => repo.rename_section(&format!("remote.{name}"), None)?,
    }
    push_default(&repo, name, None)?;

    if !kept.is_empty() {
        let mut text = String::from(if kept.len() == 1 {
            msg::NOTE_ONE_BRANCH
        } else {
            msg::NOTE_BRANCHES
        });
        for b in &kept {
            text.push('\n');
            text.push_str(&msg::branch_delete_hint(b));
        }
        eprintln!("{text}");
    }
    Ok(0)
}

/// Delete the references `remote` fetches into, unless another remote
/// fetches into them too. Local branches are kept; their short names are
/// returned, sorted.
fn delete_refs(repo: &Repo, remotes: &Remotes, remote: &Remote) -> Result<Vec<String>, Die> {
    let mut others = Vec::new();
    for other in remotes.names() {
        if other == remote.name {
            continue;
        }
        if let Ok(r) = remotes.get(other, None) {
            others.push(r);
        }
    }
    let maps = |r: &Remote, name: &str| {
        r.fetch.positive().any(|s| {
            s.map_dst(name.as_bytes())
                .is_some_and(|src| !r.fetch.is_excluded(&src))
        })
    };
    let mut kept = Vec::new();
    let mut doomed = Vec::new();
    for (name, target) in raw_refs(repo, "") {
        if !maps(remote, &name) || others.iter().any(|o| maps(o, &name)) {
            continue;
        }
        if name.starts_with("refs/remotes/") {
            doomed.push((name, target));
        } else if let Some(b) = name.strip_prefix("refs/heads/") {
            kept.push(b.to_owned());
        }
    }
    if !doomed.is_empty() {
        let hash = repo.setup.format.object_format;
        let objects = Objects::open(&repo.setup);
        let mut hooks = UpdateHooks::new(&repo.setup, &repo.config, &objects);
        let mut tx = Transaction::new(&repo.store);
        let mut result = Ok(());
        for (name, target) in &doomed {
            let update = match target {
                Target::Object(oid) => RefUpdate::delete(name.as_str(), hash, Some(*oid)),
                Target::Symbolic(_) => RefUpdate::symref_delete(name.as_str(), hash, None),
            };
            result = result.and_then(|()| tx.push(update.no_deref(true)));
        }
        result
            .and_then(|()| tx.commit(&mut hooks))
            .map_err(|e| Die::Error(e.to_string(), 1))?;
    }
    kept.sort();
    Ok(kept)
}
