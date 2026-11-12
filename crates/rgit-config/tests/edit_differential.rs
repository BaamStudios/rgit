// SPDX-License-Identifier: MIT
//! Differential tests of the writer: every edit of a corpus of files must
//! produce the same bytes and exit code as `git config -f`. Skipped when
//! `git` is not installed.

use std::path::{Path, PathBuf};
use std::process::Command;

use bstr::ByteSlice;
use rgit_config::EditError;
use rgit_config::edit::{self, SetRequest, ValuePattern};

fn have_git() -> bool {
    Command::new("git").arg("--version").output().is_ok()
}

fn tmpdir(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("rgit-config-edit")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Run `git config -f <file> <args>` on `data`; returns the exit code and
/// the resulting file.
fn git_edit(dir: &Path, data: &[u8], args: &[&str]) -> (i32, Vec<u8>) {
    let file = dir.join("config");
    std::fs::write(&file, data).unwrap();
    let out = Command::new("git")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .current_dir(dir)
        .arg("config")
        .arg("-f")
        .arg(&file)
        .args(args)
        .output()
        .expect("run git");
    let code = out.status.code().unwrap_or(-1);
    (code, std::fs::read(&file).unwrap_or_default())
}

/// The same edit with the library; mirrors the legacy `git config` syntax.
fn our_edit(data: &[u8], args: &[&str]) -> (i32, Vec<u8>) {
    let mut add = false;
    let mut replace_all = false;
    let mut unset = false;
    let mut unset_all = false;
    let mut fixed = false;
    let mut comment = None;
    let mut rename = false;
    let mut remove = false;
    let mut rest = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match *a {
            "--add" => add = true,
            "--replace-all" => replace_all = true,
            "--unset" => unset = true,
            "--unset-all" => unset_all = true,
            "--fixed-value" => fixed = true,
            "--comment" => comment = Some(it.next().unwrap().as_bytes()),
            "--rename-section" => rename = true,
            "--remove-section" => remove = true,
            _ => rest.push(a.as_bytes()),
        }
    }
    let code = |r: Result<Vec<u8>, EditError>| match r {
        Ok(new) => (0, new),
        Err(e) => (e.exit_code(), data.to_vec()),
    };
    if rename {
        return code(edit::rename_section(data, rest[0], Some(rest[1])));
    }
    if remove {
        return code(edit::rename_section(data, rest[0], None));
    }
    let (key, value, pattern) = if unset || unset_all {
        (rest[0], None, rest.get(1).copied())
    } else {
        (rest[0], Some(rest[1]), rest.get(2).copied())
    };
    let pattern = if add {
        ValuePattern::Never
    } else {
        match pattern {
            None => ValuePattern::Any,
            Some(p) if fixed => ValuePattern::exact(p),
            Some(p) => match ValuePattern::regex(p) {
                Ok(p) => p,
                Err(e) => return (e.exit_code(), data.to_vec()),
            },
        }
    };
    let req = SetRequest {
        key,
        value,
        pattern: &pattern,
        all: replace_all || unset_all,
        comment,
    };
    if let Err(e) = rgit_config::Key::parse(key) {
        return (EditError::from(e).exit_code(), data.to_vec());
    }
    code(edit::set(data, &req))
}

const FILES: &[&str] = &[
    "",
    "[s]\nk = v\n",
    "[s]\nk = v",
    "[s] k = v ; c\n[t]\n",
    "# top comment\n[s]\n\t# about k\n\tk = v # inline\n\tother = 1\n\n[t]\n\tx = 1\n",
    "[s]\nk=1\nx=1\n   k=2\n[t]\n[s]\nk=3\ny=1\n",
    "[s]\r\nk = 1\r\n[t]\r\nx = 1\r\n",
    "\u{feff}[s]\nk=1\n",
    "[s.Sub]\nk=1\n[s \"Sub\"]\nk=2\n[S \"sub\"]\nk=3\n",
    "[s]\n[s]\nk=1\n\n[s]\n\n[u]\nz=1\n",
    "[t]\nx=1\n\n\n[s]\n\tk=1\n\n\n",
    "[s]\nk\nk=\nk = \"quoted ; value\" \n",
    "[s]\na = 1 \\\n  2\n\tk=v ;c\n",
    "  [s]  \n    k = 1  \n  [t]   # t\n    j = 2\n",
    "[one]\n\tkey = not [two \"subsection\"]\n[two \"subsection\"]\n[two \"subsection\"]\n\tkey = true\n[TWO \"subsection\"]\n[one]\n",
    "[s]\nk=1\n[s]\nj=1\n",
    "[s]\nj=1\n[s]\nk=1\n[s]\n",
    "[t]\n# for s\n\n[s]\nk=1\n",
    "[s]\nk=1 # c\n\n# d\n[t]\n",
    "[s] # header comment\nk=1\n",
    "[t] [s] k=1\n",
    "k=0\n[s]\nk=1\n",
    "[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n[remote \"origin\"]\n\turl = /x\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n",
];

const OPS: &[&[&str]] = &[
    &["s.k", "w"],
    &["S.K", "w"],
    &["s.new", "x"],
    &["S.New", "X"],
    &["s.Sub.k", "w"],
    &["s.sub.k", "w"],
    &["s.Sub.j", "w"],
    &["t.x", "w"],
    &["t.new", "a b"],
    &["new.key", " lead"],
    &["New.Sub.Key", "a\"b\\c\td\ne"],
    &["two.subsection.key", "false"],
    &["s.k", "w", "1"],
    &["s.k", "w", "!1"],
    &["s.k", "w", "^$"],
    &["s.k", "w", "["],
    &["--fixed-value", "s.k", "w", "1"],
    &["--fixed-value", "s.k", "w", ""],
    &["--add", "s.k", "w"],
    &["--add", "t.new", "w"],
    &["--replace-all", "s.k", "w"],
    &["--replace-all", "s.k", "w", "[12]"],
    &["--comment", "note", "s.k", "w"],
    &["--comment", "#note", "s.new", "w"],
    &["--comment", "\t# note", "--add", "s.k", "w"],
    &["--unset", "s.k"],
    &["--unset", "S.K"],
    &["--unset", "s.k", "1"],
    &["--unset", "s.k", "!1"],
    &["--unset", "s.nope"],
    &["--unset", "two.subsection.key"],
    &["--unset", "s.Sub.k"],
    &["--unset", "t.x"],
    &["--unset", "branch.main.remote"],
    &["--unset-all", "s.k"],
    &["--unset-all", "s.k", "^[12]$"],
    &["--unset-all", "one.key"],
    &["--fixed-value", "--unset-all", "s.k", ""],
    &["--rename-section", "s", "new.Sub"],
    &["--rename-section", "s.Sub", "u"],
    &["--rename-section", "s.sub", "u"],
    &["--rename-section", "t", "T.a\"b"],
    &["--rename-section", "branch.main", "branch.next"],
    &["--rename-section", "s", "bad name"],
    &["--rename-section", "nope", "x"],
    &["--remove-section", "s"],
    &["--remove-section", "s.Sub"],
    &["--remove-section", "t"],
    &["--remove-section", "two.subsection"],
    &["--remove-section", "remote.origin"],
    &["s.1k", "v"],
    &["sk", "v"],
    &["s.", "v"],
];

#[test]
fn corpus_matches_git() {
    if !have_git() {
        eprintln!("git not found, skipping");
        return;
    }
    let dir = tmpdir("corpus");
    let mut failures = Vec::new();
    let mut count = 0;
    for file in FILES {
        for op in OPS {
            count += 1;
            let (git_code, git_out) = git_edit(&dir, file.as_bytes(), op);
            let (our_code, our_out) = our_edit(file.as_bytes(), op);
            if (git_code, &git_out) != (our_code, &our_out) {
                failures.push(format!(
                    "file {:?} op {:?}\n  git  {git_code} {:?}\n  ours {our_code} {:?}",
                    file,
                    op,
                    git_out.as_bstr(),
                    our_out.as_bstr()
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {count} cases differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn long_lines_match_git() {
    if !have_git() {
        return;
    }
    let dir = tmpdir("long");
    for n in [524_286usize, 524_287, 524_288] {
        let mut data = b"[b]\n".to_vec();
        data.extend(std::iter::repeat_n(b'x', n));
        data.extend_from_slice(b"\n[a] g = h\n");
        let op = ["--rename-section", "a", "xyz"];
        assert_eq!(git_edit(&dir, &data, &op), our_edit(&data, &op), "{n}");
    }
}
