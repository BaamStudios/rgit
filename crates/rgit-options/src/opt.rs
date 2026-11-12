// SPDX-License-Identifier: MIT
//! Option definitions.
//!
//! Options store into caller-owned [`Cell`]s and [`RefCell`]s. Several
//! options may share one cell (for example a boolean, a counter and a bit
//! option on the same integer, or the "command mode" options of a command),
//! exactly as several git options can share one C variable.

use std::cell::{Cell, RefCell};

/// Error returned by option callbacks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallbackError {
    /// Print `error: <msg>` and exit with 129 (no usage).
    Error(String),
    /// Print `error: <msg>` followed by the usage and exit with 129.
    Usage(String),
    /// Print `fatal: <msg>` and exit with 128.
    Fatal(String),
    /// Exit with 129 without printing anything (the callback already did).
    Silent,
}

/// Callback for [`Opt::callback`]: receives the argument (if any) and
/// whether the option was negated (`--no-<name>`).
pub type Callback<'a> = Box<dyn FnMut(Option<&str>, bool) -> Result<(), CallbackError> + 'a>;

/// Callback for [`Opt::number`]: receives the digits of `-<n>`.
pub type NumberCallback<'a> = Box<dyn FnMut(&str) -> Result<(), CallbackError> + 'a>;

pub(crate) enum Kind<'a> {
    Group,
    Flag(&'a Cell<bool>),
    Bool(&'a Cell<i32>),
    CountUp(&'a Cell<i32>),
    Bit(&'a Cell<i32>, i32),
    NegBit(&'a Cell<i32>, i32),
    SetInt(&'a Cell<i32>, i32),
    Integer(&'a Cell<i32>, i64, i64),
    Unsigned(&'a Cell<u64>, u64),
    String(&'a RefCell<Option<String>>),
    StringList(&'a RefCell<Vec<String>>),
    Filename(&'a RefCell<Option<String>>),
    Callback(Callback<'a>, Option<&'a Cell<i32>>),
    Number(NumberCallback<'a>),
    Noop,
    Alias(&'a str),
    Subcommand,
    Passthru(&'a RefCell<Option<String>>),
    PassthruArgv(&'a RefCell<Vec<String>>),
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct OptFlags {
    pub hidden: bool,
    pub noneg: bool,
    pub nocomplete: bool,
    pub cmdmode: bool,
    pub lastarg_default: bool,
    pub literal_argh: bool,
    pub nodash: bool,
    pub optarg: bool,
    pub noarg: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArgMode {
    None,
    Required,
    Optional,
}

/// One entry of an option table (an option, a group header or a subcommand).
pub struct Opt<'a> {
    pub(crate) kind: Kind<'a>,
    pub(crate) short: Option<char>,
    pub(crate) long: Option<&'a str>,
    pub(crate) argh: Option<&'a str>,
    pub(crate) help: &'a str,
    pub(crate) flags: OptFlags,
    pub(crate) default_value: Option<&'a str>,
}

impl<'a> Opt<'a> {
    fn new(
        kind: Kind<'a>,
        short: impl Into<Option<char>>,
        long: impl Into<Option<&'a str>>,
        help: &'a str,
    ) -> Self {
        Opt {
            kind,
            short: short.into(),
            long: long.into(),
            argh: None,
            help,
            flags: OptFlags::default(),
            default_value: None,
        }
    }

    /// Start an option group; `title` is printed as a header in the usage.
    pub fn group(title: &'a str) -> Self {
        Self::new(Kind::Group, None, None, title)
    }

    /// A group header that is only shown by `--help-all`.
    pub fn hidden_group(title: &'a str) -> Self {
        Self::group(title).hidden()
    }

    /// A boolean stored as `bool`: `--opt` sets true, `--no-opt` false.
    pub fn flag(
        short: impl Into<Option<char>>,
        long: impl Into<Option<&'a str>>,
        value: &'a Cell<bool>,
        help: &'a str,
    ) -> Self {
        Self::new(Kind::Flag(value), short, long, help)
    }

    /// `OPT_BOOL`: `--opt` sets 1, `--no-opt` sets 0.
    pub fn bool(
        short: impl Into<Option<char>>,
        long: impl Into<Option<&'a str>>,
        value: &'a Cell<i32>,
        help: &'a str,
    ) -> Self {
        Self::new(Kind::Bool(value), short, long, help)
    }

    /// `OPT_COUNTUP`: every use increments (starting from 0 even when the
    /// value is negative), `--no-opt` resets to 0.
    pub fn countup(
        short: impl Into<Option<char>>,
        long: impl Into<Option<&'a str>>,
        value: &'a Cell<i32>,
        help: &'a str,
    ) -> Self {
        Self::new(Kind::CountUp(value), short, long, help)
    }

    /// `OPT__VERBOSE`: `-v, --verbose`, a counter.
    pub fn verbose(value: &'a Cell<i32>, help: &'a str) -> Self {
        Self::countup('v', "verbose", value, help)
    }

    /// `OPT__QUIET`: `-q, --quiet`, a counter.
    pub fn quiet(value: &'a Cell<i32>, help: &'a str) -> Self {
        Self::countup('q', "quiet", value, help)
    }

    /// `OPT__DRY_RUN`: `-n, --dry-run`.
    pub fn dry_run(value: &'a Cell<i32>, help: &'a str) -> Self {
        Self::bool('n', "dry-run", value, help)
    }

    /// `OPT__FORCE`: `-f, --force`, a counter.
    pub fn force(value: &'a Cell<i32>, help: &'a str) -> Self {
        Self::countup('f', "force", value, help)
    }

    /// `OPT_BIT`: `--opt` ors `mask` in, `--no-opt` clears it.
    pub fn bit(
        short: impl Into<Option<char>>,
        long: impl Into<Option<&'a str>>,
        value: &'a Cell<i32>,
        help: &'a str,
        mask: i32,
    ) -> Self {
        Self::new(Kind::Bit(value, mask), short, long, help)
    }

    /// `OPT_NEGBIT`: `--opt` clears `mask`, `--no-opt` sets it.
    pub fn negbit(
        short: impl Into<Option<char>>,
        long: impl Into<Option<&'a str>>,
        value: &'a Cell<i32>,
        help: &'a str,
        mask: i32,
    ) -> Self {
        Self::new(Kind::NegBit(value, mask), short, long, help)
    }

    /// `OPT_SET_INT`: `--opt` stores `set`, `--no-opt` stores 0.
    pub fn set_int(
        short: impl Into<Option<char>>,
        long: impl Into<Option<&'a str>>,
        value: &'a Cell<i32>,
        help: &'a str,
        set: i32,
    ) -> Self {
        Self::new(Kind::SetInt(value, set), short, long, help)
    }

    /// `OPT_CMDMODE`: like [`Opt::set_int`], but only one of the command-mode
    /// options sharing `value` may be given; it cannot be negated.
    pub fn cmdmode(
        short: impl Into<Option<char>>,
        long: impl Into<Option<&'a str>>,
        value: &'a Cell<i32>,
        help: &'a str,
        set: i32,
    ) -> Self {
        Self::set_int(short, long, value, help, set)
            .noneg()
            .cmdmode_flag()
    }

    /// `OPT_INTEGER`: an `int` argument with optional k/m/g suffix.
    /// The accepted range defaults to that of a 32-bit `int`; see [`Opt::range`].
    pub fn integer(
        short: impl Into<Option<char>>,
        long: impl Into<Option<&'a str>>,
        value: &'a Cell<i32>,
        help: &'a str,
    ) -> Self {
        Self::new(
            Kind::Integer(value, i32::MIN as i64, i32::MAX as i64),
            short,
            long,
            help,
        )
        .argh("n")
    }

    /// `OPT_UNSIGNED`: a non-negative argument with optional k/m/g suffix.
    /// It cannot be negated. See [`Opt::max`] to narrow the range.
    pub fn unsigned(
        short: impl Into<Option<char>>,
        long: impl Into<Option<&'a str>>,
        value: &'a Cell<u64>,
        help: &'a str,
    ) -> Self {
        Self::new(Kind::Unsigned(value, u64::MAX), short, long, help)
            .argh("n")
            .noneg()
    }

    /// `OPT_STRING`: `--opt <argh>` stores the argument, `--no-opt` unsets it.
    pub fn string(
        short: impl Into<Option<char>>,
        long: impl Into<Option<&'a str>>,
        value: &'a RefCell<Option<String>>,
        argh: &'a str,
        help: &'a str,
    ) -> Self {
        Self::new(Kind::String(value), short, long, help).argh(argh)
    }

    /// `OPT_STRING_LIST`: every use appends, `--no-opt` clears the list.
    pub fn string_list(
        short: impl Into<Option<char>>,
        long: impl Into<Option<&'a str>>,
        value: &'a RefCell<Vec<String>>,
        argh: &'a str,
        help: &'a str,
    ) -> Self {
        Self::new(Kind::StringList(value), short, long, help).argh(argh)
    }

    /// `OPT_FILENAME`: like [`Opt::string`], but a relative argument is
    /// prefixed with the parser's prefix (see [`crate::Parser::prefix`]).
    pub fn filename(
        short: impl Into<Option<char>>,
        long: impl Into<Option<&'a str>>,
        value: &'a RefCell<Option<String>>,
        help: &'a str,
    ) -> Self {
        Self::new(Kind::Filename(value), short, long, help).argh("file")
    }

    /// `OPT_CALLBACK`: the argument is handed to `f`. Takes a required
    /// argument unless [`Opt::noarg`] or [`Opt::optional`] is used.
    pub fn callback(
        short: impl Into<Option<char>>,
        long: impl Into<Option<&'a str>>,
        argh: Option<&'a str>,
        help: &'a str,
        f: impl FnMut(Option<&str>, bool) -> Result<(), CallbackError> + 'a,
    ) -> Self {
        let mut o = Self::new(Kind::Callback(Box::new(f), None), short, long, help);
        o.argh = argh;
        o
    }

    /// `OPT_NUMBER_CALLBACK`: recognise `-<digits>`. Short options that are
    /// digits take precedence.
    pub fn number(help: &'a str, f: impl FnMut(&str) -> Result<(), CallbackError> + 'a) -> Self {
        Self::new(Kind::Number(Box::new(f)), None, None, help).noneg()
    }

    /// `OPT_NOOP_NOARG`: accepted and ignored; hidden from `-h`.
    pub fn noop(short: impl Into<Option<char>>, long: impl Into<Option<&'a str>>) -> Self {
        Self::new(Kind::Noop, short, long, crate::messages::NOOP_HELP).hidden()
    }

    /// `OPT_ALIAS`: another name for the option with long name `target`.
    /// Aliases do not make abbreviations ambiguous.
    pub fn alias(short: impl Into<Option<char>>, long: &'a str, target: &'a str) -> Self {
        Self::new(Kind::Alias(target), short, long, "")
    }

    /// `OPT_SUBCOMMAND`: a subcommand name (no dashes, no abbreviation).
    pub fn subcommand(name: &'a str) -> Self {
        Self::new(Kind::Subcommand, None, name, "").noneg()
    }

    /// `OPT_PASSTHRU`: store the option as typed (e.g. `--opt=value`).
    pub fn passthru(
        short: impl Into<Option<char>>,
        long: impl Into<Option<&'a str>>,
        value: &'a RefCell<Option<String>>,
        argh: Option<&'a str>,
        help: &'a str,
    ) -> Self {
        let mut o = Self::new(Kind::Passthru(value), short, long, help);
        o.argh = argh;
        o
    }

    /// `OPT_PASSTHRU_ARGV`: append every use, as typed, to a list.
    pub fn passthru_argv(
        short: impl Into<Option<char>>,
        long: impl Into<Option<&'a str>>,
        value: &'a RefCell<Vec<String>>,
        argh: Option<&'a str>,
        help: &'a str,
    ) -> Self {
        let mut o = Self::new(Kind::PassthruArgv(value), short, long, help);
        o.argh = argh;
        o
    }

    // ----- modifiers ---------------------------------------------------

    /// Set the argument name shown in the usage (`<argh>`).
    pub fn argh(mut self, argh: &'a str) -> Self {
        self.argh = Some(argh);
        self
    }

    /// Show the argument help verbatim instead of `<argh>`.
    pub fn literal_argh(mut self) -> Self {
        self.flags.literal_argh = true;
        self
    }

    /// Hide from `-h` (still shown by `--help-all`).
    pub fn hidden(mut self) -> Self {
        self.flags.hidden = true;
        self
    }

    /// Disable the `--no-` form.
    pub fn noneg(mut self) -> Self {
        self.flags.noneg = true;
        self
    }

    /// Do not offer this option in `--git-completion-helper`.
    pub fn nocomplete(mut self) -> Self {
        self.flags.nocomplete = true;
        self
    }

    /// The option takes no argument.
    pub fn noarg(mut self) -> Self {
        self.flags.noarg = true;
        self.flags.optarg = false;
        self
    }

    /// The argument is optional and only accepted in stuck form
    /// (`--opt=<v>`, `-o<v>`); without it, `default` is used.
    pub fn optional(mut self, default: Option<&'a str>) -> Self {
        self.flags.optarg = true;
        self.flags.noarg = false;
        self.default_value = default;
        self
    }

    /// Without an argument, `default` is used, but only when the option is
    /// the last word on the command line.
    pub fn lastarg_default(mut self, default: &'a str) -> Self {
        self.flags.lastarg_default = true;
        self.default_value = Some(default);
        self
    }

    /// Option is a single character used without a dash (like `+`).
    pub fn nodash(mut self) -> Self {
        self.flags.nodash = true;
        self
    }

    /// Mark as a command-mode option. `slot` is required for callbacks: it
    /// is the shared integer the conflict check looks at.
    pub fn cmdmode_slot(mut self, slot: &'a Cell<i32>) -> Self {
        if let Kind::Callback(_, s) = &mut self.kind {
            *s = Some(slot);
        }
        self.cmdmode_flag()
    }

    /// Name the integer a callback writes, so that it takes part in the
    /// conflict check of command-mode options sharing `slot` (without
    /// being a command-mode option itself).
    pub fn value_slot(mut self, slot: &'a Cell<i32>) -> Self {
        if let Kind::Callback(_, s) = &mut self.kind {
            *s = Some(slot);
        }
        self
    }

    fn cmdmode_flag(mut self) -> Self {
        self.flags.cmdmode = true;
        self
    }

    /// Narrow the accepted range of an [`Opt::integer`] option.
    pub fn range(mut self, min: i64, max: i64) -> Self {
        if let Kind::Integer(v, _, _) = self.kind {
            self.kind = Kind::Integer(v, min, max);
        }
        self
    }

    /// Narrow the accepted maximum of an [`Opt::unsigned`] option.
    pub fn max(mut self, max: u64) -> Self {
        if let Kind::Unsigned(v, _) = self.kind {
            self.kind = Kind::Unsigned(v, max);
        }
        self
    }

    // ----- queries -----------------------------------------------------

    pub(crate) fn is_group(&self) -> bool {
        matches!(self.kind, Kind::Group)
    }

    pub(crate) fn is_subcommand(&self) -> bool {
        matches!(self.kind, Kind::Subcommand)
    }

    pub(crate) fn is_alias(&self) -> bool {
        matches!(self.kind, Kind::Alias(_))
    }

    pub(crate) fn is_number(&self) -> bool {
        matches!(self.kind, Kind::Number(_))
    }

    /// Whether the option itself (not counting the `no-` handling of its
    /// name) can be negated.
    pub(crate) fn negatable(&self) -> bool {
        !self.flags.noneg
            && !matches!(
                self.kind,
                Kind::Group | Kind::Subcommand | Kind::Number(_) | Kind::Unsigned(..)
            )
    }

    pub(crate) fn arg_mode(&self) -> ArgMode {
        let takes_arg = match self.kind {
            Kind::Integer(..)
            | Kind::Unsigned(..)
            | Kind::String(_)
            | Kind::StringList(_)
            | Kind::Filename(_) => true,
            Kind::Callback(..) | Kind::Passthru(_) | Kind::PassthruArgv(_) => !self.flags.noarg,
            _ => false,
        };
        if !takes_arg {
            ArgMode::None
        } else if self.flags.optarg {
            ArgMode::Optional
        } else {
            ArgMode::Required
        }
    }

    /// The integer cell this option modifies, used for the command-mode
    /// conflict check.
    pub(crate) fn int_slot(&self) -> Option<&'a Cell<i32>> {
        match self.kind {
            Kind::Bool(c)
            | Kind::CountUp(c)
            | Kind::Bit(c, _)
            | Kind::NegBit(c, _)
            | Kind::SetInt(c, _)
            | Kind::Integer(c, _, _) => Some(c),
            Kind::Callback(_, s) => s,
            _ => None,
        }
    }
}
