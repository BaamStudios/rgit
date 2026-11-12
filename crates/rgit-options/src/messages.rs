// SPDX-License-Identifier: MIT
//! Every user-visible message of the option parser, in one place so that they
//! can be reviewed against git's observable output (see CLEANROOM.md).

/// How the user named an option: `switch `x'` for short options,
/// `option `name'` for long ones.
pub(crate) fn opt_ref(short: Option<char>, long: &str) -> String {
    match short {
        Some(c) => format!("switch `{c}'"),
        None => format!("option `{long}'"),
    }
}

pub(crate) fn unknown_option(body: &str) -> String {
    format!("unknown option `{body}'")
}

pub(crate) fn unknown_switch(c: char) -> String {
    format!("unknown switch `{c}'")
}

pub(crate) fn requires_value(what: &str) -> String {
    format!("{what} requires a value")
}

pub(crate) fn takes_no_value(what: &str) -> String {
    format!("{what} takes no value")
}

pub(crate) fn ambiguous(arg: &str, first: &str, second: &str) -> String {
    format!("ambiguous option: {arg} (could be --{first} or --{second})")
}

pub(crate) fn did_you_mean(body: &str) -> String {
    format!("did you mean `--{body}` (with two dashes)?")
}

pub(crate) fn disallowed_abbrev(name: &str) -> String {
    format!("disallowed abbreviated or ambiguous option '{name}'")
}

pub(crate) fn cannot_be_used_together(a: &str, b: &str) -> String {
    format!("options '{a}' and '{b}' cannot be used together")
}

pub(crate) fn expects_numerical(what: &str) -> String {
    format!("{what} expects a numerical value")
}

pub(crate) fn expects_integer(what: &str) -> String {
    format!("{what} expects an integer value with an optional k/m/g suffix")
}

pub(crate) fn expects_unsigned(what: &str) -> String {
    format!("{what} expects a non-negative integer value with an optional k/m/g suffix")
}

pub(crate) fn not_in_range(arg: &str, what: &str, min: i128, max: i128) -> String {
    // `what` is "option `x'" or "switch `x'"; git prints the option part only.
    format!("value {arg} for {what} not in range [{min},{max}]")
}

pub(crate) fn unknown_subcommand(name: &str) -> String {
    format!("unknown subcommand: `{name}'")
}

pub(crate) const NEED_SUBCOMMAND: &str = "need a subcommand";

pub(crate) fn alias_of(target: &str) -> String {
    format!("alias of --{target}")
}

pub(crate) const NOOP_HELP: &str = "no-op (backward compatibility)";
