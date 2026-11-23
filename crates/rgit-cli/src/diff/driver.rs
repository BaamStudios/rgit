// SPDX-License-Identifier: MIT
//! Diff drivers (gitattributes(5), "Generating diff text"): the `diff`
//! attribute of a path and the `diff.<driver>.*` settings it selects —
//! `binary`, `xfuncname`/`funcname` (or the built-in patterns of
//! `rgit_diff::userdiff`), `textconv`, `command`,
//! `trustExitCode`, `algorithm` and `wordRegex`.
//!
//! Behaviour checked against the `git` binary: a pattern line that does
//! not compile is fatal ("Invalid regexp to look for hunk header", naming
//! that line), as is a negated last line; an unknown
//! `diff.<driver>.algorithm` is ignored; a failing textconv program is
//! fatal ("unable to read files to diff").

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use regex::bytes::RegexBuilder;
use rgit_attr::State;
use rgit_config::ConfigSet;
use rgit_diff::{Algorithm, FuncName, FuncPattern};

use crate::Die;
use crate::messages::userdiff as msg;

/// The settings of one driver.
#[derive(Debug, Default)]
pub struct Driver {
    /// `diff.<driver>.binary`.
    pub binary: Option<bool>,
    /// The hunk-header patterns (configured, else built in).
    pub funcname: Option<FuncName>,
    /// `diff.<driver>.textconv`.
    pub textconv: Option<String>,
    /// `diff.<driver>.command`.
    pub command: Option<String>,
    /// `diff.<driver>.trustExitCode`.
    pub trust_exit_code: bool,
    /// `diff.<driver>.algorithm` (and whether it asks for `minimal`).
    pub algorithm: Option<(Algorithm, bool)>,
    /// `diff.<driver>.wordRegex`, else the built-in word pattern (for
    /// `--word-diff`).
    pub word_regex: Option<regex::bytes::Regex>,
}

/// What the `diff` attribute says about a path.
#[derive(Debug, Clone, Default)]
pub enum DiffAttr {
    #[default]
    Unspecified,
    /// `diff`: always text.
    Set,
    /// `-diff`: always binary.
    Unset,
    /// `diff=<driver>`.
    Driver(Rc<Driver>),
}

impl DiffAttr {
    pub fn driver(&self) -> Option<&Driver> {
        match self {
            DiffAttr::Driver(d) => Some(d),
            _ => None,
        }
    }

    /// Whether the attribute decides that the path is binary
    /// (`Some(true)`) or text (`Some(false)`).
    pub fn binary(&self) -> Option<bool> {
        match self {
            DiffAttr::Unset => Some(true),
            DiffAttr::Set => Some(false),
            DiffAttr::Driver(d) => d.binary,
            DiffAttr::Unspecified => None,
        }
    }

    /// The textconv program, if any.
    pub fn textconv(&self) -> Option<&str> {
        self.driver().and_then(|d| d.textconv.as_deref())
    }
}

/// Compile the value of `diff.<driver>.xfuncname` (`extended`) or
/// `.funcname`: one pattern per line.
pub fn compile_funcname(value: &str, extended: bool) -> Result<FuncName, Die> {
    let lines: Vec<&str> = value.split('\n').collect();
    let mut patterns = Vec::with_capacity(lines.len());
    for (i, line) in lines.iter().enumerate() {
        let (negate, p) = match line.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (false, *line),
        };
        if negate && i + 1 == lines.len() {
            return Err(Die::Fatal(msg::last_negated(line)));
        }
        let bad = || Die::Fatal(msg::invalid_funcname_regex(p));
        let text = rgit_revwalk::walk::grep::translate(p, extended).map_err(|_| bad())?;
        let regex = RegexBuilder::new(&text)
            .unicode(false)
            .build()
            .map_err(|_| bad())?;
        patterns.push(FuncPattern { negate, regex });
    }
    Ok(FuncName::new(patterns))
}

/// Read the settings of driver `name`.
pub fn load(config: &ConfigSet, name: &str) -> Result<Driver, Die> {
    let get = |k: &str| {
        config
            .string(format!("diff.{name}.{k}"))
            .ok()
            .flatten()
            .map(|v| v.to_string())
    };
    let flag = |k: &str| {
        config
            .bool(format!("diff.{name}.{k}"))
            .map_err(|e| Die::Fatal(e.to_string()))
    };
    let builtin = rgit_diff::userdiff::builtin(name);
    let funcname = if let Some(v) = get("xfuncname") {
        Some(compile_funcname(&v, true)?)
    } else if let Some(v) = get("funcname") {
        Some(compile_funcname(&v, false)?)
    } else {
        builtin.map(|b| b.funcname())
    };
    Ok(Driver {
        binary: flag("binary")?,
        funcname,
        textconv: get("textconv"),
        command: get("command"),
        trust_exit_code: flag("trustexitcode")?.unwrap_or(false),
        algorithm: get("algorithm").and_then(|v| Algorithm::parse(&v)),
        word_regex: match get("wordregex") {
            Some(v) => Some(super::options::word_regex(&v)?),
            None => builtin.map(|b| b.word_regex()),
        },
    })
}

/// The drivers of one command, each read once.
#[derive(Default)]
pub struct Drivers {
    cache: RefCell<HashMap<Vec<u8>, Rc<Driver>>>,
}

impl Drivers {
    /// The meaning of a `diff` attribute state.
    pub fn resolve(&self, config: &ConfigSet, state: &State) -> Result<DiffAttr, Die> {
        Ok(match state {
            State::Unspecified => DiffAttr::Unspecified,
            State::Set => DiffAttr::Set,
            State::Unset => DiffAttr::Unset,
            State::Value(v) => {
                if let Some(d) = self.cache.borrow().get(v) {
                    return Ok(DiffAttr::Driver(d.clone()));
                }
                let d = Rc::new(load(config, &String::from_utf8_lossy(v))?);
                self.cache.borrow_mut().insert(v.clone(), d.clone());
                DiffAttr::Driver(d)
            }
        })
    }
}

/// Run textconv program `cmd` on `data` (written to a temporary file
/// whose name is the program's argument).
pub fn run_textconv(cmd: &str, data: &[u8]) -> Result<Vec<u8>, ()> {
    let tmp = TempFile::new(data, b"").map_err(|_| ())?;
    let out = rgit_ident::editor::shell_command(cmd, &[tmp.path.as_os_str()])
        .stdin(std::process::Stdio::null())
        .output();
    match out {
        Ok(o) if o.status.success() => Ok(o.stdout),
        _ => Err(()),
    }
}

/// A temporary file, removed when dropped.
pub struct TempFile {
    pub path: std::path::PathBuf,
}

impl TempFile {
    /// A new file holding `data`, named like the last component of
    /// `path` inside a new temporary directory.
    pub fn new(data: &[u8], path: &[u8]) -> std::io::Result<TempFile> {
        static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let base = path.rsplit(|&c| c == b'/').next().unwrap_or_default();
        let base = if base.is_empty() { &b"blob"[..] } else { base };
        let dir = std::env::temp_dir().join(format!("rgit-blob-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(String::from_utf8_lossy(base).as_ref());
        std::fs::write(&path, data)?;
        Ok(TempFile { path })
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        if let Some(d) = self.path.parent() {
            let _ = std::fs::remove_dir(d);
        }
    }
}
