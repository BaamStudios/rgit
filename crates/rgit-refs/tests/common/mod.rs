// SPDX-License-Identifier: MIT
//! Helpers for the differential tests: two copies of one repository, one
//! changed by the reference `git`, the other by this crate, then compared.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rgit_difftest::Fixture;
use rgit_hash::{HashKind, ObjectId};
use rgit_refs::files::FilesStore;
use rgit_refs::{OldValue, RefHooks, RefUpdate, Transaction, TransactionError, messages};

/// A repository with three commits `c[0..3]` and an annotated tag.
pub struct Repo {
    pub fx: Fixture,
    pub c: Vec<ObjectId>,
    pub tag: ObjectId,
}

impl Repo {
    /// `None` when no reference git is available (test skipped).
    pub fn new() -> Option<Self> {
        let mut fx = Fixture::new().git(["init", "-q"]);
        if fx.is_skipped() {
            return None;
        }
        let mut c = Vec::new();
        for i in 0..3 {
            fx = fx
                .git(["commit", "-q", "--allow-empty", "-m", &format!("c{i}")])
                .tick();
            c.push(oid(&fx.git_stdout(["rev-parse", "HEAD"])));
        }
        fx = fx.git(["tag", "-a", "-m", "t", "annotated", "HEAD~1"]);
        let tag = oid(&fx.git_stdout(["rev-parse", "annotated"]));
        Some(Self { fx, c, tag })
    }

    /// Two fresh copies of the repository: (git side, rgit side).
    pub fn twins(&self, label: &str) -> (PathBuf, PathBuf) {
        let base = self.fx.home_dir().join("twins").join(label);
        let _ = std::fs::remove_dir_all(&base);
        let a = base.join("git/.git");
        let b = base.join("rgit/.git");
        copy_dir(&self.fx.work_dir().join(".git"), &a);
        copy_dir(&self.fx.work_dir().join(".git"), &b);
        (a, b)
    }

    pub fn git(&self) -> PathBuf {
        self.fx.reference_git().to_path_buf()
    }
}

pub fn oid(stdout: &[u8]) -> ObjectId {
    let s = String::from_utf8_lossy(stdout);
    ObjectId::from_hex(HashKind::Sha1, s.trim().as_bytes()).expect("hex oid")
}

pub fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let ty = entry.file_type().unwrap();
        let dest = to.join(entry.file_name());
        if ty.is_dir() {
            copy_dir(&entry.path(), &dest);
        } else if ty.is_symlink() {
            #[cfg(unix)]
            std::os::unix::fs::symlink(std::fs::read_link(entry.path()).unwrap(), &dest).unwrap();
        } else {
            std::fs::copy(entry.path(), &dest).unwrap();
        }
    }
}

/// Output of a git command.
pub struct GitRun {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Runs the reference git with `GIT_DIR=git_dir`.
pub fn run_git(repo: &Repo, git_dir: &Path, args: &[&str], stdin: &[u8]) -> GitRun {
    let mut cmd = Command::new(repo.git());
    cmd.env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", repo.fx.home_dir())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_DIR", git_dir)
        .env("LC_ALL", "C")
        .env("GIT_COMMITTER_NAME", "C")
        .env("GIT_COMMITTER_EMAIL", "c@example.com")
        .env("GIT_COMMITTER_DATE", "1700000000 +0000")
        .args(args)
        .current_dir(git_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn git");
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    let out = child.wait_with_output().unwrap();
    GitRun {
        ok: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// Everything the reference store owns below `git_dir`: the `refs/` tree
/// (directories included), `packed-refs`, root reference files and any
/// left-over `*.lock` file.
pub fn snapshot(git_dir: &Path) -> String {
    let mut out = BTreeMap::new();
    walk(git_dir, git_dir, &mut out);
    let mut s = String::new();
    for (k, v) in out {
        s.push_str(&k);
        s.push_str(" => ");
        s.push_str(&v);
        s.push('\n');
    }
    s
}

fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let path = e.path();
        let rel = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let top_level = !rel.contains('/');
        let name = e.file_name().to_string_lossy().into_owned();
        let ty = e.file_type().unwrap();
        let interesting = rel == "refs"
            || rel.starts_with("refs/")
            || rel == "packed-refs"
            || name.ends_with(".lock")
            || (top_level
                && !name.is_empty()
                && name
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b == b'_' || b == b'-'));
        if ty.is_dir() {
            if rel == "objects" || rel == "logs" {
                continue;
            }
            if interesting {
                out.insert(rel.clone(), "<dir>".into());
            }
            walk(root, &path, out);
        } else if interesting {
            let content = if ty.is_symlink() {
                format!("<symlink {}>", std::fs::read_link(&path).unwrap().display())
            } else {
                format!(
                    "{:?}",
                    String::from_utf8_lossy(&std::fs::read(&path).unwrap())
                )
            };
            out.insert(rel, content);
        }
    }
}

/// One `update-ref --stdin` command.
#[derive(Clone, Debug)]
pub enum Cmd {
    Update(String, ObjectId, Option<ObjectId>),
    Create(String, ObjectId),
    Delete(String, Option<ObjectId>),
    Verify(String, Option<ObjectId>),
    SymrefUpdate(String, String, Option<OldValue>),
    SymrefCreate(String, String),
    SymrefDelete(String, Option<String>),
    SymrefVerify(String, Option<String>),
}

/// A command plus `option no-deref`.
#[derive(Clone, Debug)]
pub struct Step {
    pub cmd: Cmd,
    pub no_deref: bool,
}

pub fn stdin_text(steps: &[Step]) -> String {
    let mut s = String::new();
    for step in steps {
        if step.no_deref {
            s.push_str("option no-deref\n");
        }
        let line = match &step.cmd {
            Cmd::Update(r, n, Some(o)) => format!("update {r} {n} {o}"),
            Cmd::Update(r, n, None) => format!("update {r} {n}"),
            Cmd::Create(r, n) => format!("create {r} {n}"),
            Cmd::Delete(r, Some(o)) => format!("delete {r} {o}"),
            Cmd::Delete(r, None) => format!("delete {r}"),
            Cmd::Verify(r, Some(o)) => format!("verify {r} {o}"),
            Cmd::Verify(r, None) => format!("verify {r}"),
            Cmd::SymrefUpdate(r, t, None) => format!("symref-update {r} {t}"),
            Cmd::SymrefUpdate(r, t, Some(OldValue::Symbolic(o))) => {
                format!("symref-update {r} {t} ref {o}")
            }
            Cmd::SymrefUpdate(r, t, Some(OldValue::Object(o))) => {
                format!("symref-update {r} {t} oid {o}")
            }
            Cmd::SymrefUpdate(..) => unreachable!(),
            Cmd::SymrefCreate(r, t) => format!("symref-create {r} {t}"),
            Cmd::SymrefDelete(r, Some(o)) => format!("symref-delete {r} {o}"),
            Cmd::SymrefDelete(r, None) => format!("symref-delete {r}"),
            Cmd::SymrefVerify(r, Some(o)) => format!("symref-verify {r} {o}"),
            Cmd::SymrefVerify(r, None) => format!("symref-verify {r}"),
        };
        s.push_str(&line);
        s.push('\n');
    }
    s
}

pub fn to_update(step: &Step) -> RefUpdate {
    let k = HashKind::Sha1;
    let null = ObjectId::null(k);
    let u = match &step.cmd {
        Cmd::Update(r, n, o) => RefUpdate::update(r.clone(), *n, *o),
        Cmd::Create(r, n) => RefUpdate::create(r.clone(), *n),
        Cmd::Delete(r, o) => RefUpdate::delete(r.clone(), k, *o),
        Cmd::Verify(r, o) => RefUpdate::verify(r.clone(), o.unwrap_or(null)),
        Cmd::SymrefUpdate(r, t, o) => RefUpdate::symref_update(r.clone(), t.clone(), o.clone()),
        Cmd::SymrefCreate(r, t) => RefUpdate::symref_create(r.clone(), t.clone()),
        Cmd::SymrefDelete(r, o) => RefUpdate::symref_delete(r.clone(), k, o.clone()),
        Cmd::SymrefVerify(r, o) => RefUpdate::symref_verify(r.clone(), o.clone()),
    };
    if step.no_deref { u.no_deref(true) } else { u }
}

/// Stands in for the object database: the annotated tag is the only
/// object that is not a commit.
pub struct TestHooks {
    pub tag: ObjectId,
}

impl RefHooks for TestHooks {
    fn check_object(&mut self, name: &str, oid: &ObjectId) -> Result<(), String> {
        if *oid == self.tag && (name == "HEAD" || name.starts_with("refs/heads/")) {
            return Err(messages::non_commit_on_branch(oid, name));
        }
        Ok(())
    }
}

/// Applies `steps` with this crate; returns the message git would print.
pub fn apply_rgit(repo: &Repo, git_dir: &Path, steps: &[Step]) -> Result<(), String> {
    let store = FilesStore::new(git_dir, HashKind::Sha1);
    let mut tx = Transaction::new(&store);
    for s in steps {
        tx.push(to_update(s)).map_err(|e| e.to_string())?;
    }
    tx.commit(&mut TestHooks { tag: repo.tag })
        .map_err(|e: TransactionError| format!("fatal: {e}\n"))
}

/// Whether two updates reach the same reference and at least one of them
/// through a symbolic reference. git's choice of error message then depends
/// on internals (dangling referents, `HEAD`), so only the outcome and the
/// state are compared.
fn reaches_twice(git_dir: &Path, steps: &[Step]) -> bool {
    use rgit_refs::RefStore;
    let store = FilesStore::new(git_dir, HashKind::Sha1);
    let mut seen: Vec<(String, bool)> = Vec::new();
    for step in steps {
        let name = to_update(step).name;
        let mut finals = vec![(name.clone(), false)];
        if !step.no_deref {
            if let Ok(Some(r)) = store.resolve(&name) {
                finals = vec![(r.final_name().to_owned(), r.is_symbolic())];
            }
        }
        if let Ok(Some(rgit_refs::Target::Symbolic(t))) = store.read_raw("HEAD") {
            if finals.iter().any(|(n, _)| *n == t) || name == "HEAD" {
                finals.push(("HEAD".into(), true));
                finals.push((t, true));
            }
        }
        for (f, via) in finals {
            if seen.iter().any(|(n, v)| *n == f && (*v || via)) {
                return true;
            }
            seen.push((f, via));
        }
    }
    false
}

/// Whether a step follows a symbolic reference.
fn via_symref(git_dir: &Path, steps: &[Step]) -> bool {
    use rgit_refs::RefStore;
    let store = FilesStore::new(git_dir, HashKind::Sha1);
    steps.iter().any(|step| {
        !step.no_deref
            && store
                .resolve(&to_update(step).name)
                .ok()
                .flatten()
                .is_some_and(|r| r.is_symbolic())
    })
}

/// Runs `steps` on both twins and asserts identical outcome, message and
/// state. Returns whether the transaction succeeded.
pub fn compare_steps(repo: &Repo, a: &Path, b: &Path, steps: &[Step]) -> bool {
    let text = stdin_text(steps);
    let before = snapshot(a);
    let aliased = reaches_twice(b, steps);
    let symref = via_symref(b, steps);
    let git = run_git(repo, a, &["update-ref", "--stdin"], text.as_bytes());
    let ours = apply_rgit(repo, b, steps);
    let git_err = git.stderr.replace(&a.display().to_string(), "$GIT_DIR");
    if std::env::var_os("RGIT_REFS_DEBUG").is_some() {
        eprint!("{git_err}");
    }
    let our_err = match &ours {
        Ok(()) => String::new(),
        Err(e) => e.replace(&b.display().to_string(), "$GIT_DIR"),
    };
    // Known git quirk: a reference reached twice through symbolic
    // references is not reported as a duplicate; git fails late with
    // "couldn't write '<ref>.lock'", this crate when locking it again.
    //
    // When a referent of a symbolic reference conflicts with another name
    // (directory/file), git's choice among several conflicts depends on its
    // internal ordering; only the outcome is compared then.
    let df = |m: &str| m.contains("cannot process") || m.contains("exists; cannot create");
    let both_fail = !git.ok && ours.is_err();
    let quirk = (both_fail && git_err.contains("couldn't write"))
        || (both_fail && aliased)
        || (both_fail && df(&git_err) && df(&our_err) && symref);
    assert_eq!(
        (git.ok, if quirk { "" } else { git_err.as_str() }),
        (ours.is_ok(), if quirk { "" } else { our_err.as_str() }),
        "outcome differs for:\n{text}\nstate before:\n{before}"
    );
    let mut sa = snapshot(a);
    let mut sb = snapshot(b);
    if both_fail && aliased {
        // Where git gives up decides which empty directories it removed.
        let no_dirs = |s: &str| -> String {
            s.lines()
                .filter(|l| !l.ends_with("<dir>"))
                .map(|l| format!("{l}\n"))
                .collect()
        };
        sa = no_dirs(&sa);
        sb = no_dirs(&sb);
    }
    rgit_difftest::assert_same_text(
        &format!("state after\n{text}\nstate before:\n{before}"),
        &sa,
        &sb,
    );
    git.ok
}
