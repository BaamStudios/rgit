// SPDX-License-Identifier: MIT
//! Command-line option parsing with the semantics of git's parse-options.
//!
//! Written from git's documentation (`Documentation/technical/api-parse-options.adoc`,
//! `Documentation/gitcli.adoc`) and the observable behaviour of the `git`
//! binary; see CLEANROOM.md.
//!
//! Supported: bundled short options (`-ab`), stuck and separate arguments
//! (`-oArg`, `-o Arg`, `--opt=Arg`, `--opt Arg`), optional arguments (stuck
//! form only), unique-prefix abbreviation of long options, `--no-<opt>`
//! negation (and `--<opt>` for options named `no-<opt>`), `--`,
//! `--end-of-options`, counters, bit options, command modes, callbacks,
//! `-<n>` number options, subcommands, `-h`/`--help-all` usage and
//! `--git-completion-helper`.
//!
//! ```
//! use std::cell::{Cell, RefCell};
//! use rgit_options::{Opt, Parser};
//!
//! let verbose = Cell::new(0);
//! let name = RefCell::new(None);
//! let mut p = Parser::new(
//!     &["git demo [<options>] [--] <path>..."],
//!     vec![
//!         Opt::verbose(&verbose, "be verbose"),
//!         Opt::string('n', "name", &name, "name", "set the name"),
//!     ],
//! );
//! let argv: Vec<String> = ["demo", "-vv", "--na=x", "file"].map(String::from).into();
//! let parsed = p.parse(&argv).unwrap();
//! drop(p);
//! assert_eq!(verbose.get(), 2);
//! assert_eq!(name.borrow().as_deref(), Some("x"));
//! assert_eq!(parsed.args, ["file"]);
//! ```

mod messages;
mod num;
mod opt;
mod usage;

use std::cell::Cell;
use std::collections::HashMap;
use std::io::Write;

pub use opt::{Callback, CallbackError, NumberCallback, Opt};

use opt::{ArgMode, Kind};

/// Exit code for usage errors.
pub const EXIT_USAGE: i32 = 129;
/// Exit code for fatal errors.
pub const EXIT_FATAL: i32 = 128;

/// The parser wants the program to stop: help was requested, or there was
/// an error. Carries the exact output and the exit code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exit {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Exit {
    fn stdout(text: String) -> Self {
        Exit {
            code: 0,
            stdout: text,
            stderr: String::new(),
        }
    }

    fn stderr(code: i32, text: String) -> Self {
        Exit {
            code,
            stdout: String::new(),
            stderr: text,
        }
    }

    /// Write the output to stdout/stderr and return the exit code.
    pub fn emit(&self) -> i32 {
        if !self.stdout.is_empty() {
            let mut o = std::io::stdout().lock();
            let _ = o.write_all(self.stdout.as_bytes());
            let _ = o.flush();
        }
        if !self.stderr.is_empty() {
            let _ = std::io::stderr().lock().write_all(self.stderr.as_bytes());
        }
        self.code
    }
}

/// Flags changing how [`Parser::parse`] works (git's `PARSE_OPT_*`).
#[derive(Debug, Clone, Copy, Default)]
pub struct ParseFlags {
    /// Keep the `--` separator in the remaining arguments.
    pub keep_dashdash: bool,
    /// Stop at the first non-option argument.
    pub stop_at_non_option: bool,
    /// Keep `argv[0]` (the command name) in the remaining arguments.
    pub keep_argv0: bool,
    /// Keep unknown options in the remaining arguments instead of failing.
    pub keep_unknown_opt: bool,
    /// Do not handle `-h`, `--help` and `--help-all`.
    pub no_internal_help: bool,
    /// With subcommands: it is fine if none is given.
    pub subcommand_optional: bool,
}

/// Result of a successful parse.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Parsed {
    /// Remaining (non-option) arguments. When a subcommand was found, it is
    /// the first element (after `argv[0]` with `keep_argv0`), followed by all
    /// arguments after it, unparsed.
    pub args: Vec<String>,
    /// The subcommand found, if any.
    pub subcommand: Option<String>,
}

/// How the user spelled an option, for messages.
#[derive(Clone)]
enum How {
    Short(char),
    /// Canonical long name (with `no-` when negated).
    Long(String),
}

impl How {
    fn what(&self) -> String {
        match self {
            How::Short(c) => messages::opt_ref(Some(*c), ""),
            How::Long(l) => messages::opt_ref(None, l),
        }
    }

    fn desc(&self, value: Option<&str>) -> String {
        match (self, value) {
            (How::Short(c), _) => format!("-{c}"),
            (How::Long(l), Some(v)) => format!("--{l}={v}"),
            (How::Long(l), None) => format!("--{l}"),
        }
    }
}

enum Lookup {
    Found {
        idx: usize,
        unset: bool,
        name: String,
    },
    Unknown,
}

/// The option parser: a usage text plus an option table.
pub struct Parser<'a> {
    usage: Vec<&'a str>,
    opts: Vec<Opt<'a>>,
    flags: ParseFlags,
    prefix: Option<String>,
    disallow_abbrev: bool,
    /// Command-mode bookkeeping: slot address -> (description of the last
    /// option that changed it, whether that option is a command mode).
    cmdmode_seen: HashMap<usize, (String, bool)>,
}

fn git_env_bool(name: &str) -> bool {
    match std::env::var(name) {
        Ok(v) => matches!(v.to_ascii_lowercase().as_str(), "true" | "yes" | "on" | "1"),
        Err(_) => false,
    }
}

impl<'a> Parser<'a> {
    /// Create a parser. `usage` holds the alternative usage strings.
    ///
    /// Abbreviated long options are rejected (as `fatal:`) when
    /// `GIT_TEST_DISALLOW_ABBREVIATED_OPTIONS` is true, like git does.
    pub fn new(usage: &[&'a str], opts: Vec<Opt<'a>>) -> Self {
        Parser {
            usage: usage.to_vec(),
            opts,
            flags: ParseFlags::default(),
            prefix: None,
            disallow_abbrev: git_env_bool("GIT_TEST_DISALLOW_ABBREVIATED_OPTIONS"),
            cmdmode_seen: HashMap::new(),
        }
    }

    /// Set the parse flags.
    pub fn flags(mut self, flags: ParseFlags) -> Self {
        self.flags = flags;
        self
    }

    /// Set the prefix used for [`Opt::filename`] arguments (usually the path
    /// of the current directory relative to the work tree, with a trailing
    /// slash).
    pub fn prefix(mut self, prefix: Option<&str>) -> Self {
        self.prefix = prefix.map(str::to_string);
        self
    }

    /// Override the `GIT_TEST_DISALLOW_ABBREVIATED_OPTIONS` setting.
    pub fn disallow_abbreviations(mut self, disallow: bool) -> Self {
        self.disallow_abbrev = disallow;
        self
    }

    /// The `-h` text (or the `--help-all` text with `all`).
    pub fn usage_text(&self, all: bool) -> String {
        usage::usage_text(&self.usage, &self.opts, all)
    }

    /// Usage error: usage on stderr, exit 129 (`usage_with_options`).
    pub fn usage_error(&self) -> Exit {
        Exit::stderr(EXIT_USAGE, self.usage_text(false))
    }

    /// `error: <msg>` followed by the usage, exit 129.
    pub fn error_with_usage(&self, msg: &str) -> Exit {
        Exit::stderr(
            EXIT_USAGE,
            format!("error: {msg}\n{}", self.usage_text(false)),
        )
    }

    /// `fatal: <msg>`, a blank line and the usage, exit 129
    /// (`usage_msg_opt`).
    pub fn usage_msg(&self, msg: &str) -> Exit {
        Exit::stderr(
            EXIT_USAGE,
            format!("fatal: {msg}\n\n{}", self.usage_text(false)),
        )
    }

    fn error(msg: &str) -> Exit {
        Exit::stderr(EXIT_USAGE, format!("error: {msg}\n"))
    }

    fn bug(msg: &str) -> Exit {
        Exit::stderr(EXIT_FATAL, format!("BUG: {msg}\n"))
    }

    fn check_flags(&self) -> Result<(), Exit> {
        let f = &self.flags;
        if f.keep_unknown_opt && f.stop_at_non_option && !f.subcommand_optional {
            return Err(Self::bug(messages::BUG_STOP_AND_KEEP_UNKNOWN));
        }
        if self.opts.iter().any(Opt::is_subcommand) {
            if f.stop_at_non_option {
                return Err(Self::bug(messages::BUG_SUBCOMMAND_STOP));
            }
            if f.keep_unknown_opt && !f.subcommand_optional {
                return Err(Self::bug(messages::BUG_SUBCOMMAND_KEEP_UNKNOWN));
            }
            if f.keep_dashdash && !f.subcommand_optional {
                return Err(Self::bug(messages::BUG_SUBCOMMAND_KEEP_DASHDASH));
            }
        }
        if self.opts.iter().filter(|o| o.is_number()).count() > 1 {
            return Err(Self::bug(messages::BUG_MULTIPLE_NUMBER_OPTIONS));
        }
        Ok(())
    }

    /// Parse `argv` (`argv[0]` is the command name). Returns the remaining
    /// arguments, or an [`Exit`] for help output and errors.
    pub fn parse(&mut self, argv: &[String]) -> Result<Parsed, Exit> {
        self.check_flags()?;
        self.cmdmode_seen.clear();
        let has_subcommands = self.opts.iter().any(Opt::is_subcommand);
        let mut out = Vec::new();
        if self.flags.keep_argv0 {
            if let Some(a0) = argv.first() {
                out.push(a0.clone());
            }
        }
        let mut subcommand = None;
        let mut kept_unknown = false;
        let mut i = 1;
        while i < argv.len() {
            let arg = argv[i].as_str();
            if !arg.starts_with('-') || arg == "-" {
                let mut chars = arg.chars();
                if let (Some(c), None) = (chars.next(), chars.next()) {
                    if let Some(idx) = self
                        .opts
                        .iter()
                        .position(|o| o.flags.nodash && o.short == Some(c))
                    {
                        self.apply(idx, false, How::Short(c), None)?;
                        i += 1;
                        continue;
                    }
                }
                if has_subcommands {
                    if !kept_unknown {
                        if let Some(o) = self
                            .opts
                            .iter()
                            .find(|o| o.is_subcommand() && o.long == Some(arg))
                        {
                            subcommand = o.long.map(str::to_string);
                            out.extend(argv[i..].iter().cloned());
                            break;
                        }
                    }
                    if self.flags.subcommand_optional {
                        out.extend(argv[i..].iter().cloned());
                        break;
                    }
                    return Err(self.error_with_usage(&messages::unknown_subcommand(arg)));
                }
                if self.flags.stop_at_non_option {
                    out.extend(argv[i..].iter().cloned());
                    break;
                }
                out.push(arg.to_string());
                i += 1;
                continue;
            }
            if arg == "--" {
                if self.flags.keep_dashdash {
                    out.push(arg.to_string());
                }
                out.extend(argv[i + 1..].iter().cloned());
                break;
            }
            if arg == "--end-of-options" {
                out.extend(argv[i + 1..].iter().cloned());
                break;
            }
            if let Some(body) = arg.strip_prefix("--") {
                i = self.parse_long(body, argv, i, &mut out, &mut kept_unknown)?;
            } else {
                i = self.parse_short(&arg[1..], argv, i, &mut out, &mut kept_unknown)?;
            }
        }
        if has_subcommands && subcommand.is_none() && !self.flags.subcommand_optional {
            return Err(self.error_with_usage(messages::NEED_SUBCOMMAND));
        }
        Ok(Parsed {
            args: out,
            subcommand,
        })
    }

    fn resolve_idx(&self, idx: usize) -> usize {
        if let Kind::Alias(t) = self.opts[idx].kind {
            if let Some(j) = self
                .opts
                .iter()
                .position(|o| !o.is_alias() && o.long == Some(t))
            {
                return j;
            }
        }
        idx
    }

    fn can_negate(&self, idx: usize) -> bool {
        !self.opts[idx].flags.noneg && self.opts[self.resolve_idx(idx)].negatable()
    }

    fn lookup_long(&self, name: &str) -> Result<Lookup, Exit> {
        let candidates = || {
            self.opts.iter().enumerate().filter_map(|(i, o)| {
                if o.is_group() || o.is_subcommand() {
                    return None;
                }
                o.long.map(|l| (i, l))
            })
        };
        // Exact matches first.
        for (i, long) in candidates() {
            if long == name {
                return Ok(Lookup::Found {
                    idx: self.resolve_idx(i),
                    unset: false,
                    name: long.to_string(),
                });
            }
        }
        for (i, long) in candidates() {
            if !self.can_negate(i) {
                continue;
            }
            if name.strip_prefix("no-") == Some(long) {
                return Ok(Lookup::Found {
                    idx: self.resolve_idx(i),
                    unset: true,
                    name: name.to_string(),
                });
            }
            if long.strip_prefix("no-") == Some(name) {
                return Ok(Lookup::Found {
                    idx: self.resolve_idx(i),
                    unset: true,
                    name: name.to_string(),
                });
            }
        }
        // Abbreviations.
        let mut found: Vec<(usize, bool, String)> = Vec::new();
        let mut add = |idx: usize, unset: bool, shown: String| {
            if !found.iter().any(|(j, u, _)| *j == idx && *u == unset) {
                found.push((idx, unset, shown));
            }
        };
        let mut last_neg = None;
        for (i, long) in candidates() {
            let target = self.resolve_idx(i);
            if long.starts_with(name) {
                add(target, false, long.to_string());
            }
            if !self.can_negate(i) {
                continue;
            }
            if let Some(rest) = name.strip_prefix("no-") {
                if long.starts_with(rest) && !long.starts_with("no-") {
                    add(target, true, format!("no-{long}"));
                    last_neg = Some(format!("no-{long}"));
                }
            }
            if let Some(pos) = long.strip_prefix("no-") {
                if pos.starts_with(name) {
                    add(target, true, pos.to_string());
                }
            }
        }
        if found.is_empty() {
            return Ok(Lookup::Unknown);
        }
        if self.disallow_abbrev {
            return Err(Exit::stderr(
                EXIT_FATAL,
                format!("fatal: {}\n", messages::disallowed_abbrev(name)),
            ));
        }
        if name == "no-" {
            // git reports the last negatable option twice here.
            if let Some(n) = last_neg {
                return Err(self.error_with_usage(&messages::ambiguous(name, &n, &n)));
            }
        }
        if found.len() > 1 {
            let a = &found[found.len() - 2].2;
            let b = &found[found.len() - 1].2;
            return Err(self.error_with_usage(&messages::ambiguous(name, a, b)));
        }
        let (idx, unset, _) = found.remove(0);
        let canonical = {
            let long = self.opts[idx].long.unwrap_or_default();
            match (unset, long.strip_prefix("no-")) {
                (false, _) => long.to_string(),
                (true, Some(pos)) => pos.to_string(),
                (true, None) => format!("no-{long}"),
            }
        };
        Ok(Lookup::Found {
            idx,
            unset,
            name: canonical,
        })
    }

    fn parse_long(
        &mut self,
        body: &str,
        argv: &[String],
        i: usize,
        out: &mut Vec<String>,
        kept_unknown: &mut bool,
    ) -> Result<usize, Exit> {
        if !self.flags.no_internal_help {
            match body {
                "help" => return Err(Exit::stdout(self.usage_text(false))),
                "help-all" => return Err(Exit::stdout(self.usage_text(true))),
                _ => {}
            }
        }
        match body {
            "git-completion-helper" => {
                return Err(Exit::stdout(usage::completion(&self.opts, false)));
            }
            "git-completion-helper-all" => {
                return Err(Exit::stdout(usage::completion(&self.opts, true)));
            }
            _ => {}
        }
        let (name, value) = match body.split_once('=') {
            Some((n, v)) => (n, Some(v)),
            None => (body, None),
        };
        match self.lookup_long(name)? {
            Lookup::Unknown => {
                if self.flags.keep_unknown_opt {
                    out.push(argv[i].clone());
                    *kept_unknown = true;
                    Ok(i + 1)
                } else {
                    Err(self.error_with_usage(&messages::unknown_option(body)))
                }
            }
            Lookup::Found { idx, unset, name } => {
                let how = How::Long(name);
                if unset || self.opts[idx].arg_mode() == ArgMode::None {
                    if value.is_some() {
                        return Err(Self::error(&messages::takes_no_value(&how.what())));
                    }
                    self.apply(idx, unset, how, None)?;
                    return Ok(i + 1);
                }
                self.take_value(idx, how, value, argv, i + 1)
            }
        }
    }

    /// Handle an option that may take an argument. `stuck` is the argument
    /// given in stuck form; `next` is the index of the next word.
    fn take_value(
        &mut self,
        idx: usize,
        how: How,
        stuck: Option<&str>,
        argv: &[String],
        next: usize,
    ) -> Result<usize, Exit> {
        let opt = &self.opts[idx];
        match opt.arg_mode() {
            ArgMode::None => {
                self.apply(idx, false, how, None)?;
                Ok(next)
            }
            ArgMode::Optional => {
                let v = stuck
                    .map(str::to_string)
                    .or(opt.default_value.map(str::to_string));
                self.apply(idx, false, how, v.as_deref())?;
                Ok(next)
            }
            ArgMode::Required => {
                if let Some(v) = stuck {
                    self.apply(idx, false, how, Some(v))?;
                    Ok(next)
                } else if opt.flags.lastarg_default && next >= argv.len() {
                    let d = opt.default_value.map(str::to_string);
                    self.apply(idx, false, how, d.as_deref())?;
                    Ok(next)
                } else if let Some(v) = argv.get(next) {
                    self.apply(idx, false, how, Some(v))?;
                    Ok(next + 1)
                } else {
                    Err(Self::error(&messages::requires_value(&how.what())))
                }
            }
        }
    }

    fn is_long_prefix(&self, s: &str) -> bool {
        self.opts.iter().enumerate().any(|(i, o)| {
            if o.is_group() || o.is_subcommand() {
                return false;
            }
            let Some(long) = o.long else { return false };
            long.starts_with(s)
                || (self.can_negate(i)
                    && s.strip_prefix("no-").is_some_and(|r| long.starts_with(r)))
        })
    }

    fn parse_short(
        &mut self,
        body: &str,
        argv: &[String],
        i: usize,
        out: &mut Vec<String>,
        kept_unknown: &mut bool,
    ) -> Result<usize, Exit> {
        let short_pos = |opts: &[Opt<'_>], c: char| {
            opts.iter()
                .position(|o| o.short == Some(c) && !o.flags.nodash && !o.is_group())
        };
        let number_opt = self.opts.iter().position(Opt::is_number);
        let first = body.chars().next().unwrap_or_default();
        let starts_number = first.is_ascii_digit()
            && number_opt.is_some()
            && short_pos(&self.opts, first).is_none();
        if !starts_number && body.chars().count() >= 3 && self.is_long_prefix(body) {
            return Err(Self::error(&messages::did_you_mean(body)));
        }
        let mut rest = body;
        while let Some(c) = rest.chars().next() {
            let found = short_pos(&self.opts, c);
            // `-<digits>` for the number option; the bundle may go on after
            // the digits (`-3n` is `-3 -n`, observed with `git grep`).
            if let Some(idx) = number_opt.filter(|_| found.is_none() && c.is_ascii_digit()) {
                let end = rest
                    .find(|ch: char| !ch.is_ascii_digit())
                    .unwrap_or(rest.len());
                let (digits, after) = rest.split_at(end);
                self.apply(idx, false, How::Short(c), Some(digits))?;
                rest = after;
                continue;
            }
            rest = &rest[c.len_utf8()..];
            let Some(idx) = found else {
                if c == 'h' && !self.flags.no_internal_help {
                    return Err(Exit::stdout(self.usage_text(false)));
                }
                if self.flags.keep_unknown_opt {
                    out.push(argv[i].clone());
                    *kept_unknown = true;
                    return Ok(i + 1);
                }
                return Err(self.error_with_usage(&messages::unknown_switch(c)));
            };
            let idx = self.resolve_idx(idx);
            match self.opts[idx].arg_mode() {
                ArgMode::None => self.apply(idx, false, How::Short(c), None)?,
                ArgMode::Optional => {
                    let stuck = (!rest.is_empty()).then_some(rest);
                    return self.take_value(idx, How::Short(c), stuck, argv, i + 1);
                }
                ArgMode::Required => {
                    let stuck = (!rest.is_empty()).then_some(rest);
                    return self.take_value(idx, How::Short(c), stuck, argv, i + 1);
                }
            }
        }
        Ok(i + 1)
    }

    fn number_error(
        &self,
        e: num::NumError,
        how: &How,
        arg: &str,
        unsigned: bool,
        min: i128,
        max: i128,
    ) -> Exit {
        let what = how.what();
        let msg = match e {
            num::NumError::Empty => messages::expects_numerical(&what),
            num::NumError::Invalid if unsigned => messages::expects_unsigned(&what),
            num::NumError::Invalid => messages::expects_integer(&what),
            num::NumError::Range => messages::not_in_range(arg, &what, min, max),
        };
        Self::error(&msg)
    }

    fn callback_error(&self, e: CallbackError) -> Exit {
        match e {
            CallbackError::Error(m) => Self::error(&m),
            CallbackError::Usage(m) => self.error_with_usage(&m),
            CallbackError::Fatal(m) => Exit::stderr(EXIT_FATAL, format!("fatal: {m}\n")),
            CallbackError::Silent => Exit::stderr(EXIT_USAGE, String::new()),
        }
    }

    /// Apply option `idx` (already resolved through aliases).
    fn apply(
        &mut self,
        idx: usize,
        unset: bool,
        how: How,
        value: Option<&str>,
    ) -> Result<(), Exit> {
        let passthru_text = || {
            let mut s = match &how {
                How::Short(c) => format!("-{c}"),
                How::Long(l) => format!("--{l}"),
            };
            if let Some(v) = value {
                if matches!(how, How::Long(_)) {
                    s.push('=');
                }
                s.push_str(v);
            }
            s
        };
        let old = self.opts[idx].int_slot().map(Cell::get);
        let prefix = self.prefix.clone();
        let opt = &mut self.opts[idx];
        let mut cb_err = None;
        let mut num_err = None;
        match &mut opt.kind {
            Kind::Group | Kind::Subcommand | Kind::Alias(_) | Kind::Noop => {}
            Kind::Flag(c) => c.set(!unset),
            Kind::Bool(c) => c.set(i32::from(!unset)),
            Kind::CountUp(c) => c.set(if unset { 0 } else { c.get().max(0) + 1 }),
            Kind::Bit(c, m) => c.set(if unset { c.get() & !*m } else { c.get() | *m }),
            Kind::NegBit(c, m) => c.set(if unset { c.get() | *m } else { c.get() & !*m }),
            Kind::SetInt(c, v) => c.set(if unset { 0 } else { *v }),
            Kind::Integer(c, min, max) => {
                if unset {
                    c.set(0);
                } else {
                    let v = value.unwrap_or_default();
                    match num::parse_scaled(v, *min as i128, *max as i128, true) {
                        Ok(n) => c.set(n as i32),
                        Err(e) => {
                            num_err = Some((e, v.to_string(), false, *min as i128, *max as i128))
                        }
                    }
                }
            }
            Kind::Unsigned(c, max) => {
                if unset {
                    c.set(0);
                } else {
                    let v = value.unwrap_or_default();
                    match num::parse_scaled(v, 0, *max as i128, false) {
                        Ok(n) => c.set(n as u64),
                        Err(e) => num_err = Some((e, v.to_string(), true, 0, *max as i128)),
                    }
                }
            }
            Kind::String(c) => {
                *c.borrow_mut() = if unset {
                    None
                } else {
                    value.map(str::to_string)
                }
            }
            Kind::StringList(c) => {
                if unset {
                    c.borrow_mut().clear();
                } else if let Some(v) = value {
                    c.borrow_mut().push(v.to_string());
                }
            }
            Kind::Filename(c) => {
                *c.borrow_mut() = if unset {
                    None
                } else {
                    value.map(|v| match &prefix {
                        Some(p) if !std::path::Path::new(v).is_absolute() && v != "-" => {
                            format!("{p}{v}")
                        }
                        _ => v.to_string(),
                    })
                }
            }
            Kind::Callback(f, _) => {
                if let Err(e) = f(value, unset) {
                    cb_err = Some(e);
                }
            }
            Kind::Number(f) => {
                if let Err(e) = f(value.unwrap_or_default()) {
                    cb_err = Some(e);
                }
            }
            Kind::Passthru(c) => {
                *c.borrow_mut() = Some(passthru_text());
            }
            Kind::PassthruArgv(c) => c.borrow_mut().push(passthru_text()),
        }
        if let Some((e, arg, unsigned, min, max)) = num_err {
            return Err(self.number_error(e, &how, &arg, unsigned, min, max));
        }
        if let Some(e) = cb_err {
            return Err(self.callback_error(e));
        }
        self.check_cmdmode(idx, &how, value, old)
    }

    /// Command modes: an option that changes an integer shared with
    /// command-mode options conflicts with the option that changed it
    /// before, if either of the two is a command-mode option. Options that
    /// leave the value as it is are ignored.
    fn check_cmdmode(
        &mut self,
        idx: usize,
        how: &How,
        value: Option<&str>,
        old: Option<i32>,
    ) -> Result<(), Exit> {
        let Some(slot) = self.opts[idx].int_slot() else {
            return Ok(());
        };
        let shared = self
            .opts
            .iter()
            .any(|o| o.flags.cmdmode && o.int_slot().is_some_and(|s| std::ptr::eq(s, slot)));
        if !shared || old == Some(slot.get()) {
            return Ok(());
        }
        let addr = slot as *const _ as usize;
        let desc = how.desc(value);
        let is_cmdmode = self.opts[idx].flags.cmdmode;
        if let Some((prev, prev_cmdmode)) = self.cmdmode_seen.get(&addr) {
            if (is_cmdmode || *prev_cmdmode) && *prev != desc {
                return Err(Self::error(&messages::cannot_be_used_together(&desc, prev)));
            }
        }
        self.cmdmode_seen.insert(addr, (desc, is_cmdmode));
        Ok(())
    }
}
