// SPDX-License-Identifier: MIT
//! The merge and diff tools `mergetool` and `difftool` know, how they are
//! selected (`--tool`, `merge.tool`, `diff.tool`, the `gui` variants and a
//! guess among the installed tools), listed (`--tool-help`) and run.
//!
//! The command lines of the built-in tools are rgit's own, written from
//! each tool's documentation of its command-line interface (CLEANROOM.md,
//! "mergetool definitions"); the names, their descriptions in the
//! `--tool-help` listing and the guess order are git's user-visible
//! behaviour, observed with the `git` binary.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use rgit_config::ConfigSet;

use crate::messages::mergetool as msg;

/// What the tool is used for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Merge,
    Diff,
}

impl Mode {
    /// The configuration section prefix: `merge` or `diff`.
    pub fn word(self) -> &'static str {
        match self {
            Mode::Merge => "merge",
            Mode::Diff => "diff",
        }
    }
}

/// A built-in tool.
struct Tool {
    name: &'static str,
    /// The program, unless `<mode>tool.<name>.path` says otherwise.
    exe: &'static str,
    /// `--tool-help` description as a merge tool, `None` when it cannot
    /// merge.
    merge: Option<&'static str>,
    /// `--tool-help` description as a diff tool, `None` when it cannot diff.
    diff: Option<&'static str>,
}

const GUI: &str = " (requires a graphical session)";

macro_rules! tool {
    ($name:expr, $exe:expr, $merge:expr, $diff:expr) => {
        Tool {
            name: $name,
            exe: $exe,
            merge: $merge,
            diff: $diff,
        }
    };
}

const VIM_CUSTOM: &str = " with a custom layout (see `git help mergetool`'s `BACKEND SPECIFIC HINTS` section)";
const VIM_1: &str = " with a 2 panes layout (LOCAL and REMOTE)";
const VIM_2: &str = " with a 3 panes layout (LOCAL, MERGED and REMOTE)";
const VIM_3: &str = " where only the MERGED file is shown";

/// The built-in tools in `--tool-help` order. Descriptions are completed by
/// [`describe`].
const TOOLS: &[Tool] = &[
    tool!("araxis", "compare", Some("Use Araxis Merge*"), Some("Use Araxis Merge*")),
    tool!("bc", "bcompare", Some("Use Beyond Compare*"), Some("Use Beyond Compare*")),
    tool!("bc3", "bcompare", Some("Use Beyond Compare*"), Some("Use Beyond Compare*")),
    tool!("bc4", "bcompare", Some("Use Beyond Compare*"), Some("Use Beyond Compare*")),
    tool!("codecompare", "CodeMerge", Some("Use Code Compare*"), Some("Use Code Compare*")),
    tool!("deltawalker", "DeltaWalker", Some("Use DeltaWalker*"), Some("Use DeltaWalker*")),
    tool!("diffmerge", "diffmerge", Some("Use DiffMerge*"), Some("Use DiffMerge*")),
    tool!("diffuse", "diffuse", Some("Use Diffuse*"), Some("Use Diffuse*")),
    tool!("ecmerge", "ecmerge", Some("Use ECMerge*"), Some("Use ECMerge*")),
    tool!("emerge", "emacs", Some("Use Emacs' Emerge"), Some("Use Emacs' Emerge")),
    tool!("examdiff", "ExamDiff", Some("Use ExamDiff Pro*"), Some("Use ExamDiff Pro*")),
    tool!("guiffy", "guiffy", Some("Use Guiffy's Diff Tool*"), Some("Use Guiffy's Diff Tool*")),
    tool!("gvimdiff", "gvim", Some("Use gVim*+"), Some("Use gVim*")),
    tool!("gvimdiff1", "gvim", Some("Use gVim*1"), None),
    tool!("gvimdiff2", "gvim", Some("Use gVim*2"), None),
    tool!("gvimdiff3", "gvim", Some("Use gVim*3"), None),
    tool!("kdiff3", "kdiff3", Some("Use KDiff3*"), Some("Use KDiff3*")),
    tool!("kompare", "kompare", None, Some("Use Kompare*")),
    tool!(
        "meld",
        "meld",
        Some("Use Meld* with optional `auto merge` (see `git help mergetool`'s `CONFIGURATION` section)"),
        Some("Use Meld*")
    ),
    tool!("nvimdiff", "nvim", Some("Use Neovim+"), Some("Use Neovim")),
    tool!("nvimdiff1", "nvim", Some("Use Neovim1"), None),
    tool!("nvimdiff2", "nvim", Some("Use Neovim2"), None),
    tool!("nvimdiff3", "nvim", Some("Use Neovim3"), None),
    tool!("opendiff", "opendiff", Some("Use FileMerge*"), Some("Use FileMerge*")),
    tool!("p4merge", "p4merge", Some("Use HelixCore P4Merge*"), Some("Use HelixCore P4Merge*")),
    tool!("smerge", "smerge", Some("Use Sublime Merge*"), Some("Use Sublime Merge*")),
    tool!("tkdiff", "tkdiff", Some("Use TkDiff*"), Some("Use TkDiff*")),
    tool!("tortoisemerge", "tortoisemerge", Some("Use TortoiseMerge*"), None),
    tool!("vimdiff", "vim", Some("Use Vim+"), Some("Use Vim")),
    tool!("vimdiff1", "vim", Some("Use Vim1"), None),
    tool!("vimdiff2", "vim", Some("Use Vim2"), None),
    tool!("vimdiff3", "vim", Some("Use Vim3"), None),
    tool!("vscode", "code", Some("Use Visual Studio Code*"), Some("Use Visual Studio Code*")),
    tool!("winmerge", "WinMergeU", Some("Use WinMerge*"), Some("Use WinMerge*")),
    tool!("xxdiff", "xxdiff", Some("Use xxdiff*"), Some("Use xxdiff*")),
];

/// Expands the markers of a description: `*` the graphical-session note,
/// `+` the custom layout note, `1`..`3` the layout variants.
fn describe(raw: &str) -> String {
    let mut out = String::new();
    for c in raw.chars() {
        match c {
            '*' => out.push_str(GUI),
            '+' => out.push_str(VIM_CUSTOM),
            '1' if raw.ends_with('1') => out.push_str(VIM_1),
            '2' if raw.ends_with('2') => out.push_str(VIM_2),
            '3' if raw.ends_with('3') => out.push_str(VIM_3),
            c => out.push(c),
        }
    }
    out
}

fn find(name: &str) -> Option<&'static Tool> {
    TOOLS.iter().find(|t| t.name == name)
}

fn description(t: &Tool, mode: Mode) -> Option<&'static str> {
    match mode {
        Mode::Merge => t.merge,
        Mode::Diff => t.diff,
    }
}

/// Whether `name` is a built-in tool for `mode`.
pub(crate) fn is_builtin(name: &str, mode: Mode) -> bool {
    find(name).is_some_and(|t| description(t, mode).is_some())
}

fn config_string(config: &ConfigSet, key: &str) -> Option<String> {
    config
        .string(key)
        .ok()
        .flatten()
        .map(|v| String::from_utf8_lossy(v).into_owned())
}

fn config_bool(config: &ConfigSet, key: &str) -> Option<bool> {
    config.bool(key).ok().flatten()
}

/// `<mode>tool.<tool>.<key>`, for diffs falling back to `mergetool.*`.
fn tool_config(config: &ConfigSet, mode: Mode, tool: &str, key: &str) -> Option<String> {
    if mode == Mode::Diff {
        if let Some(v) = config_string(config, &format!("difftool.{tool}.{key}")) {
            return Some(v);
        }
    }
    config_string(config, &format!("mergetool.{tool}.{key}"))
}

/// The custom command of `tool`, if configured.
pub(crate) fn custom_cmd(config: &ConfigSet, mode: Mode, tool: &str) -> Option<String> {
    tool_config(config, mode, tool, "cmd")
}

/// Whether `tool` can be used: a custom command or a built-in tool.
pub(crate) fn is_valid(config: &ConfigSet, mode: Mode, tool: &str) -> bool {
    custom_cmd(config, mode, tool).is_some() || is_builtin(tool, mode)
}

/// The program of a built-in tool (`<mode>tool.<tool>.path` overrides it).
fn program(config: &ConfigSet, mode: Mode, t: &Tool) -> String {
    tool_config(config, mode, t.name, "path").unwrap_or_else(|| t.exe.to_owned())
}

/// Whether `program` can be found (a path, or a name in `$PATH`).
fn on_path(program: &str) -> bool {
    if program.contains('/') {
        return crate::external::is_executable(Path::new(program));
    }
    std::env::var_os("PATH").is_some_and(|p| {
        std::env::split_paths(&p).any(|d| crate::external::is_executable(&d.join(program)))
    })
}

fn display_set() -> bool {
    std::env::var_os("DISPLAY").is_some_and(|d| !d.is_empty())
}

/// The tools tried, in order, when none is configured.
fn guess_candidates(mode: Mode) -> Vec<&'static str> {
    let platform = match mode {
        Mode::Merge => "tortoisemerge",
        Mode::Diff => "kompare",
    };
    let mut out = Vec::new();
    if display_set() {
        out.extend(["opendiff", "kdiff3", "tkdiff", "xxdiff", "meld", platform, "gvimdiff"]);
        out.extend([
            "diffuse",
            "diffmerge",
            "ecmerge",
            "p4merge",
            "araxis",
            "bc",
            "codecompare",
            "smerge",
        ]);
    } else {
        out.push(platform);
    }
    out.extend(["emerge", "vimdiff", "nvimdiff"]);
    out
}

/// The tool to use: `explicit` (`--tool`, `GIT_DIFF_TOOL`), else the
/// configured one (the `gui` variant first when `gui`), else a guess. The
/// flag says whether the tool was guessed (the caller then prompts).
/// Messages about the selection go to standard error.
pub(crate) fn select(
    config: &ConfigSet,
    mode: Mode,
    gui: bool,
    explicit: Option<&str>,
) -> (String, bool) {
    if let Some(t) = explicit.filter(|t| !t.is_empty()) {
        return (t.to_owned(), false);
    }
    let mut keys: Vec<String> = Vec::new();
    if gui {
        if mode == Mode::Diff {
            keys.push("diff.guitool".into());
        }
        keys.push("merge.guitool".into());
    }
    if mode == Mode::Diff {
        keys.push("diff.tool".into());
    }
    keys.push("merge.tool".into());
    for key in &keys {
        if let Some(t) = config_string(config, key).filter(|t| !t.is_empty()) {
            if is_valid(config, mode, &t) {
                return (t, false);
            }
            eprintln!("{}", msg::unknown_configured_tool(key, &t));
            break;
        }
    }
    let key = format!("{}.tool", mode.word());
    let cmd = format!("{}tool", mode.word());
    let candidates = guess_candidates(mode);
    eprintln!("{}", msg::guess_intro(&key, &cmd, &candidates.join(" ")));
    // A tool is taken when a program of its name is installed (observed).
    if let Some(t) = candidates.iter().find(|t| on_path(t)) {
        return ((*t).to_owned(), true);
    }
    eprintln!("{}", msg::no_known_tool(mode.word()));
    (String::new(), true)
}

/// `--tool-help`.
pub(crate) fn print_help(config: Option<&ConfigSet>, mode: Mode) {
    let cmd = format!("{}tool", mode.word());
    let mut available = Vec::new();
    let mut unavailable = Vec::new();
    let empty = ConfigSet::new();
    let config = config.unwrap_or(&empty);
    for t in TOOLS {
        let Some(d) = description(t, mode) else {
            continue;
        };
        let line = format!("\t\t{:<17}{}", t.name, describe(d));
        if on_path(&program(config, mode, t)) {
            available.push(line);
        } else {
            unavailable.push(line);
        }
    }
    let mut out = String::new();
    out.push_str(&msg::tool_help_header(&cmd));
    out.push('\n');
    for l in &available {
        out.push_str(l);
        out.push('\n');
    }
    let mut user: Vec<(String, String)> = Vec::new();
    for e in config.entries() {
        let name = String::from_utf8_lossy(&e.name).into_owned();
        let tool = ["mergetool.", "difftool."]
            .iter()
            .filter(|p| mode == Mode::Diff || **p == "mergetool.")
            .find_map(|p| name.strip_prefix(p).and_then(|r| r.strip_suffix(".cmd")));
        if let Some(tool) = tool {
            let value = e
                .value
                .as_ref()
                .map(|v| String::from_utf8_lossy(v).into_owned())
                .unwrap_or_default();
            user.retain(|(t, _)| t != tool);
            user.push((tool.to_owned(), value));
        }
    }
    user.sort();
    if !user.is_empty() {
        out.push('\n');
        out.push_str(msg::TOOL_HELP_USER_DEFINED);
        out.push('\n');
        for (t, c) in &user {
            out.push_str(&format!("\t\t{t}.cmd {c}\n"));
        }
    }
    if !unavailable.is_empty() {
        out.push('\n');
        out.push_str(msg::TOOL_HELP_UNAVAILABLE);
        out.push('\n');
        for l in &unavailable {
            out.push_str(l);
            out.push('\n');
        }
    }
    out.push('\n');
    out.push_str(msg::TOOL_HELP_FOOTER);
    out.push('\n');
    print!("{out}");
}

/// The files a tool works on.
pub(crate) struct Files {
    pub base: String,
    pub local: String,
    pub remote: String,
    pub merged: String,
    /// Whether a common base exists.
    pub base_present: bool,
}

/// Whether the exit code of `tool` tells about success:
/// `<mode>tool.<tool>.trustExitCode` (and for diffs the
/// `difftool.trustExitCode` handled by the caller), else the tool's
/// default: custom commands are not trusted, built-in tools that report
/// failure through their exit code are.
pub(crate) fn trust_exit_code(config: &ConfigSet, mode: Mode, tool: &str) -> bool {
    let key = format!("{}tool.{tool}.trustexitcode", mode.word());
    if let Some(b) = config_bool(config, &key) {
        return b;
    }
    if mode == Mode::Diff {
        if let Some(b) = config_bool(config, &format!("mergetool.{tool}.trustexitcode")) {
            return b;
        }
    }
    if custom_cmd(config, mode, tool).is_some() {
        return false;
    }
    matches!(
        tool,
        "emerge"
            | "kdiff3"
            | "tkdiff"
            | "vimdiff"
            | "vimdiff1"
            | "vimdiff2"
            | "vimdiff3"
            | "gvimdiff"
            | "gvimdiff1"
            | "gvimdiff2"
            | "gvimdiff3"
            | "nvimdiff"
            | "nvimdiff1"
            | "nvimdiff2"
            | "nvimdiff3"
    )
}

/// How a run of a tool ended.
pub(crate) enum Outcome {
    /// The exit code.
    Ran(i32),
    /// Neither a custom command nor a built-in tool (the error is printed).
    NotSet,
}

/// Runs `tool` on `files`; `cwd` is where it runs.
pub(crate) fn run(config: &ConfigSet, mode: Mode, tool: &str, files: &Files, cwd: &Path) -> Outcome {
    use std::io::Write as _;
    let _ = std::io::stdout().flush();
    let env: [(&str, &str); 4] = [
        ("BASE", &files.base),
        ("LOCAL", &files.local),
        ("REMOTE", &files.remote),
        ("MERGED", &files.merged),
    ];
    if let Some(cmd) = custom_cmd(config, mode, tool) {
        let status = Command::new("sh")
            .arg("-c")
            .arg(&cmd)
            .envs(env)
            .current_dir(cwd)
            .status();
        return Outcome::Ran(code(status));
    }
    let Some(t) = find(tool).filter(|t| description(t, mode).is_some()) else {
        eprintln!("error: {}", msg::tool_not_set(mode.word(), tool));
        return Outcome::NotSet;
    };
    let prog = program(config, mode, t);
    let args = match mode {
        Mode::Merge => merge_args(config, t.name, &prog, files),
        Mode::Diff => diff_args(t.name, files),
    };
    // Like a command line in the shell: a missing program is exit code 127.
    let status = Command::new("sh")
        .arg("-c")
        .arg("\"$0\" \"$@\"")
        .arg(&prog)
        .args(&args)
        .envs(env)
        .current_dir(cwd)
        .status();
    let rc = code(status);
    if rc == 0 && mode == Mode::Merge && matches!(t.name, "vimdiff1" | "gvimdiff1" | "nvimdiff1") {
        // These edit LOCAL against REMOTE; LOCAL is the result.
        let _ = std::fs::copy(cwd.join(&files.local), cwd.join(&files.merged));
    }
    Outcome::Ran(rc)
}

fn code(status: std::io::Result<std::process::ExitStatus>) -> i32 {
    match status {
        Ok(s) => s.code().unwrap_or(128),
        Err(_) => 127,
    }
}

/// The arguments of a built-in tool merging `files` (from the tools'
/// documented command lines).
fn merge_args(config: &ConfigSet, name: &str, prog: &str, f: &Files) -> Vec<String> {
    let (b, l, r, m) = (f.base.clone(), f.local.clone(), f.remote.clone(), f.merged.clone());
    let v = |a: &[&str]| a.iter().map(|s| (*s).to_owned()).collect::<Vec<String>>();
    let with_base = f.base_present;
    match name {
        "araxis" => {
            if with_base {
                v(&["-wait", "-merge", "-3", "-a1", &b, &l, &r, &m])
            } else {
                v(&["-wait", "-2", &l, &r, &m])
            }
        }
        "bc" | "bc3" | "bc4" => {
            if with_base {
                v(&[&l, &r, &b, &format!("-mergeoutput={m}")])
            } else {
                v(&[&l, &r, &format!("-mergeoutput={m}")])
            }
        }
        "codecompare" => {
            let mut a = v(&[&format!("-MF={l}"), &format!("-TF={r}")]);
            if with_base {
                a.push(format!("-BF={b}"));
            }
            a.push(format!("-RF={m}"));
            a
        }
        "deltawalker" => {
            if with_base {
                v(&[&l, &r, &b, &format!("-merged={m}")])
            } else {
                v(&[&l, &r, &format!("-merged={m}")])
            }
        }
        "diffmerge" => {
            if with_base {
                v(&["--merge", &format!("--result={m}"), &l, &b, &r])
            } else {
                v(&["--merge", &format!("--result={m}"), &l, &r])
            }
        }
        "diffuse" => {
            if with_base {
                v(&[&l, &m, &r, &b])
            } else {
                v(&[&l, &m, &r])
            }
        }
        "ecmerge" => {
            if with_base {
                v(&[&b, &l, &r, "--default", "--mode=merge3", &format!("--to={m}")])
            } else {
                v(&[&l, &r, "--default", "--mode=merge2", &format!("--to={m}")])
            }
        }
        "emerge" => {
            if with_base {
                v(&["-f", "emerge-files-with-ancestor-command", &l, &r, &b, &m])
            } else {
                v(&["-f", "emerge-files-command", &l, &r, &m])
            }
        }
        "examdiff" => {
            if with_base {
                v(&["-merge", &l, &b, &r, &format!("-o:{m}"), "-nh"])
            } else {
                v(&["-merge", &l, &r, &format!("-o:{m}"), "-nh"])
            }
        }
        "guiffy" => {
            if with_base {
                v(&["-s", &l, &r, &b, &m])
            } else {
                v(&["-m", &l, &r, &m])
            }
        }
        "kdiff3" => {
            let mut a = v(&["--auto"]);
            if with_base {
                a.extend(v(&["--L1", &format!("{m} (Base)")]));
                a.extend(v(&["--L2", &format!("{m} (Local)"), "--L3", &format!("{m} (Remote)")]));
                a.extend(v(&["-o", &m, &b, &l, &r]));
            } else {
                a.extend(v(&["--L1", &format!("{m} (Local)"), "--L2", &format!("{m} (Remote)")]));
                a.extend(v(&["-o", &m, &l, &r]));
            }
            a
        }
        "meld" => {
            let has_output = config_bool(config, "mergetool.meld.hasoutput").unwrap_or_else(|| {
                Command::new(prog)
                    .arg("--help")
                    .output()
                    .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("--output"))
            });
            let auto_merge = config_string(config, "mergetool.meld.useautomerge")
                .is_some_and(|v| v.eq_ignore_ascii_case("true"));
            let mut a = Vec::new();
            if auto_merge {
                a.push("--auto-merge".to_owned());
            }
            if has_output {
                a.extend(v(&[&l, if with_base { &b } else { &m }, &r, &format!("--output={m}")]));
            } else {
                a.extend(v(&[&l, &m, &r]));
            }
            a
        }
        "opendiff" => {
            if with_base {
                v(&[&l, &r, "-ancestor", &b, "-merge", &m])
            } else {
                v(&[&l, &r, "-merge", &m])
            }
        }
        "p4merge" => {
            // p4merge wants a base; without one an empty file stands in.
            v(&[&b, &r, &l, &m])
        }
        "smerge" => {
            if with_base {
                v(&["mergetool", &b, &l, &r, "-o", &m])
            } else {
                v(&["mergetool", &l, &r, "-o", &m])
            }
        }
        "tkdiff" => {
            if with_base {
                v(&["-a", &b, "-o", &m, &l, &r])
            } else {
                v(&["-o", &m, &l, &r])
            }
        }
        "tortoisemerge" => {
            let mut a = Vec::new();
            if with_base {
                a.push(format!("-base:{b}"));
            }
            a.extend([format!("-mine:{l}"), format!("-theirs:{r}"), format!("-merged:{m}")]);
            a
        }
        "vimdiff" | "gvimdiff" | "nvimdiff" => {
            // Diff mode with LOCAL, BASE and REMOTE side by side and the
            // result below them (vim's -d and window commands).
            if with_base {
                v(&["-f", "-d", "-c", "4wincmd w | wincmd J", &l, &b, &r, &m])
            } else {
                v(&["-f", "-d", "-c", "3wincmd w | wincmd J", &l, &r, &m])
            }
        }
        "vimdiff1" | "gvimdiff1" | "nvimdiff1" => v(&["-f", "-d", &l, &r]),
        "vimdiff2" | "gvimdiff2" | "nvimdiff2" => v(&["-f", "-d", "-c", "wincmd l", &l, &m, &r]),
        "vimdiff3" | "gvimdiff3" | "nvimdiff3" => v(&["-f", &m]),
        "vscode" => v(&["--wait", "--merge", &l, &r, &b, &m]),
        "winmerge" => v(&["-u", "-e", "-wl", "-wr", "-am", &b, &l, &r, "-o", &m]),
        "xxdiff" => {
            if with_base {
                v(&["-X", "--show-merged-pane", "--merged-filename", &m, &l, &b, &r])
            } else {
                v(&["-X", "--show-merged-pane", "--merged-filename", &m, &l, &r])
            }
        }
        _ => Vec::new(),
    }
}

/// The arguments of a built-in tool comparing LOCAL with REMOTE.
fn diff_args(name: &str, f: &Files) -> Vec<String> {
    let (l, r) = (f.local.clone(), f.remote.clone());
    let v = |a: &[&str]| a.iter().map(|s| (*s).to_owned()).collect::<Vec<String>>();
    match name {
        "araxis" => v(&["-wait", "-2", &l, &r]),
        "codecompare" | "deltawalker" | "diffmerge" | "diffuse" | "kompare" | "opendiff"
        | "p4merge" | "tkdiff" | "xxdiff" | "bc" | "bc3" | "bc4" | "guiffy" => v(&[&l, &r]),
        "ecmerge" => v(&["--default", "--mode=diff2", &l, &r]),
        "emerge" => v(&["-f", "emerge-files-command", &l, &r]),
        "examdiff" => v(&[&l, &r, "-nh"]),
        "kdiff3" => v(&["--auto", "--L1", &format!("{} (A)", f.merged), "--L2", &format!("{} (B)", f.merged), &l, &r]),
        "meld" => v(&[&l, &r]),
        "smerge" => v(&["mergetool", &l, &r]),
        "vimdiff" | "gvimdiff" | "nvimdiff" => v(&["-R", "-f", "-d", "-c", "wincmd l", &l, &r]),
        "vscode" => v(&["--wait", "--diff", &l, &r]),
        "winmerge" => v(&["-u", "-e", &l, &r]),
        _ => Vec::new(),
    }
}

/// `$TMPDIR` (or `/tmp`) without trailing slashes.
pub(crate) fn temp_dir() -> PathBuf {
    let dir: OsString = std::env::var_os("TMPDIR")
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| "/tmp".into());
    let s = dir.to_string_lossy().into_owned();
    let trimmed = s.trim_end_matches('/');
    PathBuf::from(if trimmed.is_empty() { "/" } else { trimmed })
}

/// Creates a new directory `<tmp>/<prefix>XXXXXX`.
pub(crate) fn make_temp_dir(prefix: &str) -> std::io::Result<PathBuf> {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let base = temp_dir();
    let mut seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(1)
        ^ (u64::from(std::process::id()) << 32)
        | 1;
    loop {
        let mut name = prefix.to_owned();
        for _ in 0..6 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            name.push(CHARS[(seed % CHARS.len() as u64) as usize] as char);
        }
        let dir = base.join(&name);
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
}

/// Reads one answer line from standard input; `None` at end of input.
///
/// Reads byte by byte, so that nothing after the line is taken from other
/// processes sharing the input (the tools, the next `difftool` helper).
pub(crate) fn read_answer() -> Option<String> {
    use std::io::{Read as _, Write as _};
    let _ = std::io::stdout().flush();
    let mut line = Vec::new();
    let mut stdin = std::io::stdin();
    #[cfg(unix)]
    let mut raw = {
        use std::os::unix::io::{AsRawFd as _, FromRawFd as _};
        // SAFETY: fd 0 stays open for the life of the process; the
        // ManuallyDrop keeps this File from closing it.
        std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(stdin.as_raw_fd()) })
    };
    let mut byte = [0u8; 1];
    loop {
        #[cfg(unix)]
        let n = raw.read(&mut byte);
        #[cfg(not(unix))]
        let n = stdin.read(&mut byte);
        match n {
            Ok(0) | Err(_) => {
                if line.is_empty() {
                    return None;
                }
                break;
            }
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) => line.push(byte[0]),
        }
    }
    let _ = &mut stdin;
    let s = String::from_utf8_lossy(&line).into_owned();
    Some(s.trim_end_matches('\r').to_owned())
}
