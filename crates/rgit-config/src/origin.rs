// SPDX-License-Identifier: MIT
//! Where a configuration value came from.

use std::fmt;
use std::path::{Path, PathBuf};

use bstr::{BString, ByteVec};

/// Configuration scope, in the order git reads them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scope {
    /// `$(prefix)/etc/gitconfig` or `GIT_CONFIG_SYSTEM`.
    System,
    /// `$XDG_CONFIG_HOME/git/config`, `~/.gitconfig` or `GIT_CONFIG_GLOBAL`.
    Global,
    /// `$GIT_DIR/config`.
    Local,
    /// `$GIT_DIR/config.worktree`.
    Worktree,
    /// `-c`, `--config-env`, `GIT_CONFIG_*`, and files given with `--file`
    /// or `--blob`.
    Command,
    /// `.gitmodules` and other special sources.
    Submodule,
    /// Unknown.
    Unknown,
}

impl Scope {
    /// The name `git config --show-scope` prints.
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::System => "system",
            Scope::Global => "global",
            Scope::Local => "local",
            Scope::Worktree => "worktree",
            Scope::Command => "command",
            Scope::Submodule => "submodule",
            Scope::Unknown => "unknown",
        }
    }

    /// Protected configuration (system, global, command) may set options
    /// that are ignored in repository-controlled files.
    pub fn is_protected(self) -> bool {
        matches!(self, Scope::System | Scope::Global | Scope::Command)
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The source a value was read from.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Origin {
    /// A file, with the path as git would print it (not canonicalized).
    File(PathBuf),
    /// `--file -`.
    Stdin,
    /// `--blob=<name>`; the name as given or the object id.
    Blob(BString),
    /// `-c`, `--config-env` and the `GIT_CONFIG_*` environment variables.
    CommandLine,
}

impl Origin {
    /// The path of a file origin.
    pub fn path(&self) -> Option<&Path> {
        match self {
            Origin::File(p) => Some(p),
            _ => None,
        }
    }

    /// Text used in error messages: `file <path>`, `standard input`,
    /// `blob <name>`, `command line`.
    pub fn describe(&self) -> String {
        match self {
            Origin::File(p) => format!("file {}", p.display()),
            Origin::Stdin => "standard input".into(),
            Origin::Blob(b) => format!("blob {b}"),
            Origin::CommandLine => "command line".into(),
        }
    }

    /// The kind prefix of `--show-origin` (`file`, `standard input`, ...).
    pub fn kind(&self) -> &'static str {
        match self {
            Origin::File(_) => "file",
            Origin::Stdin => "standard input",
            Origin::Blob(_) => "blob",
            Origin::CommandLine => "command line",
        }
    }

    /// The name part of `--show-origin`; empty for stdin and command line.
    pub fn name(&self) -> BString {
        match self {
            Origin::File(p) => path_bytes(p),
            Origin::Blob(b) => b.clone(),
            Origin::Stdin | Origin::CommandLine => BString::default(),
        }
    }

    /// The `--show-origin` column: `kind:name`, with the name C-quoted when
    /// needed unless `nul_terminated` (`-z`) is set.
    pub fn show_origin(&self, nul_terminated: bool) -> BString {
        let mut out = BString::from(self.kind());
        out.push(b':');
        let name = self.name();
        if nul_terminated {
            out.extend_from_slice(&name);
        } else {
            out.extend_from_slice(&quote_c_style(&name));
        }
        out
    }
}

pub(crate) fn path_bytes(p: &Path) -> BString {
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

/// Quote `name` the way git quotes paths (`core.quotePath` = true): returns
/// the input unchanged when no quoting is needed.
pub fn quote_c_style(name: &[u8]) -> BString {
    let needs = |c: u8| c < 0x20 || c == b'"' || c == b'\\' || c >= 0x7f;
    if !name.iter().any(|&c| needs(c)) {
        return name.into();
    }
    let mut out = BString::from("\"");
    for &c in name {
        match c {
            0x07 => out.push_str("\\a"),
            0x08 => out.push_str("\\b"),
            b'\t' => out.push_str("\\t"),
            b'\n' => out.push_str("\\n"),
            0x0b => out.push_str("\\v"),
            0x0c => out.push_str("\\f"),
            b'\r' => out.push_str("\\r"),
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            c if needs(c) => out.push_str(format!("\\{c:03o}")),
            c => out.push(c),
        }
    }
    out.push(b'"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn show_origin() {
        let o = Origin::File("we ird\"x.cfg".into());
        assert_eq!(o.show_origin(false), "file:\"we ird\\\"x.cfg\"");
        assert_eq!(o.show_origin(true), "file:we ird\"x.cfg");
        assert_eq!(Origin::Stdin.show_origin(false), "standard input:");
        assert_eq!(Origin::CommandLine.show_origin(false), "command line:");
        assert_eq!(quote_c_style("ä".as_bytes()), "\"\\303\\244\"");
    }
}
