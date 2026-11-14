// SPDX-License-Identifier: MIT
//! Differential tests of the untracked and ignored listings against the
//! reference git: `ls-files -o [-i] [--directory] [--no-empty-directory]`
//! and `status --ignored[=<mode>]` with every `--untracked-files` mode.

use rgit_difftest::{Check, Fixture};

/// Untracked directories with and without ignored contents, ignored
/// directories (matching a pattern or not, empty or not), a nested
/// repository, an ignored directory holding a tracked file, and names
/// whose order depends on the byte after a directory name.
fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q"])
        .write("t/k", "k\n")
        .write("tracked_ign/keep", "k\n")
        .write(
            ".gitignore",
            "*.o\nig/\nig2/\nbuild/\ntracked_ign/\n!keep.o\n",
        )
        .git(["add", "."])
        .git(["add", "-f", "tracked_ign/keep"])
        .git(["commit", "-q", "-m", "init"])
        .write("allig/a.o", "\n")
        .write("allig/s/b.o", "\n")
        .write("mix/a.o", "\n")
        .write("mix/keep.o", "\n")
        .write("mix/s/m", "\n")
        .write("m3/build/x", "\n")
        .write("m3/f", "\n")
        .write("ig/x/f", "\n")
        .write("ig2/f.o", "\n")
        .write("t/x.o", "\n")
        .write("t/ig/q", "\n")
        .write("t/u/f", "\n")
        .write("tracked_ign/new", "\n")
        .write("tracked_ign/sub/n", "\n")
        .write("odb.o", "\n")
        .write("odb/a.o", "\n")
        .write("odb/b", "\n")
        .write("path2-junk", "\n")
        .write("path2/file2", "\n")
        .write("top.txt", "\n")
        .git(["init", "-q", "nr/inner"])
        .git(["init", "-q", "nest"])
}

#[test]
fn ls_files_others_and_ignored() {
    let mut fx = fixture();
    for dir_opts in [
        &[][..],
        &["--directory"],
        &["--directory", "--no-empty-directory"],
    ] {
        for ign in [&[][..], &["-i"]] {
            let mut argv = vec!["ls-files", "-o", "--exclude-standard"];
            argv.extend_from_slice(ign);
            argv.extend_from_slice(dir_opts);
            fx.compare(&argv).checks(Check::OUTPUT).run();
        }
    }
    for args in [
        &["ls-files", "-o"][..],
        &["ls-files", "-o", "--directory", "mix", "t/"],
        &[
            "ls-files",
            "-o",
            "-i",
            "--exclude-standard",
            "--directory",
            "t/",
            "allig",
        ],
    ] {
        fx.compare(args).checks(Check::OUTPUT).run();
    }
    fx.compare(["ls-files", "-o", "--exclude-standard"])
        .in_dir("t")
        .checks(Check::OUTPUT)
        .run();
}

#[test]
fn status_ignored_modes() {
    let mut fx = fixture();
    for untracked in ["-uno", "-unormal", "-uall"] {
        for ignored in [
            "--ignored",
            "--ignored=traditional",
            "--ignored=matching",
            "--ignored=no",
        ] {
            for format in ["--porcelain", "--porcelain=v2", "--short"] {
                fx.compare(["status", format, untracked, ignored])
                    .checks(Check::OUTPUT)
                    .run();
            }
        }
    }
    for args in [
        &["status", "--ignored"][..],
        &[
            "-c",
            "advice.statusHints=false",
            "status",
            "--ignored",
            "-uall",
        ],
        &["status", "-s", "--ignored", "mix"],
        &["status", "-s", "--ignored", "t/", "allig/s"],
        &["status", "-s", "--ignored=bogus"],
    ] {
        fx.compare(args).checks(Check::OUTPUT).run();
    }
    fx.compare(["status", "--ignored"])
        .in_dir("t")
        .checks(Check::OUTPUT)
        .run();
}
