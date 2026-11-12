// SPDX-License-Identifier: MIT
//! Behavioural tests of the option parser. The option table and strings are
//! our own; the behaviour checked is that of git's parse-options as
//! described in api-parse-options.adoc and observed with the git binary.

use std::cell::{Cell, RefCell};

use rgit_options::{CallbackError, Exit, Opt, ParseFlags, Parsed, Parser};

#[derive(Default)]
struct Vals {
    flag: Cell<i32>,
    count: Cell<i32>,
    int: Cell<i32>,
    small: Cell<i32>,
    uns: Cell<u64>,
    text: RefCell<Option<String>>,
    file: RefCell<Option<String>>,
    list: RefCell<Vec<String>>,
    mode: Cell<i32>,
    verbose: Cell<i32>,
    seen: RefCell<Vec<String>>,
}

fn table(v: &Vals) -> Vec<Opt<'_>> {
    vec![
        Opt::bool(None, "on", &v.flag, "turn it on"),
        Opt::bool('N', "no-worry", &v.flag, "starts with no-"),
        Opt::bool('X', "no-way", &v.flag, "cannot be negated").noneg(),
        Opt::countup('c', "count", &v.flag, "count up"),
        Opt::bit('8', "bit8", &v.flag, "or in 8", 8),
        Opt::integer('i', "int", &v.int, "an int"),
        Opt::integer(None, "small", &v.small, "a small int").range(-100, 100),
        Opt::unsigned('u', "uns", &v.uns, "an unsigned"),
        Opt::set_int(None, "set7", &v.mode, "set 7", 7),
        Opt::cmdmode(None, "first", &v.mode, "mode one", 1),
        Opt::cmdmode(None, "second", &v.mode, "mode two", 2),
        Opt::filename('F', "file", &v.file, "a file"),
        Opt::group("Text options"),
        Opt::string('t', "text", &v.text, "str", "some text"),
        Opt::string(None, "text2", &v.text, "str", "more text"),
        Opt::string(None, "te", &v.text, "te", "exact short name"),
        Opt::string('o', None, &v.text, "str", "short only"),
        Opt::string_list(None, "item", &v.list, "item", "add an item"),
        Opt::callback('L', "len", Some("str"), "callback", |arg, unset| {
            match (arg, unset) {
                (_, true) => Err(CallbackError::Silent),
                (Some(a), false) => {
                    v.seen.borrow_mut().push(format!("len {}", a.len()));
                    Ok(())
                }
                (None, false) => Ok(()),
            }
        }),
        Opt::number("number", |n| {
            v.seen.borrow_mut().push(format!("num {n}"));
            Ok(())
        }),
        Opt::countup('+', None, &v.count, "plus").nodash(),
        Opt::verbose(&v.verbose, "be loud"),
        Opt::callback(None, "level", Some("n"), "optional", |arg, _| {
            v.seen
                .borrow_mut()
                .push(format!("level {}", arg.unwrap_or("default")));
            Ok(())
        })
        .optional(None),
        Opt::noop(None, "old"),
        Opt::alias('A', "alias-text", "text"),
        Opt::hidden_group("Hidden"),
        Opt::bool(None, "secret", &v.flag, "hidden bool").hidden(),
    ]
}

const USAGE: &[&str] = &["demo [<options>]"];

fn run(v: &Vals, args: &[&str]) -> Result<Parsed, Exit> {
    let argv: Vec<String> = std::iter::once("demo")
        .chain(args.iter().copied())
        .map(String::from)
        .collect();
    let mut p = Parser::new(USAGE, table(v))
        .prefix(Some("sub/"))
        .disallow_abbreviations(false);
    p.parse(&argv)
}

fn err(args: &[&str]) -> Exit {
    let v = Vals::default();
    run(&v, args).expect_err("expected failure")
}

#[test]
fn booleans_and_negation() {
    let v = Vals::default();
    run(&v, &["--on"]).unwrap();
    assert_eq!(v.flag.get(), 1);
    run(&v, &["--no-on"]).unwrap();
    assert_eq!(v.flag.get(), 0);
    run(&v, &["-N"]).unwrap();
    assert_eq!(v.flag.get(), 1);
    run(&v, &["--worry"]).unwrap();
    assert_eq!(v.flag.get(), 0);
    run(&v, &["-N", "--no-no-worry"]).unwrap();
    assert_eq!(v.flag.get(), 0);
    run(&v, &["-XN"]).unwrap();
    assert_eq!(v.flag.get(), 1);
    let e = err(&["--way"]);
    assert_eq!(e.code, 129);
    assert!(
        e.stderr
            .starts_with("error: unknown option `way'\nusage: demo")
    );
    assert!(
        err(&["--no-no-way"])
            .stderr
            .starts_with("error: unknown option `no-no-way'")
    );
}

#[test]
fn integers() {
    let v = Vals::default();
    run(&v, &["-i", "-2345"]).unwrap();
    assert_eq!(v.int.get(), -2345);
    run(&v, &["-i234k"]).unwrap();
    assert_eq!(v.int.get(), 239616);
    run(&v, &["--int=0x10"]).unwrap();
    assert_eq!(v.int.get(), 16);
    run(&v, &["-u", "3g"]).unwrap();
    assert_eq!(v.uns.get(), 3 << 30);
    assert_eq!(
        err(&["--uns", "-1"]).stderr,
        "error: option `uns' expects a non-negative integer value with an optional k/m/g suffix\n"
    );
    assert_eq!(
        err(&["-i", "x"]).stderr,
        "error: switch `i' expects an integer value with an optional k/m/g suffix\n"
    );
    assert_eq!(
        err(&["--int="]).stderr,
        "error: option `int' expects a numerical value\n"
    );
    assert_eq!(
        err(&["--small", "101"]).stderr,
        "error: value 101 for option `small' not in range [-100,100]\n"
    );
    assert_eq!(
        err(&["--int=2g"]).stderr,
        "error: value 2g for option `int' not in range [-2147483648,2147483647]\n"
    );
    // unsigned cannot be negated
    assert!(
        err(&["--no-uns"])
            .stderr
            .starts_with("error: unknown option `no-uns'")
    );
}

#[test]
fn missing_and_superfluous_values() {
    let e = err(&["-t"]);
    assert_eq!(
        (e.code, e.stderr.as_str()),
        (129, "error: switch `t' requires a value\n")
    );
    assert_eq!(
        err(&["--text"]).stderr,
        "error: option `text' requires a value\n"
    );
    assert_eq!(
        err(&["--on=1"]).stderr,
        "error: option `on' takes no value\n"
    );
    assert_eq!(
        err(&["--no-on=1"]).stderr,
        "error: option `no-on' takes no value\n"
    );
    assert_eq!(
        err(&["--no-ite=1"]).stderr,
        "error: option `no-item' takes no value\n"
    );
    assert_eq!(
        err(&["--fir=1"]).stderr,
        "error: option `first' takes no value\n"
    );
}

#[test]
fn intermingled_and_dashdash() {
    let v = Vals::default();
    let p = run(
        &v,
        &[
            "a1", "--text", "123", "b1", "--on", "-i", "13", "--", "--on",
        ],
    )
    .unwrap();
    assert_eq!(p.args, ["a1", "b1", "--on"]);
    assert_eq!(v.text.borrow().as_deref(), Some("123"));
    assert_eq!(v.int.get(), 13);
    let p = run(&v, &["--end-of-options", "--verbose"]).unwrap();
    assert_eq!(p.args, ["--verbose"]);
    assert_eq!(v.verbose.get(), 0);
    let p = run(&v, &["-", "x"]).unwrap();
    assert_eq!(p.args, ["-", "x"]);
}

#[test]
fn abbreviations() {
    let v = Vals::default();
    run(&v, &["--in", "2"]).unwrap();
    assert_eq!(v.int.get(), 2);
    run(&v, &["--in=3", "-cc", "--no-c"]).unwrap();
    assert_eq!((v.int.get(), v.flag.get()), (3, 0));
    // exact name wins over longer names it abbreviates
    run(&v, &["--te", "x"]).unwrap();
    assert_eq!(v.text.borrow().as_deref(), Some("x"));
    // aliases do not make abbreviations ambiguous
    run(&v, &["--alias", "y"]).unwrap();
    assert_eq!(v.text.borrow().as_deref(), Some("y"));
    run(&v, &["-A", "z"]).unwrap();
    assert_eq!(v.text.borrow().as_deref(), Some("z"));
    let e = err(&["--tex", "1"]);
    assert_eq!(e.code, 129);
    assert!(
        e.stderr
            .starts_with("error: ambiguous option: tex (could be --text or --text2)\nusage: ")
    );
}

#[test]
fn disallowed_abbreviations() {
    let v = Vals::default();
    let argv: Vec<String> = ["demo", "--in", "2"].map(String::from).into();
    let e = Parser::new(USAGE, table(&v))
        .disallow_abbreviations(true)
        .parse(&argv)
        .unwrap_err();
    assert_eq!(e.code, 128);
    assert_eq!(
        e.stderr,
        "fatal: disallowed abbreviated or ambiguous option 'in'\n"
    );
}

#[test]
fn typos() {
    assert_eq!(
        err(&["-count"]).stderr,
        "error: did you mean `--count` (with two dashes)?\n"
    );
    assert_eq!(
        err(&["-no-on"]).stderr,
        "error: did you mean `--no-on` (with two dashes)?\n"
    );
    // two letters are a bundle, not a typo
    let v = Vals::default();
    run(&v, &["-cc"]).unwrap();
    assert_eq!(v.flag.get(), 2);
    assert!(
        err(&["-z"])
            .stderr
            .starts_with("error: unknown switch `z'\nusage: ")
    );
}

#[test]
fn bundles_and_stuck_values() {
    let v = Vals::default();
    run(&v, &["-vvc8", "-tabc", "-F", "f.txt"]).unwrap();
    assert_eq!(v.verbose.get(), 2);
    assert_eq!(v.flag.get(), 9);
    assert_eq!(v.text.borrow().as_deref(), Some("abc"));
    assert_eq!(v.file.borrow().as_deref(), Some("sub/f.txt"));
    run(&v, &["--file=/abs"]).unwrap();
    assert_eq!(v.file.borrow().as_deref(), Some("/abs"));
}

#[test]
fn counters_lists_callbacks_numbers() {
    let v = Vals::default();
    v.verbose.set(-1);
    run(&v, &["-v", "-v", "-v"]).unwrap();
    assert_eq!(v.verbose.get(), 3);
    run(&v, &["--no-verbose"]).unwrap();
    assert_eq!(v.verbose.get(), 0);
    run(
        &v,
        &[
            "--item",
            "a",
            "--item=b",
            "--no-item",
            "--item=c",
            "--item",
            "d",
        ],
    )
    .unwrap();
    assert_eq!(*v.list.borrow(), ["c", "d"]);
    run(
        &v,
        &[
            "--len=four",
            "-12345",
            "+",
            "+",
            "--level",
            "--level=3",
            "--old",
        ],
    )
    .unwrap();
    assert_eq!(
        *v.seen.borrow(),
        ["len 4", "num 12345", "level default", "level 3"]
    );
    assert_eq!(v.count.get(), 2);
    // a digit that is a short option wins over the number option
    run(&v, &["-8"]).unwrap();
    assert_eq!(v.flag.get() & 8, 8);
    let e = err(&["--no-len"]);
    assert_eq!(
        (e.code, e.stdout.as_str(), e.stderr.as_str()),
        (129, "", "")
    );
}

#[test]
fn cmdmode() {
    let v = Vals::default();
    run(&v, &["--first", "--first"]).unwrap();
    assert_eq!(v.mode.get(), 1);
    assert_eq!(
        err(&["--first", "--second"]).stderr,
        "error: options '--second' and '--first' cannot be used together\n"
    );
    assert_eq!(
        err(&["--second", "--set7"]).stderr,
        "error: options '--set7' and '--second' cannot be used together\n"
    );
    assert_eq!(
        err(&["--set7", "--second"]).stderr,
        "error: options '--second' and '--set7' cannot be used together\n"
    );
}

const EXPECTED_HELP: &str = "\
usage: demo [<options>]

    --[no-]on             turn it on
    -N, --no-worry        starts with no-
    --worry               opposite of --no-worry
    -X, --no-way          cannot be negated
    -c, --[no-]count      count up
    -8, --[no-]bit8       or in 8
    -i, --[no-]int <n>    an int
    --[no-]small <n>      a small int
    -u, --uns <n>         an unsigned
    --[no-]set7           set 7
    --first               mode one
    --second              mode two
    -F, --[no-]file <file>
                          a file

Text options
    -t, --[no-]text <str> some text
    --[no-]text2 <str>    more text
    --[no-]te <te>        exact short name
    -o <str>              short only
    --[no-]item <item>    add an item
    -L, --[no-]len <str>  callback
    -NUM                  number
    +                     plus
    -v, --[no-]verbose    be loud
    --[no-]level[=<n>]    optional
    -A, --[no-]alias-text <str>
                          alias of --text

";

#[test]
fn help_output() {
    let e = err(&["-h"]);
    assert_eq!(e.code, 0);
    assert_eq!(e.stderr, "");
    assert_eq!(e.stdout, EXPECTED_HELP);
    assert_eq!(err(&["--help"]).stdout, EXPECTED_HELP);
    // -h wins when it comes first, an unknown option when it comes first
    assert_eq!(err(&["-h", "--bogus"]).code, 0);
    assert_eq!(err(&["--bogus", "-h"]).code, 129);
    let all = err(&["--help-all"]).stdout;
    assert!(all.contains("    --[no-]old            no-op (backward compatibility)\n"));
    assert!(all.ends_with("\nHidden\n    --[no-]secret         hidden bool\n\n"));
}

#[test]
fn multi_line_usage() {
    let v = Vals::default();
    let p = Parser::new(
        &["git thing [-a] [-b]\n[-c] <x>", "git thing -d <y>"],
        vec![Opt::bool('a', None, &v.flag, "a")],
    );
    assert_eq!(
        p.usage_text(false),
        "usage: git thing [-a] [-b]\n                 [-c] <x>\n   or: git thing -d <y>\n\n    -a                    a\n\n"
    );
}

#[test]
fn completion_helper() {
    let v = Vals::default();
    let out = err(&["--git-completion-helper"]).stdout;
    assert!(out.starts_with("--on --no-worry --no-way --count --bit8 --int= --small= --uns= "));
    // `--no-worry` is listed already, so all negations follow the `--`,
    // after the positive form `--worry`.
    assert!(out.contains(" --worry -- --no-on --no-count "));
    assert!(!out.contains("--no-uns"));
    assert!(!out.contains("secret"));
    drop(v);
}

#[test]
fn flags() {
    let v = Vals::default();
    let mk =
        |f: ParseFlags| Parser::new(USAGE, vec![Opt::integer(None, "opt", &v.int, "opt")]).flags(f);
    let argv = |a: &[&str]| -> Vec<String> { a.iter().map(|s| s.to_string()).collect() };

    let p = mk(ParseFlags {
        keep_dashdash: true,
        ..Default::default()
    })
    .parse(&argv(&["cmd", "--opt=1", "--", "--opt=2", "--unknown"]))
    .unwrap();
    assert_eq!(p.args, ["--", "--opt=2", "--unknown"]);

    let p = mk(ParseFlags {
        keep_argv0: true,
        ..Default::default()
    })
    .parse(&argv(&["cmd", "arg0", "--opt=3"]))
    .unwrap();
    assert_eq!(p.args, ["cmd", "arg0"]);
    assert_eq!(v.int.get(), 3);

    let p = mk(ParseFlags {
        stop_at_non_option: true,
        ..Default::default()
    })
    .parse(&argv(&["cmd", "--opt=4", "arg0", "--opt=5", "--unknown"]))
    .unwrap();
    assert_eq!(p.args, ["arg0", "--opt=5", "--unknown"]);
    assert_eq!(v.int.get(), 4);

    let p = mk(ParseFlags {
        keep_unknown_opt: true,
        ..Default::default()
    })
    .parse(&argv(&["cmd", "--unknown=1", "--opt=6", "-u2"]))
    .unwrap();
    assert_eq!(p.args, ["--unknown=1", "-u2"]);
    assert_eq!(v.int.get(), 6);

    let e = mk(ParseFlags {
        no_internal_help: true,
        ..Default::default()
    })
    .parse(&argv(&["cmd", "-h"]))
    .unwrap_err();
    assert_eq!(e.code, 129);
    assert!(e.stderr.starts_with("error: unknown switch `h'\nusage: "));

    let p = mk(ParseFlags {
        no_internal_help: true,
        keep_unknown_opt: true,
        ..Default::default()
    })
    .parse(&argv(&["cmd", "-h", "--help", "--help-all"]))
    .unwrap();
    assert_eq!(p.args, ["-h", "--help", "--help-all"]);
}

fn sub_parser<'a>(v: &'a Vals, f: ParseFlags) -> Parser<'a> {
    Parser::new(
        &["cmd [--opt=<n>] (one | two) ..."],
        vec![
            Opt::subcommand("one"),
            Opt::subcommand("two"),
            Opt::integer(None, "opt", &v.int, "opt"),
        ],
    )
    .flags(f)
}

#[test]
fn subcommands() {
    let v = Vals::default();
    let argv = |a: &[&str]| -> Vec<String> { a.iter().map(|s| s.to_string()).collect() };
    let none = ParseFlags::default();
    let optional = ParseFlags {
        subcommand_optional: true,
        ..Default::default()
    };

    let p = sub_parser(&v, none)
        .parse(&argv(&["cmd", "--opt=1", "two", "one", "--opt=2"]))
        .unwrap();
    assert_eq!(p.subcommand.as_deref(), Some("two"));
    assert_eq!(p.args, ["two", "one", "--opt=2"]);
    assert_eq!(v.int.get(), 1);

    for args in [
        &["cmd"][..],
        &["cmd", "--", "one"],
        &["cmd", "--end-of-options", "one"],
    ] {
        let e = sub_parser(&v, none).parse(&argv(args)).unwrap_err();
        assert_eq!(e.code, 129);
        assert!(
            e.stderr.starts_with("error: need a subcommand\nusage: "),
            "{args:?}"
        );
    }
    for bad in ["nope", "on", "no-one"] {
        let e = sub_parser(&v, none)
            .parse(&argv(&["cmd", bad]))
            .unwrap_err();
        assert!(
            e.stderr
                .starts_with(&format!("error: unknown subcommand: `{bad}'\nusage: "))
        );
    }

    let p = sub_parser(&v, optional)
        .parse(&argv(&["cmd", "branch", "file"]))
        .unwrap();
    assert_eq!(
        (p.subcommand, p.args),
        (None, vec!["branch".into(), "file".into()])
    );

    let ko = ParseFlags {
        subcommand_optional: true,
        keep_unknown_opt: true,
        ..Default::default()
    };
    let p = sub_parser(&v, ko)
        .parse(&argv(&["cmd", "--sub-opt", "two"]))
        .unwrap();
    assert_eq!(
        (p.subcommand, p.args),
        (None, vec!["--sub-opt".into(), "two".into()])
    );

    let kd = ParseFlags {
        keep_dashdash: true,
        ..ko
    };
    let p = sub_parser(&v, kd)
        .parse(&argv(&["cmd", "--", "--x", "f"]))
        .unwrap();
    assert_eq!(p.args, ["--", "--x", "f"]);

    let e = sub_parser(
        &v,
        ParseFlags {
            stop_at_non_option: true,
            ..none
        },
    )
    .parse(&argv(&["cmd", "one"]))
    .unwrap_err();
    assert!(e.stderr.starts_with("BUG: "));
    assert_ne!(e.code, 0);

    let e = sub_parser(&v, none)
        .parse(&argv(&["cmd", "--git-completion-helper"]))
        .unwrap_err();
    assert_eq!(e.stdout, "one two --opt= --no-opt\n");
}

#[test]
fn usage_description() {
    let v = Vals::default();
    let p = Parser::new(
        &[
            "demo <x>",
            "demo -y",
            "",
            "Some description.",
            "Second line.",
        ],
        vec![Opt::bool('a', None, &v.flag, "a")],
    );
    assert_eq!(
        p.usage_text(false),
        "usage: demo <x>\n   or: demo -y\n\n    Some description.\n    Second line.\n\n    -a                    a\n\n"
    );
}

#[test]
fn number_in_bundle() {
    let v = Vals::default();
    run(&v, &["-12c"]).unwrap();
    assert_eq!(*v.seen.borrow(), ["num 12"]);
    assert_eq!(v.flag.get(), 1);
    run(&v, &["-c3"]).unwrap();
    assert_eq!(*v.seen.borrow(), ["num 12", "num 3"]);
    assert_eq!(
        err(&["-1z"]).stderr.lines().next(),
        Some("error: unknown switch `z'")
    );
}

#[test]
fn cmdmode_only_on_change() {
    let v = Vals::default();
    // Neither option is a command mode: no conflict.
    run(&v, &["--set7", "-i", "5"]).unwrap();
    let v = Vals::default();
    // Options that leave the value unchanged do not count.
    run(&v, &["--no-set7", "--first"]).unwrap();
    assert_eq!(
        err(&["--first", "--no-set7"]).stderr,
        "error: options '--no-set7' and '--first' cannot be used together\n"
    );
}

#[test]
fn completion_helper_all() {
    let out = err(&["--git-completion-helper-all"]).stdout;
    assert!(out.contains(" --old "));
    assert!(out.contains(" --alias-text= "));
    assert!(out.contains(" --secret "));
}

/// Usage printed after parsing shows aliases unresolved, with an unnamed
/// argument; `-h` and errors found while parsing resolve them.
#[test]
fn alias_in_usage_after_parse() {
    let on = Cell::new(false);
    let mk = || {
        Parser::new(
            &["demo"],
            vec![
                Opt::flag(None, "use-map", &on, "use the map"),
                Opt::alias(None, "map", "use-map"),
            ],
        )
    };
    let resolved = "    --[no-]map            alias of --use-map\n";
    let raw = "    --[no-]map ...        alias of --use-map\n";
    let help = mk()
        .parse(&["demo".to_string(), "-h".to_string()])
        .unwrap_err();
    assert!(help.stdout.contains(resolved), "{}", help.stdout);
    let unknown = mk()
        .parse(&["demo".to_string(), "--bogus".to_string()])
        .unwrap_err();
    assert!(unknown.stderr.contains(resolved), "{}", unknown.stderr);
    let p = mk();
    assert!(p.usage_error().stderr.contains(raw));
    assert!(p.usage_msg("x").stderr.contains(raw));
    assert!(p.error_with_usage("x").stderr.contains(raw));
}
