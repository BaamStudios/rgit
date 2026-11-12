// SPDX-License-Identifier: MIT

//! `parse-options`, `parse-options-flags` and `parse-subcommand` (t0040).
//!
//! The option tables, help texts and usage strings are those printed by the
//! reference `test-tool <cmd> -h`; the value dump format and the `--expect`
//! check are those observed in its output for t0040's calls. Parsing itself
//! is rgit's option parser (`rgit-options`).

use std::cell::{Cell, RefCell};
use std::fmt::Write as _;
use std::io::{self, Write};

use rgit_options::{CallbackError, Opt, ParseFlags, Parser};

use crate::messages;

/// Prefix the reference binary applies to relative `--file` arguments.
const FILE_PREFIX: &str = "prefix/";
/// Value of `abbrev` before any `--abbrev`.
const DEFAULT_ABBREV: i32 = 7;
/// `--abbrev=<n>` below this is raised to it.
const MINIMUM_ABBREV: i32 = 4;

fn argv_with(name: &str, args: &[String]) -> Vec<String> {
    std::iter::once(name.to_string())
        .chain(args.iter().cloned())
        .collect()
}

fn write_stdout(text: &str) -> i32 {
    let mut out = io::stdout().lock();
    if out
        .write_all(text.as_bytes())
        .and_then(|()| out.flush())
        .is_err()
    {
        return 128;
    }
    0
}

/// `-NUM`: the reference converts the digits like C `atoi`/`strtol` into an
/// `int`, so values that do not fit wrap around.
fn number_to_int(digits: &str) -> i32 {
    let mut v: i64 = 0;
    for b in digits.bytes() {
        v = v.saturating_mul(10).saturating_add(i64::from(b - b'0'));
    }
    v as i32
}

/// `--abbrev[=<n>]`.
fn parse_abbrev(arg: Option<&str>, unset: bool) -> Result<i32, CallbackError> {
    if unset {
        return Ok(0);
    }
    let Some(arg) = arg else {
        return Ok(-1);
    };
    let n: i64 = arg
        .parse()
        .ok()
        .filter(|_| !arg.starts_with('+'))
        .ok_or_else(|| CallbackError::Error(messages::ABBREV_NOT_NUMERIC.to_string()))?;
    Ok(n.clamp(i64::from(MINIMUM_ABBREV), i64::from(i32::MAX)) as i32)
}

/// The `--expect` lines, keyed by the text before the first colon.
#[derive(Default)]
struct Expectations(Vec<(String, String)>);

impl Expectations {
    fn add(&mut self, arg: Option<&str>) -> Result<(), CallbackError> {
        let Some(arg) = arg else {
            return Err(CallbackError::Fatal(messages::EXPECT_MALFORMED.into()));
        };
        let Some((key, _)) = arg.split_once(':') else {
            return Err(CallbackError::Fatal(messages::EXPECT_NO_COLON.into()));
        };
        if self.0.iter().any(|(k, _)| k == key) {
            return Err(CallbackError::Fatal(messages::EXPECT_DUPLICATE.into()));
        }
        self.0.push((key.to_string(), arg.to_string()));
        Ok(())
    }

    /// Checks the dump. Returns the diff output and whether it matched.
    fn check(&self, dump: &str) -> (String, bool) {
        let mut diff = String::new();
        for line in dump.lines() {
            let key = line.split_once(':').map_or(line, |(k, _)| k);
            if let Some((_, want)) = self.0.iter().find(|(k, _)| k == key) {
                if want != line {
                    let _ = writeln!(diff, "-{want}\n+{line}");
                }
            }
        }
        let ok = diff.is_empty();
        (diff, ok)
    }
}

pub fn parse_options(args: &[String]) -> i32 {
    let boolean = Cell::new(0);
    let integer = Cell::new(0);
    let i16_ = Cell::new(0);
    let unsigned = Cell::new(0u64);
    let u16_ = Cell::new(0u64);
    let string = RefCell::new(None::<String>);
    let file = RefCell::new(None::<String>);
    let list = RefCell::new(Vec::<String>::new());
    let abbrev = Cell::new(DEFAULT_ABBREV);
    let verbose = Cell::new(-1);
    let quiet = Cell::new(0);
    let dry_run = Cell::new(0);
    let ambiguous = Cell::new(0);
    let longhelp = Cell::new(0);
    let callback = RefCell::new(None::<String>);
    let expect = RefCell::new(Expectations::default());

    let opts = vec![
        Opt::bool(None, "yes", &boolean, "get a boolean"),
        Opt::bool('D', "no-doubt", &boolean, "begins with 'no-'"),
        Opt::bool('B', "no-fear", &boolean, "be brave").noneg(),
        Opt::countup('b', "boolean", &boolean, "increment by one"),
        Opt::bit('4', "or4", &boolean, "bitwise-or boolean with ...0100", 4),
        Opt::negbit(None, "neg-or4", &boolean, "same as --no-or4", 4),
        Opt::group(""),
        Opt::integer('i', "integer", &integer, "get a integer"),
        Opt::integer(None, "i16", &i16_, "get a 16 bit integer").range(-32768, 32767),
        Opt::integer('j', None, &integer, "get a integer, too"),
        Opt::unsigned('u', "unsigned", &unsigned, "get an unsigned integer"),
        Opt::unsigned(None, "u16", &u16_, "get a 16 bit unsigned integer").max(65535),
        Opt::set_int(None, "set23", &integer, "set integer to 23", 23),
        Opt::cmdmode(
            None,
            "mode1",
            &integer,
            "set integer to 1 (cmdmode option)",
            1,
        ),
        Opt::cmdmode(
            None,
            "mode2",
            &integer,
            "set integer to 2 (cmdmode option)",
            2,
        ),
        Opt::callback(
            None,
            "mode34",
            Some("(3|4)"),
            "set integer to 3 or 4 (cmdmode option)",
            |arg, unset| {
                let v = match (unset, arg) {
                    (true, _) => 0,
                    (false, Some("3")) => 3,
                    (false, Some("4")) => 4,
                    (false, a) => {
                        return Err(CallbackError::Error(messages::invalid_value(
                            "--mode34",
                            a.unwrap_or_default(),
                        )));
                    }
                };
                integer.set(v);
                Ok(())
            },
        )
        .literal_argh()
        .cmdmode_slot(&integer),
        Opt::callback(
            'L',
            "length",
            Some("str"),
            "get length of <str>",
            |arg, unset| {
                if unset {
                    return Err(CallbackError::Silent);
                }
                let arg = arg.unwrap_or_default();
                integer.set(arg.len() as i32);
                *callback.borrow_mut() = Some(arg.to_string());
                Ok(())
            },
        )
        .value_slot(&integer),
        Opt::callback(
            'F',
            "file",
            Some("file"),
            "set file to <file>",
            |arg, unset| {
                *file.borrow_mut() = match arg {
                    _ if unset => None,
                    None | Some("") => None,
                    Some(a) if a == "-" || std::path::Path::new(a).is_absolute() => {
                        Some(a.to_string())
                    }
                    Some(a) => Some(format!("{FILE_PREFIX}{a}")),
                };
                Ok(())
            },
        ),
        Opt::group("String options"),
        Opt::string('s', "string", &string, "string", "get a string"),
        Opt::string(None, "string2", &string, "str", "get another string"),
        Opt::string(
            None,
            "st",
            &string,
            "st",
            "get another string (pervert ordering)",
        ),
        Opt::string('o', None, &string, "str", "get another string"),
        Opt::noop(None, "obsolete"),
        Opt::bool(
            None,
            "longhelp",
            &longhelp,
            "help text of this entry\nspans multiple lines",
        )
        .noneg(),
        Opt::string_list(None, "list", &list, "str", "add str to list"),
        Opt::group("Magic arguments"),
        Opt::number("set integer to NUM", |digits| {
            integer.set(number_to_int(digits));
            Ok(())
        }),
        Opt::countup('+', None, &boolean, "same as -b").nodash(),
        Opt::set_int(None, "ambiguous", &ambiguous, "positive ambiguity", 1).noneg(),
        Opt::set_int(None, "no-ambiguous", &ambiguous, "negative ambiguity", 0).noneg(),
        Opt::group("Standard options"),
        Opt::callback(
            None,
            "abbrev",
            Some("n"),
            "use <n> digits to display object names",
            |arg, unset| {
                abbrev.set(parse_abbrev(arg, unset)?);
                Ok(())
            },
        )
        .optional(None),
        Opt::verbose(&verbose, "be verbose"),
        Opt::dry_run(&dry_run, "dry run"),
        Opt::quiet(&quiet, "be quiet"),
        Opt::callback(
            None,
            "expect",
            Some("string"),
            "expected output in the variable dump",
            |arg, unset| expect.borrow_mut().add(if unset { None } else { arg }),
        ),
        Opt::group("Alias"),
        Opt::string('A', "alias-source", &string, "string", "get a string"),
        Opt::alias('Z', "alias-target", "alias-source"),
        Opt::hidden_group("Hidden options"),
        Opt::bool(None, "hidden-bool", &boolean, "get a boolean").hidden(),
        Opt::integer('k', "hidden-integer", &integer, "get a integer").hidden(),
    ];

    let argv = argv_with("parse-options", args);
    let mut parser = Parser::new(
        &[
            "test-tool parse-options <options>",
            "",
            "A helper function for the parse-options API.",
        ],
        opts,
    );
    let parsed = parser.parse(&argv);
    drop(parser);
    let parsed = match parsed {
        Ok(p) => p,
        Err(exit) => return exit.emit(),
    };

    let mut dump = String::new();
    if let Some(cb) = callback.borrow().as_deref() {
        let _ = writeln!(dump, "Callback: \"{cb}\", 0");
    }
    let not_set = |v: &Option<String>| v.clone().unwrap_or_else(|| "(not set)".into());
    let _ = writeln!(dump, "boolean: {}", boolean.get());
    let _ = writeln!(dump, "integer: {}", integer.get());
    let _ = writeln!(dump, "i16: {}", i16_.get());
    let _ = writeln!(dump, "unsigned: {}", unsigned.get());
    let _ = writeln!(dump, "u16: {}", u16_.get());
    let _ = writeln!(dump, "timestamp: 0");
    let _ = writeln!(dump, "string: {}", not_set(&string.borrow()));
    let _ = writeln!(dump, "abbrev: {}", abbrev.get());
    let _ = writeln!(dump, "verbose: {}", verbose.get());
    let _ = writeln!(dump, "quiet: {}", quiet.get());
    let _ = writeln!(
        dump,
        "dry run: {}",
        if dry_run.get() != 0 { "yes" } else { "no" }
    );
    let _ = writeln!(dump, "file: {}", not_set(&file.borrow()));
    for item in list.borrow().iter() {
        let _ = writeln!(dump, "list: {item}");
    }
    for (i, arg) in parsed.args.iter().enumerate() {
        let _ = writeln!(dump, "arg {i:02}: {arg}");
    }

    let expect = expect.borrow();
    if expect.0.is_empty() {
        return write_stdout(&dump);
    }
    let (diff, ok) = expect.check(&dump);
    let code = write_stdout(&diff);
    if code != 0 {
        code
    } else if ok {
        0
    } else {
        1
    }
}

/// Which helper is running: they share the flag options.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Helper {
    Flags,
    Subcommand,
}

/// Parses the flag options in front of `cmd`. Returns the flags and the
/// arguments from `cmd` on.
fn parse_flag_options(helper: Helper, args: &[String]) -> Result<(ParseFlags, Vec<String>), i32> {
    let keep_dashdash = Cell::new(false);
    let stop_at_non_option = Cell::new(false);
    let keep_argv0 = Cell::new(false);
    let keep_unknown_opt = Cell::new(false);
    let no_internal_help = Cell::new(false);
    let subcommand_optional = Cell::new(false);
    let opts = vec![
        Opt::group("flag-options:"),
        Opt::flag(
            None,
            "keep-dashdash",
            &keep_dashdash,
            "pass PARSE_OPT_KEEP_DASHDASH to parse_options()",
        ),
        Opt::flag(
            None,
            "stop-at-non-option",
            &stop_at_non_option,
            "pass PARSE_OPT_STOP_AT_NON_OPTION to parse_options()",
        ),
        Opt::flag(
            None,
            "keep-argv0",
            &keep_argv0,
            "pass PARSE_OPT_KEEP_ARGV0 to parse_options()",
        ),
        Opt::flag(
            None,
            "keep-unknown-opt",
            &keep_unknown_opt,
            "pass PARSE_OPT_KEEP_UNKNOWN_OPT to parse_options()",
        ),
        Opt::flag(
            None,
            "no-internal-help",
            &no_internal_help,
            "pass PARSE_OPT_NO_INTERNAL_HELP to parse_options()",
        ),
        Opt::flag(
            None,
            "subcommand-optional",
            &subcommand_optional,
            "pass PARSE_OPT_SUBCOMMAND_OPTIONAL to parse_options()",
        ),
    ];
    let (name, usage) = match helper {
        Helper::Flags => (
            "parse-options-flags",
            "test-tool parse-options-flags [flag-options] cmd [options]",
        ),
        Helper::Subcommand => (
            "parse-subcommand",
            "test-tool parse-subcommand [flag-options] cmd <subcommand>",
        ),
    };
    let argv = argv_with(name, args);
    let mut parser = Parser::new(&[usage], opts).flags(ParseFlags {
        stop_at_non_option: true,
        ..ParseFlags::default()
    });
    let rest = match parser.parse(&argv) {
        Ok(p) => p.args,
        Err(exit) => return Err(exit.emit()),
    };
    if rest.first().map(String::as_str) != Some("cmd") {
        return Err(parser.error_with_usage(messages::CMD_MANDATORY).emit());
    }
    drop(parser);
    Ok((
        ParseFlags {
            keep_dashdash: keep_dashdash.get(),
            stop_at_non_option: stop_at_non_option.get(),
            keep_argv0: keep_argv0.get(),
            keep_unknown_opt: keep_unknown_opt.get(),
            no_internal_help: no_internal_help.get(),
            subcommand_optional: subcommand_optional.get(),
        },
        rest,
    ))
}

/// The reference's sanity checks of the flag combination, in its order.
/// They abort with `BUG: ...` and exit code 99.
fn check_flags(flags: &ParseFlags, has_subcommands: bool) -> Result<(), i32> {
    let bug = if flags.subcommand_optional && !has_subcommands {
        Some(messages::BUG_SUBCOMMAND_OPTIONAL_WITHOUT_SUBCOMMANDS)
    } else if has_subcommands && flags.stop_at_non_option {
        Some(messages::BUG_SUBCOMMANDS_STOP_AT_NON_OPTION)
    } else if has_subcommands && flags.keep_unknown_opt && !flags.subcommand_optional {
        Some(messages::BUG_SUBCOMMANDS_KEEP_UNKNOWN_OPT)
    } else if has_subcommands && flags.keep_dashdash && !flags.subcommand_optional {
        Some(messages::BUG_SUBCOMMANDS_KEEP_DASHDASH)
    } else if flags.stop_at_non_option && flags.keep_unknown_opt {
        Some(messages::BUG_STOP_AT_NON_OPTION_KEEP_UNKNOWN)
    } else {
        None
    };
    match bug {
        Some(msg) => {
            eprintln!("{msg}");
            Err(messages::BUG_EXIT_CODE)
        }
        None => Ok(()),
    }
}

fn dump_args(out: &mut String, args: &[String]) {
    for (i, arg) in args.iter().enumerate() {
        let _ = writeln!(out, "arg {i:02}: {arg}");
    }
}

pub fn parse_options_flags(args: &[String]) -> i32 {
    let (flags, argv) = match parse_flag_options(Helper::Flags, args) {
        Ok(v) => v,
        Err(code) => return code,
    };
    if let Err(code) = check_flags(&flags, false) {
        return code;
    }
    let opt = Cell::new(0);
    let mut parser = Parser::new(
        &["<...> cmd [options]"],
        vec![Opt::integer('o', "opt", &opt, "an integer option")],
    )
    .flags(flags);
    let parsed = parser.parse(&argv);
    drop(parser);
    let parsed = match parsed {
        Ok(p) => p,
        Err(exit) => return exit.emit(),
    };
    let mut out = format!("opt: {}\n", opt.get());
    dump_args(&mut out, &parsed.args);
    write_stdout(&out)
}

pub fn parse_subcommand(args: &[String]) -> i32 {
    let (flags, argv) = match parse_flag_options(Helper::Subcommand, args) {
        Ok(v) => v,
        Err(code) => return code,
    };
    if let Err(code) = check_flags(&flags, true) {
        return code;
    }
    let opt = Cell::new(0);
    let mut parser = Parser::new(
        &["<...> cmd subcmd-one", "<...> cmd subcmd-two"],
        // The subcommands come first: the reference lists them first for
        // `--git-completion-helper`.
        vec![
            Opt::subcommand("subcmd-one"),
            Opt::subcommand("subcmd-two"),
            Opt::integer('o', "opt", &opt, "an integer option"),
        ],
    )
    .flags(flags);
    let parsed = parser.parse(&argv);
    drop(parser);
    let parsed = match parsed {
        Ok(p) => p,
        Err(exit) => return exit.emit(),
    };
    let func = match parsed.subcommand.as_deref() {
        Some("subcmd-two") => "subcmd_two",
        _ => "subcmd_one",
    };
    let mut out = format!("opt: {}\nfn: {func}\n", opt.get());
    dump_args(&mut out, &parsed.args);
    write_stdout(&out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abbrev_values() {
        assert_eq!(parse_abbrev(None, false), Ok(-1));
        assert_eq!(parse_abbrev(None, true), Ok(0));
        assert_eq!(parse_abbrev(Some("2"), false), Ok(4));
        assert_eq!(parse_abbrev(Some("-3"), false), Ok(4));
        assert_eq!(parse_abbrev(Some("100"), false), Ok(100));
        assert!(parse_abbrev(Some(""), false).is_err());
        assert!(parse_abbrev(Some("4x"), false).is_err());
    }

    #[test]
    fn numbers_wrap_like_c() {
        assert_eq!(number_to_int("12345"), 12345);
        assert_eq!(number_to_int("12345678901"), -539222987);
    }

    #[test]
    fn expectations() {
        let mut e = Expectations::default();
        e.add(Some("boolean: 1")).unwrap();
        e.add(Some("list: a")).unwrap();
        assert!(e.add(Some("boolean: 2")).is_err());
        assert!(e.add(Some("nocolon")).is_err());
        let (diff, ok) = e.check("boolean: 1\nlist: a\nlist: b\n");
        assert!(!ok);
        assert_eq!(diff, "-list: a\n+list: b\n");
    }
}
