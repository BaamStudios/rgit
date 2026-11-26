// SPDX-License-Identifier: MIT
//! `git bugreport` (Documentation/git-bugreport.adoc): a text file with
//! the questions to answer, `[System Info]` (build options, `uname`,
//! compiler, libc, `$SHELL`) and `[Enabled Hooks]`; `--diagnose` adds the
//! archive of `git diagnose`.
//!
//! Intentional deviation: a `[Where to Report]` section sends the report
//! to rgit's issue tracker (README), never to the Git project.
//!
//! Observed from git: works outside a repository (no hooks then), creates
//! leading directories of `-o`, refuses to overwrite a report, prints
//! `Created new report at '<path>'.` on standard error; a positional
//! argument is an error with the plain usage.

use std::io::Write;

use crate::commands::diagnose::{self, DiagnoseMode};
use crate::messages::bugreport as msg;
use crate::prelude::*;

/// `uname`: system name, release, version and machine.
#[cfg(unix)]
fn uname() -> String {
    // SAFETY: `uts` is a writable utsname; uname fills it.
    let mut uts: libc::utsname = unsafe { std::mem::zeroed() };
    if unsafe { libc::uname(&mut uts) } != 0 {
        return String::new();
    }
    let field = |f: &[libc::c_char]| {
        let bytes: Vec<u8> = f.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
        String::from_utf8_lossy(&bytes).into_owned()
    };
    format!(
        "{} {} {} {}",
        field(&uts.sysname),
        field(&uts.release),
        field(&uts.version),
        field(&uts.machine)
    )
}

#[cfg(not(unix))]
fn uname() -> String {
    format!("{} {}", std::env::consts::OS, std::env::consts::ARCH)
}

fn libc_info() -> String {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        // SAFETY: returns a static NUL-terminated string.
        let v = unsafe { std::ffi::CStr::from_ptr(libc::gnu_get_libc_version()) };
        return format!("glibc: {}", v.to_string_lossy());
    }
    #[allow(unreachable_code)]
    std::env::consts::OS.to_string()
}

fn system_info() -> String {
    let mut s = String::from("[System Info]\n");
    s += "git version:\n";
    s += &format!("{}\n", rgit_version::version_line());
    s += &crate::commands::version::build_options_text();
    s += &format!("uname: {}\n", uname());
    s += &format!("compiler info: rustc ({})\n", std::env::consts::ARCH);
    s += &format!("libc info: {}\n", libc_info());
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "<unset>".into());
    s += &format!("$SHELL (typically, interactive shell): {shell}\n");
    s
}

fn hooks_info(ctx: &Context, setup: Option<&rgit_repo::Setup>) -> Result<String, Die> {
    let mut s = String::from("[Enabled Hooks]\n");
    let Some(setup) = setup else {
        s += &format!("{}\n", msg::NO_HOOKS_OUTSIDE_REPO);
        return Ok(s);
    };
    let config = crate::repo::config(ctx, setup);
    let hooks = crate::hooks::for_repo(ctx, setup, &config)?;
    let mut names: Vec<&str> = rgit_hook::KNOWN_HOOKS.to_vec();
    names.sort_unstable();
    for name in names {
        if hooks.exists(name) {
            s += &format!("{name}\n");
        }
    }
    Ok(s)
}

/// The usage without the options, as git prints it for a positional
/// argument.
fn plain_usage() -> String {
    let mut lines = msg::BUGREPORT_USAGE.split('\n');
    let mut out = format!("usage: {}\n", lines.next().unwrap_or(""));
    for l in lines {
        out += &format!("              {l}\n");
    }
    out
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let output: RefCell<Option<String>> = RefCell::new(None);
    // `None`: the default suffix; `Some(None)`: `--no-suffix`.
    let suffix: RefCell<Option<Option<String>>> = RefCell::new(None);
    let diag: Cell<Option<DiagnoseMode>> = Cell::new(None);
    let mut parser = Parser::new(
        &[msg::BUGREPORT_USAGE],
        vec![
            Opt::callback(None, "diagnose", Some("mode"), msg::HELP_DIAGNOSE, |v, neg| {
                if neg {
                    diag.set(None);
                    return Ok(());
                }
                let v = v.unwrap_or("stats");
                diag.set(Some(DiagnoseMode::parse(v).ok_or_else(|| {
                    CallbackError::Error(msg::invalid_diagnose_value(v))
                })?));
                Ok(())
            })
            .optional(None),
            Opt::string('o', "output-directory", &output, "path", msg::HELP_OUTPUT_DIRECTORY),
            Opt::callback('s', "suffix", Some("format"), msg::HELP_SUFFIX, |v, neg| {
                *suffix.borrow_mut() = Some(if neg { None } else { v.map(str::to_string) });
                Ok(())
            }),
        ],
    );
    let rest = parser.parse(argv)?;
    drop(parser);
    if let Some(arg) = rest.args.first() {
        return Err(Die::Output(rgit_options::Exit {
            code: 129,
            stdout: String::new(),
            stderr: format!("error: {}\n{}", msg::unknown_argument(arg), plain_usage()),
        }));
    }
    let setup = crate::repo::setup_gently(ctx)?;

    let suffix = match suffix.into_inner() {
        None => Some("%Y-%m-%d-%H%M".to_string()),
        Some(s) => s,
    };
    let (stamp, now) = match &suffix {
        Some(fmt) => {
            let (s, d) = diagnose::local_now(fmt);
            (Some(s), d)
        }
        None => (None, diagnose::local_now("").1),
    };
    let dir = output.into_inner().unwrap_or_default();
    let name = match &stamp {
        Some(s) => format!("git-bugreport-{s}.txt"),
        None => "git-bugreport.txt".to_string(),
    };
    let (path, shown) = diagnose::output_path(setup.as_ref(), &dir, &name);
    diagnose::create_leading(&path, &shown)?;

    if let Some(mode) = diag.get() {
        let zip = match &stamp {
            Some(s) => format!("git-diagnostics-{s}.zip"),
            None => "git-diagnostics.zip".to_string(),
        };
        let (zpath, zshown) = diagnose::output_path(setup.as_ref(), &dir, &zip);
        let required;
        let setup = match &setup {
            Some(s) => s,
            None => {
                required = crate::repo::setup(ctx, crate::repo::Mode::Required)?;
                &required
            }
        };
        diagnose::create_archive(setup, &zpath, &zshown, mode, now)?;
    }

    let mut report = String::from(msg::TEMPLATE);
    report += "\n\n";
    report += &system_info();
    report += "\n\n";
    report += msg::WHERE_TO_REPORT;
    report += "\n\n";
    report += &hooks_info(ctx, setup.as_ref())?;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|e| Die::Fatal(msg::unable_to_create(&shown, &crate::global::strerror(&e))))?;
    file.write_all(report.as_bytes())
        .map_err(|e| Die::Fatal(e.to_string()))?;
    eprintln!("{}", msg::created_report(&shown));
    Ok(0)
}
