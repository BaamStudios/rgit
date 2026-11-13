// SPDX-License-Identifier: MIT

//! Compares pathspec matching with the reference `git ls-files -z -- <pathspec>`
//! on an index of varied (made up) paths, in all four combinations of
//! `GIT_GLOB_PATHSPECS` and `GIT_ICASE_PATHSPECS`, at the top and in a
//! subdirectory. Also compares error messages. Skipped when no `git` binary
//! is available.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use rgit_pathspec::{GlobalFlags, ImplicitBase, ParseOptions, Pathspec};

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn git_cmd(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(dir)
        .env("HOME", dir)
        .env("XDG_CONFIG_HOME", dir.join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("LC_ALL", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_LITERAL_PATHSPECS")
        .env_remove("GIT_GLOB_PATHSPECS")
        .env_remove("GIT_NOGLOB_PATHSPECS")
        .env_remove("GIT_ICASE_PATHSPECS");
    cmd
}

fn git_ok(dir: &Path, args: &[&str], stdin: &[u8]) -> Vec<u8> {
    let mut child = git_cmd(dir)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn git");
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

/// Paths in the test index. No directory/file conflicts.
const PATHS: &[&str] = &[
    "fooo",
    "Foo2",
    "FOO3",
    "bar",
    "bAr",
    "foobar",
    "foo-bar",
    "foo*",
    "f*",
    "f**",
    "f?z",
    "f[o][o]",
    "*",
    "**",
    "?",
    "[abc]",
    "a",
    "B",
    "]",
    "-",
    "^",
    "!x",
    ":colon",
    "sp ace",
    "back\\slash",
    "tab\tname",
    "caf\u{e9}",
    "abcXdefXghi",
    "aaaaaaabababab",
    "foo/bar/baz/to",
    "foo/bba/arr",
    "foo/b/a/z/bar",
    "foo/baz/bar",
    "foo/bar.c",
    "foo/Bar.C",
    "Foo/deep/x",
    "deep/foo/bar/baz/x",
    "deep/foo/bar/baz2",
    "XXX/foo",
    "XXX/\\",
    "bar2/baz/foo",
    "ab/cXd/efXg/hi",
    "sub/file",
    "sub/file2",
    "sub/sub/file",
    "sub/sub/sub/file",
    "sub2/file",
    "SUB3/File",
    "sub/a.c",
    "sub/x/b.c",
    "sub/gen/c.c",
    "sub/*star/q",
    "a1B",
    "dir.with.dots/x.y",
    "x/y/z/w/v",
];

/// Patterns written for this test; all four global modes are tried.
const PATTERNS: &[&str] = &[
    "foo",
    "FOO",
    "fo",
    "foo/",
    "foo/bar",
    "foo/*",
    "foo/**",
    "foo**",
    "foo*",
    "f*",
    "*f",
    "*foo*",
    "*ob*a*r*",
    "*ab",
    "foo\\*",
    "f\\*",
    "f\\*\\*",
    "\\*",
    "**",
    "*",
    "?",
    "???",
    "??",
    "[abc]",
    "\\[abc]",
    "[[]abc]",
    "f[o][o]",
    "*[al]?",
    "[!a-g]*",
    "[^a-g]*",
    "a[]]b",
    "[]-]",
    "[!]-]",
    "[]-a]",
    "[--A]",
    "[ --]",
    "[a-e-n]",
    "[\\]]",
    "[\\\\]",
    "[\\-^]",
    "[[:alpha:]][[:digit:]][[:upper:]]",
    "[[:upper:]]*",
    "[[:lower:]]*",
    "[[:punct:]]",
    "[[:spaci:]]*",
    "[[:digit]ab]",
    "[[::]x]",
    "[a-c[:digit:]x-z]*",
    "[B-a]*",
    "[Z-y]*",
    "[A-Z]*",
    "[a-z]*",
    "ab[",
    "a[",
    "[!",
    "foo\\",
    "\\Fooo",
    "\\fooo",
    "[\\F]ooo",
    "[\\E-\\G]ooo",
    "[E-\\G]ooo",
    "[\\e-\\g]OOO",
    "f\\O*",
    "f\\o*",
    "[\\f]OO3",
    "[f]OO3",
    "[F]ooo",
    "\\fOO3",
    "\\FOO3",
    "[\\F]OO3",
    "[\\f]ooo",
    "\\f*",
    "\\F*",
    "[!\\F]*",
    "[!\\f]*",
    "[!F]*",
    "XXX/\\",
    "*/\\",
    "*/\\\\",
    "back\\\\slash",
    "**/foo",
    "**/bar*",
    "**/bar/*",
    "**/bar/**",
    "**/bar**",
    "*/bar/**",
    "**/bar/*/*",
    "foo/**/bar",
    "foo/**/**/bar",
    "foo*bar",
    "foo**bar",
    "foo?bar",
    "foo[/]bar",
    "foo[^a-z]bar",
    "f[^eiu][^eiu][^eiu][^eiu][^eiu]r",
    "*/*/*",
    "**/**/**",
    "*X*i",
    "*/*X*/*/*i",
    "**/*X*/**/*i",
    "*Xg*i",
    "foo/*arr",
    "foo/**arr",
    "foo/*z",
    "foo/**z",
    "***/x",
    "x/***",
    "x/**/***/v",
    "sub",
    "sub/",
    "sub/sub/",
    "sub/*file",
    "sub/*/file",
    "su*/",
    "s?b",
    "*.c",
    "*.C",
    "sub/*.c",
    "sub/\\*star",
    "sub/*star",
    ".",
    "./foo",
    "sub/../foo",
    "sub//file",
    "sub/./file",
    "dir.with.dots",
    "dir.with.dots/",
    "caf*",
    "sp ace",
    "tab*",
    ":colon",
    "::colon",
    ":(top)foo",
    ":(literal)f*",
    ":(literal)foo*",
    ":(literal)sub",
    ":(glob)f*",
    ":(glob)**/bar",
    ":(glob)sub/*",
    ":(glob)sub/**",
    ":(glob)sub",
    ":(icase)SUB",
    ":(icase)bar",
    ":(icase)f*",
    ":(icase,glob)FOO/*",
    ":(exclude)sub",
    ":!sub",
    ":^*/file2",
    ":/foo",
    ":/:foo",
    ":/",
    ":",
    ":!",
    ":(top)",
    ":(attr)foo",
    ":(prefix:0)foo",
    ":(prefix:3)sub/file",
    ":!(top)foo",
    ":)x",
    ":*",
    ":.",
];

/// Extra multi-argument cases.
const COMBOS: &[&[&str]] = &[
    &[".", ":(exclude)sub"],
    &[".", ":(exclude)sub/sub/file"],
    &[".", ":!sub"],
    &[".", ":(exclude,icase)SUB"],
    &[".", ":(exclude)sub/*file"],
    &[".", ":(exclude,glob)sub/*/file"],
    &["sub/sub/", ":(exclude)sub2"],
    &[":^*/file2", ":^sub2"],
    &["foo", "bar", "nope"],
    &["*.c", ":!sub/gen"],
    &[":", "foo"],
    &[":!foo", ":!bar"],
];

/// Arguments that git rejects.
const BAD: &[&str] = &[
    "",
    ":(foo)x",
    ":(top",
    ":(literal,glob)x",
    ":#x",
    ":@x",
    ":_x",
    ":`x",
    ":( top)x",
    ":(prefix:x)a",
    ":(prefix)a",
    ":(attr:a,attr:b)a",
    ":(attr:)a",
    ":(attr:-)a",
    ":(attr:a=b=c)a",
    "../x",
    "sub/../../x",
    "/x",
];

struct Repo {
    dir: PathBuf,
}

impl Repo {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("rgit-pathspec-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub/sub")).unwrap();
        std::fs::create_dir_all(dir.join("foo")).unwrap();
        // Canonical path, as git reports it.
        let dir = dir.canonicalize().unwrap();
        git_ok(&dir, &["init", "-q"], b"");
        let blob = git_ok(&dir, &["hash-object", "-w", "--stdin"], b"");
        let blob = String::from_utf8(blob).unwrap();
        let mut info = Vec::new();
        for p in PATHS {
            info.extend_from_slice(format!("100644 {}\t{p}\0", blob.trim()).as_bytes());
        }
        git_ok(&dir, &["update-index", "-z", "--index-info"], &info);
        let listed = git_ok(&dir, &["ls-files", "-z"], b"");
        let listed: BTreeSet<&[u8]> = listed
            .split(|&b| b == 0)
            .filter(|p| !p.is_empty())
            .collect();
        let expected: BTreeSet<&[u8]> = PATHS.iter().map(|p| p.as_bytes()).collect();
        assert_eq!(listed, expected, "test index does not hold PATHS");
        Self { dir }
    }

    fn git(&self, cwd: &str, envs: &[(&str, &str)], args: &[&[u8]]) -> Output {
        let mut cmd = git_cmd(&self.dir.join(cwd));
        cmd.args(["ls-files", "-z", "--full-name", "--"]);
        for a in args {
            cmd.arg(std::ffi::OsStr::from_encoded_bytes_unchecked_safe(a));
        }
        for (k, v) in envs {
            cmd.env(k, v);
        }
        cmd.output().unwrap()
    }
}

/// `OsStr::from_encoded_bytes_unchecked` is unsafe; our arguments are
/// always valid UTF-8, so go through `str`.
trait FromBytes {
    fn from_encoded_bytes_unchecked_safe(b: &[u8]) -> &std::ffi::OsStr;
}

impl FromBytes for std::ffi::OsStr {
    fn from_encoded_bytes_unchecked_safe(b: &[u8]) -> &std::ffi::OsStr {
        std::ffi::OsStr::new(std::str::from_utf8(b).expect("utf-8 test argument"))
    }
}

const MODES: [(bool, bool); 4] = [(false, false), (true, false), (false, true), (true, true)];

fn envs(glob: bool, icase: bool) -> Vec<(&'static str, &'static str)> {
    let mut v = Vec::new();
    if glob {
        v.push(("GIT_GLOB_PATHSPECS", "1"));
    }
    if icase {
        v.push(("GIT_ICASE_PATHSPECS", "1"));
    }
    v
}

fn ours(
    repo: &Repo,
    cwd: &str,
    glob: bool,
    icase: bool,
    args: &[&[u8]],
) -> Result<BTreeSet<Vec<u8>>, String> {
    let prefix = if cwd.is_empty() {
        Vec::new()
    } else {
        format!("{cwd}/").into_bytes()
    };
    let opts = ParseOptions {
        prefix: prefix.clone(),
        root: Some(repo.dir.to_str().unwrap().as_bytes().to_vec()),
        globals: GlobalFlags {
            glob,
            icase,
            ..Default::default()
        },
        implicit_base: ImplicitBase::Cwd,
        ..Default::default()
    };
    let ps = Pathspec::parse(args, &opts).map_err(|e| e.to_string().trim_end().to_owned())?;
    // Without pathspec, ls-files lists only paths below the current directory.
    Ok(PATHS
        .iter()
        .map(|p| p.as_bytes())
        .filter(|p| !ps.is_empty() || p.starts_with(&prefix))
        .filter(|p| ps.matches(p))
        .map(<[u8]>::to_vec)
        .collect())
}

fn theirs(out: &Output) -> Result<BTreeSet<Vec<u8>>, String> {
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(err
            .trim_end()
            .strip_prefix("fatal: ")
            .unwrap_or(&err)
            .to_owned());
    }
    Ok(out
        .stdout
        .split(|&b| b == 0)
        .filter(|p| !p.is_empty())
        .map(<[u8]>::to_vec)
        .collect())
}

fn show(r: &Result<BTreeSet<Vec<u8>>, String>) -> String {
    match r {
        Ok(set) => format!(
            "{:?}",
            set.iter()
                .map(|p| String::from_utf8_lossy(p).into_owned())
                .collect::<Vec<_>>()
        ),
        Err(e) => format!("error: {e}"),
    }
}

fn compare(repo: &Repo, cwd: &str, args: &[&[u8]], failures: &mut Vec<String>) {
    for (glob, icase) in MODES {
        let expected = theirs(&repo.git(cwd, &envs(glob, icase), args));
        let actual = ours(repo, cwd, glob, icase, args);
        if expected != actual {
            failures.push(format!(
                "cwd={cwd:?} glob={glob} icase={icase} args={:?}\n  git:  {}\n  rgit: {}",
                args.iter()
                    .map(|a| String::from_utf8_lossy(a))
                    .collect::<Vec<_>>(),
                show(&expected),
                show(&actual)
            ));
        }
    }
}

/// Small deterministic generator for random patterns.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        let n = usize::try_from(self.next()).unwrap() % items.len();
        items[n]
    }
}

fn random_patterns(count: usize) -> Vec<String> {
    const PIECES: &[&str] = &[
        "f",
        "o",
        "b",
        "a",
        "r",
        "A",
        "B",
        "F",
        "X",
        "/",
        "/",
        "*",
        "*",
        "**",
        "?",
        "[a-c]",
        "[!o]",
        "[[:upper:]]",
        "[]a]",
        "\\*",
        "\\",
        "-",
        "sub",
        "foo",
        "file",
        ".",
        "[",
        "\\F",
        "\\o",
        "[F-o]",
        "[\\O]",
        "[[:lower:]]",
        "O",
    ];
    let mut rng = Lcg(0x5eed_1234);
    let mut out = Vec::new();
    while out.len() < count {
        let len = 1 + usize::try_from(rng.next()).unwrap() % 6;
        let mut p = String::new();
        for _ in 0..len {
            p.push_str(rng.pick(PIECES));
        }
        // Avoid leading magic and `..` (outside repository errors are tested
        // separately).
        if p.starts_with(':') || p.contains("..") {
            continue;
        }
        out.push(p);
    }
    out
}

#[test]
fn ls_files_matches_git() {
    if !git_available() {
        eprintln!("git not available, skipping");
        return;
    }
    let repo = Repo::new("ls-files");
    let mut failures = Vec::new();
    for p in PATTERNS {
        compare(&repo, "", &[p.as_bytes()], &mut failures);
    }
    for args in COMBOS {
        let args: Vec<&[u8]> = args.iter().map(|a| a.as_bytes()).collect();
        compare(&repo, "", &args, &mut failures);
    }
    let count = std::env::var("RGIT_PATHSPEC_RANDOM")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(150);
    for p in random_patterns(count) {
        compare(&repo, "", &[p.as_bytes()], &mut failures);
    }
    let _ = std::fs::remove_dir_all(&repo.dir);
    assert!(
        failures.is_empty(),
        "{} mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn subdirectory_matches_git() {
    if !git_available() {
        eprintln!("git not available, skipping");
        return;
    }
    let repo = Repo::new("subdir");
    let abs_file = format!("{}/sub/file", repo.dir.display());
    let abs_root = repo.dir.display().to_string();
    let mut failures = Vec::new();
    let mut cases: Vec<Vec<String>> = [
        ".",
        "..",
        "file",
        "*",
        "*file",
        "sub",
        "sub/",
        "../foo",
        "../sub2/file",
        ":/foo",
        ":(top)sub2",
        ":/",
        ":",
        ":!file",
        ":/!sub2",
        ":(icase)FILE",
        ":(icase)../SUB2",
        ":(glob)*",
        ":(glob)**/file",
        ":(literal)*",
        ":(exclude)sub",
        "../../x",
        "./../../x",
        ":(icase)../../x",
        "../..",
        ":(top)../foo",
    ]
    .iter()
    .map(|s| vec![(*s).to_owned()])
    .collect();
    cases.push(vec![abs_file]);
    cases.push(vec![abs_root.clone()]);
    cases.push(vec![format!("{abs_root}/")]);
    cases.push(vec![format!("{abs_root}/sub/../foo")]);
    cases.push(vec![":/".into(), ":/!sub2".into()]);
    cases.push(vec![":(icase)bar".into(), "../bar".into()]);
    for cwd in ["sub", "sub/sub", "foo"] {
        for case in &cases {
            let args: Vec<&[u8]> = case.iter().map(|a| a.as_bytes()).collect();
            compare(&repo, cwd, &args, &mut failures);
        }
    }
    let _ = std::fs::remove_dir_all(&repo.dir);
    assert!(
        failures.is_empty(),
        "{} mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn errors_match_git() {
    if !git_available() {
        eprintln!("git not available, skipping");
        return;
    }
    let repo = Repo::new("errors");
    let mut failures = Vec::new();
    for p in BAD {
        let expected = theirs(&repo.git("", &[], &[p.as_bytes()]));
        assert!(expected.is_err(), "git accepted {p:?}");
        let actual = ours(&repo, "", false, false, &[p.as_bytes()]);
        if expected != actual {
            failures.push(format!(
                "{p:?}\n  git:  {}\n  rgit: {}",
                show(&expected),
                show(&actual)
            ));
        }
    }
    for (env, expected) in [
        (
            vec![("GIT_GLOB_PATHSPECS", "1"), ("GIT_NOGLOB_PATHSPECS", "1")],
            "global 'glob' and 'noglob' pathspec settings are incompatible",
        ),
        (
            vec![("GIT_LITERAL_PATHSPECS", "1"), ("GIT_ICASE_PATHSPECS", "1")],
            "global 'literal' pathspec setting is incompatible with all other global pathspec settings",
        ),
        (
            vec![("GIT_LITERAL_PATHSPECS", "bogus")],
            "bad boolean environment value 'bogus' for 'GIT_LITERAL_PATHSPECS'",
        ),
    ] {
        let got = theirs(&repo.git("", &env, &[b"foo"]));
        assert_eq!(got, Err(expected.to_owned()));
        let flags = GlobalFlags::from_lookup(|name| {
            env.iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.as_bytes().to_vec())
        })
        .and_then(|f| f.validate().map(|()| f));
        assert_eq!(flags.unwrap_err().to_string(), expected);
    }
    let _ = std::fs::remove_dir_all(&repo.dir);
    assert!(
        failures.is_empty(),
        "{} mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn literal_and_noglob_modes_match_git() {
    if !git_available() {
        eprintln!("git not available, skipping");
        return;
    }
    let repo = Repo::new("literal");
    let mut failures = Vec::new();
    let cases = [
        "foo",
        "f*",
        "f[o][o]",
        ":(literal)foo",
        ":(glob)f*",
        ":(glob)**/bar",
        "**/bar",
        ":/foo",
        ":!foo",
        "sub",
        "*",
    ];
    for (var, flags) in [
        (
            "GIT_LITERAL_PATHSPECS",
            GlobalFlags {
                literal: true,
                ..Default::default()
            },
        ),
        (
            "GIT_NOGLOB_PATHSPECS",
            GlobalFlags {
                noglob: true,
                ..Default::default()
            },
        ),
    ] {
        for p in cases {
            let expected = theirs(&repo.git("", &[(var, "1")], &[p.as_bytes()]));
            let opts = ParseOptions {
                globals: flags,
                ..Default::default()
            };
            let actual = Pathspec::parse([p], &opts)
                .map(|ps| {
                    PATHS
                        .iter()
                        .map(|p| p.as_bytes())
                        .filter(|p| ps.matches(p))
                        .map(<[u8]>::to_vec)
                        .collect()
                })
                .map_err(|e| e.to_string());
            if expected != actual {
                failures.push(format!(
                    "{var} {p:?}\n  git:  {}\n  rgit: {}",
                    show(&expected),
                    show(&actual)
                ));
            }
        }
    }
    let _ = std::fs::remove_dir_all(&repo.dir);
    assert!(
        failures.is_empty(),
        "{} mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
