// SPDX-License-Identifier: MIT
//! Commands rgit hands off to an installed git's programs (#710): the
//! foreign-SCM bridges and the legacy script commands (`svn`, `p4`, the
//! CVS tools, ...). rgit neither bundles nor copies them (they are GPL
//! programs); it runs an installed `git-<name>` as a separate process, found
//! like any external command (exec path, then `$PATH`) and else in the exec
//! path of an installed git (`git --exec-path`). The program then calls
//! rgit as `git` for the plumbing: rgit's exec path is in front of its
//! `PATH`, and when no `git` there is rgit, a temporary `git` link to rgit
//! is put in front of that.

use crate::argv::CommandArgs as _;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use crate::Context;
use crate::external::{is_executable, is_self};

/// A command rgit hands off: its name and what to install to get it.
pub struct Handoff {
    pub name: &'static str,
    /// The package hint of the "not built into rgit" message.
    pub install: &'static str,
}

const fn h(name: &'static str, install: &'static str) -> Handoff {
    Handoff { name, install }
}

/// Every command handed off to an installed git.
pub const HANDOFFS: &[Handoff] = &[
    h(
        "svn",
        "package \"git-svn\" / \"git\" with Subversion Perl bindings",
    ),
    h("p4", "package \"git-p4\" / \"git\" with Python"),
    h("cvsimport", "package \"git-cvs\" / \"git\" with cvsps"),
    h("cvsexportcommit", "package \"git-cvs\" / \"git\" with cvs"),
    h("cvsserver", "package \"git-cvs\" / \"git\" with Perl DBI"),
    h("archimport", "package \"git-arch\" / \"git\" with tla"),
    h("quiltimport", "package \"git\""),
    h("instaweb", "package \"git\" with gitweb"),
    h("gui", "package \"git-gui\" / \"git\" with Tcl/Tk"),
    h("citool", "package \"git-gui\" / \"git\" with Tcl/Tk"),
    h("web--browse", "package \"git\""),
];

/// The hand-off entry of `name`, if rgit hands it off.
pub fn lookup(name: &str) -> Option<&'static Handoff> {
    HANDOFFS.iter().find(|h| h.name == name)
}

/// `git-<name>` in the exec path of an installed git, for a handed-off
/// command.
pub fn find(name: &str) -> Option<PathBuf> {
    lookup(name)?;
    let dir = installed_exec_path()?;
    let p = dir.join(format!("git-{name}"));
    (is_executable(&p) && !is_self(&p)).then_some(p)
}

/// The exec path of the first `git` on `$PATH` that is not rgit itself
/// (`git --exec-path`); asked once per process.
fn installed_exec_path() -> Option<&'static Path> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let path = std::env::var_os("PATH")?;
        let git = std::env::split_paths(&path)
            .filter(|d| !d.as_os_str().is_empty())
            .map(|d| d.join(if cfg!(windows) { "git.exe" } else { "git" }))
            .find(|p| is_executable(p) && !is_self(p))?;
        let out = Command::new(&git)
            .raw_arg("--exec-path")
            .env_remove("GIT_EXEC_PATH")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let text = String::from_utf8(out.stdout).ok()?;
        let dir = text.trim_end_matches(['\n', '\r']);
        (!dir.is_empty()).then(|| PathBuf::from(dir))
    })
    .as_deref()
}

/// A temporary directory holding a `git` link to rgit, put in front of a
/// handed-off program's `PATH`; removed when dropped.
pub struct GitShim {
    dir: PathBuf,
}

impl GitShim {
    /// A shim when `git` in rgit's exec path is not rgit itself (rgit
    /// installed as `rgit`); `None` when not needed or not possible.
    pub fn new(ctx: &Context) -> Option<GitShim> {
        if is_self(&ctx.exec_path.join("git")) {
            return None;
        }
        #[cfg(unix)]
        {
            let me = std::env::current_exe().ok()?;
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0);
            let dir =
                std::env::temp_dir().join(format!("rgit-handoff-{}-{nanos}", std::process::id()));
            std::fs::create_dir(&dir).ok()?;
            let shim = GitShim { dir };
            std::os::unix::fs::symlink(me, shim.dir.join("git")).ok()?;
            Some(shim)
        }
        #[cfg(not(unix))]
        {
            None
        }
    }

    /// `path` with the shim directory in front.
    pub fn path(&self, path: Option<&OsString>) -> OsString {
        let mut dirs = vec![self.dir.clone()];
        if let Some(p) = path {
            dirs.extend(std::env::split_paths(p));
        }
        std::env::join_paths(dirs).unwrap_or_else(|_| self.dir.clone().into_os_string())
    }
}

impl Drop for GitShim {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
