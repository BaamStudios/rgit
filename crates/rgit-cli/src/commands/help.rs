// SPDX-License-Identifier: MIT
//! `git help` (Documentation/git-help.adoc): command and guide listings,
//! alias descriptions, and the manual in a man viewer, info or a browser.
//!
//! rgit ships no manual of its own (git's is GPL-2.0, CLEANROOM.md). The
//! viewers find the pages of an installed git; when they do not, rgit points
//! to the online documentation at git-scm.com.

use std::io::Write;
use std::path::Path;
use std::process::Command;

use crate::alias::{self, EarlyConfig};
use crate::cmdlist::{self, Category, Group};
use crate::messages::help as msg;
use crate::prelude::*;

const MODE_ALL: i32 = 1;
const MODE_GUIDES: i32 = 2;
const MODE_CONFIG: i32 = 3;
const MODE_USER: i32 = 4;
const MODE_DEVELOPER: i32 = 5;
const MODE_CONFIG_VARS: i32 = 6;
const MODE_CONFIG_SECTIONS: i32 = 7;

const FORMAT_MAN: i32 = 1;
const FORMAT_INFO: i32 = 2;
const FORMAT_WEB: i32 = 3;

const ONLINE_DOCS: &str = "https://git-scm.com/docs/";

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let mode = Cell::new(0);
    let format = Cell::new(0);
    let external = Cell::new(-1);
    let aliases = Cell::new(-1);
    let verbose = Cell::new(1);
    let exclude_guides = Cell::new(false);
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::cmdmode('a', "all", &mode, msg::HELP_ALL, MODE_ALL),
            Opt::bool(None, "external-commands", &external, msg::HELP_EXTERNAL),
            Opt::bool(None, "aliases", &aliases, msg::HELP_ALIASES),
            Opt::set_int('m', "man", &format, msg::HELP_MAN, FORMAT_MAN),
            Opt::set_int('w', "web", &format, msg::HELP_WEB, FORMAT_WEB),
            Opt::set_int('i', "info", &format, msg::HELP_INFO, FORMAT_INFO),
            Opt::bool('v', "verbose", &verbose, msg::HELP_VERBOSE),
            Opt::cmdmode('g', "guides", &mode, msg::HELP_GUIDES, MODE_GUIDES),
            Opt::cmdmode(None, "user-interfaces", &mode, msg::HELP_USER_INTERFACES, MODE_USER),
            Opt::cmdmode(
                None,
                "developer-interfaces",
                &mode,
                msg::HELP_DEVELOPER_INTERFACES,
                MODE_DEVELOPER,
            ),
            Opt::cmdmode('c', "config", &mode, msg::HELP_CONFIG, MODE_CONFIG),
            Opt::cmdmode(None, "config-for-completion", &mode, "", MODE_CONFIG_VARS).hidden(),
            Opt::cmdmode(
                None,
                "config-sections-for-completion",
                &mode,
                "",
                MODE_CONFIG_SECTIONS,
            )
            .hidden(),
            Opt::flag(None, "exclude-guides", &exclude_guides, "").hidden(),
        ],
    );
    let args = parser.parse(argv)?.args;
    let mode_name = match mode.get() {
        MODE_ALL => Some("--all"),
        MODE_GUIDES => Some("--guides"),
        MODE_CONFIG => Some("--config"),
        MODE_USER => Some("--user-interfaces"),
        MODE_DEVELOPER => Some("--developer-interfaces"),
        MODE_CONFIG_VARS => Some("--config-for-completion"),
        MODE_CONFIG_SECTIONS => Some("--config-sections-for-completion"),
        _ => None,
    };
    if let Some(m) = mode_name {
        let fmt = match format.get() {
            FORMAT_MAN => Some("--man"),
            FORMAT_INFO => Some("--info"),
            FORMAT_WEB => Some("--web"),
            _ => None,
        };
        if let Some(f) = fmt {
            return Err(parser.usage_msg(&msg::options_incompatible(m, f)).into());
        }
    }
    if mode.get() != MODE_ALL && (external.get() != -1 || aliases.get() != -1) {
        return Err(parser
            .usage_msg(msg::ONLY_WITH_ALL)
            .into());
    }
    if let Some(m) = mode_name {
        if !args.is_empty() {
            return Err(parser.usage_msg(&msg::option_takes_no_arguments(m)).into());
        }
    }
    drop(parser);
    let mut out = String::new();
    match mode.get() {
        MODE_ALL => {
            let early = EarlyConfig::load(ctx)?;
            if verbose.get() != 0 {
                list_all(ctx, &early, external.get() != 0, aliases.get() != 0, &mut out);
            } else {
                list_columns(ctx, &mut out);
                out.push('\n');
                common_help(&mut out);
            }
        }
        MODE_GUIDES => {
            out.push_str(msg::GUIDES_HEADING);
            out.push('\n');
            listing(&cmdlist::implemented_in(Category::Guide), &mut out);
            out.push('\n');
            out.push_str(msg::HELP_FOOTER);
            out.push('\n');
        }
        MODE_USER | MODE_DEVELOPER => {
            let (heading, cat) = if mode.get() == MODE_USER {
                (msg::USER_INTERFACES_HEADING, Category::UserInterfaces)
            } else {
                (msg::DEVELOPER_INTERFACES_HEADING, Category::DeveloperInterfaces)
            };
            out.push_str(heading);
            out.push('\n');
            listing(&cmdlist::implemented_in(cat), &mut out);
            out.push('\n');
        }
        MODE_CONFIG | MODE_CONFIG_VARS | MODE_CONFIG_SECTIONS => {
            return Err(Die::Fatal(msg::CONFIG_LIST_UNSUPPORTED.into()));
        }
        _ => {
            let Some(name) = args.first() else {
                common_help(&mut out);
                print!("{out}");
                return Ok(0);
            };
            return show_page(ctx, name, format.get(), exclude_guides.get());
        }
    }
    let _ = std::io::stdout().write_all(out.as_bytes());
    Ok(0)
}

/// `git help` without arguments (and `git` without a command): the
/// top-level usage followed by the common commands, grouped.
pub fn common_help(out: &mut String) {
    out.push_str(crate::messages::GIT_USAGE);
    out.push('\n');
    out.push_str(msg::COMMON_HEADING);
    out.push('\n');
    let common: Vec<&cmdlist::Entry> = cmdlist::implemented_in(Category::Main)
        .into_iter()
        .filter(|e| e.group.is_some())
        .collect();
    let width = common.iter().map(|e| e.name.len()).max().unwrap_or(0) + 3;
    for g in Group::ALL {
        let members: Vec<_> = common.iter().filter(|e| e.group == Some(*g)).collect();
        if members.is_empty() {
            continue;
        }
        out.push('\n');
        out.push_str(g.heading());
        out.push('\n');
        for e in members {
            out.push_str(&format!("   {:<width$}{}\n", e.name, e.description));
        }
    }
    out.push('\n');
    out.push_str(msg::HELP_FOOTER);
    out.push('\n');
}

/// Entries as `   <name><padding><description>`, padded to the longest
/// name plus three.
fn listing(entries: &[&cmdlist::Entry], out: &mut String) {
    let width = entries.iter().map(|e| e.name.len()).max().unwrap_or(0) + 3;
    for e in entries {
        out.push_str(&format!("   {:<width$}{}\n", e.name, e.description));
    }
}

/// `git help -a`: every category, then external commands and aliases.
fn list_all(ctx: &Context, early: &EarlyConfig, external: bool, aliases: bool, out: &mut String) {
    let sections: Vec<(Category, Vec<&cmdlist::Entry>)> = Category::ALL_LISTED
        .iter()
        .map(|c| (*c, cmdlist::implemented_in(*c)))
        .collect();
    let others: Vec<String> = if external {
        let main = cmdlist::main_commands(ctx);
        cmdlist::other_commands(ctx, &main).into_iter().collect()
    } else {
        Vec::new()
    };
    let alias_list = if aliases {
        alias::list(&early.config)
    } else {
        Vec::new()
    };
    let width = sections
        .iter()
        .flat_map(|(_, v)| v.iter().map(|e| e.name.len()))
        .chain(others.iter().map(String::len))
        .chain(alias_list.iter().map(|(n, _)| n.len()))
        .max()
        .unwrap_or(0)
        + 3;
    out.push_str(msg::ALL_HEADING);
    out.push('\n');
    for (cat, entries) in &sections {
        out.push('\n');
        out.push_str(cat.heading());
        out.push('\n');
        for e in entries {
            out.push_str(&format!("   {:<width$}{}\n", e.name, e.description));
        }
    }
    if !others.is_empty() {
        out.push('\n');
        out.push_str(msg::EXTERNAL_COMMANDS);
        out.push('\n');
        for o in &others {
            out.push_str(&format!("   {o}\n"));
        }
    }
    if !alias_list.is_empty() {
        out.push('\n');
        out.push_str(msg::COMMAND_ALIASES);
        out.push('\n');
        for (n, v) in &alias_list {
            out.push_str(&format!("   {n:<width$}{v}\n"));
        }
    }
}

/// `git help -a --no-verbose`: command names in columns.
fn list_columns(ctx: &Context, out: &mut String) {
    let main: Vec<String> = cmdlist::main_commands(ctx).into_iter().collect();
    let others: Vec<String> = cmdlist::other_commands(ctx, &cmdlist::main_commands(ctx))
        .into_iter()
        .collect();
    let width = main.iter().chain(&others).map(String::len).max().unwrap_or(0) + 2;
    let term = std::env::var("COLUMNS")
        .ok()
        .and_then(|c| c.parse::<usize>().ok())
        .filter(|&c| c > 0)
        .unwrap_or(80);
    out.push_str(crate::messages::GIT_USAGE);
    out.push('\n');
    out.push_str(&msg::available_in(&ctx.exec_path.display().to_string()));
    out.push_str("\n\n");
    columns(&main, width, term, out);
    if !others.is_empty() {
        out.push('\n');
        out.push_str(msg::AVAILABLE_ELSEWHERE);
        out.push_str("\n\n");
        columns(&others, width, term, out);
    }
}

/// Names column by column, indented by two spaces.
fn columns(names: &[String], width: usize, term: usize, out: &mut String) {
    let cols = (term.saturating_sub(3) / width).max(1);
    let rows = names.len().div_ceil(cols);
    for r in 0..rows {
        let mut line = String::from("  ");
        let mut c = 0;
        while let Some(n) = names.get(c * rows + r) {
            let last = names.get((c + 1) * rows + r).is_none();
            if last {
                line.push_str(n);
            } else {
                line.push_str(&format!("{n:<width$}"));
            }
            c += 1;
        }
        out.push_str(&line);
        out.push('\n');
    }
}

/// The manual page for `name`: `git-<cmd>` for commands, `git` itself,
/// `git<name>` for guides and everything else.
fn page_name(ctx: &Context, name: &str) -> String {
    if name == "git" {
        "git".into()
    } else if cmdlist::is_command(ctx, name) {
        format!("git-{name}")
    } else {
        format!("git{name}")
    }
}

fn show_page(ctx: &mut Context, name: &str, format: i32, exclude_guides: bool) -> CmdResult {
    let early = EarlyConfig::load(ctx)?;
    let mut name = name.to_string();
    if !cmdlist::is_command(ctx, &name) {
        if let Some(value) = alias::lookup(&early.config, &name)? {
            // `git help <alias>` answers on stdout, `git <alias> --help`
            // on stderr.
            let note = msg::is_aliased_to(&name, &value);
            if exclude_guides {
                eprintln!("{note}");
            } else {
                println!("{note}");
            }
            // `git <alias> --help` shows the page of the aliased command.
            let first = alias::split(&value).ok().and_then(|w| w.into_iter().next());
            match first {
                Some(cmd)
                    if exclude_guides
                        && !value.starts_with('!')
                        && cmdlist::is_command(ctx, &cmd) =>
                {
                    name = cmd;
                }
                _ => return Ok(0),
            }
        } else if exclude_guides {
            return match crate::autocorrect::unknown(ctx, &early, &name) {
                Ok(Some(corrected)) => show_page(ctx, &corrected, format, exclude_guides),
                Ok(None) => Ok(1),
                Err(r) => r,
            };
        }
    }
    let page = page_name(ctx, &name);
    let format = match format {
        0 => match early.config.string("help.format") {
            Ok(Some(f)) => match f.to_string().as_str() {
                "man" => FORMAT_MAN,
                "info" => FORMAT_INFO,
                "web" | "html" => FORMAT_WEB,
                other => return Err(Die::Fatal(msg::unrecognized_help_format(other))),
            },
            Ok(None) => FORMAT_MAN,
            Err(e) => return Err(Die::Fatal(e.to_string())),
        },
        f => f,
    };
    match format {
        FORMAT_WEB => show_html(ctx, &early, &page),
        FORMAT_INFO => show_info(ctx, &page),
        _ => show_man(ctx, &early, &page),
    }
}

fn string(config: &rgit_config::ConfigSet, key: &str) -> Option<String> {
    config.string(key).ok().flatten().map(|s| s.to_string())
}

/// Run `<cmd> "$@"` through the shell with `args`.
fn shell(cmd: &str, args: &[&str]) -> Command {
    let mut c = Command::new("sh");
    c.arg("-c").arg(format!("{cmd} \"$@\"")).arg(cmd).args(args);
    c
}

fn exit_code(status: std::process::ExitStatus) -> i32 {
    status.code().unwrap_or(128)
}

fn show_man(ctx: &Context, early: &EarlyConfig, page: &str) -> CmdResult {
    let viewers: Vec<String> = early
        .config
        .get_all("man.viewer")
        .filter_map(|e| e.value.as_ref().map(|v| v.to_string()))
        .collect();
    for tool in &viewers {
        if let Some(cmd) = string(&early.config, &format!("man.{tool}.cmd")) {
            let status = shell(&cmd, &[page]).envs(ctx.child_env()).status();
            if let Ok(s) = status {
                return Ok(exit_code(s));
            }
            continue;
        }
        let path = string(&early.config, &format!("man.{tool}.path"));
        if tool == "man" {
            return run_man(ctx, path.as_deref().unwrap_or("man"), page);
        }
        if path.is_some() {
            eprintln!("warning: {}", msg::path_for_unsupported_man_viewer(tool));
        }
        eprintln!("warning: {}", msg::unknown_man_viewer(tool));
    }
    run_man(ctx, "man", page)
}

fn run_man(ctx: &Context, prog: &str, page: &str) -> CmdResult {
    match Command::new(prog).arg(page).envs(ctx.child_env()).status() {
        Ok(s) if s.success() => Ok(0),
        Ok(s) => {
            eprintln!("{}", msg::online_manual(page));
            Ok(exit_code(s))
        }
        Err(_) => {
            eprintln!("{}", msg::online_manual(page));
            Err(Die::Fatal(msg::NO_MAN_VIEWER.into()))
        }
    }
}

fn show_info(ctx: &Context, page: &str) -> CmdResult {
    match Command::new("info").arg(page).envs(ctx.child_env()).status() {
        Ok(s) => Ok(exit_code(s)),
        Err(_) => {
            eprintln!("{}", msg::online_manual(page));
            Err(Die::Fatal(msg::NO_INFO_VIEWER.into()))
        }
    }
}

/// Browsers that are started as `<name> <url>` without configuration.
const KNOWN_BROWSERS: &[&str] = &[
    "firefox",
    "iceweasel",
    "seamonkey",
    "iceape",
    "chrome",
    "google-chrome",
    "chromium",
    "chromium-browser",
    "konqueror",
    "opera",
    "w3m",
    "elinks",
    "links",
    "lynx",
    "dillo",
    "open",
    "start",
    "cygstart",
    "xdg-open",
];

fn show_html(ctx: &Context, early: &EarlyConfig, page: &str) -> CmdResult {
    let configured = string(&early.config, "help.htmlpath");
    let base = configured
        .clone()
        .unwrap_or_else(|| crate::global::doc_path(&ctx.exec_path, "--html-path").display().to_string());
    let mut url = format!("{base}/{page}.html");
    if !base.contains("://") && !Path::new(&url).is_file() {
        if configured.is_some() {
            return Err(Die::Fatal(msg::documentation_not_found(&url)));
        }
        // rgit installs no HTML pages: use the online documentation.
        url = format!("{ONLINE_DOCS}{page}");
    }
    let tool = string(&early.config, "help.browser").or_else(|| string(&early.config, "web.browser"));
    if let Some(tool) = tool {
        if let Some(cmd) = string(&early.config, &format!("browser.{tool}.cmd")) {
            return match shell(&cmd, &[&url]).envs(ctx.child_env()).status() {
                Ok(s) => Ok(exit_code(s)),
                Err(e) => Err(Die::Fatal(msg::cannot_run(&cmd, &crate::global::strerror(&e)))),
            };
        }
        if KNOWN_BROWSERS.contains(&tool.as_str()) {
            let prog = string(&early.config, &format!("browser.{tool}.path")).unwrap_or(tool);
            if let Ok(s) = Command::new(&prog).arg(&url).envs(ctx.child_env()).status() {
                return Ok(exit_code(s));
            }
        } else {
            eprintln!("{}", msg::unknown_browser(&tool));
        }
    }
    let default = std::env::var("BROWSER").ok().filter(|b| !b.is_empty());
    let prog = default.unwrap_or_else(|| {
        if cfg!(target_os = "macos") {
            "open".into()
        } else {
            "xdg-open".into()
        }
    });
    match Command::new(&prog).arg(&url).envs(ctx.child_env()).status() {
        Ok(s) => Ok(exit_code(s)),
        Err(_) => {
            eprintln!("{}", msg::open_in_browser(&url));
            Ok(0)
        }
    }
}
