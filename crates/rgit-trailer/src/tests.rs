// SPDX-License-Identifier: MIT
//! Unit tests; expectations come from the `git` binary.

use super::*;

fn run(input: &str, conf: &Config, opts: &Options, args: &[&str]) -> String {
    let args: Vec<NewTrailer> = args.iter().map(|a| NewTrailer::new(*a)).collect();
    let mut errors = Vec::new();
    let out = process(input.as_bytes(), conf, opts, &args, &mut |e| {
        errors.push(e.to_owned())
    });
    String::from_utf8(out).unwrap()
}

fn opts() -> Options {
    Options {
        comment: "#".into(),
        ..Default::default()
    }
}

fn add(input: &str, args: &[&str]) -> String {
    run(input, &Config::default(), &opts(), args)
}

fn parsed(input: &str) -> String {
    let o = Options {
        only_trailers: true,
        only_input: true,
        unfold: true,
        ..opts()
    };
    run(input, &Config::default(), &o, &[])
}

#[test]
fn separator() {
    assert_eq!(find_separator(b"Acked-by: x", b":"), Some(8));
    assert_eq!(find_separator(b"A-1 \t: c", b":"), Some(5));
    assert_eq!(find_separator(b"A b: c", b":"), None);
    assert_eq!(find_separator(b"A_b: c", b":"), None);
    assert_eq!(find_separator(b" a: c", b":"), None);
    assert_eq!(find_separator(b":c", b":"), Some(0));
}

#[test]
fn block_detection() {
    assert_eq!(parsed("subj\n\nA: b\n"), "A: b\n");
    // The title is never a trailer block.
    assert_eq!(parsed("A: b\n"), "");
    assert_eq!(parsed("\nA: b\n"), "A: b\n");
    assert_eq!(parsed("subj\nA: b\n\nB: c\n"), "B: c\n");
    // One non-trailer spoils a block without known trailers.
    assert_eq!(parsed("subj\n\nA: b\nfoo\n"), "");
    // A quarter is enough with a git-generated one.
    assert_eq!(
        parsed("subj\n\nSigned-off-by: b\nfoo\nbar\nbaz\n"),
        "Signed-off-by: b\n"
    );
    assert_eq!(parsed("subj\n\nSigned-off-by: b\nfoo\nbar\nbaz\nqux\n"), "");
    // URLs are not trailers, a space before the colon makes one.
    assert_eq!(
        parsed("subj\n\nhttps://x.org\nhttps ://y.org\nSigned-off-by: s\n"),
        "https: //y.org\nSigned-off-by: s\n"
    );
}

#[test]
fn continuation_and_unfold() {
    assert_eq!(parsed("s\n\nfoo: a  \n  b\t\n\tc\n"), "foo: a   b\t c\n");
    let o = Options {
        only_trailers: true,
        ..opts()
    };
    assert_eq!(
        run("s\n\nfoo: a  \n  b\t\n\tc\n", &Config::default(), &o, &[]),
        "foo: a  \n  b\t\n\tc\n"
    );
}

#[test]
fn ignored_tail_and_divider() {
    assert_eq!(
        add("subj\n\nA: b\n# c\n\n# d\n\n", &["x=y"]),
        "subj\n\nA: b\nx: y\n# c\n\n# d\n\n"
    );
    assert_eq!(
        add("subj\n\nA: b\nConflicts:\n\tfoo\n", &["x=y"]),
        "subj\n\nA: b\nx: y\nConflicts:\n\tfoo\n"
    );
    assert_eq!(
        add("subj\n\nbody\n\n\n", &["x=y"]),
        "subj\n\nbody\n\nx: y\n\n\n"
    );
    assert_eq!(add("subj\n\nA: b\n  \n", &["x=y"]), "subj\n\nA: b\nx: y\n");
    assert_eq!(
        add("subj\n---\nA: b\n", &["x=y"]),
        "subj\n\nx: y\n---\nA: b\n"
    );
    assert_eq!(add("", &[]), "\n");
    assert_eq!(add("area: change", &["a=b"]), "area: change\n\na: b\n");
    let cut = format!("#{}", messages::SCISSORS);
    assert_eq!(
        add(&format!("subj\n\nA: b\n{cut}\nB: c\n"), &["x=y"]),
        format!("subj\n\nA: b\nx: y\n{cut}\nB: c\n")
    );
}

#[test]
fn placement_rules() {
    let input = "subj\n\nfoo: a\nbar: x\nfoo: b\n";
    let with = |w: Where, e: IfExists, arg: &str| {
        let a = NewTrailer {
            where_: Some(w),
            if_exists: Some(e),
            ..NewTrailer::new(arg)
        };
        let out = process(
            input.as_bytes(),
            &Config::default(),
            &opts(),
            &[a],
            &mut |_| {},
        );
        String::from_utf8(out).unwrap()
    };
    assert_eq!(
        with(Where::After, IfExists::Replace, "foo: c"),
        "subj\n\nfoo: a\nbar: x\nfoo: c\n"
    );
    assert_eq!(
        with(Where::Before, IfExists::Replace, "foo: c"),
        "subj\n\nfoo: c\nbar: x\nfoo: b\n"
    );
    assert_eq!(
        with(Where::Start, IfExists::AddIfDifferentNeighbor, "foo: a"),
        "subj\n\nfoo: a\nbar: x\nfoo: b\n"
    );
    assert_eq!(
        with(Where::End, IfExists::AddIfDifferentNeighbor, "foo: a"),
        "subj\n\nfoo: a\nbar: x\nfoo: b\nfoo: a\n"
    );
    // Keys match up to the shorter one, ignoring case.
    assert_eq!(
        with(Where::End, IfExists::Replace, "FOOBAR: c"),
        "subj\n\nfoo: a\nbar: x\nFOOBAR: c\n"
    );
}

#[test]
fn configured_keys() {
    let mut conf = Config {
        separators: ":=#".into(),
        ..Config::default()
    };
    conf.items.push(Item {
        name: "bug".into(),
        key: Some("Bug #".into()),
        ..Default::default()
    });
    conf.items.push(Item {
        name: "ack".into(),
        key: Some("Acked-by= ".into()),
        ..Default::default()
    });
    assert_eq!(
        run("", &conf, &opts(), &["bug = 42", "Ack: x", "other: y"]),
        "\nBug #42\nAcked-by= x\nother: y\n"
    );
    // A configured key makes a block count with a quarter of trailers.
    assert_eq!(
        run("s\n\nfoo\nbar\nbaz\nBu: 1\n", &conf, &opts(), &["x=y"]),
        "s\n\nfoo\nbar\nbaz\nBug #1\nx: y\n"
    );
}

#[test]
fn arguments() {
    assert_eq!(add("", &["Reviewed-by"]), "\nReviewed-by: \n");
    assert_eq!(add("", &[" : y"]), "\n: y: \n");
    assert_eq!(add("", &["x://y"]), "\nx://y: \n");
    let conf = Config::default();
    assert!(parse_argument(b":x", &conf).is_err());
    let o = Options {
        trim_empty: true,
        ..opts()
    };
    assert_eq!(
        run("", &conf, &o, &["ack=Peff", "Reviewed-by", "sob:"]),
        "\nack: Peff\n"
    );
}

#[test]
fn commands() {
    let mut conf = Config::default();
    conf.items.push(Item {
        name: "bug".into(),
        key: Some("Bug".into()),
        cmd: Some("echo \"[$1]\"".into()),
        ..Default::default()
    });
    // `cmd` alone does not add a trailer by itself.
    assert_eq!(run("s\n", &conf, &opts(), &[]), "s\n\n");
    assert_eq!(
        run("s\n\nBug: old\n", &conf, &opts(), &["bug=x", "bug"]),
        // The value is also appended as an argument.
        "s\n\nBug: old\nBug: [x] x\nBug: [[x] x] [x] x\n"
    );
    conf.items[0].cmd = None;
    conf.items[0].command = Some("echo \"<$ARG>\"".into());
    assert_eq!(
        run("s\n", &conf, &opts(), &["bug=1"]),
        "s\n\nBug: <>\nBug: <1>\n"
    );
}
