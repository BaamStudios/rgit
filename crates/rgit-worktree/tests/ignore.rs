// SPDX-License-Identifier: MIT
//! Exclude precedence checked against `git ls-files --others` and
//! `--ignored` on a layout like the one of t3001 (command-line patterns,
//! per-directory files, `--exclude-from`, `core.excludesFile`).

use std::path::Path;

use rgit_difftest::{Fixture, assert_same_text};
use rgit_worktree::ignore::Excludes;

fn layout() -> Fixture {
    let mut fx = Fixture::new().git(["init", "-q"]);
    for dir in ["", "one/", "one/two/", "three/"] {
        for i in 1..=8 {
            fx = fx.write(format!("{dir}a.{i}"), "");
        }
    }
    fx.write("#ignore1", "")
        .write("#ignore2", "")
        .write("#hidden", "")
        .write("e-file", "")
        .write(
            ".git/ignore",
            ".gitignore\n\\#ignore1\n\\#ignore2*\n\\#hid*n\n*.7\n!*.8\n",
        )
        .write(".gitignore", "*.1\r\n/*.3\r\n!*.6\r\n")
        .write("one/.gitignore", "*.2\ntwo/*.4\n!*.7\n*.8\n")
        .write("one/two/.gitignore", "!*.2\n!*.8\n")
        .write("excludes-file", "*.[1-8]\ne*\n\\#*\n")
}

/// All files below `root` except `.git`, sorted like git sorts paths.
fn files(root: &Path, rel: &str, out: &mut Vec<String>) {
    let dir = if rel.is_empty() {
        root.to_path_buf()
    } else {
        root.join(rel)
    };
    for e in std::fs::read_dir(dir).unwrap() {
        let e = e.unwrap();
        let name = e.file_name().into_string().unwrap();
        if rel.is_empty() && name == ".git" {
            continue;
        }
        let path = format!("{rel}{name}");
        if e.file_type().unwrap().is_dir() {
            files(root, &format!("{path}/"), out);
        } else {
            out.push(path);
        }
    }
}

fn listing(fx: &Fixture, ex: &mut Excludes, ignored: bool) -> String {
    let mut all = Vec::new();
    files(&fx.work_dir(), "", &mut all);
    all.sort();
    let mut out = String::new();
    for p in all {
        let excluded = ex
            .match_with_leading_dirs(p.as_bytes(), false)
            .is_some_and(|m| m.is_excluded());
        if excluded == ignored {
            out.push_str(&p);
            out.push('\n');
        }
    }
    out
}

#[test]
fn command_line_per_directory_and_file_precedence() {
    let fx = layout();
    if fx.is_skipped() {
        return;
    }
    for ignored in [false, true] {
        let mut ex = Excludes::new(fx.work_dir());
        ex.add_pattern(b"*.6");
        ex.set_per_directory(b".gitignore".to_vec());
        ex.add_file(&fx.work_dir().join(".git/ignore"), b".git/ignore".to_vec())
            .unwrap();
        let mut args = vec![
            "ls-files",
            "--others",
            "--exclude=*.6",
            "--exclude-per-directory=.gitignore",
            "--exclude-from=.git/ignore",
        ];
        if ignored {
            args.push("--ignored");
        }
        let git = String::from_utf8(fx.git_stdout(&args)).unwrap();
        assert_same_text(&args.join(" "), &git, &listing(&fx, &mut ex, ignored));
    }
}

#[test]
fn exclude_standard() {
    let fx = layout().git(["config", "core.excludesFile", "excludes-file"]);
    if fx.is_skipped() {
        return;
    }
    let root = fx.work_dir();
    let info = root.join(".git/info/exclude");
    std::fs::create_dir_all(info.parent().unwrap()).unwrap();
    std::fs::write(&info, "!a.5\n!e-file\n").unwrap();
    for ignored in [false, true] {
        let mut ex = Excludes::new(&root);
        ex.add_standard(
            Some((&root.join("excludes-file"), b"excludes-file".to_vec())),
            (&info, b".git/info/exclude".to_vec()),
        );
        let mut args = vec!["ls-files", "--others", "--exclude-standard"];
        if ignored {
            args.push("--ignored");
        }
        let git = String::from_utf8(fx.git_stdout(&args)).unwrap();
        assert_same_text(&args.join(" "), &git, &listing(&fx, &mut ex, ignored));
    }
}

#[test]
fn ignore_case() {
    let fx = layout()
        .write("Upper/x.TXT", "")
        .write("lower.txt", "")
        .write(".gitignore", "upper/\n*.txt\n");
    if fx.is_skipped() {
        return;
    }
    let mut ex = Excludes::new(fx.work_dir());
    ex.set_per_directory(b".gitignore".to_vec());
    ex.set_ignore_case(true);
    let args = [
        "-c",
        "core.ignorecase=true",
        "ls-files",
        "--others",
        "--ignored",
        "--exclude-per-directory=.gitignore",
    ];
    let git = String::from_utf8(fx.git_stdout(args)).unwrap();
    assert_same_text("ls-files -o -i (icase)", &git, &listing(&fx, &mut ex, true));
}
