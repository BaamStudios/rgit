// SPDX-License-Identifier: MIT
//! The fixture (a deterministic temporary repository) and the comparison runner.

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use tempfile::TempDir;

use crate::diff::unified_diff;
use crate::env::{self, BaseEnv};
use crate::normalize::Normalizer;
use crate::state::{Check, DEFAULT_GIT_FILES, Inspector};

/// A temporary, deterministic environment with a work directory in which
/// setup commands run with the reference git and comparisons run both binaries.
///
/// Layout of the temporary directory:
/// `state/home` (`$HOME`, with a `.gitconfig` setting `init.defaultBranch`),
/// `state/work` (the directory commands run in), `bin/<side>/git` (a link
/// to the binary of each side, first in `PATH`).
///
/// When no reference git is found, the fixture is *skipped*: every method is a
/// no-op and comparisons return `None` (unless `RGIT_REQUIRE_REFERENCE=1`,
/// which turns this into a panic).
#[derive(Debug)]
pub struct Fixture {
    inner: Option<Inner>,
}

#[derive(Debug)]
struct Inner {
    /// `None` once the directory was kept for inspection.
    dir: Option<TempDir>,
    root: PathBuf,
    reference: PathBuf,
    rgit: Option<PathBuf>,
    env: BaseEnv,
}

impl Default for Fixture {
    fn default() -> Self {
        Fixture::new()
    }
}

impl Fixture {
    /// A fresh fixture with an empty work directory (no repository yet).
    ///
    /// The reference git comes from [`env::reference_git`]; rgit from
    /// `$RGIT_BIN` unless set with [`Fixture::rgit`].
    pub fn new() -> Self {
        match env::reference_git_or_skip() {
            Some(reference) => Fixture::with_reference(reference),
            None => Fixture { inner: None },
        }
    }

    /// A fresh fixture using `reference` as the reference git.
    ///
    /// # Panics
    /// If the temporary directory cannot be created.
    pub fn with_reference(reference: impl Into<PathBuf>) -> Self {
        let dir = tempfile::Builder::new()
            .prefix("rgit-difftest-")
            .tempdir()
            .expect("create temporary directory");
        let root = dir.path().canonicalize().expect("canonicalize temp dir");
        let home = root.join("state/home");
        fs::create_dir_all(&home).expect("create home");
        fs::create_dir_all(root.join("state/work")).expect("create work dir");
        fs::write(home.join(".gitconfig"), env::global_config()).expect("write .gitconfig");
        let reference = reference.into();
        link_binary(&root.join("bin/git"), &reference);
        let mut fx = Fixture {
            inner: Some(Inner {
                dir: Some(dir),
                env: BaseEnv {
                    home,
                    ceiling: root.clone(),
                    time: env::START_TIME,
                    extra: Vec::new(),
                },
                root,
                reference,
                rgit: None,
            }),
        };
        if let Some(rgit) = env::rgit_from_env() {
            fx = fx.rgit(rgit);
        }
        fx
    }

    /// Sets the rgit binary under test (e.g. `env!("CARGO_BIN_EXE_rgit")`).
    pub fn rgit(mut self, path: impl Into<PathBuf>) -> Self {
        if let Some(inner) = &mut self.inner {
            let path = path.into();
            link_binary(&inner.root.join("bin/rgit"), &path);
            inner.rgit = Some(path);
        }
        self
    }

    /// True when no reference git was found and the fixture does nothing.
    pub fn is_skipped(&self) -> bool {
        self.inner.is_none()
    }

    fn inner(&self) -> &Inner {
        self.inner
            .as_ref()
            .expect("fixture is skipped (no reference git); check Fixture::is_skipped() first")
    }

    /// The work directory where commands run.
    ///
    /// # Panics
    /// If the fixture is skipped.
    pub fn work_dir(&self) -> PathBuf {
        self.inner().root.join("state/work")
    }

    /// The isolated `$HOME`.
    ///
    /// # Panics
    /// If the fixture is skipped.
    pub fn home_dir(&self) -> PathBuf {
        self.inner().env.home.clone()
    }

    /// The reference git binary.
    ///
    /// # Panics
    /// If the fixture is skipped.
    pub fn reference_git(&self) -> &Path {
        &self.inner().reference
    }

    /// The current fixture time (seconds since the epoch, used for author and committer dates).
    pub fn time(&self) -> i64 {
        self.inner.as_ref().map_or(env::START_TIME, |i| i.env.time)
    }

    /// Advances the fixture clock by 60 seconds.
    pub fn tick(mut self) -> Self {
        if let Some(inner) = &mut self.inner {
            inner.env.time += 60;
        }
        self
    }

    /// Sets an extra environment variable for every later command, on both sides.
    pub fn env(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        if let Some(inner) = &mut self.inner {
            inner
                .env
                .extra
                .push((key.as_ref().to_owned(), value.as_ref().to_owned()));
        }
        self
    }

    /// Runs the reference git in the work directory and returns its raw output.
    /// Useful for library tests that compare their own result with git's.
    ///
    /// # Panics
    /// If the fixture is skipped or git cannot be started.
    pub fn git_output<I, S>(&self, args: I) -> Output
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let inner = self.inner();
        let args = os_args(args);
        let mut cmd = Command::new(&inner.reference);
        inner.env.apply(&mut cmd, Some(&inner.root.join("bin/git")));
        cmd.args(&args)
            .current_dir(inner.root.join("state/work"))
            .stdin(Stdio::null());
        cmd.output()
            .unwrap_or_else(|e| panic!("cannot run {}: {e}", inner.reference.display()))
    }

    /// Runs the reference git and returns stdout.
    ///
    /// # Panics
    /// If the command fails (the message contains its stderr) or the fixture is skipped.
    pub fn git_stdout<I, S>(&self, args: I) -> Vec<u8>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let args = os_args(args);
        let out = self.git_output(&args);
        assert!(
            out.status.success(),
            "setup command `git {}` failed ({}):\n{}{}",
            display_args(&args),
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    }

    /// Runs a setup command with the reference git.
    ///
    /// # Panics
    /// If the command fails.
    pub fn git<I, S>(self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        if self.inner.is_some() {
            self.git_stdout(args);
        }
        self
    }

    fn path(&self, rel: impl AsRef<Path>) -> PathBuf {
        let rel = rel.as_ref();
        assert!(
            rel.is_relative(),
            "fixture paths are relative to the work dir"
        );
        self.work_dir().join(rel)
    }

    /// Writes a file (relative to the work directory), creating parent directories.
    pub fn write(self, rel: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> Self {
        if self.inner.is_some() {
            let path = self.path(rel);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("create parent directories");
            }
            fs::write(&path, contents).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
        }
        self
    }

    /// Writes a file and makes it executable (mode 755 on Unix).
    pub fn write_executable(self, rel: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> Self {
        let rel = rel.as_ref().to_path_buf();
        let fx = self.write(&rel, contents);
        #[cfg(unix)]
        if fx.inner.is_some() {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(fx.path(&rel), fs::Permissions::from_mode(0o755))
                .expect("chmod 755");
        }
        fx
    }

    /// Creates a symbolic link at `rel` pointing to `target` (Unix only).
    #[cfg(unix)]
    pub fn symlink(self, target: impl AsRef<Path>, rel: impl AsRef<Path>) -> Self {
        if self.inner.is_some() {
            let path = self.path(rel);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("create parent directories");
            }
            std::os::unix::fs::symlink(target, &path).expect("create symlink");
        }
        self
    }

    /// Removes a file or directory (recursively) below the work directory.
    pub fn remove(self, rel: impl AsRef<Path>) -> Self {
        if self.inner.is_some() {
            let path = self.path(rel);
            let result = if path.is_dir() {
                fs::remove_dir_all(&path)
            } else {
                fs::remove_file(&path)
            };
            result.unwrap_or_else(|e| panic!("remove {}: {e}", path.display()));
        }
        self
    }

    /// Starts a comparison of `args` run with git and with rgit.
    /// Configure it, then call [`Comparison::run`].
    pub fn compare<I, S>(&mut self, args: I) -> Comparison<'_>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        Comparison {
            fx: self,
            args: os_args(args),
            stdin: None,
            env: Vec::new(),
            dir: PathBuf::new(),
            repo: PathBuf::new(),
            checks: Check::ALL.into_iter().collect(),
            normalizers: Vec::new(),
            git_files: DEFAULT_GIT_FILES.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// Runs `args` with both binaries and asserts that output and state are
    /// identical (all checks, no normalisers). Afterwards the fixture holds the
    /// reference git's result.
    ///
    /// # Panics
    /// On any difference, with a unified diff per differing aspect.
    #[track_caller]
    pub fn same<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.compare(args).run();
        self
    }
}

/// Where a normaliser applies.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Scope {
    Stdout,
    Stderr,
    State,
}

/// A configured comparison; see [`Fixture::compare`].
#[must_use = "a comparison does nothing until .run() or .try_run()"]
pub struct Comparison<'a> {
    fx: &'a mut Fixture,
    args: Vec<OsString>,
    stdin: Option<Vec<u8>>,
    env: Vec<(OsString, OsString)>,
    dir: PathBuf,
    repo: PathBuf,
    checks: BTreeSet<Check>,
    normalizers: Vec<(Scope, Normalizer)>,
    git_files: Vec<String>,
}

/// Output of one side of a comparison.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunResult {
    /// Raw standard output.
    pub stdout: Vec<u8>,
    /// Raw standard error.
    pub stderr: Vec<u8>,
    /// Exit code; `None` if terminated by a signal.
    pub code: Option<i32>,
}

/// Raw results of both sides of a successful comparison.
#[derive(Clone, Debug)]
pub struct Outcome {
    /// The reference git's result.
    pub git: RunResult,
    /// rgit's result.
    pub rgit: RunResult,
}

/// A failed comparison. `Display` and `Debug` both print the full report.
pub struct Mismatch {
    report: String,
}

impl Mismatch {
    /// The readable report (command, binaries and one unified diff per differing aspect).
    pub fn report(&self) -> &str {
        &self.report
    }
}

impl fmt::Display for Mismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.report)
    }
}

impl fmt::Debug for Mismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.report)
    }
}

impl std::error::Error for Mismatch {}

impl Comparison<'_> {
    /// Runs the command in `rel` (relative to the work directory) instead of the work directory.
    pub fn in_dir(mut self, rel: impl Into<PathBuf>) -> Self {
        self.dir = rel.into();
        self
    }

    /// Inspects the repository at `rel` (relative to the work directory)
    /// for the state checks; default is the work directory itself.
    pub fn repo(mut self, rel: impl Into<PathBuf>) -> Self {
        self.repo = rel.into();
        self
    }

    /// Feeds `data` to the command's standard input (default: empty).
    pub fn stdin(mut self, data: impl Into<Vec<u8>>) -> Self {
        self.stdin = Some(data.into());
        self
    }

    /// Sets an environment variable for this command only, on both sides.
    pub fn env(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        self.env
            .push((key.as_ref().to_owned(), value.as_ref().to_owned()));
        self
    }

    /// Checks exactly the given aspects.
    pub fn checks(mut self, checks: impl IntoIterator<Item = Check>) -> Self {
        self.checks = checks.into_iter().collect();
        self
    }

    /// Does not check `check`.
    pub fn skip(mut self, check: Check) -> Self {
        self.checks.remove(&check);
        self
    }

    /// Checks only the exit code.
    pub fn only_exit_code(self) -> Self {
        self.checks([Check::ExitCode])
    }

    /// Checks exit code, stdout and stderr, but not the repository state.
    pub fn output_only(self) -> Self {
        self.checks(Check::OUTPUT)
    }

    /// Applies `n` to stdout and stderr of both sides before comparing.
    pub fn normalize(mut self, n: Normalizer) -> Self {
        self.normalizers.push((Scope::Stdout, n.clone()));
        self.normalizers.push((Scope::Stderr, n));
        self
    }

    /// Applies `n` to stdout only.
    pub fn normalize_stdout(mut self, n: Normalizer) -> Self {
        self.normalizers.push((Scope::Stdout, n));
        self
    }

    /// Applies `n` to stderr only.
    pub fn normalize_stderr(mut self, n: Normalizer) -> Self {
        self.normalizers.push((Scope::Stderr, n));
        self
    }

    /// Applies `n` to the text of every state check (refs, objects, worktree, ...).
    pub fn normalize_state(mut self, n: Normalizer) -> Self {
        self.normalizers.push((Scope::State, n));
        self
    }

    /// Also compares `name` (relative to the git directory) in [`Check::GitFiles`].
    pub fn git_file(mut self, name: impl Into<String>) -> Self {
        self.git_files.push(name.into());
        self
    }

    /// Runs the comparison; panics with a readable report on mismatch.
    /// Returns `None` if the fixture is skipped.
    #[track_caller]
    pub fn run(self) -> Option<Outcome> {
        match self.try_run() {
            Ok(outcome) => outcome,
            Err(mismatch) => panic!("{mismatch}"),
        }
    }

    /// Runs the comparison and returns the mismatch instead of panicking.
    /// In both cases the fixture continues from the reference git's result.
    ///
    /// # Panics
    /// If no rgit binary is configured, or on I/O errors in the temporary directory.
    pub fn try_run(self) -> Result<Option<Outcome>, Mismatch> {
        let Some(mut inner) = self.fx.inner.take() else {
            return Ok(None);
        };
        let result = self.run_both(&mut inner);
        self.fx.inner = Some(inner);
        result
    }

    fn run_both(&self, inner: &mut Inner) -> Result<Option<Outcome>, Mismatch> {
        let rgit = inner.rgit.clone().unwrap_or_else(|| {
            panic!(
                "no rgit binary: call Fixture::rgit(env!(\"CARGO_BIN_EXE_rgit\")) or set {}",
                env::RGIT_BIN_VAR
            )
        });
        let root = inner.root.clone();
        let state = root.join("state");
        let snapshot = root.join("snapshot");
        let git_result = root.join("result-git");
        let rgit_result = root.join("result-rgit");

        fs::rename(&state, &snapshot).expect("move state to snapshot");

        let mut sides = Vec::new();
        for (bin, bin_dir, result_dir) in [
            (&inner.reference, root.join("bin/git"), &git_result),
            (&rgit, root.join("bin/rgit"), &rgit_result),
        ] {
            copy_tree(&snapshot, &state).expect("copy snapshot");
            let run = self.exec(&inner.env, bin, &bin_dir, &state.join("work"));
            let sections = self.collect(inner, &state.join("work"));
            fs::rename(&state, result_dir).expect("move result");
            sides.push((run, sections));
        }
        fs::rename(&git_result, &state).expect("restore reference result");

        let (rgit_run, rgit_sections) = sides.pop().expect("two sides");
        let (git_run, git_sections) = sides.pop().expect("two sides");
        let report = self.report(
            inner,
            &rgit,
            &git_run,
            &rgit_run,
            &git_sections,
            &rgit_sections,
        );

        match report {
            None => {
                let _ = fs::remove_dir_all(&snapshot);
                let _ = fs::remove_dir_all(&rgit_result);
                Ok(Some(Outcome {
                    git: git_run,
                    rgit: rgit_run,
                }))
            }
            Some(mut report) => {
                if env::keep_requested() {
                    // Forget the TempDir so it is not deleted on drop.
                    std::mem::forget(inner.dir.take());
                    report.push_str(&format!(
                        "\nkept: {} (snapshot/ = before, state/ = git, result-rgit/ = rgit)\n",
                        root.display()
                    ));
                } else {
                    let _ = fs::remove_dir_all(&snapshot);
                    let _ = fs::remove_dir_all(&rgit_result);
                    report.push_str(&format!(
                        "\n(set {}=1 to keep the temporary directories)\n",
                        env::KEEP_VAR
                    ));
                }
                Err(Mismatch { report })
            }
        }
    }

    fn exec(&self, base: &BaseEnv, bin: &Path, bin_dir: &Path, work: &Path) -> RunResult {
        let mut cmd = Command::new(bin);
        base.apply(&mut cmd, Some(bin_dir));
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        cmd.args(&self.args)
            .current_dir(work.join(&self.dir))
            .stdin(if self.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd
            .spawn()
            .unwrap_or_else(|e| panic!("cannot run {}: {e}", bin.display()));
        let writer = match (child.stdin.take(), self.stdin.clone()) {
            (Some(mut pipe), Some(data)) => Some(std::thread::spawn(move || {
                // The command may exit without reading all input; that is not an error here.
                let _ = pipe.write_all(&data);
            })),
            _ => None,
        };
        let out = child.wait_with_output().expect("wait for command");
        if let Some(w) = writer {
            let _ = w.join();
        }
        RunResult {
            stdout: out.stdout,
            stderr: out.stderr,
            code: out.status.code(),
        }
    }

    fn collect(&self, inner: &Inner, work: &Path) -> Vec<(Check, String)> {
        let inspector = Inspector {
            reference: &inner.reference,
            env: &inner.env,
            bin_dir: &inner.root.join("bin/git"),
        };
        let repo = work.join(&self.repo);
        self.checks
            .iter()
            .filter(|c| c.is_state())
            .map(|&c| (c, inspector.section(c, work, &repo, &self.git_files)))
            .collect()
    }

    fn normalized(&self, scope: Scope, text: String) -> String {
        self.normalizers
            .iter()
            .filter(|(s, _)| *s == scope)
            .fold(text, |t, (_, n)| n.apply(&t))
    }

    fn report(
        &self,
        inner: &Inner,
        rgit: &Path,
        git_run: &RunResult,
        rgit_run: &RunResult,
        git_sections: &[(Check, String)],
        rgit_sections: &[(Check, String)],
    ) -> Option<String> {
        let mut body = String::new();
        let mut differs = |label: &str, a: &str, b: &str| {
            let d = unified_diff(a, b, &format!("git ({label})"), &format!("rgit ({label})"));
            if !d.is_empty() {
                body.push_str(&format!("\n### {label} differs\n{d}"));
            }
        };

        if self.checks.contains(&Check::ExitCode) {
            differs(
                "exit code",
                &format!("{}\n", code_text(git_run.code)),
                &format!("{}\n", code_text(rgit_run.code)),
            );
        }
        for (check, scope, a, b) in [
            (
                Check::Stdout,
                Scope::Stdout,
                &git_run.stdout,
                &rgit_run.stdout,
            ),
            (
                Check::Stderr,
                Scope::Stderr,
                &git_run.stderr,
                &rgit_run.stderr,
            ),
        ] {
            if self.checks.contains(&check) {
                let a = self.normalized(scope, String::from_utf8_lossy(a).into_owned());
                let b = self.normalized(scope, String::from_utf8_lossy(b).into_owned());
                differs(check.label(), &a, &b);
            }
        }
        for ((check, a), (_, b)) in git_sections.iter().zip(rgit_sections) {
            let a = self.normalized(Scope::State, a.clone());
            let b = self.normalized(Scope::State, b.clone());
            differs(check.label(), &a, &b);
        }

        if body.is_empty() {
            return None;
        }

        let mut report = format!(
            "rgit-difftest: `git {}` differs between git and rgit\n  reference: {}\n  rgit:      {}\n",
            display_args(&self.args),
            inner.reference.display(),
            rgit.display()
        );
        if !self.dir.as_os_str().is_empty() {
            report.push_str(&format!("  cwd:       {}\n", self.dir.display()));
        }
        if !self.repo.as_os_str().is_empty() {
            report.push_str(&format!("  repo:      {}\n", self.repo.display()));
        }
        let checks: Vec<&str> = self.checks.iter().map(|c| c.label()).collect();
        report.push_str(&format!("  checks:    {}\n", checks.join(", ")));
        for (scope, n) in &self.normalizers {
            report.push_str(&format!("  normalise: {scope:?}: {}\n", n.name()));
        }
        report.push_str(&body);
        let shows_output =
            self.checks.contains(&Check::Stdout) && self.checks.contains(&Check::Stderr);
        if git_run.code != rgit_run.code && !shows_output {
            for (name, run) in [("git", git_run), ("rgit", rgit_run)] {
                report.push_str(&format!(
                    "\n### {name} output (for context)\n--- stdout\n{}--- stderr\n{}",
                    String::from_utf8_lossy(&run.stdout),
                    String::from_utf8_lossy(&run.stderr)
                ));
            }
        }
        Some(report)
    }
}

fn code_text(code: Option<i32>) -> String {
    match code {
        Some(c) => format!("exit code {c}"),
        None => "terminated by a signal".to_string(),
    }
}

fn os_args<I, S>(args: I) -> Vec<OsString>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    args.into_iter().map(|a| a.as_ref().to_owned()).collect()
}

fn display_args(args: &[OsString]) -> String {
    args.iter()
        .map(|a| {
            let a = a.to_string_lossy();
            if !a.is_empty()
                && a.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_=./:@%+,^~".contains(c))
            {
                a.into_owned()
            } else {
                format!("'{}'", a.replace('\'', r"'\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Creates `dir/git` pointing at `binary`, so that `git` in `PATH` resolves
/// to the binary of the side being run.
fn link_binary(dir: &Path, binary: &Path) {
    fs::create_dir_all(dir).expect("create bin dir");
    let link = dir.join(if cfg!(windows) { "git.exe" } else { "git" });
    let _ = fs::remove_file(&link);
    let target = binary
        .canonicalize()
        .unwrap_or_else(|_| binary.to_path_buf());
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, &link).expect("link binary");
    #[cfg(not(unix))]
    fs::copy(&target, &link).map(|_| ()).expect("copy binary");
}

/// Recursively copies `from` to `to`, keeping symlinks, permissions and modification times.
fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let src = entry.path();
        let dst = to.join(entry.file_name());
        let ft = entry.file_type()?;
        if ft.is_symlink() {
            let target = fs::read_link(&src)?;
            #[cfg(unix)]
            std::os::unix::fs::symlink(&target, &dst)?;
            #[cfg(not(unix))]
            {
                let _ = target;
                fs::copy(&src, &dst)?;
            }
        } else if ft.is_dir() {
            copy_tree(&src, &dst)?;
            fs::set_permissions(&dst, fs::metadata(&src)?.permissions())?;
        } else {
            fs::copy(&src, &dst)?;
            let mtime = fs::metadata(&src)?.modified()?;
            // Read-only files (loose objects) cannot be opened for writing; the
            // time stamp of those does not matter to git.
            if let Ok(f) = fs::File::options().write(true).open(&dst) {
                f.set_modified(mtime)?;
            }
        }
    }
    Ok(())
}
