// SPDX-License-Identifier: MIT
//! Differential tests of the legacy merge plumbing against the reference
//! git: `merge-index` with `git-merge-one-file` and a custom program,
//! `merge-one-file` for each kind of unmerged path, `unpack-file`, and the
//! strategy programs `merge-ours`, `merge-resolve`, `merge-octopus` and
//! `merge-subtree`.

use rgit_difftest::{Fixture, Normalizer};

/// Blobs `o` (base), `u` (ours), `t` (theirs) and `c` (conflicting with
/// `u`), their ids in the files `O`, `U`, `T`, `C`.
fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
        .write("o", "1\n2\n3\n4\n5\n")
        .write("u", "one\n2\n3\n4\n5\n")
        .write("t", "1\n2\n3\n4\nfive\n")
        .write("c", "ONE\n2\n3\n4\n5\n")
        .write("empty", "")
        .write(
            ".git/stage.sh",
            "# usage: stage <path> <mode1> <id1> <mode2> <id2> <mode3> <id3>\n\
             stage() {\n\
             \tp=$1; shift\n\
             \tgit update-index --force-remove -- \"$p\"\n\
             \t{\n\
             \t\ttest -n \"$2\" && printf '%s %s 1\\t%s\\n' $1 $2 \"$p\"\n\
             \t\ttest -n \"$4\" && printf '%s %s 2\\t%s\\n' $3 $4 \"$p\"\n\
             \t\ttest -n \"$6\" && printf '%s %s 3\\t%s\\n' $5 $6 \"$p\"\n\
             \t} | git update-index --index-info\n\
             \ttest -z \"$4\" || git checkout-index -f --stage=2 -- \"$p\"\n\
             }\n\
             O=$(git hash-object -w o); U=$(git hash-object -w u)\n\
             T=$(git hash-object -w t); C=$(git hash-object -w c)\n\
             rm -f o u t c\n",
        )
}

fn labels() -> Normalizer {
    Normalizer::regex(r"\.merge_file_[A-Za-z0-9]{6}", ".merge_file_X")
}

fn run(script: &str) {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let alias = format!("alias.t=!. .git/stage.sh; {script}");
    fx.compare(["-c", alias.as_str(), "t"])
        .normalize(labels())
        .normalize_state(labels())
        .run();
}

#[test]
fn merge_one_file_cases() {
    let m = "git merge-index git-merge-one-file -a";
    let cases = [
        // removed on one side, unchanged on the other
        format!("stage f 100644 $O 100644 $O '' ''; {m}"),
        format!("stage f 100644 $O '' '' 100644 $O; {m}"),
        format!("stage d/e/f 100644 $O 100644 $O '' ''; {m}"),
        format!("stage f 100644 $O 100755 $O '' ''; {m}"),
        format!("stage f 100644 $O 100644 $U '' ''; {m}"),
        // added on one side, or identically on both
        format!("stage f '' '' 100644 $U '' ''; {m}"),
        format!("stage d/f '' '' '' '' 100755 $T; {m}"),
        format!("stage f '' '' '' '' 100644 $T; echo x >f; {m}"),
        format!("stage f '' '' 100644 $U 100644 $U; {m}"),
        format!("stage f '' '' 100644 $U 100755 $U; {m}"),
        // changed on both sides
        format!("stage f 100644 $O 100644 $U 100644 $T; {m}"),
        format!("stage f 100644 $O 100644 $U 100644 $C; {m}"),
        format!("stage f 100644 $O 100644 $U 100755 $T; {m}"),
        format!("stage f 100644 $O 100644 $U 100644 $T; echo dirty >f; {m}"),
        format!("stage f '' '' 100644 $U 100644 $C; {m}"),
        format!("git hash-object -w empty; stage f '' '' 100644 $U 100644 $C; {m}"),
        format!("stage f 120000 $O 120000 $U 120000 $O; {m}"),
        format!("stage f 100644 $O 100644 $U 160000 $T; {m}"),
    ];
    for c in &cases {
        run(c);
    }
}

#[test]
fn merge_index_options() {
    let prog = "printf '#!/bin/sh\\necho \"[$*] $(pwd) $GIT_PREFIX\"\\ntest \"$4\" != b\\n' >.git/p; \
                chmod +x .git/p; P=$(pwd)/.git/p; \
                stage a 100644 $O 100644 $U 100644 $T; stage b '' '' 100644 $U 100755 $T; \
                stage c 100644 $O '' '' 100644 $T; ";
    let cases = [
        "git merge-index $P -a",
        "git merge-index -o $P -a",
        "git merge-index -q $P -a",
        "git merge-index -o -q $P -a",
        "git merge-index $P a c",
        "git merge-index $P -- a",
        "git merge-index $P a -a",
        "git merge-index $P nosuch",
        "git merge-index $P -x",
        "git merge-index -x $P a",
        "git merge-index $P",
        "git merge-index -h",
        "mkdir -p d && cd d && git merge-index $P -a",
        "git merge-index ./nosuch -a",
        "git merge-index nosuch-program -a",
        "git merge-index -o git-merge-one-file -a",
    ];
    for c in cases {
        run(&format!("{prog}{c}"));
    }
}

#[test]
fn unpack_file() {
    let cases = [
        "n=$(git unpack-file $O) && cat \"$n\" && ls -l \"$n\" | cut -c1-10",
        "git unpack-file HEAD",
        "git unpack-file nosuch",
        "git unpack-file 4b825dc642cb6eb9a060e54bf8d69288fbee4904",
        "git unpack-file $O $O",
        "git unpack-file -h",
        "git unpack-file",
    ];
    for c in cases {
        run(c);
    }
}

/// `main`: base - t (changes line 5); `o`: base - u (changes line 1);
/// `c`: base - c (conflicts with `o`) adding `n`.
fn history() -> String {
    "printf '1\\n2\\n3\\n' >f && git add f && git commit -q -m base && git tag base && \
     printf '1\\n2\\nthree\\n' >f && git commit -q -am t && git tag tt && \
     git checkout -q -b o base && printf 'one\\n2\\n3\\n' >f && git commit -q -am o && \
     git checkout -q -b c base && printf 'ONE\\n2\\n3\\n' >f && echo n >n && git add n && \
     git commit -q -am c && git checkout -q o; "
        .to_owned()
}

#[test]
fn strategy_programs() {
    let cases = [
        "git merge-ours base -- HEAD tt",
        "echo x >>f; git add f; git merge-ours",
        "git merge-ours -h",
        "git merge-resolve base -- HEAD tt",
        "git merge-resolve base -- HEAD c",
        "git merge-resolve base -- HEAD tt c",
        "git merge-resolve -- HEAD tt",
        "echo x >>f; git add f; git merge-resolve base -- HEAD tt",
        "git merge-resolve -h",
        "git merge-octopus base -- HEAD tt",
        "git checkout -q main; git merge-octopus base -- HEAD o c",
        "git checkout -q -b q base; echo q >q; git add q; git commit -q -m q; \
         git merge-octopus base -- HEAD tt o",
        "git checkout -q base; git merge-octopus base -- HEAD tt o",
        "git merge-octopus -h",
        "git merge-subtree base -- HEAD tt",
        "git merge-subtree --foo base -- HEAD tt",
        "git merge-recursive --foo base -- HEAD tt",
    ];
    for c in cases {
        run(&format!("{}{c}", history()));
    }
}
