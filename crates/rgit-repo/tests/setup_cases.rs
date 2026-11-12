// SPDX-License-Identifier: MIT
//! Differential tests of repository setup outside the t1510 matrix: `.git`
//! files, ceilings, ownership, `safe.bareRepository`, repository format
//! and extensions, `HEAD` validation, linked work trees.

mod common;

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use common::World;

struct Cases {
    w: World,
    n: usize,
    count: usize,
    mismatches: Vec<String>,
}

impl Cases {
    fn new() -> Option<Cases> {
        Some(Cases {
            w: World::new()?,
            n: 0,
            count: 0,
            mismatches: Vec::new(),
        })
    }

    /// A fresh directory with a new repository `repo` inside.
    fn repo(&mut self) -> PathBuf {
        self.n += 1;
        let dir = self.w.root.join(format!("c{}", self.n));
        std::fs::create_dir_all(&dir).unwrap();
        self.w.git(&dir, &["init", "-q", "repo"]);
        dir.join("repo")
    }

    fn git(&self, cwd: &Path, args: &[&str]) {
        self.w.git(cwd, args);
    }

    fn check(&mut self, what: &str, cwd: &Path, extra: &[(&str, &str)]) {
        let extra: Vec<(&str, OsString)> = extra.iter().map(|(k, v)| (*k, v.into())).collect();
        let expect = self.w.reference(cwd, &extra);
        let got = self.w.ours(cwd, &extra);
        self.count += 1;
        if expect != got {
            self.mismatches.push(format!(
                "{what}\n  cwd={}\n  env={extra:?}\n--- git\n{expect}--- rgit\n{got}",
                cwd.display()
            ));
        }
    }

    fn finish(self) {
        assert!(
            self.mismatches.is_empty(),
            "{} of {} cases differ:\n{}",
            self.mismatches.len(),
            self.count,
            self.mismatches.join("\n")
        );
    }
}

fn write(p: &Path, data: &str) {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(p, data).unwrap();
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

#[test]
fn gitfiles() {
    let Some(mut c) = Cases::new() else { return };
    let repo = c.repo();
    let base = repo.parent().unwrap().to_path_buf();
    let gf = base.join("gf");
    std::fs::create_dir_all(gf.join("s")).unwrap();
    let real = s(&repo.join(".git")).to_string();
    for (what, content) in [
        ("garbage", "garbage\n".to_string()),
        ("no colon", format!("gitdir {real}\n")),
        ("missing target", format!("gitdir: {real}.not\n")),
        ("not a repo", format!("gitdir: {}\n", s(&repo))),
        ("relative", "gitdir: ../repo/.git".to_string()),
        ("crlf", "gitdir: ../repo/.git\r\n".to_string()),
        ("blank lines", "gitdir: ../repo/.git\n\n".to_string()),
        ("spaces", "gitdir:   ../repo/.git  \n".to_string()),
        ("empty path", "gitdir: \n".to_string()),
        ("empty file", String::new()),
    ] {
        write(&gf.join(".git"), &content);
        c.check(what, &gf, &[]);
        c.check(what, &gf.join("s"), &[]);
        c.check(what, &gf.join("s"), &[("GIT_DIR", "../.git")]);
    }
    std::fs::remove_file(gf.join(".git")).unwrap();
    std::fs::create_dir(gf.join(".git")).unwrap();
    c.check("empty .git directory is ignored", &gf, &[]);
    std::fs::remove_dir(gf.join(".git")).unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("/dev/null", gf.join(".git")).unwrap();
        c.check("symlink to a device", &gf, &[]);
        std::fs::remove_file(gf.join(".git")).unwrap();
        std::os::unix::fs::symlink("../repo/.git", gf.join(".git")).unwrap();
        c.check("symlink to a .git directory", &gf.join("s"), &[]);
        std::fs::remove_file(gf.join(".git")).unwrap();
    }
    c.check(
        "GIT_DIR is HEAD",
        &gf,
        &[("GIT_DIR", &format!("{real}/HEAD"))],
    );
    c.check("GIT_DIR missing", &gf, &[("GIT_DIR", "nope")]);
    c.check("GIT_DIR empty", &repo, &[("GIT_DIR", "")]);
    c.check("GIT_DIR is a work tree", &base, &[("GIT_DIR", "repo")]);
    c.check("GIT_WORK_TREE empty", &repo, &[("GIT_WORK_TREE", "")]);
    c.check(
        "GIT_WORK_TREE two missing components",
        &repo,
        &[("GIT_WORK_TREE", "no/such")],
    );
    c.check(
        "GIT_WORK_TREE missing then ..",
        &repo,
        &[("GIT_WORK_TREE", "no/../x")],
    );
    c.finish();
}

#[test]
fn ceilings() {
    let Some(mut c) = Cases::new() else { return };
    let repo = c.repo();
    let sub = repo.join("sub/dir");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::create_dir_all(repo.join("s/d")).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("sub", repo.join("top")).unwrap();
    let r = s(&repo).to_string();
    let parent = s(repo.parent().unwrap()).to_string();
    for ceil in [
        String::new(),
        parent.clone(),
        format!("{parent}/"),
        r.clone(),
        format!("{r}/"),
        format!("{r}/sub"),
        format!("{r}/sub/"),
        format!("{r}/top"),
        format!("{r}/top/"),
        format!(":{r}/top"),
        format!("{r}/sub/dir"),
        format!("{r}/su"),
        format!("{r}/sub/di"),
        format!("/foo:{r}/sub"),
        format!("{r}/sub:/bar"),
        "relative/path".to_string(),
    ] {
        for cwd in [&repo, &sub, &repo.join("s/d")] {
            c.check("ceiling", cwd, &[("GIT_CEILING_DIRECTORIES", &ceil)]);
        }
        c.check(
            "ceiling with GIT_DIR",
            &sub,
            &[
                ("GIT_CEILING_DIRECTORIES", &ceil),
                ("GIT_DIR", "../../.git"),
            ],
        );
    }
    c.check("no repository", &c.w.root.clone(), &[]);
    c.check(
        "across file systems",
        &c.w.root.clone(),
        &[("GIT_DISCOVERY_ACROSS_FILESYSTEM", "1")],
    );
    c.finish();
}

#[test]
fn ownership() {
    let Some(mut c) = Cases::new() else { return };
    let repo = c.repo();
    std::fs::create_dir_all(repo.join("sub")).unwrap();
    let base = repo.parent().unwrap().to_path_buf();
    let bare = base.join("b.git");
    c.git(&base, &["init", "-q", "--bare", "b.git"]);
    let dubious = ("GIT_TEST_ASSUME_DIFFERENT_OWNER", "1");
    let r = s(&repo).to_string();
    let param = |v: &str| format!("'safe.directory'='{v}'");
    for cwd in [
        repo.clone(),
        repo.join("sub"),
        repo.join(".git"),
        repo.join(".git/refs"),
        bare.clone(),
        bare.join("refs"),
    ] {
        c.check("not safe", &cwd, &[dubious]);
        c.check(
            "explicit GIT_DIR is not checked",
            &cwd,
            &[dubious, ("GIT_DIR", s(&repo.join(".git")))],
        );
        for value in [
            r.clone(),
            format!("{r}/"),
            "*".to_string(),
            format!("{}/*", s(&base)),
            format!("{}no/*", s(&base)),
            ".".to_string(),
            s(&bare).to_string(),
            format!("{r}/.git"),
            "bogus".to_string(),
        ] {
            c.check(
                "safe.directory",
                &cwd,
                &[dubious, ("GIT_CONFIG_PARAMETERS", &param(&value))],
            );
        }
        let star_bogus = format!("{} {}", param("*"), param("bogus"));
        c.check(
            "star then relative",
            &cwd,
            &[dubious, ("GIT_CONFIG_PARAMETERS", &star_bogus)],
        );
        let reset = format!("{} {}", param("*"), param(""));
        c.check("reset", &cwd, &[dubious, ("GIT_CONFIG_PARAMETERS", &reset)]);
        c.check(
            "GIT_CONFIG_COUNT",
            &cwd,
            &[
                dubious,
                ("GIT_CONFIG_COUNT", "1"),
                ("GIT_CONFIG_KEY_0", "safe.directory"),
                ("GIT_CONFIG_VALUE_0", "*"),
            ],
        );
    }
    // Repository configuration is not protected.
    c.git(&repo, &["config", "safe.directory", &r]);
    c.check("safe.directory in repository config", &repo, &[dubious]);
    c.finish();
}

#[test]
fn bare_repository_setting() {
    let Some(mut c) = Cases::new() else { return };
    let repo = c.repo();
    let base = repo.parent().unwrap().to_path_buf();
    c.git(&repo, &["commit", "-q", "--allow-empty", "-m", "x"]);
    c.git(&repo, &["worktree", "add", "-q", "../wt"]);
    c.git(&base, &["init", "-q", "--bare", "b.git"]);
    c.git(&base, &["init", "-q", "--bare", "repo/.git/modules/m"]);
    let explicit = ("GIT_CONFIG_PARAMETERS", "'safe.bareRepository'='explicit'");
    let bogus = ("GIT_CONFIG_PARAMETERS", "'safe.bareRepository'='bogus'");
    let all = ("GIT_CONFIG_PARAMETERS", "'safe.bareRepository'='all'");
    for cwd in [
        base.join("b.git"),
        base.join("b.git/refs"),
        repo.join(".git"),
        repo.join(".git/objects"),
        repo.join(".git/worktrees/wt"),
        repo.join(".git/modules/m"),
        base.join("wt"),
    ] {
        c.check("explicit", &cwd, &[explicit]);
        c.check("all", &cwd, &[all]);
        c.check("bogus", &cwd, &[bogus]);
        c.check("explicit with GIT_DIR", &cwd, &[explicit, ("GIT_DIR", ".")]);
    }
    c.check("linked work tree", &base.join("wt"), &[]);
    c.check(
        "linked work tree git dir",
        &repo.join(".git/worktrees/wt"),
        &[],
    );
    c.check("GIT_COMMON_DIR", &repo, &[("GIT_COMMON_DIR", "foo")]);
    c.check(
        "GIT_COMMON_DIR relative",
        &repo,
        &[("GIT_COMMON_DIR", ".git")],
    );
    c.finish();
}

#[test]
fn repository_format() {
    let Some(mut c) = Cases::new() else { return };
    let repo = c.repo();
    std::fs::create_dir_all(repo.join("sub")).unwrap();
    std::fs::create_dir_all(repo.join(".git/x")).unwrap();
    let cfg = repo.join(".git/config");
    for (version, exts) in [
        ("0", ""),
        ("1", ""),
        ("1", "noop"),
        ("1", "no-such-extension"),
        ("0", "no-such-extension"),
        ("0", "noop"),
        ("0", "noop-v1"),
        ("1", "noop-v1"),
        ("99", ""),
        ("2", ""),
        ("abc", ""),
        ("-1", "foo=1\nnoop-v1"),
        ("1k", ""),
        ("1", "no-such=1\nother=2"),
        ("0", "noop-v1=1\nobjectformat=sha1"),
        ("1", "objectformat=sha512"),
        ("1", "objectformat=SHA256"),
        ("1", "objectformat"),
        ("1", "refstorage=foo"),
        ("1", "refstorage=bogus://x"),
        ("1", "refstorage=files://foo"),
        ("1", "refstorage=files"),
        ("1", "worktreeconfig=maybe"),
        ("0", "worktreeconfig=maybe"),
        ("0", "worktreeconfig"),
        ("0", "preciousobjects=maybe"),
        ("1", "preciousobjects"),
        ("1", "partialclone"),
        ("1", "partialclone=origin"),
        ("1", "relativeworktrees=x"),
        ("0", "relativeworktrees=true\nsubmodulepathconfig"),
        ("1", "submodulepathconfig"),
        ("1", "objectformat=x\nfoo=1"),
        ("99", "objectformat=x"),
    ] {
        let mut text = format!("[core]\n\trepositoryformatversion = {version}\n");
        if !exts.is_empty() {
            text.push_str("[extensions]\n");
            for e in exts.lines() {
                text.push_str(&format!("\t{e}\n"));
            }
        }
        write(&cfg, &text);
        let what = format!("version={version} {exts:?}");
        c.check(&what, &repo, &[]);
        c.check(&what, &repo.join("sub"), &[]);
        c.check(&what, &repo.join(".git/x"), &[]);
        c.check(&what, &repo.join("sub"), &[("GIT_DIR", "../.git")]);
    }
    for (what, text) in [
        ("syntax error", "[core\n"),
        ("bad core.bare", "[core]\n\tbare = maybe\n"),
        ("value-less core.worktree", "[core]\n\tworktree\n"),
    ] {
        write(&cfg, text);
        c.check(what, &repo, &[]);
        c.check(what, &repo.join("sub"), &[]);
        c.check(what, &repo.join(".git/x"), &[]);
    }
    c.finish();
}

#[test]
fn head_validation() {
    let Some(mut c) = Cases::new() else { return };
    let repo = c.repo();
    let git = repo.join(".git");
    for (what, head) in [
        ("garbage", "garbage\n"),
        ("ref outside refs/", "ref: foo\n"),
        ("ref with spaces", "ref:   refs/heads/x\n"),
        ("full sha1", "0123456789012345678901234567890123456789\n"),
        ("short sha1", "01234567890123456789012345678901234567\n"),
        (
            "sha1 with junk",
            "0123456789012345678901234567890123456789x\n",
        ),
    ] {
        write(&git.join("HEAD"), head);
        c.check(what, &repo, &[]);
    }
    write(&git.join("HEAD"), "ref: refs/heads/main\n");
    std::fs::remove_dir(git.join("refs/tags")).unwrap();
    c.check("refs/tags missing", &repo, &[]);
    std::fs::rename(git.join("objects"), git.join("objects.away")).unwrap();
    c.check("objects missing", &repo, &[]);
    let away = s(&git.join("objects.away")).to_string();
    c.check(
        "GIT_OBJECT_DIRECTORY",
        &repo,
        &[("GIT_OBJECT_DIRECTORY", &away)],
    );
    std::fs::rename(git.join("objects.away"), git.join("objects")).unwrap();
    #[cfg(unix)]
    {
        std::fs::remove_file(git.join("HEAD")).unwrap();
        std::os::unix::fs::symlink("refs/heads/main", git.join("HEAD")).unwrap();
        c.check("symlink HEAD", &repo, &[]);
        std::fs::remove_file(git.join("HEAD")).unwrap();
        std::os::unix::fs::symlink("../x", git.join("HEAD")).unwrap();
        c.check("symlink HEAD outside refs", &repo, &[]);
        std::fs::remove_file(git.join("HEAD")).unwrap();
    }
    std::fs::create_dir(git.join("HEAD")).unwrap();
    c.check("HEAD is a directory", &repo, &[]);
    c.finish();
}

#[test]
fn bare_and_worktree_conflict() {
    let Some(mut c) = Cases::new() else { return };
    let repo = c.repo();
    let r = s(&repo).to_string();
    c.git(&repo, &["config", "core.bare", "true"]);
    c.git(&repo, &["config", "core.worktree", &r]);
    c.check("inside .git", &repo.join(".git"), &[]);
    c.check("GIT_DIR=.git", &repo, &[("GIT_DIR", ".git")]);
    c.check(
        "GIT_WORK_TREE wins",
        &repo,
        &[("GIT_DIR", ".git"), ("GIT_WORK_TREE", ".")],
    );
    c.finish();
}
