// SPDX-License-Identifier: MIT
//! Library-style differential test with an `rgit-difftest` [`Fixture`]: the
//! reference git builds a repository, rgit-object parses the raw commit and
//! tree, and the text rendered from the parsed objects must equal git's own
//! `log`/`ls-tree` output (a mismatch panics with a unified diff).

use rgit_difftest::{Fixture, assert_same_text};
use rgit_object::{Commit, HashKind, Tree};

#[test]
fn parsed_commit_and_tree_match_git() {
    let fx = Fixture::new()
        .git(["init", "-q"])
        .write("hello.txt", "hello\n")
        .write("dir/sub.txt", "sub\n")
        .write("run.sh", "#!/bin/sh\n")
        .git(["add", "."])
        .git(["commit", "-q", "-m", "first"])
        .tick()
        .write("hello.txt", "hello again\n")
        .git(["commit", "-q", "-am", "second\n\nwith body"]);
    if fx.is_skipped() {
        return;
    }

    let raw = fx.git_stdout(["cat-file", "commit", "HEAD"]);
    let commit = Commit::parse(HashKind::Sha1, &raw).unwrap();
    let author = commit.author().unwrap();
    let committer = commit.committer().unwrap();
    let ours = format!(
        "{}\n{}\n{} <{}>\n{} <{}>\n{}",
        commit.tree().unwrap(),
        commit
            .parents()
            .unwrap()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(" "),
        String::from_utf8_lossy(author.name),
        String::from_utf8_lossy(author.email),
        String::from_utf8_lossy(committer.name),
        String::from_utf8_lossy(committer.email),
        String::from_utf8_lossy(commit.message()),
    );
    let theirs = fx.git_stdout(["log", "-1", "--format=%T%n%P%n%an <%ae>%n%cn <%ce>%n%B"]);
    // `log --format=%B` adds one newline after the message.
    let theirs = String::from_utf8_lossy(&theirs);
    assert_same_text(
        "commit",
        theirs.strip_suffix('\n').unwrap_or(&theirs),
        &ours,
    );

    let raw = fx.git_stdout(["cat-file", "tree", "HEAD^{tree}"]);
    let tree = Tree::parse(HashKind::Sha1, &raw).unwrap();
    let mut ours = String::new();
    for e in &tree.entries {
        let mode = e.mode.to_string();
        let kind = match mode.as_str() {
            "040000" => "tree",
            "160000" => "commit",
            _ => "blob",
        };
        ours.push_str(&format!(
            "{mode} {kind} {}\t{}\n",
            e.oid,
            String::from_utf8_lossy(&e.name)
        ));
    }
    let theirs = fx.git_stdout(["ls-tree", "HEAD"]);
    assert_same_text("ls-tree", &String::from_utf8_lossy(&theirs), &ours);
}
