// SPDX-License-Identifier: MIT
//! Cruft packs (`pack-objects --cruft`, `repack --cruft`, `gc`) and
//! geometric repacking (`repack --geometric`) against the reference git.
//!
//! Most cases build one repository, copy it, run git in one copy and
//! rgit in the other and compare what the object store holds afterwards:
//! which packs exist (as sorted object lists), which of them are cruft
//! packs, and the per-object times their `.mtimes` files record. Packs
//! need not be byte-identical, but the `.mtimes` layout and entries must
//! be, and git must read what rgit wrote (`verify-pack`, `fsck`).

use std::process::Command;

use rgit_difftest::{Check, Fixture};

fn fixture() -> Fixture {
    Fixture::new().rgit(env!("CARGO_BIN_EXE_rgit"))
}

/// Run `script` with `sh` in the fixture's work directory; `$RGIT` is the
/// binary under test and `$GIT` the reference git, both isolated from the
/// user's configuration. Returns stdout; panics on failure.
fn sh(fx: &Fixture, script: &str) -> String {
    let out = Command::new("sh")
        .arg("-c")
        .arg(format!("set -e\n{HELPERS}\n{script}"))
        .current_dir(fx.work_dir())
        .env("RGIT", env!("CARGO_BIN_EXE_rgit"))
        .env("GIT", fx.reference_git())
        .env("HOME", fx.home_dir())
        .env("XDG_CONFIG_HOME", fx.home_dir().join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "A U Thor")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_COMMITTER_NAME", "C O Mitter")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env_remove("GIT_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .output()
        .expect("sh");
    assert!(
        out.status.success(),
        "script failed:\n{script}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Shell helpers for the scripts.
///
/// - `commit <name>`: a commit adding `<name>.t`.
/// - `age <seconds> <object>...`: date loose objects back.
/// - `layout <dir>`: per pack its kind (`cruft`/`pack`) and objects,
///   for cruft packs with the recorded times (read with `od`, so this
///   also checks the `.mtimes` layout), sorted.
const HELPERS: &str = r#"
commit () { echo "$1" >"$1.t" && "$GIT" add "$1.t" && "$GIT" commit -q -m "$1"; }
age () {
	s=$1; shift
	for o in "$@"; do
		p=.git/objects/$(echo $o | cut -c1-2)/$(echo $o | cut -c3-)
		touch -d "@$(( $(date +%s) - s ))" "$p"
	done
}
layout () {
	for idx in "$1"/.git/objects/pack/*.idx; do
		test -f "$idx" || continue
		base=${idx%.idx}
		"$GIT" -C "$1" show-index <"$idx" | cut -d' ' -f2 | sort >objs
		if test -f "$base.mtimes"; then
			n=$(wc -l <objs)
			test "$(head -c 12 "$base.mtimes" | od -An -tx1 | tr -d ' \n')" = 4d544d450000000100000001
			test $(wc -c <"$base.mtimes") = $((12 + 4 * n + 40))
			od -v -An -tu4 --endian=big -j 12 -N $((4 * n)) -w4 "$base.mtimes" | tr -d ' ' >times
			"$GIT" -C "$1" show-index <"$idx" | cut -d' ' -f2 >ids
			echo "cruft $(paste -d: ids times | sort | tr '\n' ' ')"
		else
			echo "pack $(tr '\n' ' ' <objs)"
		fi
	done | sort
}
"#;

/// A repository with reachable history in a pack, an unreachable branch
/// (packed), unreachable loose objects of various ages and a loose
/// reachable commit; reflogs are gone so nothing else keeps them.
fn repo() -> Fixture {
    let fx = fixture().git(["init", "-q"]);
    if fx.is_skipped() {
        return fx;
    }
    sh(
        &fx,
        r#"
        "$GIT" config maintenance.auto false
        "$GIT" config gc.reflogExpire never
        "$GIT" config gc.reflogExpireUnreachable never
        commit base && "$GIT" branch -M main && "$GIT" repack -adq
        "$GIT" checkout -q --orphan side && commit side1 && commit side2
        "$GIT" repack -dq
        "$GIT" checkout -q main && "$GIT" branch -D side >/dev/null
        old=$(echo old | "$GIT" hash-object -w --stdin)
        mid=$(echo mid | "$GIT" hash-object -w --stdin)
        new=$(echo new | "$GIT" hash-object -w --stdin)
        tree=$(printf '100644 blob %s\tf\n' $old | "$GIT" mktree)
        age 100000 $old
        age 5000 $mid $tree
        commit more
        "$GIT" reflog expire --all --expire=all
        "#,
    );
    fx
}

/// Run `cmd` (a shell snippet using `$G` for the git to run) in copies
/// `a` (git) and `b` (rgit) of the repository and return both layouts.
fn both(fx: &Fixture, cmd: &str) -> (String, String) {
    sh(
        fx,
        &format!(
            r#"
            rm -rf a b && mkdir a b && cp -a .git a/ && cp -a .git b/
            (cd a && G="$GIT" && {cmd})
            (cd b && G="$RGIT" && {cmd})
            "#
        ),
    );
    (sh(fx, "layout a"), sh(fx, "layout b"))
}

/// Times differ between the copies by the seconds between the runs:
/// compare the layouts without them, and the times with some slack.
fn assert_same_layout(git: &str, rgit: &str, what: &str) {
    let strip = |s: &str| -> String {
        s.lines()
            .map(|l| {
                l.split(' ')
                    .map(|w| w.split(':').next().unwrap_or(""))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert_eq!(strip(rgit), strip(git), "{what}: layout");
    let times = |s: &str| -> Vec<u64> {
        s.split_whitespace()
            .filter_map(|w| w.split(':').nth(1))
            .filter_map(|t| t.parse().ok())
            .collect()
    };
    for (r, g) in times(rgit).iter().zip(times(git)) {
        assert!(
            r.abs_diff(g) <= 5,
            "{what}: times\ngit:\n{git}\nrgit:\n{rgit}"
        );
    }
}

#[test]
fn pack_objects_cruft_matches_git() {
    let fx = repo();
    if fx.is_skipped() {
        return;
    }
    // The packs about to go are marked with `-`; the reachable pack stays.
    let input = r#"reach=$("$G" pack-objects --all --reflog --indexed-objects .git/objects/pack/pack </dev/null)
        for p in .git/objects/pack/pack-*.pack; do
            n=$(basename $p)
            if test "$n" = "pack-$reach.pack"; then echo "$n"; else echo "-$n"; fi
        done >in"#;
    for opts in [
        "",
        "--cruft-expiration=never",
        "--cruft-expiration=1.hour.ago",
        "--cruft-expiration=now",
    ] {
        let (git, rgit) = both(
            &fx,
            &format!(
                r#"{input} && "$G" pack-objects --cruft {opts} .git/objects/pack/pack <in >/dev/null"#
            ),
        );
        assert_same_layout(&git, &rgit, opts);
    }
}

#[test]
fn repack_and_gc_cruft_match_git() {
    let fx = repo();
    if fx.is_skipped() {
        return;
    }
    for cmd in [
        r#""$G" repack --cruft -d"#,
        r#""$G" repack --cruft"#,
        r#""$G" repack --cruft -d --cruft-expiration=1.hour.ago"#,
        r#""$G" repack --cruft -d --cruft-expiration=now"#,
        r#""$G" repack --cruft -d && "$G" repack --cruft -d --cruft-expiration=1.hour.ago"#,
        r#""$G" gc -q"#,
        r#""$G" gc -q --no-prune"#,
        r#""$G" gc -q --prune=1.hour.ago"#,
        r#""$G" gc -q --prune=now"#,
        r#""$G" gc -q --no-cruft --prune=1.hour.ago"#,
        r#""$G" -c gc.cruftPacks=false gc -q"#,
        r#""$G" gc -q && "$G" gc -q --prune=now"#,
    ] {
        let (git, rgit) = both(&fx, cmd);
        assert_same_layout(&git, &rgit, cmd);
    }
    // git reads what rgit wrote.
    sh(
        &fx,
        r#"rm -rf b && mkdir b && cp -a .git b/ && cd b
        "$RGIT" gc -q
        "$GIT" fsck --full --no-dangling
        for p in .git/objects/pack/*.pack; do "$GIT" verify-pack $p; done
        test -n "$(ls .git/objects/pack/*.mtimes)""#,
    );
}

#[test]
fn expire_to_keeps_the_pruned_objects() {
    let fx = repo();
    if fx.is_skipped() {
        return;
    }
    // The unreachable objects that survive (the packed side branch and
    // the loose `new` blob) must share one mtime: when they do not, git
    // (2.56) also copies some of them, picked by object id, into the
    // --expire-to pack although they are not pruned. With the fixture's
    // timing that depended on whether `new` was written in the same
    // second as the side pack, which made this test flaky.
    sh(
        &fx,
        r#"t=$(date +%s)
        touch -d "@$t" .git/objects/pack/*
        new=$(echo new | "$GIT" hash-object --stdin)
        touch -d "@$t" .git/objects/$(echo $new | cut -c1-2)/$(echo $new | cut -c3-)"#,
    );
    let cmd = r#""$G" init -q --bare expired.git
        "$G" repack -d --cruft --cruft-expiration=1.hour.ago --expire-to=expired.git/objects/pack/pack"#;
    let (git, rgit) = both(&fx, cmd);
    assert_same_layout(&git, &rgit, "repository");
    let pruned = |d: &str| {
        sh(
            &fx,
            &format!(
                "for i in {d}/expired.git/objects/pack/*.idx; do \"$GIT\" show-index <$i | cut -d' ' -f2; done | sort"
            ),
        )
    };
    assert_eq!(pruned("b"), pruned("a"));
    // Exactly the objects older than the expiration: `old`, `mid` and
    // the tree naming `old`.
    let expired = sh(
        &fx,
        r#"old=$(echo old | "$GIT" hash-object --stdin)
        { echo $old
          echo mid | "$GIT" hash-object --stdin
          printf '100644 blob %s\tf\n' $old | "$GIT" mktree --missing
        } | sort"#,
    );
    assert_eq!(pruned("a"), expired);
}

#[test]
fn objects_written_again_leave_the_cruft_pack() {
    let fx = repo();
    if fx.is_skipped() {
        return;
    }
    sh(
        &fx,
        r#""$RGIT" repack --cruft -d
        o=$(echo old | "$RGIT" hash-object --stdin)
        test ! -f .git/objects/$(echo $o | cut -c1-2)/$(echo $o | cut -c3-)
        echo old | "$RGIT" hash-object -w --stdin >/dev/null
        test -f .git/objects/$(echo $o | cut -c1-2)/$(echo $o | cut -c3-)"#,
    );
}

#[test]
fn prune_keeps_what_recent_cruft_objects_reach() {
    let fx = fixture().git(["init", "-q"]);
    if fx.is_skipped() {
        return;
    }
    // A recent tree only in a cruft pack, naming an old loose blob.
    let script = r#"commit base && "$GIT" repack -adq
        P=$(basename .git/objects/pack/*.pack)
        b=$(echo oldblob | "$GIT" hash-object -w --stdin)
        t=$(printf '100644 blob %s\tf\n' $b | "$GIT" mktree)
        K=$(echo $b | "$GIT" pack-objects .git/objects/pack/pack)
        printf '%s\npack-%s.pack\n' $P $K | "$GIT" pack-objects --cruft .git/objects/pack/pack >/dev/null
        rm -f .git/objects/pack/pack-$K.*
        rm .git/objects/$(echo $t | cut -c1-2)/$(echo $t | cut -c3-)
        age 100000 $b
        "$RGIT" prune --expire=1.hour.ago
        test -f .git/objects/$(echo $b | cut -c1-2)/$(echo $b | cut -c3-)"#;
    sh(&fx, script);
}

#[test]
fn cruft_errors_match_git() {
    let mut fx = repo();
    if fx.is_skipped() {
        return;
    }
    for (args, stdin) in [
        (&["pack-objects", "--cruft", "x"][..], "pack-0000.pack\n"),
        (&["pack-objects", "--cruft", "--revs", "x"], ""),
        (&["repack", "--cruft", "-k"], ""),
        (&["repack", "--geometric=2", "-a"], ""),
        (&["repack", "--geometric=2", "--cruft"], ""),
        (&["repack", "--cruft", "--max-cruft-size=foo"], ""),
        (&["repack", "--geometric=foo"], ""),
    ] {
        fx.compare(args)
            .stdin(stdin)
            .checks([Check::ExitCode, Check::Stdout, Check::Stderr])
            .run();
    }
}

#[test]
fn geometric_matches_git() {
    let fx = fixture().git(["init", "-q"]);
    if fx.is_skipped() {
        return;
    }
    // Packs of 3, 3, 6 and 12 objects (one, one, two and four commits)
    // and three loose objects.
    sh(
        &fx,
        r#""$GIT" config maintenance.auto false
        n=0
        for size in 1 1 2 4; do
            i=0
            while test $i -lt $size; do n=$((n+1)); i=$((i+1)); commit c$n; done
            "$GIT" repack -dq
        done
        commit loose"#,
    );
    for cmd in [
        r#""$G" repack --geometric=2 -d"#,
        r#""$G" repack --geometric=2"#,
        r#""$G" repack --geometric=3 -d"#,
        r#""$G" repack -g2 -d && "$G" repack --geometric 2 -d"#,
        r#"touch .git/objects/pack/$(ls .git/objects/pack | grep pack$ | head -1 | sed s/pack$/keep/) && "$G" repack --geometric=2 -d"#,
    ] {
        let (git, rgit) = both(&fx, cmd);
        assert_eq!(rgit, git, "{cmd}");
    }
    let (git, rgit) = both(
        &fx,
        r#""$G" repack --geometric=2 -d && "$G" repack --geometric=2 -d >out; cat out"#,
    );
    assert_eq!(rgit, git);
    sh(
        &fx,
        r#"cd b && "$GIT" fsck --full --no-dangling && grep "Nothing new to pack" out"#,
    );
}
