// SPDX-License-Identifier: MIT

//! Behaviour of the public API beyond plain matching (which the
//! differential tests cover): seen tracking, pruning, magic checks,
//! implicit items and the magic string for subprocesses.

use rgit_pathspec::{
    DirInterest, ImplicitBase, Magic, MatchKind, ParseOptions, Pathspec, PathspecError, messages,
};

fn parse(args: &[&str]) -> Pathspec {
    Pathspec::parse(args, &ParseOptions::default()).unwrap()
}

fn parse_in(prefix: &str, args: &[&str]) -> Pathspec {
    let opts = ParseOptions {
        prefix: prefix.as_bytes().to_vec(),
        ..Default::default()
    };
    Pathspec::parse(args, &opts).unwrap()
}

#[test]
fn match_kinds() {
    let ps = parse(&["sub", "*.c", "a.c"]);
    assert_eq!(ps.match_path(b"sub/x"), Some(MatchKind::Recursive));
    assert_eq!(ps.match_path(b"b.c"), Some(MatchKind::Wildcard));
    assert_eq!(ps.match_path(b"a.c"), Some(MatchKind::Exact));
    assert_eq!(ps.match_path(b"subx"), None);
    assert_eq!(parse(&[]).match_path(b"x"), Some(MatchKind::Recursive));
}

#[test]
fn seen_and_unmatched() {
    let ps = parse(&["a", "nope", "s*", ":!a"]);
    let mut seen = vec![false; ps.len()];
    for path in [&b"a"[..], b"sub/b"] {
        let _ = ps.match_path_seen(path, &mut seen);
    }
    // `a` counts as seen although it is excluded, like git.
    let unmatched: Vec<_> = ps.unmatched(&seen).map(|i| i.original().to_vec()).collect();
    assert_eq!(unmatched, vec![b"nope".to_vec()]);
    assert_eq!(
        messages::did_not_match_any_files(&unmatched[0]),
        "pathspec 'nope' did not match any files"
    );
}

#[test]
fn exclude_only_gets_implicit_item() {
    let cwd = parse_in("sub/", &[":!sub/"]);
    assert_eq!(cwd.len(), 2);
    assert!(cwd.items()[1].is_implicit());
    assert!(cwd.matches(b"sub/file"));
    assert!(!cwd.matches(b"sub/sub/file"));
    assert!(!cwd.matches(b"file"));

    let opts = ParseOptions {
        prefix: b"sub/".to_vec(),
        implicit_base: ImplicitBase::Top,
        ..Default::default()
    };
    let top = Pathspec::parse([":!sub/"], &opts).unwrap();
    assert!(top.matches(b"file"));
    assert!(top.matches(b"sub2/file"));
    assert!(!top.matches(b"sub/sub/file"));
    let seen = vec![false; top.len()];
    assert_eq!(top.unmatched(&seen).count(), 0);
}

#[test]
fn dir_interest() {
    let ps = parse(&["sub/deep", "src/*.c", ":!src/gen"]);
    assert_eq!(ps.dir_interest(b"sub"), DirInterest::Maybe);
    assert_eq!(ps.dir_interest(b"sub/deep"), DirInterest::All);
    assert_eq!(ps.dir_interest(b"sub/deep/x"), DirInterest::All);
    assert_eq!(ps.dir_interest(b"sub/other"), DirInterest::None);
    assert_eq!(ps.dir_interest(b"src"), DirInterest::Maybe);
    assert_eq!(ps.dir_interest(b"src/x"), DirInterest::Maybe);
    assert_eq!(ps.dir_interest(b"src/gen"), DirInterest::None);
    assert_eq!(ps.dir_interest(b"other"), DirInterest::None);
    assert_eq!(parse(&[]).dir_interest(b"x"), DirInterest::All);
    assert_eq!(parse(&["."]).dir_interest(b"x"), DirInterest::All);
    assert_eq!(
        parse(&[":(icase)SUB"]).dir_interest(b"sub"),
        DirInterest::All
    );
    assert_eq!(parse(&["su"]).dir_interest(b"sub"), DirInterest::None);
}

#[test]
fn common_prefix() {
    assert_eq!(parse(&["sub/a/x", "sub/a/y*"]).common_prefix(), b"sub/a/");
    assert_eq!(parse(&["sub/a/x", "sub/b"]).common_prefix(), b"sub/");
    assert_eq!(parse(&["sub/a", "other"]).common_prefix(), b"");
    assert_eq!(parse(&["s*/a"]).common_prefix(), b"");
    assert_eq!(
        parse_in("Dir/", &[":(icase)Sub/x"]).common_prefix(),
        b"Dir/"
    );
    assert_eq!(parse(&[]).common_prefix(), b"");
}

#[test]
fn unsupported_magic() {
    let opts = ParseOptions {
        allowed_magic: Magic::TOP,
        ..Default::default()
    };
    let err = Pathspec::parse([":(icase,exclude,top,attr:x,glob)a"], &opts).unwrap_err();
    assert_eq!(
        err.to_string(),
        ":(icase,exclude,top,attr:x,glob)a: pathspec magic not supported by this command: \
         'glob', 'icase', 'exclude' (mnemonic: '!'), 'attr'"
    );
    assert!(Pathspec::parse([":/a"], &opts).is_ok());
}

#[test]
fn attr_items_use_the_callback() {
    let ps = parse(&[":(attr:text)*.c", ":(exclude,attr:-diff)*.c"]);
    let item = &ps.items()[0];
    assert!(item.magic().contains(Magic::ATTR));
    assert_eq!(item.attr().unwrap().raw, b"text");
    let r = ps.match_path_with(b"a.c", None, &mut |path, spec| {
        // pretend: a.c has `text` set and `diff` set
        assert_eq!(path, b"a.c");
        spec.raw == b"text"
    });
    assert_eq!(r, Some(MatchKind::Wildcard));
    let r = ps.match_path_with(b"a.c", None, &mut |_, _| false);
    assert_eq!(r, None);
    let r = ps.match_path_with(b"a.c", None, &mut |_, _| true);
    assert_eq!(r, None, "excluded by the second item");
}

#[test]
fn magic_string_for_subprocesses() {
    let ps = parse_in("sub/", &[":(icase)Foo*", ":!x"]);
    assert_eq!(
        ps.items()[0].to_magic_string(),
        b":(icase,prefix:4)sub/Foo*"
    );
    assert_eq!(ps.items()[1].to_magic_string(), b":(exclude,prefix:4)sub/x");
    // Round trip at the top.
    let again = parse(&[std::str::from_utf8(&ps.items()[0].to_magic_string()).unwrap()]);
    assert!(again.matches(b"sub/foobar"));
    assert!(!again.matches(b"SUB/foobar"));
}

#[test]
fn literal_and_glob_conflict() {
    let err = Pathspec::parse([":(literal,glob)x"], &ParseOptions::default()).unwrap_err();
    assert_eq!(
        err,
        PathspecError::LiteralAndGlob {
            arg: b":(literal,glob)x".to_vec()
        }
    );
}
