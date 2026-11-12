// SPDX-License-Identifier: MIT
//! `git config` (Documentation/git-config.adoc): the subcommands `list`,
//! `get`, `set`, `unset`, `rename-section`, `remove-section` and `edit`,
//! and the legacy option syntax (`git config <name> [<value>]`, `--get`,
//! `--add`, `--unset`, `-l`, ...).
//!
//! Reading uses `rgit_config`'s loader, writing its in-place editor
//! (`rgit_config::edit`) through `config.lock`.
//!
//! Not implemented yet (they need other subsystems): `--blob` (object
//! database and revision syntax, #14/#32), `--type=color`, `--get-color`,
//! `--get-colorbool` (#75) and `--type=expiry-date` on reading (#74).

use std::cell::{Cell, RefCell};
use std::io::Write;
use std::path::{Path, PathBuf};

use bstr::{BStr, BString, ByteSlice};
use rgit_config::edit::{EditError, SetRequest, ValuePattern};
use rgit_config::sources::{self, RepoConfig, ScopeFilter};
use rgit_config::value::{self, BoolOrInt, IntWidth};
use rgit_config::{
    CommandLine, ConfigSet, Entry, Environment, Level, LoadOptions, Loader, Origin, Scope,
    SourceSpec, StaticRepoFacts, SystemEnv, WriteError, write,
};
use rgit_fs::{LockOptions, Timeout};

use crate::prelude::*;

/// User-visible messages of this command (CLEANROOM.md: verbatim where
/// scripts depend on them).
mod msg {
    pub const USAGE: &[&str] = &[
        "git config list [<file-option>] [<display-option>] [--includes]",
        "git config get [<file-option>] [<display-option>] [--includes] [--all] [--regexp] [--value=<pattern>] [--fixed-value] [--default=<default>] [--url=<url>] <name>",
        "git config set [<file-option>] [--type=<type>] [--all] [--value=<pattern>] [--fixed-value] <name> <value>",
        "git config unset [<file-option>] [--all] [--value=<pattern>] [--fixed-value] <name>",
        "git config rename-section [<file-option>] <old-name> <new-name>",
        "git config remove-section [<file-option>] <name>",
        "git config edit [<file-option>]",
        "git config [<file-option>] --get-colorbool <name> [<stdout-is-tty>]",
    ];
    pub const USAGE_LIST: &str =
        "git config list [<file-option>] [<display-option>] [--includes]";
    pub const USAGE_GET: &str = "git config get [<file-option>] [<display-option>] [--includes] [--all] [--regexp=<regexp>] [--value=<pattern>] [--fixed-value] [--default=<default>] <name>";
    pub const USAGE_SET: &str = "git config set [<file-option>] [--type=<type>] [--comment=<message>] [--all] [--value=<pattern>] [--fixed-value] <name> <value>";
    pub const USAGE_UNSET: &str =
        "git config unset [<file-option>] [--all] [--value=<pattern>] [--fixed-value] <name>";
    pub const USAGE_RENAME: &str = "git config rename-section [<file-option>] <old-name> <new-name>";
    pub const USAGE_REMOVE: &str = "git config remove-section [<file-option>] <name>";
    pub const USAGE_EDIT: &str = "git config edit [<file-option>]";

    pub const NO_ACTION: &str = "no action specified";
    pub const ONE_FILE: &str = "only one config file at a time";
    pub const ONE_TYPE: &str = "only one type at a time";
    pub const NOT_IN_GIT_DIR: &str = "not in a git directory";
    pub const HOME_NOT_SET: &str = "$HOME not set";
    pub const WRITE_STDIN: &str = "writing to stdin is not supported";
    pub const EDIT_STDIN: &str = "editing stdin is not supported";
    pub const NAME_ONLY: &str = "--name-only is only applicable to --list or --get-regexp";
    pub const SHOW_ORIGIN: &str =
        "--show-origin is only applicable to --get, --get-all, --get-regexp, and --list";
    pub const DEFAULT_ONLY_GET: &str = "--default is only applicable to --get";
    pub const COMMENT_ONLY_WRITE: &str =
        "--comment is only applicable to add/set/replace operations";
    pub const FIXED_VALUE_LEGACY: &str = "--fixed-value only applies with 'value-pattern'";
    pub const FIXED_VALUE_NEW: &str = "--fixed-value only applies with --value=<pattern>";
    pub const APPEND_VALUE: &str = "--append cannot be used with --value=<pattern>";
    pub const DEFAULT_ALL: &str = "--default= cannot be used with --all or --url=";
    pub const WORKTREE_MULTIPLE: &str = "--worktree cannot be used with multiple working trees unless the config\nextension worktreeConfig is enabled. Please read \"CONFIGURATION FILE\"\nsection in \"git help worktree\" for details";
    pub const GLOBAL_TEMPLATE_HEAD: &str = "# This is Git's per-user configuration file.\n[user]\n# Please adapt and uncomment the following lines:\n";

    pub fn wrong_args(min: usize, max: usize) -> String {
        if min == max {
            format!("wrong number of arguments, should be {min}")
        } else {
            format!("wrong number of arguments, should be from {min} to {max}")
        }
    }
    pub fn unrecognized_type(t: &str) -> String {
        format!("unrecognized --type argument, {t}")
    }
    pub fn inside_repo_only(opt: &str) -> String {
        format!("{opt} can only be used inside a git repository")
    }
    pub fn invalid_key_pattern(p: &str) -> String {
        format!("invalid key pattern: {p}")
    }
    pub fn unable_to_read(path: &str, err: &str) -> String {
        format!("unable to read config file '{path}': {err}")
    }
    pub fn unable_to_access(path: &str, err: &str) -> String {
        format!("unable to access '{path}': {err}")
    }
    pub fn missing_value(key: &str, valid: bool) -> String {
        if valid {
            format!("missing value to set to the variable '{key}'")
        } else {
            format!("missing value to set to a variable with an invalid name '{key}'")
        }
    }
    pub fn did_you_mean(key: &str, value: &str) -> String {
        format!("did you mean \"git config set {key} {value}\"?")
    }
    pub fn editor_problem(editor: &str) -> String {
        format!("there was a problem with the editor '{editor}'")
    }
    pub fn not_implemented(what: &str) -> String {
        format!("rgit: {what} is not implemented yet")
    }
}

/// `--type` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Type {
    Bool,
    Int,
    BoolOrInt,
    BoolOrStr,
    Path,
    ExpiryDate,
    Color,
}

impl Type {
    fn parse(s: &str) -> Option<Type> {
        Some(match s {
            "bool" => Type::Bool,
            "int" => Type::Int,
            "bool-or-int" => Type::BoolOrInt,
            "bool-or-str" => Type::BoolOrStr,
            "path" => Type::Path,
            "expiry-date" => Type::ExpiryDate,
            "color" => Type::Color,
            _ => return None,
        })
    }
}

/// Legacy action modes (`OPT_CMDMODE` values).
mod action {
    pub const GET: i32 = 1;
    pub const GET_ALL: i32 = 2;
    pub const GET_REGEXP: i32 = 3;
    pub const GET_URLMATCH: i32 = 4;
    pub const REPLACE_ALL: i32 = 5;
    pub const ADD: i32 = 6;
    pub const UNSET: i32 = 7;
    pub const UNSET_ALL: i32 = 8;
    pub const RENAME_SECTION: i32 = 9;
    pub const REMOVE_SECTION: i32 = 10;
    pub const LIST: i32 = 11;
    pub const EDIT: i32 = 12;
    pub const GET_COLOR: i32 = 13;
    pub const GET_COLORBOOL: i32 = 14;
    /// Implicit: `git config <name> <value>`.
    pub const SET: i32 = 15;
    /// Implicit: `git config <name> <value> <value-pattern>`.
    pub const SET_ALL: i32 = 16;
}

/// All option values; a subcommand registers only the options it accepts.
struct Opts {
    global: Cell<bool>,
    system: Cell<bool>,
    local: Cell<bool>,
    worktree: Cell<bool>,
    file: RefCell<Option<String>>,
    blob: RefCell<Option<String>>,
    null: Cell<bool>,
    name_only: Cell<bool>,
    show_origin: Cell<bool>,
    show_scope: Cell<bool>,
    show_names: Cell<i32>,
    ty: Cell<Option<Type>>,
    includes: Cell<i32>,
    all: Cell<bool>,
    regexp: Cell<bool>,
    value: RefCell<Option<String>>,
    fixed_value: Cell<bool>,
    url: RefCell<Option<String>>,
    default: RefCell<Option<String>>,
    comment: RefCell<Option<String>>,
    append: Cell<bool>,
    action: Cell<i32>,
}

impl Opts {
    fn new() -> Self {
        Opts {
            global: Cell::new(false),
            system: Cell::new(false),
            local: Cell::new(false),
            worktree: Cell::new(false),
            file: RefCell::new(None),
            blob: RefCell::new(None),
            null: Cell::new(false),
            name_only: Cell::new(false),
            show_origin: Cell::new(false),
            show_scope: Cell::new(false),
            show_names: Cell::new(-1),
            ty: Cell::new(None),
            includes: Cell::new(-1),
            all: Cell::new(false),
            regexp: Cell::new(false),
            value: RefCell::new(None),
            fixed_value: Cell::new(false),
            url: RefCell::new(None),
            default: RefCell::new(None),
            comment: RefCell::new(None),
            append: Cell::new(false),
            action: Cell::new(0),
        }
    }

    fn file_opts(&self) -> Vec<Opt<'_>> {
        vec![
            Opt::group("Config file location"),
            Opt::flag(None, "global", &self.global, "use global config file"),
            Opt::flag(None, "system", &self.system, "use system config file"),
            Opt::flag(None, "local", &self.local, "use repository config file"),
            Opt::flag(None, "worktree", &self.worktree, "use per-worktree config file"),
            Opt::string('f', "file", &self.file, "file", "use given config file"),
            Opt::string(
                None,
                "blob",
                &self.blob,
                "blob-id",
                "read config from given blob object",
            ),
        ]
    }

    fn display_opts(&self) -> Vec<Opt<'_>> {
        vec![
            Opt::group("Display options"),
            Opt::flag('z', "null", &self.null, "terminate values with NUL byte"),
            Opt::flag(None, "name-only", &self.name_only, "show variable names only"),
            Opt::flag(
                None,
                "show-origin",
                &self.show_origin,
                "show origin of config (file, standard input, blob, command line)",
            ),
            Opt::flag(
                None,
                "show-scope",
                &self.show_scope,
                "show scope of config (worktree, local, global, system, command)",
            ),
            Opt::bool(
                None,
                "show-names",
                &self.show_names,
                "show config keys in addition to their values",
            ),
        ]
    }

    fn type_opts(&self) -> Vec<Opt<'_>> {
        let ty = &self.ty;
        let set = move |t: Option<Type>| -> Result<(), CallbackError> {
            match (ty.get(), t) {
                (Some(old), Some(new)) if old != new => {
                    Err(CallbackError::Error(msg::ONE_TYPE.into()))
                }
                _ => {
                    ty.set(t);
                    Ok(())
                }
            }
        };
        let fixed = |name: &'static str, t: Type, help: &'static str| {
            Opt::callback(None, name, None, help, move |_, _| set(Some(t)))
                .noarg()
                .noneg()
        };
        vec![
            Opt::group("Type"),
            Opt::callback('t', "type", Some("type"), "value is given this type", move |arg, neg| {
                if neg {
                    ty.set(None);
                    return Ok(());
                }
                let arg = arg.unwrap_or_default();
                match Type::parse(arg) {
                    Some(t) => set(Some(t)),
                    None => Err(CallbackError::Fatal(msg::unrecognized_type(arg))),
                }
            }),
            fixed("bool", Type::Bool, "value is \"true\" or \"false\""),
            fixed("int", Type::Int, "value is decimal number"),
            fixed("bool-or-int", Type::BoolOrInt, "value is --bool or --int"),
            fixed("bool-or-str", Type::BoolOrStr, "value is --bool or string"),
            fixed("path", Type::Path, "value is a path (file or directory name)"),
            fixed("expiry-date", Type::ExpiryDate, "value is an expiry date"),
        ]
    }

    fn includes_opt(&self) -> Opt<'_> {
        Opt::bool(
            None,
            "includes",
            &self.includes,
            "respect include directives on lookup",
        )
    }

    fn fixed_value_opt(&self) -> Opt<'_> {
        Opt::flag(
            None,
            "fixed-value",
            &self.fixed_value,
            "use string equality when comparing values to value pattern",
        )
    }

    fn comment_opt(&self) -> Opt<'_> {
        Opt::string(
            None,
            "comment",
            &self.comment,
            "value",
            "human-readable comment string (# will be prepended as needed)",
        )
    }

    fn legacy_opts(&self) -> Vec<Opt<'_>> {
        let a = &self.action;
        let mut v = self.file_opts();
        v.extend([
            Opt::group("Action"),
            Opt::cmdmode(None, "get", a, "get value: name [<value-pattern>]", action::GET),
            Opt::cmdmode(None, "get-all", a, "get all values: key [<value-pattern>]", action::GET_ALL),
            Opt::cmdmode(
                None,
                "get-regexp",
                a,
                "get values for regexp: name-regex [<value-pattern>]",
                action::GET_REGEXP,
            ),
            Opt::cmdmode(
                None,
                "get-urlmatch",
                a,
                "get value specific for the URL: section[.var] URL",
                action::GET_URLMATCH,
            ),
            Opt::cmdmode(
                None,
                "replace-all",
                a,
                "replace all matching variables: name value [<value-pattern>]",
                action::REPLACE_ALL,
            ),
            Opt::cmdmode(None, "add", a, "add a new variable: name value", action::ADD),
            Opt::cmdmode(None, "unset", a, "remove a variable: name [<value-pattern>]", action::UNSET),
            Opt::cmdmode(
                None,
                "unset-all",
                a,
                "remove all matches: name [<value-pattern>]",
                action::UNSET_ALL,
            ),
            Opt::cmdmode(
                None,
                "rename-section",
                a,
                "rename section: old-name new-name",
                action::RENAME_SECTION,
            ),
            Opt::cmdmode(None, "remove-section", a, "remove a section: name", action::REMOVE_SECTION),
            Opt::cmdmode('l', "list", a, "list all", action::LIST),
            Opt::cmdmode('e', "edit", a, "open an editor", action::EDIT),
            Opt::cmdmode(
                None,
                "get-color",
                a,
                "find the color configured: slot [<default>]",
                action::GET_COLOR,
            ),
            Opt::cmdmode(
                None,
                "get-colorbool",
                a,
                "find the color setting: slot [<stdout-is-tty>]",
                action::GET_COLORBOOL,
            ),
        ]);
        v.extend(self.display_opts());
        v.extend(self.type_opts());
        v.extend([
            Opt::group("Other"),
            Opt::string(
                None,
                "default",
                &self.default,
                "value",
                "with --get, use default value when missing entry",
            ),
            self.comment_opt(),
            self.fixed_value_opt(),
            self.includes_opt(),
        ]);
        v
    }
}

fn wrong_args(n: usize, min: usize, max: usize) -> Result<(), Die> {
    if n < min || n > max {
        return Err(Die::Error(msg::wrong_args(min, max), 129));
    }
    Ok(())
}

fn print_report(lines: &[(Level, String)]) {
    let mut err = std::io::stderr().lock();
    for (level, m) in lines {
        let _ = writeln!(err, "{} {m}", level.prefix());
    }
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let sub = argv.get(1).map(String::as_str);
    match sub {
        Some("-h") if argv.len() == 2 => {
            let mut out = String::new();
            for (i, line) in msg::USAGE.iter().enumerate() {
                out.push_str(if i == 0 { "usage: " } else { "   or: " });
                out.push_str(line);
                out.push('\n');
            }
            out.push('\n');
            print!("{out}");
            Ok(0)
        }
        Some(
            s @ ("list" | "get" | "set" | "unset" | "rename-section" | "remove-section" | "edit"),
        ) => {
            let s = s.to_string();
            run_subcommand(ctx, &s, &argv[1..])
        }
        _ => run_legacy(ctx, argv),
    }
}

fn run_subcommand(ctx: &mut Context, sub: &str, argv: &[String]) -> CmdResult {
    let o = Opts::new();
    let (usage, opts): (&str, Vec<Opt<'_>>) = match sub {
        "list" => {
            let mut v = o.file_opts();
            v.extend(o.display_opts());
            v.extend(o.type_opts());
            v.extend([Opt::group("Other"), o.includes_opt()]);
            (msg::USAGE_LIST, v)
        }
        "get" => {
            let mut v = o.file_opts();
            v.extend([
                Opt::group("Filter options"),
                Opt::flag(
                    None,
                    "all",
                    &o.all,
                    "return all values for multi-valued config options",
                ),
                Opt::flag(
                    None,
                    "regexp",
                    &o.regexp,
                    "interpret the name as a regular expression",
                ),
                Opt::string(
                    None,
                    "value",
                    &o.value,
                    "pattern",
                    "show config with values matching the pattern",
                ),
                o.fixed_value_opt(),
                Opt::string(None, "url", &o.url, "URL", "show config matching the given URL"),
            ]);
            v.extend(o.display_opts());
            v.extend(o.type_opts());
            v.extend([
                Opt::group("Other"),
                o.includes_opt(),
                Opt::string(
                    None,
                    "default",
                    &o.default,
                    "value",
                    "use default value when missing entry",
                ),
            ]);
            (msg::USAGE_GET, v)
        }
        "set" => {
            let mut v = o.file_opts();
            v.extend(o.type_opts());
            v.extend([
                Opt::group("Filter"),
                Opt::flag(
                    None,
                    "all",
                    &o.all,
                    "replace multi-valued config option with new value",
                ),
                Opt::string(
                    None,
                    "value",
                    &o.value,
                    "pattern",
                    "show config with values matching the pattern",
                ),
                o.fixed_value_opt(),
                Opt::group("Other"),
                o.comment_opt(),
                Opt::flag(
                    None,
                    "append",
                    &o.append,
                    "add a new line without altering any existing values",
                ),
            ]);
            (msg::USAGE_SET, v)
        }
        "unset" => {
            let mut v = o.file_opts();
            v.extend([
                Opt::group("Filter"),
                Opt::flag(None, "all", &o.all, "unset all multi-valued config options"),
                Opt::string(
                    None,
                    "value",
                    &o.value,
                    "pattern",
                    "unset multi-valued config options with matching values",
                ),
                o.fixed_value_opt(),
            ]);
            (msg::USAGE_UNSET, v)
        }
        "rename-section" => (msg::USAGE_RENAME, o.file_opts()),
        "remove-section" => (msg::USAGE_REMOVE, o.file_opts()),
        _ => (msg::USAGE_EDIT, o.file_opts()),
    };
    let args = Parser::new(&[usage], opts)
        .flags(stop_at_non_option())
        .parse(argv)?
        .args;
    let mut cmd = Cmd::new(ctx, &o)?;
    match sub {
        "list" => {
            wrong_args(args.len(), 0, 0)?;
            cmd.list()
        }
        "get" => {
            wrong_args(args.len(), 1, 1)?;
            if o.default.borrow().is_some() && (o.all.get() || o.url.borrow().is_some()) {
                return Err(Die::Fatal(msg::DEFAULT_ALL.into()));
            }
            let pattern = o.value.borrow().clone();
            if o.fixed_value.get() && pattern.is_none() {
                return Err(Die::Fatal(msg::FIXED_VALUE_LEGACY.into()));
            }
            if let Some(url) = o.url.borrow().clone() {
                return cmd.get_url(&args[0], &url);
            }
            let show_names = o.show_names.get() == 1;
            cmd.get(&args[0], pattern.as_deref(), o.all.get(), o.regexp.get(), show_names)
        }
        "set" => {
            if args.len() == 1 {
                return Err(missing_value(&args[0]));
            }
            wrong_args(args.len(), 2, 2)?;
            let pattern = o.value.borrow().clone();
            if o.fixed_value.get() && pattern.is_none() {
                return Err(Die::Fatal(msg::FIXED_VALUE_NEW.into()));
            }
            if o.append.get() && pattern.is_some() {
                return Err(Die::Fatal(msg::APPEND_VALUE.into()));
            }
            let pattern = if o.append.get() {
                Pat::Never
            } else {
                Pat::from(pattern.as_deref())
            };
            let all = o.all.get() && !o.append.get();
            cmd.set(&args[0], Some(&args[1]), pattern, all, true)
        }
        "unset" => {
            wrong_args(args.len(), 1, 1)?;
            let pattern = o.value.borrow().clone();
            if o.fixed_value.get() && pattern.is_none() {
                return Err(Die::Fatal(msg::FIXED_VALUE_NEW.into()));
            }
            cmd.set(&args[0], None, Pat::from(pattern.as_deref()), o.all.get(), true)
        }
        "rename-section" => {
            wrong_args(args.len(), 2, 2)?;
            cmd.rename_section(&args[0], Some(&args[1]))
        }
        "remove-section" => {
            wrong_args(args.len(), 1, 1)?;
            cmd.rename_section(&args[0], None)
        }
        _ => {
            wrong_args(args.len(), 0, 0)?;
            cmd.edit()
        }
    }
}

fn stop_at_non_option() -> ParseFlags {
    ParseFlags {
        stop_at_non_option: true,
        ..Default::default()
    }
}

/// `set` with only a name; `name=value` gets a hint when `name` is valid.
fn missing_value(arg: &str) -> Die {
    let split = arg.split_once('=');
    let key = split.map_or(arg, |(k, _)| k);
    let valid = rgit_config::Key::parse(key).is_ok();
    eprintln!("error: {}", msg::missing_value(arg, valid));
    if let (true, Some((k, v))) = (valid, split) {
        eprintln!("hint: {}", msg::did_you_mean(k, v));
    }
    Die::Exit(129)
}

fn run_legacy(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let o = Opts::new();
    let args = Parser::new(msg::USAGE, o.legacy_opts())
        .flags(stop_at_non_option())
        .parse(argv)?
        .args;
    let mut act = o.action.get();
    if act == 0 {
        act = match args.len() {
            1 => action::GET,
            2 => action::SET,
            3 => action::SET_ALL,
            _ => return Err(Die::Error(msg::NO_ACTION.into(), 129)),
        };
    }
    let mut cmd = Cmd::new(ctx, &o)?;
    use action::*;
    if o.name_only.get() && act != LIST && act != GET_REGEXP {
        return Err(Die::Error(msg::NAME_ONLY.into(), 129));
    }
    if (o.show_origin.get() || o.show_scope.get())
        && !matches!(act, GET | GET_ALL | GET_REGEXP | GET_URLMATCH | LIST)
    {
        return Err(Die::Error(msg::SHOW_ORIGIN.into(), 129));
    }
    if o.default.borrow().is_some() && act != GET {
        return Err(Die::Error(msg::DEFAULT_ONLY_GET.into(), 129));
    }
    if o.comment.borrow().is_some() && !matches!(act, ADD | SET | SET_ALL | REPLACE_ALL) {
        return Err(Die::Error(msg::COMMENT_ONLY_WRITE.into(), 129));
    }
    if o.fixed_value.get() {
        let has_pattern = match act {
            SET_ALL => true,
            GET | GET_ALL | GET_REGEXP | UNSET | UNSET_ALL => args.len() == 2,
            REPLACE_ALL => args.len() == 3,
            _ => false,
        };
        if !has_pattern {
            return Err(Die::Error(msg::FIXED_VALUE_LEGACY.into(), 129));
        }
    }
    let show_names = |default: bool| match o.show_names.get() {
        -1 => default,
        v => v == 1,
    };
    let n = args.len();
    match act {
        GET if o.action.get() == 0 => {
            // `git config name=value`: probably meant to set.
            let a = &args[0];
            if rgit_config::Key::parse(a).is_err()
                && a.split_once('=')
                    .is_some_and(|(k, _)| rgit_config::Key::parse(k).is_ok())
            {
                return Err(missing_value(a));
            }
            cmd.get(a, None, false, false, show_names(false))
        }
        GET | GET_ALL | GET_REGEXP => {
            wrong_args(n, 1, 2)?;
            let regexp = act == GET_REGEXP;
            cmd.get(
                &args[0],
                args.get(1).map(String::as_str),
                act != GET,
                regexp,
                show_names(regexp),
            )
        }
        SET => {
            wrong_args(n, 2, 2)?;
            cmd.set(&args[0], Some(&args[1]), Pat::Any, false, false)
        }
        SET_ALL => {
            wrong_args(n, 3, 3)?;
            cmd.set(&args[0], Some(&args[1]), Pat::from(Some(&args[2])), false, false)
        }
        ADD => {
            wrong_args(n, 2, 2)?;
            cmd.set(&args[0], Some(&args[1]), Pat::Never, false, false)
        }
        REPLACE_ALL => {
            wrong_args(n, 2, 3)?;
            let p = Pat::from(args.get(2).map(String::as_str));
            cmd.set(&args[0], Some(&args[1]), p, true, false)
        }
        UNSET | UNSET_ALL => {
            wrong_args(n, 1, 2)?;
            let p = Pat::from(args.get(1).map(String::as_str));
            cmd.set(&args[0], None, p, act == UNSET_ALL, false)
        }
        RENAME_SECTION => {
            wrong_args(n, 2, 2)?;
            cmd.rename_section(&args[0], Some(&args[1]))
        }
        REMOVE_SECTION => {
            wrong_args(n, 1, 1)?;
            cmd.rename_section(&args[0], None)
        }
        LIST => {
            wrong_args(n, 0, 0)?;
            cmd.list()
        }
        EDIT => {
            wrong_args(n, 0, 0)?;
            cmd.edit()
        }
        GET_URLMATCH => {
            wrong_args(n, 2, 2)?;
            cmd.get_url(&args[0], &args[1])
        }
        _ => Err(Die::Fatal(msg::not_implemented("--get-color/--get-colorbool"))),
    }
}

/// A value pattern before compilation.
enum Pat {
    Any,
    Never,
    Given(String),
}

impl Pat {
    fn from(p: Option<&str>) -> Pat {
        match p {
            Some(p) => Pat::Given(p.to_string()),
            None => Pat::Any,
        }
    }
}

/// Which file(s) the command works on.
#[derive(Debug, Clone)]
enum Location {
    /// All scopes for reading, the repository file for writing.
    Default,
    Global,
    System,
    Local,
    Worktree,
    File(String),
}

/// A repository found from the current directory.
#[derive(Debug, Clone)]
struct Repo {
    git_dir: PathBuf,
    common_dir: PathBuf,
    worktree: Option<PathBuf>,
}

impl Repo {
    fn config_path(&self) -> PathBuf {
        join(&self.common_dir, "config")
    }
}

/// `dir/name`, without a leading `./` for the current directory.
fn join(dir: &Path, name: &str) -> PathBuf {
    if dir == Path::new(".") || dir.as_os_str().is_empty() {
        PathBuf::from(name)
    } else {
        dir.join(name)
    }
}

fn is_git_dir(p: &Path) -> bool {
    p.join("HEAD").is_file()
        && (p.join("commondir").is_file() || (p.join("objects").is_dir() && p.join("refs").is_dir()))
}

/// Read a `.git` file (`gitdir: <path>`).
fn read_gitfile(file: &Path) -> Option<PathBuf> {
    let data = std::fs::read_to_string(file).ok()?;
    let target = data.lines().next()?.strip_prefix("gitdir: ")?.trim_end();
    let p = PathBuf::from(target);
    Some(if p.is_absolute() {
        p
    } else {
        file.parent()?.join(p)
    })
}

fn common_dir(git_dir: &Path) -> PathBuf {
    match std::fs::read_to_string(git_dir.join("commondir")) {
        Ok(s) => {
            let p = PathBuf::from(s.trim_end_matches('\n'));
            if p.is_absolute() { p } else { git_dir.join(p) }
        }
        Err(_) => git_dir.to_path_buf(),
    }
}

/// Minimal repository discovery (`GIT_DIR`, `--git-dir`, `.git` upwards
/// with `GIT_CEILING_DIRECTORIES`, bare repositories). Moves to the top of
/// the working tree like git does and returns the prefix of the original
/// directory. Full discovery is #26.
fn discover(ctx: &Context) -> (Option<Repo>, String) {
    let explicit = ctx
        .git_dir
        .clone()
        .or_else(|| std::env::var("GIT_DIR").ok().filter(|s| !s.is_empty()));
    if let Some(d) = explicit {
        let git_dir = PathBuf::from(d);
        let common = common_dir(&git_dir);
        let worktree = std::env::current_dir().ok();
        return (
            Some(Repo {
                git_dir,
                common_dir: common,
                worktree,
            }),
            String::new(),
        );
    }
    let Ok(cwd) = std::env::current_dir() else {
        return (None, String::new());
    };
    let ceilings: Vec<PathBuf> = std::env::var_os("GIT_CEILING_DIRECTORIES")
        .map(|v| {
            std::env::split_paths(&v)
                .filter(|p| p.is_absolute())
                .collect()
        })
        .unwrap_or_default();
    let mut dir = cwd.clone();
    loop {
        let dotgit = dir.join(".git");
        let found = if dotgit.is_dir() && is_git_dir(&dotgit) {
            Some((PathBuf::from(".git"), Some(dir.clone())))
        } else if dotgit.is_file() {
            read_gitfile(&dotgit).map(|g| (g, Some(dir.clone())))
        } else if is_git_dir(&dir) {
            let g = if dir == cwd {
                PathBuf::from(".")
            } else {
                dir.clone()
            };
            Some((g, None))
        } else {
            None
        };
        if let Some((git_dir, worktree)) = found {
            let mut prefix = String::new();
            if let Some(top) = &worktree {
                if *top != cwd {
                    if let Ok(rest) = cwd.strip_prefix(top) {
                        prefix = format!("{}/", rest.display());
                    }
                    let _ = std::env::set_current_dir(top);
                }
            }
            let common = common_dir(&git_dir);
            return (
                Some(Repo {
                    git_dir,
                    common_dir: common,
                    worktree,
                }),
                prefix,
            );
        }
        match dir.parent() {
            Some(parent) if !ceilings.iter().any(|c| c == parent) => dir = parent.to_path_buf(),
            _ => return (None, String::new()),
        }
    }
}

/// The current branch from `HEAD`, for `includeIf.onbranch`.
fn current_branch(git_dir: &Path) -> Option<BString> {
    let head = std::fs::read(git_dir.join("HEAD")).ok()?;
    let r = head.strip_prefix(b"ref: refs/heads/")?;
    Some(r.trim_ascii_end().into())
}

fn io_text(e: &std::io::Error) -> String {
    let s = e.to_string();
    match s.find(" (os error") {
        Some(i) => s[..i].to_string(),
        None => s,
    }
}

/// State shared by all actions.
struct Cmd<'o> {
    o: &'o Opts,
    location: Location,
    repo: Option<Repo>,
    env: SystemEnv,
    command_line: CommandLine,
}

impl<'o> Cmd<'o> {
    fn new(ctx: &Context, o: &'o Opts) -> Result<Self, Die> {
        let mut location = Location::Default;
        let mut count = 0;
        for (flag, loc) in [
            (o.global.get(), Location::Global),
            (o.system.get(), Location::System),
            (o.local.get(), Location::Local),
            (o.worktree.get(), Location::Worktree),
        ] {
            if flag {
                count += 1;
                location = loc;
            }
        }
        if let Some(f) = o.file.borrow().clone() {
            count += 1;
            location = Location::File(f);
        }
        if count == 0 && o.blob.borrow().is_none() {
            if let Some(f) = std::env::var("GIT_CONFIG").ok().filter(|f| !f.is_empty()) {
                location = Location::File(f);
            }
        }
        if o.blob.borrow().is_some() {
            count += 1;
        }
        if count > 1 {
            return Err(Die::Error(msg::ONE_FILE.into(), 129));
        }
        if o.blob.borrow().is_some() {
            return Err(Die::Fatal(msg::not_implemented("--blob")));
        }
        let (repo, prefix) = discover(ctx);
        if let Location::File(f) = &mut location {
            if f != "-" && !Path::new(f.as_str()).is_absolute() && !prefix.is_empty() {
                *f = format!("{prefix}{f}");
            }
        }
        if matches!(o.ty.get(), Some(Type::Color)) {
            return Err(Die::Fatal(msg::not_implemented("--type=color")));
        }
        let mut command_line = CommandLine::default();
        for (k, v) in &ctx.config_parameters {
            command_line.parameters.push(rgit_config::cmdline::Parameter {
                key: k.as_str().into(),
                value: v.as_deref().map(BString::from),
            });
        }
        Ok(Cmd {
            o,
            location,
            repo,
            env: SystemEnv::default(),
            command_line,
        })
    }

    fn require_repo(&self, opt: &str) -> Result<&Repo, Die> {
        self.repo
            .as_ref()
            .ok_or_else(|| Die::Fatal(msg::inside_repo_only(opt)))
    }

    fn worktree_config_enabled(&self, repo: &Repo) -> bool {
        let local = repo.config_path();
        std::fs::read(&local)
            .ok()
            .and_then(|d| rgit_config::parse_bytes(&d, Origin::File(local), Scope::Local).ok())
            .and_then(|s| s.bool("extensions.worktreeConfig").ok().flatten())
            .unwrap_or(false)
    }

    fn home(&self) -> Result<PathBuf, Die> {
        match self.env.var("HOME") {
            Some(h) if !h.is_empty() => Ok(PathBuf::from(h)),
            _ => Err(Die::Fatal(msg::HOME_NOT_SET.into())),
        }
    }

    /// The file to read with an explicit location: (paths, scope).
    fn read_files(&self) -> Result<Option<(Vec<PathBuf>, Scope)>, Die> {
        Ok(Some(match &self.location {
            Location::Default => return Ok(None),
            Location::File(f) => (vec![PathBuf::from(f)], Scope::Command),
            Location::Global => {
                if self.env.var("GIT_CONFIG_GLOBAL").is_none() {
                    self.home()?;
                }
                (sources::global_config_paths(&self.env), Scope::Global)
            }
            Location::System => (
                vec![
                    self.env
                        .var("GIT_CONFIG_SYSTEM")
                        .map(PathBuf::from)
                        .unwrap_or_else(|| PathBuf::from(sources::DEFAULT_SYSTEM_CONFIG)),
                ],
                Scope::System,
            ),
            Location::Local => {
                let repo = self.require_repo("--local")?;
                (vec![repo.config_path()], Scope::Local)
            }
            Location::Worktree => {
                let repo = self.require_repo("--worktree")?;
                if self.worktree_config_enabled(repo) {
                    (vec![join(&repo.git_dir, "config.worktree")], Scope::Worktree)
                } else {
                    (vec![repo.config_path()], Scope::Local)
                }
            }
        }))
    }

    /// The file to write.
    fn write_file(&self) -> Result<PathBuf, Die> {
        Ok(match &self.location {
            Location::Default | Location::Local => match &self.repo {
                Some(r) => r.config_path(),
                None if matches!(self.location, Location::Local) => {
                    return Err(Die::Fatal(msg::inside_repo_only("--local")));
                }
                None => return Err(Die::Fatal(msg::NOT_IN_GIT_DIR.into())),
            },
            Location::File(f) if f == "-" => {
                return Err(Die::Fatal(if self.repo.is_some() {
                    msg::WRITE_STDIN.into()
                } else {
                    msg::NOT_IN_GIT_DIR.into()
                }));
            }
            Location::File(f) => PathBuf::from(f),
            Location::System => self
                .env
                .var("GIT_CONFIG_SYSTEM")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(sources::DEFAULT_SYSTEM_CONFIG)),
            Location::Global => {
                if let Some(g) = self.env.var("GIT_CONFIG_GLOBAL") {
                    return Ok(PathBuf::from(g));
                }
                let home = self.home()?;
                let user = home.join(".gitconfig");
                let paths = sources::global_config_paths(&self.env);
                match paths.first() {
                    Some(xdg) if *xdg != user && xdg.exists() && !user.exists() => xdg.clone(),
                    _ => user,
                }
            }
            Location::Worktree => {
                let repo = self.require_repo("--worktree")?;
                if self.worktree_config_enabled(repo) {
                    join(&repo.git_dir, "config.worktree")
                } else if has_linked_worktrees(repo) {
                    return Err(Die::Fatal(msg::WORKTREE_MULTIPLE.into()));
                } else {
                    repo.config_path()
                }
            }
        })
    }

    fn includes(&self) -> bool {
        match self.o.includes.get() {
            -1 => matches!(self.location, Location::Default),
            v => v == 1,
        }
    }

    fn repo_facts(&self) -> Option<StaticRepoFacts> {
        let repo = self.repo.as_ref()?;
        let abs = |p: &Path| std::path::absolute(p).ok();
        Some(StaticRepoFacts {
            git_dir: abs(&repo.git_dir),
            worktree: repo.worktree.clone(),
            branch: current_branch(&repo.git_dir),
        })
    }

    /// Load the configuration to read. `strict`: a missing or unreadable
    /// explicit file is fatal (`list`).
    fn load(&self, strict: bool) -> Result<ConfigSet, Die> {
        let facts = self.repo_facts();
        let opts = LoadOptions {
            env: &self.env,
            repo: facts.as_ref().map(|f| f as &dyn rgit_config::RepoFacts),
            includes: self.includes(),
        };
        let mut set = ConfigSet::new();
        let result = match self.read_files()? {
            None => {
                let repo_cfg = self.repo.as_ref().map(|r| RepoConfig {
                    common_dir: r.common_dir.clone(),
                    git_dir: r.git_dir.clone(),
                    worktree_config: None,
                });
                sources::load_standard(
                    opts,
                    repo_cfg.as_ref(),
                    &self.command_line,
                    ScopeFilter::default(),
                    &mut set,
                )
            }
            Some(_) if matches!(&self.location, Location::File(f) if f == "-") => {
                let mut data = Vec::new();
                let _ = std::io::Read::read_to_end(&mut std::io::stdin(), &mut data);
                let mut loader = Loader::new(opts);
                loader.add(SourceSpec::Bytes {
                    data,
                    origin: Origin::Stdin,
                    scope: Scope::Command,
                });
                loader.load_into(&mut set)
            }
            Some((paths, scope)) => {
                if strict && !paths.iter().any(|p| p.is_file()) {
                    let p = paths.last().cloned().unwrap_or_default();
                    let shown = p.display().to_string();
                    let err = match std::fs::read(&p) {
                        Err(e) => e,
                        Ok(_) => std::io::Error::from(std::io::ErrorKind::IsADirectory),
                    };
                    if err.kind() == std::io::ErrorKind::IsADirectory {
                        eprintln!("warning: {}", msg::unable_to_access(&shown, &io_text(&err)));
                    }
                    return Err(Die::Fatal(msg::unable_to_read(&shown, &io_text(&err))));
                }
                let mut loader = Loader::new(opts);
                for path in paths {
                    loader.add(SourceSpec::File {
                        path,
                        scope,
                        required: false,
                    });
                }
                loader.load_into(&mut set)
            }
        };
        match result {
            Ok(()) => Ok(set),
            Err(e) => {
                // Like git, print what was read before the error.
                if strict {
                    self.print_entries(set.entries());
                }
                print_report(&e.report());
                Err(Die::Exit(128))
            }
        }
    }

    /// Format a value for output according to `--type`. `Ok(None)`: skip
    /// (a missing `:(optional)` path).
    fn format(&self, e: &Entry) -> Result<Option<BString>, rgit_config::ValueError> {
        Ok(Some(match self.o.ty.get() {
            None => e.value.clone().unwrap_or_default(),
            Some(Type::Bool) => value::format_bool(e.bool()?).into(),
            Some(Type::Int) => e.int(IntWidth::I64)?.to_string().into(),
            Some(Type::BoolOrInt) => match e.bool_or_int()? {
                BoolOrInt::Bool(b) => value::format_bool(b).into(),
                BoolOrInt::Int(i) => i.to_string().into(),
            },
            Some(Type::BoolOrStr) => {
                match value::parse_bool(e.value.as_deref().map(|v| v.as_bytes())) {
                    Some(b) => value::format_bool(b).into(),
                    None => e.value.clone().unwrap_or_default(),
                }
            }
            Some(Type::Path) => match e.path(&self.env)? {
                Some(p) => path_bytes(&p),
                None => return Ok(None),
            },
            Some(Type::ExpiryDate) | Some(Type::Color) => e.value.clone().unwrap_or_default(),
        }))
    }

    fn prefix_columns(&self, out: &mut Vec<u8>, e: &Entry) {
        let z = self.o.null.get();
        let sep = if z { 0 } else { b'\t' };
        if self.o.show_scope.get() {
            out.extend_from_slice(e.source.scope.as_str().as_bytes());
            out.push(sep);
        }
        if self.o.show_origin.get() {
            out.extend_from_slice(&e.source.origin.show_origin(z));
            out.push(sep);
        }
    }

    fn print_entries(&self, entries: &[Entry]) {
        let z = self.o.null.get();
        let mut out = Vec::new();
        for e in entries {
            let v = match self.format(e) {
                Ok(Some(v)) => v,
                _ if self.o.ty.get().is_some() => continue,
                _ => e.value.clone().unwrap_or_default(),
            };
            self.prefix_columns(&mut out, e);
            out.extend_from_slice(&e.name);
            if !self.o.name_only.get() && (e.value.is_some() || self.o.ty.get().is_some()) {
                out.push(if z { b'\n' } else { b'=' });
                out.extend_from_slice(&v);
            }
            out.push(if z { 0 } else { b'\n' });
        }
        let _ = std::io::stdout().lock().write_all(&out);
    }

    fn list(&mut self) -> CmdResult {
        if matches!(self.o.ty.get(), Some(Type::ExpiryDate)) {
            return Err(Die::Fatal(msg::not_implemented("--type=expiry-date")));
        }
        let set = self.load(true)?;
        self.print_entries(set.entries());
        Ok(0)
    }

    fn get(
        &mut self,
        name: &str,
        pattern: Option<&str>,
        all: bool,
        regexp: bool,
        show_names: bool,
    ) -> CmdResult {
        if matches!(self.o.ty.get(), Some(Type::ExpiryDate)) {
            return Err(Die::Fatal(msg::not_implemented("--type=expiry-date")));
        }
        let name_match: Box<dyn Fn(&BStr) -> bool> = if regexp {
            let re = rgit_config::edit::compile_ere(name.as_bytes())
                .ok_or_else(|| Die::Error(msg::invalid_key_pattern(name), 6))?;
            Box::new(move |n: &BStr| re.is_match(n))
        } else {
            let key = match rgit_config::key::canonicalize(name) {
                Ok(k) => k,
                Err(e) => return Err(Die::Error(e.to_string(), 1)),
            };
            Box::new(move |n: &BStr| n == key.as_bstr())
        };
        let pattern = self.compile(Pat::from(pattern))?;
        let set = self.load(false)?;
        let mut found: Vec<(&Entry, BString)> = Vec::new();
        for e in set.entries() {
            if !name_match(e.name.as_bstr())
                || !pattern.matches(Some(e.value.as_deref().map_or(&b""[..], |v| v.as_bytes())))
            {
                continue;
            }
            match self.format(e) {
                Ok(Some(v)) => found.push((e, v)),
                Ok(None) => {}
                Err(err) => return Err(value_error(e, err)),
            }
        }
        let z = self.o.null.get();
        let mut out = Vec::new();
        if found.is_empty() {
            let Some(default) = self.o.default.borrow().clone() else {
                return Ok(1);
            };
            let v = self.format_default(name, &default)?;
            let sep = if z { 0 } else { b'\t' };
            if self.o.show_scope.get() {
                out.extend_from_slice(b"command");
                out.push(sep);
            }
            if self.o.show_origin.get() {
                out.extend_from_slice(&Origin::CommandLine.show_origin(z));
                out.push(sep);
            }
            if show_names {
                out.extend_from_slice(name.as_bytes());
                out.push(if z { b'\n' } else { b' ' });
            }
            out.extend_from_slice(&v);
            out.push(if z { 0 } else { b'\n' });
            let _ = std::io::stdout().lock().write_all(&out);
            return Ok(0);
        }
        let shown: &[(&Entry, BString)] = if all {
            &found
        } else {
            &found[found.len() - 1..]
        };
        for (e, v) in shown {
            self.prefix_columns(&mut out, e);
            if show_names || self.o.name_only.get() {
                out.extend_from_slice(&e.name);
                if !self.o.name_only.get() && (e.value.is_some() || self.o.ty.get().is_some()) {
                    out.push(if z { b'\n' } else { b' ' });
                    out.extend_from_slice(v);
                }
            } else {
                out.extend_from_slice(v);
            }
            out.push(if z { 0 } else { b'\n' });
        }
        let _ = std::io::stdout().lock().write_all(&out);
        Ok(0)
    }

    /// `get --url=<url> <name>` / `--get-urlmatch <name> <url>`.
    fn get_url(&mut self, name: &str, url: &str) -> CmdResult {
        let Some(url) = rgit_config::urlmatch::Url::parse(url) else {
            return Ok(1);
        };
        let (section, key) = match name.split_once('.') {
            Some((s, rest)) => (s, Some(rest.rsplit('.').next().unwrap_or(rest))),
            None => (name, None),
        };
        let section = BString::from(section.to_ascii_lowercase());
        let key = key.map(|k| BString::from(k.to_ascii_lowercase()));
        // git shows names exactly when a whole section is listed, whatever
        // --show-names says.
        let show_names = key.is_none();
        let set = self.load(false)?;
        let found = rgit_config::urlmatch::best_entries(
            set.entries(),
            section.as_bstr(),
            key.as_ref().map(|k| k.as_bstr()),
            &url,
        );
        if found.is_empty() {
            return Ok(1);
        }
        let z = self.o.null.get();
        let mut out = Vec::new();
        for (full, e) in found {
            let v = match self.format(e) {
                Ok(Some(v)) => v,
                Ok(None) => continue,
                Err(err) => return Err(value_error(e, err)),
            };
            self.prefix_columns(&mut out, e);
            if show_names {
                out.extend_from_slice(&full);
                if e.value.is_some() || self.o.ty.get().is_some() {
                    out.push(if z { b'\n' } else { b' ' });
                    out.extend_from_slice(&v);
                }
            } else {
                out.extend_from_slice(&v);
            }
            out.push(if z { 0 } else { b'\n' });
        }
        let _ = std::io::stdout().lock().write_all(&out);
        Ok(0)
    }

    /// `--default` run through `--type`.
    fn format_default(&self, name: &str, v: &str) -> Result<BString, Die> {
        let bad_int = |e: rgit_config::IntError| {
            Die::Fatal(format!(
                "bad numeric config value '{v}' for '{name}': {}",
                e.reason()
            ))
        };
        let bad_bool = || Die::Fatal(format!("bad boolean config value '{v}' for '{name}'"));
        Ok(match self.o.ty.get() {
            Some(Type::Bool) => value::parse_bool(Some(v.as_bytes()))
                .map(|b| value::format_bool(b).into())
                .ok_or_else(bad_bool)?,
            Some(Type::Int) => value::parse_int(v.as_bytes(), IntWidth::I64)
                .map_err(bad_int)?
                .to_string()
                .into(),
            Some(Type::BoolOrInt) => match value::parse_bool_or_int(Some(v.as_bytes())) {
                Ok(BoolOrInt::Bool(b)) => value::format_bool(b).into(),
                Ok(BoolOrInt::Int(i)) => i.to_string().into(),
                Err(e) => return Err(bad_int(e)),
            },
            Some(Type::BoolOrStr) => match value::parse_bool(Some(v.as_bytes())) {
                Some(b) => value::format_bool(b).into(),
                None => v.into(),
            },
            Some(Type::Path) => match value::interpolate_path(v.as_bytes(), &self.env) {
                Ok(Some(p)) => path_bytes(&p),
                Ok(None) => BString::default(),
                Err(value::PathError::UserDir(p)) => {
                    return Err(Die::Fatal(format!("failed to expand user dir in: '{p}'")));
                }
            },
            _ => v.into(),
        })
    }

    fn compile(&self, p: Pat) -> Result<ValuePattern, Die> {
        Ok(match p {
            Pat::Any => ValuePattern::Any,
            Pat::Never => ValuePattern::Never,
            Pat::Given(p) if self.o.fixed_value.get() => ValuePattern::exact(p.as_bytes()),
            Pat::Given(p) => ValuePattern::regex(p.as_bytes())
                .map_err(|e| Die::Error(e.to_string(), e.exit_code()))?,
        })
    }

    /// Canonicalize a value for `--type` before writing.
    fn normalize_value(&self, key: &str, v: &str) -> Result<String, Die> {
        Ok(match self.o.ty.get() {
            Some(Type::Bool) => match value::parse_bool(Some(v.as_bytes())) {
                Some(b) => value::format_bool(b).to_string(),
                None => {
                    return Err(Die::Fatal(format!(
                        "bad boolean config value '{v}' for '{key}'"
                    )));
                }
            },
            Some(Type::Int) => match value::parse_int(v.as_bytes(), IntWidth::I64) {
                Ok(i) => i.to_string(),
                Err(e) => {
                    return Err(Die::Fatal(format!(
                        "bad numeric config value '{v}' for '{key}': {}",
                        e.reason()
                    )));
                }
            },
            Some(Type::BoolOrInt) => match value::parse_bool_or_int(Some(v.as_bytes())) {
                Ok(BoolOrInt::Bool(b)) => value::format_bool(b).to_string(),
                Ok(BoolOrInt::Int(i)) => i.to_string(),
                Err(e) => {
                    return Err(Die::Fatal(format!(
                        "bad numeric config value '{v}' for '{key}': {}",
                        e.reason()
                    )));
                }
            },
            Some(Type::BoolOrStr) => match value::parse_bool(Some(v.as_bytes())) {
                Some(b) => value::format_bool(b).to_string(),
                None => v.to_string(),
            },
            _ => v.to_string(),
        })
    }

    fn lock_options(&self) -> LockOptions {
        // core.configLockTimeout from the configuration on disk.
        let timeout = self
            .load_quiet()
            .and_then(|s| s.int("core.configLockTimeout").ok().flatten())
            .map(Timeout::from_millis)
            .unwrap_or(Timeout::CONFIG_DEFAULT);
        LockOptions::new().timeout(timeout)
    }

    fn load_quiet(&self) -> Option<ConfigSet> {
        let facts = self.repo_facts();
        let opts = LoadOptions {
            env: &self.env,
            repo: facts.as_ref().map(|f| f as &dyn rgit_config::RepoFacts),
            includes: true,
        };
        let repo_cfg = self.repo.as_ref().map(|r| RepoConfig {
            common_dir: r.common_dir.clone(),
            git_dir: r.git_dir.clone(),
            worktree_config: None,
        });
        let mut set = ConfigSet::new();
        sources::load_standard(
            opts,
            repo_cfg.as_ref(),
            &self.command_line,
            ScopeFilter::default(),
            &mut set,
        )
        .ok()?;
        Some(set)
    }

    fn set(
        &mut self,
        key: &str,
        value: Option<&str>,
        pattern: Pat,
        all: bool,
        new_syntax: bool,
    ) -> CmdResult {
        let has_pattern = matches!(pattern, Pat::Given(_));
        let path = self.write_file()?;
        let pattern = self.compile(pattern)?;
        let value = match value {
            Some(v) => {
                if let Err(e) = rgit_config::Key::parse(key) {
                    let code = EditError::from(e.clone()).exit_code();
                    return Err(Die::Error(e.to_string(), code));
                }
                Some(self.normalize_value(key, v)?)
            }
            None => None,
        };
        let comment = self.o.comment.borrow().clone();
        let req = SetRequest {
            key: key.as_bytes(),
            value: value.as_deref().map(str::as_bytes),
            pattern: &pattern,
            all,
            comment: comment.as_deref().map(str::as_bytes),
        };
        match write::set_in_file(&path, &self.lock_options(), &req) {
            Ok(()) => Ok(0),
            Err(e) => {
                let mut lines = e.report();
                if let (Some(EditError::MultipleValues { key }), Some(_), false) =
                    (e.edit_error(), &value, has_pattern)
                {
                    lines.push((
                        Level::Error,
                        cannot_overwrite(key.as_bstr(), new_syntax),
                    ));
                }
                Err(report_write_error(&e, lines))
            }
        }
    }

    fn rename_section(&mut self, old: &str, new: Option<&str>) -> CmdResult {
        let path = self.write_file()?;
        match write::rename_section_in_file(
            &path,
            &self.lock_options(),
            old.as_bytes(),
            new.map(str::as_bytes),
        ) {
            Ok(()) => Ok(0),
            Err(e) => {
                let lines = e.report();
                Err(report_write_error(&e, lines))
            }
        }
    }

    fn edit(&mut self) -> CmdResult {
        if matches!(&self.location, Location::File(f) if f == "-") {
            return Err(Die::Fatal(if self.repo.is_some() {
                msg::EDIT_STDIN.into()
            } else {
                msg::NOT_IN_GIT_DIR.into()
            }));
        }
        let path = self.write_file()?;
        if matches!(self.location, Location::Global) && !path.exists() {
            let template = format!(
                "{}#\tname = {}\n#\temail = {}\n",
                msg::GLOBAL_TEMPLATE_HEAD,
                default_name(),
                default_email(&self.env)
            );
            if let Err(e) = std::fs::write(&path, template) {
                return Err(Die::Error(io_text(&e), 255));
            }
        }
        let editor = self
            .env
            .var("GIT_EDITOR")
            .map(|s| s.to_string_lossy().into_owned())
            .or_else(|| {
                self.load_quiet()
                    .and_then(|s| s.string("core.editor").ok().flatten().map(|v| v.to_string()))
            })
            .or_else(|| {
                let dumb = self
                    .env
                    .var("TERM")
                    .is_none_or(|t| t.is_empty() || t == "dumb");
                if dumb {
                    None
                } else {
                    self.env.var("VISUAL").map(|s| s.to_string_lossy().into_owned())
                }
            })
            .or_else(|| self.env.var("EDITOR").map(|s| s.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "vi".into());
        if editor == ":" {
            return Ok(0);
        }
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("{editor} \"$@\""))
            .arg(&editor)
            .arg(&path)
            .status();
        match status {
            Ok(s) if s.success() => Ok(0),
            _ => Err(Die::Error(msg::editor_problem(&editor), 255)),
        }
    }
}

/// Report a value that does not fit `--type`. A value-less key where a
/// path is needed is reported with its line, like git does.
fn value_error(e: &Entry, err: rgit_config::ValueError) -> Die {
    if err.kind == rgit_config::ValueErrorKind::Missing {
        eprintln!("error: {err}");
        return Die::Fatal(format!(
            "bad config line {} in {}",
            e.line,
            e.source.origin.describe()
        ));
    }
    Die::Fatal(err.to_string())
}

fn cannot_overwrite(key: &BStr, new_syntax: bool) -> String {
    let hint = if new_syntax {
        "Use --value=<pattern>, --append or --all"
    } else {
        "Use a regexp, --add or --replace-all"
    };
    format!("cannot overwrite multiple values with a single value\n       {hint} to change {key}.")
}

fn report_write_error(e: &WriteError, lines: Vec<(Level, String)>) -> Die {
    print_report(&lines);
    let code = if lines.last().is_some_and(|(l, _)| *l == Level::Fatal) {
        128
    } else {
        e.exit_code()
    };
    Die::Exit(code)
}

/// Whether `worktrees/` holds linked working trees.
fn has_linked_worktrees(repo: &Repo) -> bool {
    std::fs::read_dir(repo.common_dir.join("worktrees"))
        .map(|mut d| d.next().is_some())
        .unwrap_or(false)
}

fn path_bytes(p: &Path) -> BString {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        BString::from(p.as_os_str().as_bytes())
    }
    #[cfg(not(unix))]
    {
        BString::from(p.to_string_lossy().as_bytes())
    }
}

/// The user's full name from the password database (GECOS), for the
/// global config template. Identity handling proper is #122.
fn default_name() -> String {
    let user = std::env::var("USER").unwrap_or_default();
    let passwd = std::fs::read_to_string("/etc/passwd").unwrap_or_default();
    passwd
        .lines()
        .map(|l| l.split(':').collect::<Vec<_>>())
        .find(|f| f.len() >= 5 && f[0] == user)
        .map(|f| f[4].split(',').next().unwrap_or("").to_string())
        .unwrap_or_default()
}

/// `user@host` with `.(none)` for a host without a domain, as git guesses
/// it when nothing is configured.
fn default_email(env: &SystemEnv) -> String {
    if let Some(e) = env.var("EMAIL") {
        return e.to_string_lossy().into_owned();
    }
    let user = env
        .var("USER")
        .map(|u| u.to_string_lossy().into_owned())
        .unwrap_or_default();
    let host = std::fs::read_to_string("/proc/sys/kernel/hostname")
        .or_else(|_| std::fs::read_to_string("/etc/hostname"))
        .unwrap_or_default()
        .trim()
        .to_string();
    if host.contains('.') {
        format!("{user}@{host}")
    } else {
        format!("{user}@{host}.(none)")
    }
}
