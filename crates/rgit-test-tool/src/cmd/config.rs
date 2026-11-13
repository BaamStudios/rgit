// SPDX-License-Identifier: MIT

//! `config` (t1305, t1308, t1309): the configuration reader API.
//!
//! Observed behaviour of the reference binary, reproduced on top of
//! `rgit-config`:
//!
//! | Call | Output |
//! |---|---|
//! | `read_early_config <key>` | every value of the key whose canonical name is exactly `<key>`, from system, global, repository (if one is found and usable) and command line; no repository needed |
//! | `get_value <key>` | the last value, `(NULL)` for a value-less key |
//! | `get_value_multi <key>` | every value |
//! | `get_int`, `get_bool`, `get_string <key>` | the last value as that type |
//! | `get <key>` | nothing; exit code only |
//! | `git_config_int <key>` | every value of the key (exact canonical name) as an int |
//! | `configset_get_value[_multi] <key> <file>...` | from the given files only |
//! | `iterate` | every entry with origin, line and scope |
//!
//! A missing key prints `Value not found for "<key>"` and exits 1; an
//! unreadable file of `configset_*` exits 2; type errors are fatal (128).

use std::io::{self, Write};
use std::path::PathBuf;

use bstr::{BStr, BString, ByteSlice};
use rgit_config::sources::{RepoConfig, ScopeFilter, load_standard};
use rgit_config::value::{parse_bool, parse_int};
use rgit_config::{
    CommandLine, ConfigSet, Entry, Error, IntWidth, Key, KeyError, LoadOptions, Loader, Origin,
    Scope, SourceSpec, StaticRepoFacts, SystemEnv,
};

use crate::messages;
use crate::repo::{self, Repo, SetupError};

const NULL: &str = "(NULL)";

/// Collects stdout and remembers the exit code.
struct Out {
    buf: Vec<u8>,
}

impl Out {
    fn new() -> Self {
        Out { buf: Vec::new() }
    }

    fn line(&mut self, text: impl AsRef<[u8]>) {
        self.buf.extend_from_slice(text.as_ref());
        self.buf.push(b'\n');
    }

    fn value(&mut self, v: Option<&BString>) {
        match v {
            Some(v) => self.line(v),
            None => self.line(NULL),
        }
    }

    /// Writes the collected output and returns `code`.
    fn finish(self, code: i32) -> i32 {
        let mut o = io::stdout().lock();
        if o.write_all(&self.buf).and_then(|()| o.flush()).is_err() {
            return 128;
        }
        code
    }
}

pub(crate) fn fatal(msg: impl std::fmt::Display) -> i32 {
    eprintln!("{}", messages::fatal(msg));
    128
}

pub(crate) fn print_fatal(lines: &[String]) -> i32 {
    for l in lines {
        eprintln!("{l}");
    }
    128
}

pub(crate) fn report(err: &Error) -> i32 {
    for (level, msg) in err.report() {
        eprintln!("{} {msg}", level.prefix());
    }
    128
}

fn syntax_error() -> i32 {
    fatal(messages::CONFIG_SYNTAX)
}

fn facts(repo: &Repo, early: bool) -> StaticRepoFacts {
    StaticRepoFacts {
        git_dir: Some(repo.abs_git_dir.clone()),
        // During early reading the work tree and branch are not known yet.
        worktree: if early { None } else { repo.worktree.clone() },
        branch: if early { None } else { repo.current_branch() },
    }
}

/// Loads the standard stack. `repo` is `None` outside a repository.
pub(crate) fn load(repo: Option<&Repo>, early: bool) -> Result<ConfigSet, Error> {
    let env = SystemEnv::default();
    let facts = repo.map(|r| facts(r, early));
    let opts = LoadOptions {
        env: &env,
        repo: facts.as_ref().map(|f| f as &dyn rgit_config::RepoFacts),
        includes: true,
    };
    let repo_config = repo.map(|r| RepoConfig::new(r.git_dir.clone()));
    let mut set = ConfigSet::new();
    load_standard(
        opts,
        repo_config.as_ref(),
        &CommandLine::default(),
        ScopeFilter::default(),
        &mut set,
    )?;
    Ok(set)
}

/// Validates a key given to a lookup and returns its canonical form.
fn lookup_key(key: &str) -> Result<BString, KeyError> {
    Key::parse(key).map(|k| k.canonical())
}

fn not_found(out: &mut Out, key: &str) -> i32 {
    out.line(messages::value_not_found(key));
    1
}

/// `in file <path>` etc., or nothing for the command line.
fn source_suffix(e: &Entry) -> Option<String> {
    match &e.source.origin {
        Origin::CommandLine => None,
        o => Some(o.describe()),
    }
}

fn int_error(value: &BStr, name: &BStr, e: &Entry, reason: &str) -> i32 {
    fatal(messages::bad_numeric(value, name, source_suffix(e), reason))
}

pub fn config(args: &[String]) -> i32 {
    if args.first().map(String::as_str) == Some("read_early_config") {
        if args.len() != 2 {
            return syntax_error();
        }
        return read_early_config(&args[1]);
    }

    let repo = match repo::discover(true) {
        Ok(r) => r,
        Err(SetupError::NotFound) => return fatal(messages::NOT_A_REPOSITORY),
        Err(SetupError::FilesystemBoundary(dir)) => {
            return fatal(messages::not_a_repository_fs(&dir.display().to_string()));
        }
        Err(SetupError::Format { reason, .. }) => return fatal(reason),
        Err(SetupError::Fatal(lines)) => return print_fatal(&lines),
    };
    let Some(op) = args.first() else {
        eprintln!("{}", messages::CONFIG_NO_COMMAND);
        return 1;
    };
    let mut out = Out::new();
    let code = match (op.as_str(), &args[1..]) {
        ("iterate", _) => match load(Some(&repo), false) {
            Ok(set) => iterate(&mut out, &set),
            Err(e) => report(&e),
        },
        ("configset_get_value", [key, files @ ..]) => configset(&mut out, key, files, false),
        ("configset_get_value_multi", [key, files @ ..]) => configset(&mut out, key, files, true),
        (op, [key]) => {
            let set = match load(Some(&repo), false) {
                Ok(s) => s,
                Err(e) => return report(&e),
            };
            match op {
                "get_value" | "get_value_multi" | "get_int" | "get_bool" | "get_string" | "get" => {
                    lookup(&mut out, &set, op, key)
                }
                "git_config_int" => git_config_int(&mut out, &set, key),
                _ => return syntax_error(),
            }
        }
        _ => return syntax_error(),
    };
    out.finish(code)
}

fn lookup(out: &mut Out, set: &ConfigSet, op: &str, key: &str) -> i32 {
    let canonical = match lookup_key(key) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("error: {e}");
            if op == "get" {
                out.line(match e {
                    KeyError::NoSection(_) | KeyError::NoVariable(_) => {
                        messages::key_no_section(key)
                    }
                    KeyError::Invalid(_) | KeyError::Newline(_) => messages::key_invalid(key),
                });
                return 1;
            }
            return not_found(out, key);
        }
    };
    let entries: Vec<&Entry> = set.get_all(canonical.as_bstr()).collect();
    let Some(last) = entries.last().copied() else {
        return not_found(out, key);
    };
    let name = BStr::new(key.as_bytes());
    match op {
        "get" => {}
        "get_value" => out.value(last.value.as_ref()),
        "get_value_multi" => {
            for e in &entries {
                out.value(e.value.as_ref());
            }
        }
        "get_int" => {
            let v = last
                .value
                .as_ref()
                .map(|v| v.as_bytes())
                .unwrap_or_default();
            match parse_int(v, IntWidth::I32) {
                Ok(n) => out.line(n.to_string()),
                Err(e) => return int_error(v.as_bstr(), name, last, e.reason()),
            }
        }
        "get_bool" => match parse_bool(last.value.as_ref().map(|v| v.as_bytes())) {
            Some(b) => out.line(if b { "1" } else { "0" }),
            None => {
                let v = last.value.clone().unwrap_or_default();
                return fatal(messages::bad_boolean(v.as_bstr(), name));
            }
        },
        "get_string" => match &last.value {
            Some(v) => out.line(v),
            None => {
                eprintln!("error: {}", messages::missing_value(name));
                return fatal(match &last.source.origin {
                    Origin::CommandLine => messages::unable_to_parse_cmdline(name),
                    o => messages::bad_config_variable(name, o, last.line),
                });
            }
        },
        _ => unreachable!("checked by the caller"),
    }
    0
}

/// `git_config_int`: walks all entries; the reference names the value
/// where the variable name belongs in its error message.
fn git_config_int(out: &mut Out, set: &ConfigSet, key: &str) -> i32 {
    for e in set.entries() {
        if e.name.as_bytes() != key.as_bytes() {
            continue;
        }
        let v = e.value.as_ref().map(|v| v.as_bytes()).unwrap_or_default();
        match parse_int(v, IntWidth::I32) {
            Ok(n) => out.line(n.to_string()),
            Err(err) => {
                let shown_name: &BStr = match &e.value {
                    Some(v) => v.as_bstr(),
                    None => BStr::new(messages::C_NULL),
                };
                // What was printed so far stays, then the error.
                let printed = std::mem::replace(out, Out::new());
                printed.finish(0);
                return int_error(v.as_bstr(), shown_name, e, err.reason());
            }
        }
    }
    0
}

fn iterate(out: &mut Out, set: &ConfigSet) -> i32 {
    for (i, e) in set.entries().iter().enumerate() {
        if i > 0 {
            out.line("");
        }
        let mut key = BString::from("key=");
        key.extend_from_slice(&e.name);
        out.line(key);
        let mut value = BString::from("value=");
        match &e.value {
            Some(v) => value.extend_from_slice(v),
            None => value.extend_from_slice(messages::C_NULL),
        }
        out.line(value);
        out.line(format!("origin={}", e.source.origin.kind()));
        let mut name = BString::from("name=");
        name.extend_from_slice(&e.source.origin.name());
        out.line(name);
        match e.source.origin {
            Origin::CommandLine => out.line("lno=-1"),
            _ => out.line(format!("lno={}", e.line)),
        }
        out.line(format!("scope={}", e.source.scope.as_str()));
    }
    0
}

/// `configset_get_value[_multi] <key> <file>...`: only the given files.
fn configset(out: &mut Out, key: &str, files: &[String], multi: bool) -> i32 {
    let env = SystemEnv::default();
    let mut set = ConfigSet::new();
    for f in files {
        let path = PathBuf::from(f);
        let mut loader = Loader::new(LoadOptions {
            env: &env,
            repo: None,
            includes: false,
        });
        loader.add(SourceSpec::File {
            path: path.clone(),
            scope: Scope::Unknown,
            required: true,
        });
        // A missing file is an error here, unlike for the standard stack.
        if !path.exists() && path.symlink_metadata().is_err() {
            eprintln!("{}", messages::configset_read_error(f));
            return 2;
        }
        match loader.load_into(&mut set) {
            Ok(()) => {}
            Err(e @ Error::Access { .. }) => {
                eprintln!("warning: {e}");
                eprintln!("{}", messages::configset_read_error(f));
                return 2;
            }
            Err(e) => return report(&e),
        }
    }
    let canonical = match lookup_key(key) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("error: {e}");
            return not_found(out, key);
        }
    };
    let entries: Vec<&Entry> = set.get_all(canonical.as_bstr()).collect();
    let Some(last) = entries.last() else {
        return not_found(out, key);
    };
    if multi {
        for e in &entries {
            out.value(e.value.as_ref());
        }
    } else {
        out.value(last.value.as_ref());
    }
    0
}

fn read_early_config(key: &str) -> i32 {
    let repo = match repo::discover(false) {
        Ok(r) => Some(r),
        Err(SetupError::NotFound | SetupError::FilesystemBoundary(_)) => None,
        Err(SetupError::Fatal(lines)) => return print_fatal(&lines),
        Err(SetupError::Format { git_dir, reason }) => {
            eprintln!(
                "warning: {}",
                messages::ignoring_git_dir(&git_dir.display().to_string(), &reason)
            );
            None
        }
    };
    let set = match load(repo.as_ref(), true) {
        Ok(s) => s,
        Err(e) => return report(&e),
    };
    let mut out = Out::new();
    for e in set.entries() {
        if e.name.as_bytes() == key.as_bytes() {
            // The reference crashes on a value-less key; print the marker.
            out.value(e.value.as_ref());
        }
    }
    out.finish(0)
}
