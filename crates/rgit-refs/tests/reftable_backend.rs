// SPDX-License-Identifier: MIT
//! The reftable backend against the reference git: two copies of one
//! repository, one changed by `git`, the other through this crate's
//! reference API. Afterwards both stacks have to hold the same tables
//! (same update indices, records and blocks; the random part of the
//! table names aside), and each side has to read what the other wrote.
//!
//! The comparisons need the git version this crate was written against
//! (2.56): the format is stable, but what git writes for an operation
//! (which log entries, when it compacts) has changed between releases.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};

use rgit_difftest::Fixture;
use rgit_hash::{HashKind, ObjectId};
use rgit_refs::files::{FilesOptions, FilesStore};
use rgit_refs::reflog::{Committer, Entry, FilesReflog, ReflogWriter};
use rgit_refs::reftable::stack::{self, ReftableOptions};
use rgit_refs::reftable::{RefValue, Table};
use rgit_refs::{
    IterItem, IterOptions, LogAllRefUpdates, RefHooks, RefStore, RefUpdate, ReflogUpdate, Target,
    Transaction,
};

/// The stack settings are per process: tests that depend on them take
/// this lock and set them.
fn settings(auto_compact: bool) -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    stack::set_options(ReftableOptions {
        auto_compact,
        ..ReftableOptions::default()
    });
    guard
}

const NAME: &str = "C O Mitter";
const EMAIL: &str = "committer@example.com";
const TIME: u64 = 1_700_000_000;

/// Two copies of a reftable repository with two commits and a tag.
struct Twins {
    fx: Fixture,
    git: PathBuf,
    rgit: PathBuf,
    c: Vec<ObjectId>,
    tag: ObjectId,
    /// The branch `HEAD` is on.
    branch: String,
    auto_compact: bool,
}

fn oid(text: &str) -> ObjectId {
    ObjectId::from_hex(HashKind::Sha1, text.trim().as_bytes()).expect("object id")
}

impl Twins {
    /// `None` (test skipped) without a reference git of the right version.
    fn new(auto_compact: bool, logs: bool) -> Option<Twins> {
        let fx = Fixture::new();
        if fx.is_skipped() {
            return None;
        }
        let version = String::from_utf8_lossy(&fx.git_output(["version"]).stdout).into_owned();
        if !version.contains(" 2.56.") {
            eprintln!(
                "skipped: the comparison needs git 2.56, found {}",
                version.trim()
            );
            return None;
        }
        let fx = fx
            .env("GIT_TEST_REFTABLE_AUTOCOMPACTION", "false")
            .git(["init", "-q", "--ref-format=reftable", "."])
            .git([
                "config",
                "core.logAllRefUpdates",
                if logs { "true" } else { "false" },
            ])
            .git(["config", "maintenance.auto", "false"])
            .git(["commit", "-q", "--allow-empty", "-m", "one"])
            .git(["commit", "-q", "--allow-empty", "-m", "two"])
            .git(["tag", "-a", "-m", "annotated", "v1"]);
        let rev = |name: &str| {
            oid(&String::from_utf8_lossy(
                &fx.git_stdout(["rev-parse", name]),
            ))
        };
        let c = vec![rev("HEAD~1"), rev("HEAD")];
        let tag = rev("v1");
        let branch = String::from_utf8_lossy(&fx.git_stdout(["symbolic-ref", "HEAD"]))
            .trim()
            .to_owned();
        let base = fx.home_dir().join("twins");
        let (git, rgit) = (base.join("git/.git"), base.join("rgit/.git"));
        copy_dir(&fx.work_dir().join(".git"), &git);
        copy_dir(&fx.work_dir().join(".git"), &rgit);
        Some(Twins {
            fx,
            git,
            rgit,
            c,
            tag,
            branch,
            auto_compact,
        })
    }

    /// Runs the reference git on its copy; returns stdout and stderr.
    fn git(&self, args: &[&str], stdin: &str) -> (bool, String, String) {
        let mut child = Command::new(self.fx.reference_git())
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.fx.home_dir())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_DIR", &self.git)
            .env("LC_ALL", "C")
            .env(
                "GIT_TEST_REFTABLE_AUTOCOMPACTION",
                if self.auto_compact { "true" } else { "false" },
            )
            .env("GIT_COMMITTER_NAME", NAME)
            .env("GIT_COMMITTER_EMAIL", EMAIL)
            .env("GIT_COMMITTER_DATE", format!("{TIME} +0100"))
            .args(args)
            .current_dir(&self.git)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn git");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn git_ok(&self, args: &[&str], stdin: &str) -> String {
        let (ok, out, err) = self.git(args, stdin);
        assert!(ok, "git {args:?}: {err}");
        out
    }

    fn store(&self, logs: LogAllRefUpdates) -> FilesStore {
        let options = FilesOptions {
            log_all_ref_updates: logs,
            ..FilesOptions::default()
        };
        let store = FilesStore::with_options(&self.rgit, HashKind::Sha1, options);
        assert!(store.is_reftable());
        store
    }

    fn logs(&self) -> FilesReflog {
        FilesReflog::new(&self.rgit, &self.rgit, HashKind::Sha1)
    }

    fn committer() -> Committer {
        Committer::new(NAME.as_bytes(), EMAIL.as_bytes(), TIME, 100)
    }

    fn hooks(&self) -> Hooks {
        Hooks {
            writer: ReflogWriter::new(self.logs(), Twins::committer()),
            peeled: HashMap::from([(self.tag, self.c[1])]),
        }
    }

    /// Commits `updates` in one transaction through this crate.
    fn apply(&self, updates: Vec<RefUpdate>) -> Result<(), String> {
        let store = self.store(LogAllRefUpdates::True);
        let mut tx = Transaction::new(&store);
        for u in updates {
            tx.push(u).map_err(|e| e.to_string())?;
        }
        tx.commit(&mut self.hooks()).map_err(|e| e.to_string())
    }

    fn assert_same(&self, label: &str) {
        rgit_difftest::assert_same_text(label, &dump(&self.git), &dump(&self.rgit));
    }
}

/// Reflog entries by the test's committer, and the peeled value of the
/// annotated tag (the object database of the tests).
struct Hooks {
    writer: ReflogWriter,
    peeled: HashMap<ObjectId, ObjectId>,
}

impl RefHooks for Hooks {
    fn reflog_append(&mut self, update: &ReflogUpdate) -> Result<(), String> {
        self.writer.reflog_append(update)
    }
    fn reflog_delete(&mut self, name: &str) {
        self.writer.reflog_delete(name);
    }
    fn peel(&mut self, oid: &ObjectId) -> Option<ObjectId> {
        self.peeled.get(oid).copied()
    }
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &dest);
        } else {
            std::fs::copy(entry.path(), &dest).unwrap();
        }
    }
}

fn tables(git_dir: &Path) -> Vec<Arc<Table>> {
    let dir = git_dir.join("reftable");
    let list = std::fs::read_to_string(dir.join("tables.list")).unwrap();
    list.lines()
        .map(|name| {
            let data = std::fs::read(dir.join(name)).unwrap();
            Arc::new(Table::parse(name, data).expect("table parses"))
        })
        .collect()
}

/// Everything in the stack except the random part of the table names.
fn dump(git_dir: &Path) -> String {
    let mut out = String::new();
    for table in tables(git_dir) {
        out.push_str(&format!(
            "table {}..{}\n",
            table.min_update_index(),
            table.max_update_index()
        ));
        for b in table.blocks().unwrap() {
            out.push_str(&format!(
                "  block {} len={} restarts={}\n",
                b.typ as char, b.len, b.restarts
            ));
        }
        let mut refs = table.refs();
        while let Some(r) = refs.next_record().unwrap() {
            let value = match &r.value {
                RefValue::Deletion => "deleted".to_owned(),
                RefValue::Direct(id) => id.to_hex(),
                RefValue::Peeled(id, p) => format!("{id} ^{p}"),
                RefValue::Symbolic(t) => format!("-> {}", String::from_utf8_lossy(t)),
            };
            out.push_str(&format!(
                "  ref {} @{} {value}\n",
                String::from_utf8_lossy(&r.name),
                r.update_index
            ));
        }
        let mut logs = table.logs();
        while let Some(l) = logs.next_record().unwrap() {
            let name = String::from_utf8_lossy(&l.refname).into_owned();
            match &l.update {
                None => out.push_str(&format!("  log {name} @{} deleted\n", l.update_index)),
                Some(u) => out.push_str(&format!(
                    "  log {name} @{} {} {} {} <{}> {} {} {:?}\n",
                    l.update_index,
                    u.old,
                    u.new,
                    String::from_utf8_lossy(&u.name),
                    String::from_utf8_lossy(&u.email),
                    u.time,
                    u.tz_offset,
                    String::from_utf8_lossy(&u.message)
                )),
            }
        }
    }
    out
}

/// The update index ranges of the tables, oldest first.
fn shape(git_dir: &Path) -> String {
    tables(git_dir)
        .iter()
        .map(|t| format!("{}-{} ", t.min_update_index(), t.max_update_index()))
        .collect()
}

#[test]
fn transactions_write_the_tables_git_writes() {
    let _guard = settings(false);
    let Some(t) = Twins::new(false, true) else {
        return;
    };
    let (one, two) = (t.c[0], t.c[1]);
    let null = HashKind::Sha1.null();

    t.git_ok(&["update-ref", "refs/heads/topic", &two.to_hex()], "");
    t.apply(vec![RefUpdate::update("refs/heads/topic", two, None)])
        .unwrap();
    t.assert_same("create a branch");

    t.git_ok(
        &[
            "update-ref",
            "-m",
            "move back",
            "refs/heads/topic",
            &one.to_hex(),
            &two.to_hex(),
        ],
        "",
    );
    t.apply(vec![
        RefUpdate::update("refs/heads/topic", one, Some(two)).message("move back"),
    ])
    .unwrap();
    t.assert_same("update with a message and an old value");

    // Tags get no reflog; the peeled value of an annotated tag is stored.
    t.git_ok(&["update-ref", "refs/tags/copy", &t.tag.to_hex()], "");
    t.apply(vec![RefUpdate::update("refs/tags/copy", t.tag, None)])
        .unwrap();
    t.assert_same("a tag pointing to a tag object");

    // Several references in one transaction, one of them the branch HEAD
    // is on: its change is logged for HEAD as well.
    let branch = t.branch.as_str();
    let stdin = format!(
        "create refs/heads/a {two}\ncreate refs/heads/b {one}\nupdate {branch} {one} {two}\n"
    );
    t.git_ok(&["update-ref", "-m", "batch", "--stdin"], &stdin);
    t.apply(vec![
        RefUpdate::create("refs/heads/a", two).message("batch"),
        RefUpdate::create("refs/heads/b", one).message("batch"),
        RefUpdate::update(branch, one, Some(two)).message("batch"),
    ])
    .unwrap();
    t.assert_same("several updates");

    // Through a symbolic reference: the target changes, both are logged.
    t.git_ok(&["symbolic-ref", "refs/heads/sym", "refs/heads/a"], "");
    t.apply(vec![
        RefUpdate::symref_update("refs/heads/sym", "refs/heads/a", None).no_deref(true),
    ])
    .unwrap();
    t.git_ok(
        &[
            "update-ref",
            "-m",
            "via sym",
            "refs/heads/sym",
            &one.to_hex(),
        ],
        "",
    );
    t.apply(vec![
        RefUpdate::update("refs/heads/sym", one, None).message("via sym"),
    ])
    .unwrap();
    t.assert_same("update through a symbolic reference");

    // Deleting a reference deletes its log entries.
    t.git_ok(&["update-ref", "-d", "refs/heads/b", &one.to_hex()], "");
    t.apply(vec![RefUpdate::delete(
        "refs/heads/b",
        HashKind::Sha1,
        Some(one),
    )])
    .unwrap();
    t.assert_same("delete");

    // What must fail fails with git's message and writes nothing.
    let cases: Vec<(Vec<String>, RefUpdate)> = vec![
        (
            vec!["refs/heads/topic/sub".into(), two.to_hex()],
            RefUpdate::update("refs/heads/topic/sub", two, None),
        ),
        (
            vec!["refs/heads".into(), two.to_hex()],
            RefUpdate::update("refs/heads", two, None),
        ),
        (
            vec!["refs/heads/topic".into(), two.to_hex(), two.to_hex()],
            RefUpdate::update("refs/heads/topic", two, Some(two)),
        ),
        (
            vec!["refs/heads/topic".into(), two.to_hex(), null.to_hex()],
            RefUpdate::create("refs/heads/topic", two),
        ),
        (
            vec!["refs/heads/none".into(), two.to_hex(), one.to_hex()],
            RefUpdate::update("refs/heads/none", two, Some(one)),
        ),
    ];
    for (args, update) in cases {
        let mut argv = vec!["update-ref"];
        argv.extend(args.iter().map(String::as_str));
        let (ok, _, err) = t.git(&argv, "");
        let ours = t.apply(vec![update]).unwrap_err();
        assert!(!ok, "git {argv:?} succeeded");
        let name = &args[0];
        assert_eq!(
            err,
            format!("fatal: update_ref failed for ref '{name}': {ours}\n"),
            "message for {argv:?}"
        );
    }
    t.assert_same("failed transactions");

    // Each side reads what the other wrote.
    let list = |dir: &Path| {
        let store = FilesStore::new(dir, HashKind::Sha1);
        let mut out = String::new();
        let options = IterOptions {
            prefix: String::new(),
            include_root_refs: true,
        };
        for item in store.iter(&options).unwrap() {
            let IterItem::Ref(r) = item else {
                panic!("unexpected item")
            };
            let value = match &r.target {
                Target::Object(id) => id.to_hex(),
                Target::Symbolic(s) => format!("-> {s}"),
            };
            out.push_str(&format!("{} {value}\n", r.name));
        }
        out
    };
    assert_eq!(list(&t.git), list(&t.rgit));
    let format = "--format=%(refname) %(if)%(symref)%(then)-> %(symref)%(else)%(objectname)%(end)";
    let by_git = |dir: &Path| {
        let out = Command::new(t.fx.reference_git())
            .env("GIT_DIR", dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", t.fx.home_dir())
            .args(["for-each-ref", "--include-root-refs", format])
            .output()
            .unwrap();
        assert!(out.status.success());
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    assert_eq!(by_git(&t.rgit), list(&t.rgit));
    assert_eq!(by_git(&t.git), by_git(&t.rgit));
    let entries = |dir: &Path, name: &str| {
        FilesReflog::new(dir, dir, HashKind::Sha1)
            .read(name)
            .unwrap()
            .unwrap_or_default()
    };
    for name in ["HEAD", branch, "refs/heads/sym", "refs/heads/a"] {
        assert_eq!(
            entries(&t.git, name),
            entries(&t.rgit, name),
            "reflog of {name}"
        );
        assert!(!entries(&t.rgit, name).is_empty(), "reflog of {name}");
    }
}

#[test]
fn compaction_follows_git() {
    let _guard = settings(true);
    // Without log blocks the file sizes, which steer compaction, do not
    // depend on the deflater.
    let Some(t) = Twins::new(true, false) else {
        return;
    };
    let two = t.c[1];
    let store = t.store(LogAllRefUpdates::False);
    let mut seed: u64 = 7;
    for round in 0..40 {
        seed = (seed * 1_103_515_245 + 12_345) % 2_147_483_648;
        let count = match seed % 9 {
            0..=3 => 1,
            4 | 5 => 3,
            6 => 10,
            7 => 40,
            _ => 150,
        };
        let names: Vec<String> = (0..count)
            .map(|i| format!("refs/heads/r{round}-{i}"))
            .collect();
        let stdin: String = names
            .iter()
            .map(|n| format!("update {n} {two}\n"))
            .collect();
        t.git_ok(&["update-ref", "--stdin"], &stdin);
        let mut tx = Transaction::new(&store);
        for n in &names {
            tx.push(RefUpdate::update(n.as_str(), two, None)).unwrap();
        }
        tx.commit(&mut t.hooks()).unwrap();
        if seed % 7 == 0 {
            t.git_ok(&["update-ref", "-d", &names[0]], "");
            let mut tx = Transaction::new(&store);
            tx.push(RefUpdate::delete(names[0].as_str(), HashKind::Sha1, None))
                .unwrap();
            tx.commit(&mut t.hooks()).unwrap();
        }
        assert_eq!(shape(&t.git), shape(&t.rgit), "tables after round {round}");
    }
    t.assert_same("after automatic compaction");

    // `pack-refs`: everything into one table; deletions are dropped.
    t.git_ok(&["pack-refs"], "");
    store.compact(false).unwrap();
    assert_eq!(tables(&t.rgit).len(), 1);
    t.assert_same("after full compaction");
    // The one table is byte for byte the one git wrote.
    assert_eq!(tables(&t.git)[0].bytes(), tables(&t.rgit)[0].bytes());
}

#[test]
fn reflog_edits_match_git() {
    let _guard = settings(false);
    let Some(t) = Twins::new(false, true) else {
        return;
    };
    let (one, two) = (t.c[0], t.c[1]);
    for (i, id) in [two, one, two].iter().enumerate() {
        let msg = format!("step {i}");
        t.git_ok(
            &["update-ref", "-m", &msg, "refs/heads/topic", &id.to_hex()],
            "",
        );
        t.apply(vec![
            RefUpdate::update("refs/heads/topic", *id, None).message(msg),
        ])
        .unwrap();
    }
    t.assert_same("setup");
    let logs = t.logs();
    let name = "refs/heads/topic";

    // Dropping one entry keeps the others at their update index.
    t.git_ok(&["reflog", "delete", "refs/heads/topic@{1}"], "");
    let mut kept = logs.read(name).unwrap().unwrap();
    assert_eq!(kept.len(), 3);
    kept.remove(1);
    logs.write_entries(name, &kept).unwrap();
    t.assert_same("reflog delete");
    assert_eq!(logs.read(name).unwrap().unwrap(), kept);

    // An emptied log still exists.
    t.git_ok(&["reflog", "expire", "--expire=all", name], "");
    logs.write_entries(name, &[]).unwrap();
    t.assert_same("reflog expire");
    assert!(logs.exists(name));
    assert_eq!(logs.read(name).unwrap(), Some(Vec::<Entry>::new()));
    assert!(logs.read_raw(name).unwrap().unwrap().is_empty());
    assert!(t.git(&["reflog", "exists", name], "").0);

    // New entries go on top of the emptied log.
    t.git_ok(&["update-ref", "-m", "again", name, &one.to_hex()], "");
    t.apply(vec![RefUpdate::update(name, one, None).message("again")])
        .unwrap();
    t.assert_same("entry after expiry");

    // Deleting the reference takes the log along.
    t.git_ok(&["update-ref", "-d", name], "");
    t.apply(vec![RefUpdate::delete(name, HashKind::Sha1, None)])
        .unwrap();
    t.assert_same("delete the reference");
    assert!(!logs.exists(name));
    assert!(logs.list().unwrap().contains(&"HEAD".to_owned()));
    assert!(!logs.list().unwrap().contains(&name.to_owned()));
}

#[test]
fn rename_and_copy_match_git() {
    let _guard = settings(false);
    let Some(t) = Twins::new(false, true) else {
        return;
    };
    let two = t.c[1];
    t.git_ok(&["branch", "src", &two.to_hex()], "");
    t.apply(vec![
        RefUpdate::create("refs/heads/src", two).message(format!("branch: Created from {two}")),
    ])
    .unwrap();
    t.assert_same("setup");
    let store = t.store(LogAllRefUpdates::True);
    let who = Twins::committer();

    t.git_ok(&["branch", "-c", "src", "dst"], "");
    store
        .rename_reftable(
            "refs/heads/src",
            "refs/heads/dst",
            &who,
            "Branch: copied refs/heads/src to refs/heads/dst",
            true,
        )
        .unwrap();
    t.assert_same("copy");

    // A rename takes two update indices: the old name goes, the new one
    // comes; the old name may become a directory of the new one.
    t.git_ok(&["branch", "-m", "src", "src/moved"], "");
    store
        .rename_reftable(
            "refs/heads/src",
            "refs/heads/src/moved",
            &who,
            "Branch: renamed refs/heads/src to refs/heads/src/moved",
            false,
        )
        .unwrap();
    t.assert_same("rename");

    let (ok, _, err) = t.git(&["branch", "-m", "dst", "src/moved/deeper"], "");
    let ours = store
        .rename_reftable(
            "refs/heads/dst",
            "refs/heads/src/moved/deeper",
            &who,
            "x",
            false,
        )
        .unwrap_err();
    assert!(!ok);
    assert_eq!(err, format!("error: {ours}\nfatal: branch rename failed\n"));
    t.assert_same("refused rename");
}

#[test]
fn a_locked_stack_is_left_alone() {
    let _guard = settings(false);
    let Some(t) = Twins::new(false, true) else {
        return;
    };
    let before = dump(&t.rgit);
    let lock = t.rgit.join("reftable/tables.list.lock");
    std::fs::write(&lock, "").unwrap();
    std::fs::write(t.git.join("reftable/tables.list.lock"), "").unwrap();

    let (ok, _, err) = t.git(&["update-ref", "refs/heads/x", &t.c[1].to_hex()], "");
    let ours = t
        .apply(vec![RefUpdate::update("refs/heads/x", t.c[1], None)])
        .unwrap_err();
    assert!(!ok);
    assert_eq!(
        err,
        format!("fatal: update_ref failed for ref 'refs/heads/x': {ours}\n")
    );

    let (ok, _, err) = t.git(&["pack-refs"], "");
    let ours = t.store(LogAllRefUpdates::True).compact(false).unwrap_err();
    assert!(!ok);
    assert_eq!(err, format!("error: unable to compact stack: {ours}\n"));

    std::fs::remove_file(&lock).unwrap();
    assert_eq!(dump(&t.rgit), before);
    assert!(
        std::fs::read_dir(t.rgit.join("reftable"))
            .unwrap()
            .flatten()
            .all(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                name == "tables.list" || name.ends_with(".ref")
            })
    );
}
