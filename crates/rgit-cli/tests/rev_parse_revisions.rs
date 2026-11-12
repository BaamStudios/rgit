// SPDX-License-Identifier: MIT
//! `rev-parse` with revision arguments against the reference git: output,
//! error messages and exit codes for hand-picked cases and for generated
//! expressions (`<ref>~<n>^<m>^{<type>}:<path>`, abbreviations of every
//! length).
//!
//! The repositories are built with the reference git; rev-parse only reads,
//! so both binaries run in the same directory.

use std::path::{Path, PathBuf};
use std::process::Command;

use rgit_difftest::Fixture;

/// Run `bin` with `args` in `dir` under a fixed environment; return
/// "<stdout>--stderr--\n<stderr>[exit N]".
fn run(bin: &Path, dir: &Path, home: &Path, args: &[&str]) -> String {
    let out = Command::new(bin)
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CEILING_DIRECTORIES", dir.parent().unwrap_or(dir))
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env("GIT_TEST_DATE_NOW", "1700000000")
        .output()
        .expect("run");
    format!(
        "{}--stderr--\n{}[exit {}]\n",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
        out.status.code().unwrap_or(-1)
    )
}

struct Pair {
    git: PathBuf,
    rgit: PathBuf,
    home: PathBuf,
}

impl Pair {
    fn new(fx: &Fixture) -> Pair {
        Pair {
            git: fx.reference_git().to_path_buf(),
            rgit: PathBuf::from(env!("CARGO_BIN_EXE_rgit")),
            home: fx.home_dir(),
        }
    }

    /// Compare one call; collect a report instead of panicking at once.
    fn check(&self, dir: &Path, args: &[&str], failures: &mut Vec<String>) {
        let g = run(&self.git, dir, &self.home, args);
        let r = run(&self.rgit, dir, &self.home, args);
        if g != r {
            failures.push(format!(
                "$ git {}\n{}",
                args.join(" "),
                rgit_difftest::unified_diff(&g, &r, "git", "rgit")
            ));
        }
    }
}

fn finish(failures: Vec<String>) {
    assert!(
        failures.is_empty(),
        "{} case(s) differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// A history with a merge, tags, branches, remotes and a subdirectory.
fn history() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("a", "a\n")
        .write("d/b", "b\n")
        .write("d/e/f", "f\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "one"])
        .tick()
        .git(["tag", "-a", "-m", "first tag", "v1"])
        .git(["tag", "light"])
        .git(["checkout", "-q", "-b", "side"])
        .write("s", "side\n")
        .git(["add", "s"])
        .git(["commit", "-q", "-m", "side one"])
        .tick()
        .write("s", "side 2\n")
        .git(["commit", "-q", "-a", "-m", "side two\n\nbody text"])
        .tick()
        .git(["checkout", "-q", "main"])
        .write("a", "a2\n")
        .git(["commit", "-q", "-a", "-m", "two"])
        .tick()
        .git(["merge", "-q", "--no-ff", "-m", "merge side", "side"])
        .tick()
        .write("a", "a3\n")
        .git(["commit", "-q", "-a", "-m", "three fix"])
        .tick()
        .git(["tag", "-a", "-m", "tree tag", "treetag", "HEAD^{tree}"])
        .git(["tag", "-a", "-m", "nested", "nested", "v1"])
        .git(["update-ref", "refs/remotes/origin/main", "HEAD~1"])
        .git([
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ])
        .git(["config", "branch.main.remote", "origin"])
        .git(["config", "branch.main.merge", "refs/heads/main"])
        .git([
            "config",
            "remote.origin.fetch",
            "+refs/heads/*:refs/remotes/origin/*",
        ])
        .git(["branch", "--track", "loc", "main"])
        .git(["checkout", "-q", "side"])
        .git(["checkout", "-q", "main"])
        .write("idx", "index only\n")
        .git(["add", "idx"])
        .write("disk", "disk only\n")
}

#[test]
fn revision_syntax() {
    let fx = history();
    if fx.is_skipped() {
        return;
    }
    let p = Pair::new(&fx);
    let dir = fx.work_dir();
    let cases: &[&[&str]] = &[
        &[
            "rev-parse",
            "HEAD",
            "@",
            "main",
            "heads/main",
            "refs/heads/main",
        ],
        &[
            "rev-parse",
            "v1",
            "tags/v1",
            "light",
            "origin",
            "origin/main",
        ],
        &[
            "rev-parse",
            "HEAD^",
            "HEAD^1",
            "HEAD^2",
            "HEAD~",
            "HEAD~2",
            "HEAD^^2",
        ],
        &[
            "rev-parse",
            "HEAD~1^2~1",
            "HEAD^0",
            "HEAD~0",
            "v1^0",
            "v1~0",
        ],
        &["rev-parse", "HEAD^3"],
        &["rev-parse", "HEAD~10"],
        &[
            "rev-parse",
            "v1^{}",
            "v1^{tag}",
            "v1^{commit}",
            "v1^{tree}",
            "v1^{object}",
        ],
        &["rev-parse", "nested^{}", "nested^{tag}", "nested^{commit}"],
        &["rev-parse", "treetag^{tree}", "treetag^{}"],
        &["rev-parse", "treetag^{commit}"],
        &["rev-parse", "v1^{blob}"],
        &["rev-parse", "--verify", "HEAD^{tag}"],
        &["rev-parse", "HEAD^{foo}"],
        &[
            "rev-parse",
            "HEAD^{/side}",
            "HEAD^{/^two}",
            ":/fix",
            ":/side one",
        ],
        &["rev-parse", "HEAD^{/nomatch}"],
        &["rev-parse", ":/!-fix"],
        &[
            "rev-parse",
            "HEAD:a",
            "HEAD:d",
            "HEAD:d/",
            "HEAD:d/e/f",
            "HEAD:",
            "v1:a",
        ],
        &["rev-parse", "HEAD:nope"],
        &["rev-parse", "HEAD:idx"],
        &["rev-parse", "HEAD:d/b/"],
        &["rev-parse", "HEAD:../a"],
        &["rev-parse", ":a", ":0:a", ":idx", ":d/b"],
        &["rev-parse", ":nope"],
        &["rev-parse", ":disk"],
        &["rev-parse", ":1:a"],
        &["rev-parse", "nope:a"],
        &[
            "rev-parse",
            "HEAD..side",
            "side...HEAD",
            "..side",
            "side..",
            "...",
        ],
        &[
            "rev-parse",
            "HEAD^@",
            "HEAD^!",
            "HEAD^-",
            "HEAD^-2",
            "^side",
        ],
        &["rev-parse", "HEAD^-3"],
        &["rev-parse", "v1^!"],
        &["rev-parse", "HEAD^{tree}^@"],
        &[
            "rev-parse",
            "HEAD@{0}",
            "HEAD@{1}",
            "main@{1}",
            "@{1}",
            "@{-1}",
        ],
        &["rev-parse", "main@{99}"],
        &["rev-parse", "@{-9}"],
        &["rev-parse", "light@{0}"],
        &["rev-parse", "main@{2000-01-01}"],
        &["rev-parse", "@{u}", "main@{upstream}", "loc@{U}"],
        &["rev-parse", "side@{u}"],
        &["rev-parse", "nope@{u}"],
        &["rev-parse", "main@{push}"],
        &["rev-parse", "loc@{push}"],
        &[
            "rev-parse",
            "--symbolic-full-name",
            "HEAD",
            "@",
            "v1",
            "^side",
            "HEAD~1",
        ],
        &[
            "rev-parse",
            "--symbolic-full-name",
            "@{u}",
            "@{-1}",
            "origin",
            "loc@{u}",
        ],
        &[
            "rev-parse",
            "--symbolic-full-name",
            "side..main",
            "HEAD^!",
            "side...main",
        ],
        &[
            "rev-parse",
            "--abbrev-ref",
            "HEAD",
            "origin/main",
            "refs/heads/side",
            "@{u}",
        ],
        &[
            "rev-parse",
            "--abbrev-ref=loose",
            "HEAD",
            "--abbrev-ref=strict",
            "v1",
        ],
        &["rev-parse", "--abbrev-ref=bad", "HEAD"],
        &[
            "rev-parse",
            "--symbolic",
            "HEAD",
            "^v1",
            "HEAD^@",
            "side..main",
        ],
        &["rev-parse", "--short", "HEAD"],
        &["rev-parse", "--short=4", "HEAD"],
        &["rev-parse", "--short=12", "--verify", "HEAD"],
        &["rev-parse", "--short=100", "HEAD"],
        &["rev-parse", "--short", "HEAD", "side"],
        &["rev-parse", "--verify", "HEAD"],
        &["rev-parse", "--verify", "nope"],
        &["rev-parse", "--verify", "-q", "nope"],
        &["rev-parse", "--verify", "HEAD", "side"],
        &["rev-parse", "--verify", "--quiet", "@{u}", "side@{u}"],
        &["rev-parse", "--verify", "side@{u}"],
        &["rev-parse", "--verify", "HEAD.."],
        &["rev-parse", "--verify", "^HEAD"],
        &["rev-parse", "--verify", "--default", "side"],
        &["rev-parse", "--default", "side", "--verify", "HEAD"],
        &["rev-parse", "--default", "side", "a"],
        &["rev-parse", "--default"],
        &["rev-parse", "--not", "HEAD", "^side", "side..main"],
        &["rev-parse", "--sq", "HEAD", "a"],
        &[
            "rev-parse",
            "--revs-only",
            "HEAD",
            "a",
            "--foo",
            "--max-count=1",
        ],
        &[
            "rev-parse",
            "--no-revs",
            "HEAD",
            "a",
            "--foo",
            "--max-count=1",
        ],
        &["rev-parse", "--flags", "HEAD", "a", "--foo"],
        &["rev-parse", "--no-flags", "HEAD", "a", "--foo"],
        &["rev-parse", "HEAD", "--", "a", "nope"],
        &["rev-parse", "nope", "--", "a"],
        &["rev-parse", "a", "HEAD"],
        &["rev-parse", "a", "d", "*.c"],
        &["rev-parse", "nope"],
        &["rev-parse", "--end-of-options", "HEAD", "--", "a"],
        &["rev-parse", "--end-of-options", "--foo"],
        &["rev-parse", "--all"],
        &[
            "rev-parse",
            "--symbolic",
            "--branches",
            "--tags",
            "--remotes",
        ],
        &[
            "rev-parse",
            "--symbolic",
            "--exclude=s*",
            "--branches",
            "--glob=refs/tags",
        ],
        &[
            "rev-parse",
            "--symbolic",
            "--branches=s*",
            "--tags=v",
            "--remotes=origin",
        ],
        &["rev-parse", "--abbrev-ref", "--branches"],
    ];
    let mut failures = Vec::new();
    for args in cases {
        p.check(&dir, args, &mut failures);
    }
    p.check(
        &dir.join("d"),
        &["rev-parse", "HEAD:./b", ":./b", "HEAD:../a", "HEAD:b"],
        &mut failures,
    );
    p.check(&dir.join("d"), &["rev-parse", "HEAD:b"], &mut failures);
    p.check(&dir.join("d"), &["rev-parse", ":b"], &mut failures);
    p.check(&dir.join("d"), &["rev-parse", "HEAD:./nope"], &mut failures);
    p.check(
        &dir.join("d"),
        &["rev-parse", "HEAD:../../a"],
        &mut failures,
    );
    finish(failures);
}

#[test]
fn abbreviations_and_ambiguity() {
    let mut fx = history();
    if fx.is_skipped() {
        return;
    }
    // 300 blobs make short prefixes collide.
    let mut names = Vec::new();
    for i in 0..300 {
        let name = format!("blobs/{i}");
        fx = fx.write(&name, format!("blob {i}\n"));
        names.push(name);
    }
    let mut args = vec!["hash-object".to_string(), "-w".to_string()];
    args.extend(names);
    fx = fx.git(args);
    let p = Pair::new(&fx);
    let dir = fx.work_dir();
    let listed =
        String::from_utf8_lossy(&fx.git_stdout(["rev-list", "--objects", "--all"])).into_owned();
    let mut ids: Vec<String> = listed
        .lines()
        .filter_map(|l| l.split(' ').next().map(str::to_string))
        .collect();
    for i in 0..300 {
        let out = fx.git_stdout(["hash-object", &format!("blobs/{i}")]);
        ids.push(String::from_utf8_lossy(&out).trim().to_string());
    }
    // Every 4-digit prefix shared by two objects.
    let mut prefixes: Vec<String> = Vec::new();
    let mut sorted = ids.clone();
    sorted.sort();
    for w in sorted.windows(2) {
        if w[0][..4] == w[1][..4] && w[0] != w[1] {
            prefixes.push(w[0][..4].to_string());
        }
    }
    let mut failures = Vec::new();
    for pre in prefixes.iter().take(6) {
        for suffix in ["", "^{commit}", "^{tree}", ":a", "^0", "..HEAD"] {
            let arg = format!("{pre}{suffix}");
            p.check(&dir, &["rev-parse", &arg], &mut failures);
        }
        p.check(&dir, &["rev-parse", "--verify", "-q", pre], &mut failures);
        let dis = format!("--disambiguate={pre}");
        p.check(&dir, &["rev-parse", &dis], &mut failures);
        p.check(
            &dir,
            &["-c", "core.disambiguate=commit", "rev-parse", pre],
            &mut failures,
        );
        p.check(
            &dir,
            &["-c", "core.disambiguate=committish", "rev-parse", pre],
            &mut failures,
        );
        p.check(
            &dir,
            &["-c", "core.disambiguate=blob", "rev-parse", pre],
            &mut failures,
        );
    }
    // Abbreviations of every length of a few ids.
    for id in ids.iter().take(12) {
        for len in 1..=id.len() {
            p.check(&dir, &["rev-parse", "--verify", &id[..len]], &mut failures);
        }
        p.check(&dir, &["rev-parse", "--short", id], &mut failures);
    }
    p.check(
        &dir,
        &["-c", "core.disambiguate=bad", "rev-parse", "HEAD"],
        &mut failures,
    );
    p.check(
        &dir,
        &["-c", "core.abbrev=3", "rev-parse", "--short", "HEAD"],
        &mut failures,
    );
    p.check(
        &dir,
        &["-c", "core.abbrev=5", "rev-parse", "--short", "HEAD"],
        &mut failures,
    );
    p.check(
        &dir,
        &["-c", "core.abbrev=no", "rev-parse", "--short", "HEAD"],
        &mut failures,
    );
    finish(failures);
}

/// A small deterministic generator (xorshift).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[(self.next() % items.len() as u64) as usize]
    }
}

/// `<ref>~<n>^<m>^{<type>}:<path>` combinations.
fn expressions(rng: &mut Rng, refs: &[&str], paths: &[&str], count: usize) -> Vec<String> {
    let mut out = Vec::new();
    for _ in 0..count {
        let mut e = rng.pick(refs).to_string();
        match rng.next() % 4 {
            0 => e.push_str(&format!("~{}", rng.next() % 4)),
            1 => e.push_str(&format!("^{}", rng.next() % 3)),
            2 => e.push('^'),
            _ => {}
        }
        if rng.next() % 3 == 0 {
            e.push_str(&format!("~{}", rng.next() % 3));
        }
        match rng.next() % 6 {
            0 => e.push_str("^{tree}"),
            1 => e.push_str("^{commit}"),
            2 => e.push_str("^{}"),
            3 => e.push_str("^{blob}"),
            _ => {}
        }
        if rng.next() % 2 == 0 {
            e.push(':');
            e.push_str(rng.pick(paths));
        }
        out.push(e);
    }
    out
}

#[test]
fn generated_expressions() {
    let fx = history();
    if fx.is_skipped() {
        return;
    }
    let p = Pair::new(&fx);
    let dir = fx.work_dir();
    let refs = [
        "HEAD", "main", "side", "v1", "nested", "light", "treetag", "origin", "@", "loc",
        "HEAD@{1}", "@{u}", "@{-1}", "nope",
    ];
    let paths = [
        "a", "d", "d/", "d/b", "d/e/f", "s", "nope", "", "d/b/", "./a",
    ];
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut failures = Vec::new();
    for e in expressions(&mut rng, &refs, &paths, 500) {
        p.check(&dir, &["rev-parse", &e], &mut failures);
    }
    finish(failures);
}

/// The same over the `upstream/git` clone (when present): abbreviations of
/// every length and paths of the real tree.
#[test]
fn upstream_clone() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../upstream/git");
    if !root.join(".git").exists() {
        return;
    }
    let fx = Fixture::new();
    if fx.is_skipped() {
        return;
    }
    let p = Pair::new(&fx);
    let head = run(
        &p.git,
        &root,
        &p.home,
        &["rev-parse", "HEAD", "HEAD^{tree}", "HEAD:Makefile"],
    );
    let ids: Vec<String> = head
        .lines()
        .take_while(|l| !l.starts_with("--stderr--"))
        .map(str::to_string)
        .collect();
    let mut failures = Vec::new();
    for id in &ids {
        for len in 1..=id.len() {
            p.check(&root, &["rev-parse", &id[..len]], &mut failures);
        }
    }
    let refs = ["HEAD", "v2.56.1", "@", "HEAD^{tree}", "v2.56.1^{}"];
    let paths = [
        "Makefile",
        "t",
        "t/",
        "Documentation/revisions.adoc",
        "Documentation/nope",
        "builtin/",
        "",
    ];
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    for e in expressions(&mut rng, &refs, &paths, 400) {
        p.check(&root, &["rev-parse", &e], &mut failures);
    }
    finish(failures);
}
