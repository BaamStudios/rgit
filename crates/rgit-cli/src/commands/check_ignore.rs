// SPDX-License-Identifier: MIT
//! `git check-ignore` (Documentation/git-check-ignore.adoc): report which
//! paths the ignore rules exclude, and with `-v` the deciding pattern.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use rgit_config::sources::{self, RepoConfig, ScopeFilter};
use rgit_config::{ConfigSet, Environment, LoadOptions, StaticRepoFacts, SystemEnv};
use rgit_index::{Index, ReadOptions};
use rgit_pathspec::{GlobalFlags, Magic, ParseOptions, Pathspec, unquote_c_style};
use rgit_worktree::ignore::{Excludes, default_excludes_file};

use crate::messages::check_ignore as msg;
use crate::prelude::*;
use crate::repo::{self, Mode};

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    // Like git, `-h` works outside a repository; everything else needs one.
    let help_only = argv.len() == 2 && (argv[1] == "-h" || argv[1] == "--help-all");
    let setup = if help_only {
        None
    } else {
        Some(repo::setup(ctx, Mode::Required)?)
    };

    let quiet = Cell::new(false);
    let verbose = Cell::new(false);
    let stdin = Cell::new(false);
    let nul = Cell::new(false);
    let non_matching = Cell::new(false);
    let no_index = Cell::new(false);
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::flag('q', "quiet", &quiet, msg::HELP_QUIET),
            Opt::flag('v', "verbose", &verbose, msg::HELP_VERBOSE),
            Opt::group(""),
            Opt::flag(None, "stdin", &stdin, msg::HELP_STDIN),
            Opt::flag('z', None, &nul, msg::HELP_Z),
            Opt::flag('n', "non-matching", &non_matching, msg::HELP_NON_MATCHING),
            Opt::flag(None, "no-index", &no_index, msg::HELP_NO_INDEX),
        ],
    );
    let args = parser.parse(argv)?.args;
    let Some(setup) = setup else {
        return Ok(0);
    };
    let work_tree = setup.require_work_tree().map_err(repo::die)?.to_path_buf();

    if stdin.get() {
        if !args.is_empty() {
            return Err(Die::Fatal(msg::STDIN_WITH_PATHS.into()));
        }
    } else {
        if nul.get() {
            return Err(Die::Fatal(msg::Z_WITHOUT_STDIN.into()));
        }
        if args.is_empty() {
            return Err(Die::Fatal(msg::NO_PATH.into()));
        }
    }
    if quiet.get() {
        if args.len() > 1 {
            return Err(Die::Fatal(msg::QUIET_SINGLE.into()));
        }
        if verbose.get() {
            return Err(Die::Fatal(msg::QUIET_VERBOSE.into()));
        }
    }
    if non_matching.get() && !verbose.get() {
        return Err(Die::Fatal(msg::NON_MATCHING_VERBOSE.into()));
    }

    let env = SystemEnv::default();
    let config = load_config(ctx, &setup, &env);
    let quote_path = config
        .bool("core.quotepath")
        .ok()
        .flatten()
        .unwrap_or(true);

    let index = if no_index.get() {
        None
    } else {
        let path = rgit_index::index_path(&setup.git_dir);
        let kind = setup.format.object_format;
        match Index::read_file(&path, kind, &ReadOptions::default()) {
            Ok(i) => Some(i),
            Err(e) => return Err(Die::Fatal(e.to_string())),
        }
    };

    let mut excludes = Excludes::new(&work_tree);
    excludes.set_ignore_case(
        config
            .bool("core.ignorecase")
            .ok()
            .flatten()
            .unwrap_or(false),
    );
    let excludes_file: Option<PathBuf> = match config.path("core.excludesfile", &env) {
        Ok(Some(p)) => Some(p),
        _ => default_excludes_file(|k| std::env::var_os(k)),
    };
    let info = setup.common_dir.join("info").join("exclude");
    excludes.add_standard(
        excludes_file.as_deref().map(|p| (p, path_bytes(p))),
        (&info, path_bytes(&info)),
    );

    let mut checker = Check {
        work_tree: &work_tree,
        prefix: setup
            .prefix
            .as_ref()
            .map(|p| p.as_encoded_bytes().to_vec())
            .unwrap_or_default(),
        globals: global_flags(ctx),
        index: index.as_ref(),
        excludes,
        quiet: quiet.get(),
        verbose: verbose.get(),
        non_matching: non_matching.get(),
        nul: nul.get(),
        quote_path,
        found: 0,
        out: std::io::stdout().lock(),
    };

    if stdin.get() {
        let term = if nul.get() { b'\0' } else { b'\n' };
        let mut input = std::io::stdin().lock();
        let mut line = Vec::new();
        loop {
            line.clear();
            let n = input
                .read_until(term, &mut line)
                .map_err(|e| Die::Fatal(e.to_string()))?;
            if n == 0 {
                break;
            }
            if line.last() == Some(&term) {
                line.pop();
            }
            let arg = if !nul.get() && line.first() == Some(&b'"') {
                unquote_c_style(&line).ok_or_else(|| Die::Fatal(msg::BADLY_QUOTED.into()))?
            } else {
                line.clone()
            };
            let path = checker.resolve(&arg)?;
            checker.report(&arg, &path)?;
            let _ = checker.out.flush();
        }
    } else {
        let mut resolved = Vec::with_capacity(args.len());
        for a in &args {
            resolved.push(checker.resolve(a.as_bytes())?);
        }
        for (a, path) in args.iter().zip(&resolved) {
            checker.report(a.as_bytes(), path)?;
        }
    }
    let _ = checker.out.flush();
    Ok(if checker.found > 0 { 0 } else { 1 })
}

struct Check<'a, W: Write> {
    work_tree: &'a Path,
    prefix: Vec<u8>,
    globals: GlobalFlags,
    index: Option<&'a Index>,
    excludes: Excludes,
    quiet: bool,
    verbose: bool,
    non_matching: bool,
    nul: bool,
    quote_path: bool,
    found: usize,
    out: W,
}

impl<W: Write> Check<'_, W> {
    /// The work tree path named by the argument `arg`, after the checks git
    /// applies to check-ignore's pathspecs.
    fn resolve(&self, arg: &[u8]) -> Result<Vec<u8>, Die> {
        let opts = ParseOptions {
            prefix: self.prefix.clone(),
            root: Some(path_bytes(self.work_tree)),
            globals: self.globals,
            allowed_magic: Magic::TOP,
            ..ParseOptions::default()
        };
        let spec = Pathspec::parse([arg], &opts).map_err(|e| Die::Fatal(e.to_string()))?;
        let path = spec
            .items()
            .first()
            .map(|i| i.pattern().to_vec())
            .unwrap_or_default();
        let shown = String::from_utf8_lossy(arg);
        for (i, &b) in path.iter().enumerate() {
            if b != b'/' {
                continue;
            }
            let lead = self.work_tree.join(path_from_bytes(&path[..i]));
            if std::fs::symlink_metadata(&lead).is_ok_and(|m| m.file_type().is_symlink()) {
                return Err(Die::Fatal(msg::beyond_symlink(&shown)));
            }
        }
        if let Some(index) = self.index {
            for e in index.entries() {
                if e.mode.is_gitlink()
                    && path.len() > e.path().len()
                    && path.starts_with(e.path())
                    && path[e.path().len()] == b'/'
                {
                    let sub = String::from_utf8_lossy(e.path());
                    return Err(Die::Fatal(msg::in_submodule(&shown, &sub)));
                }
            }
        }
        Ok(path)
    }

    /// Whether `path` (possibly with a trailing `/`) is in the index, as a
    /// file or as a directory containing entries.
    fn tracked(&self, path: &[u8]) -> bool {
        let Some(index) = self.index else {
            return false;
        };
        let p = path.strip_suffix(b"/").unwrap_or(path);
        !index.entries_for_path(p).is_empty() || !index.dir_range(p).is_empty()
    }

    fn report(&mut self, arg: &[u8], path: &[u8]) -> Result<(), Die> {
        let hit = if path.is_empty() || self.tracked(path) {
            None
        } else {
            let full = self.work_tree.join(path_from_bytes(path));
            let is_dir = std::fs::symlink_metadata(full).is_ok_and(|m| m.is_dir());
            self.excludes
                .match_with_leading_dirs(path, is_dir)
                .map(|m| {
                    (
                        m.list.source().unwrap_or_default().to_vec(),
                        m.pattern.line(),
                        m.pattern.text().to_vec(),
                        m.is_excluded(),
                    )
                })
        };
        for w in self.excludes.take_warnings() {
            eprintln!("warning: {w}");
        }
        let mut rec = Vec::new();
        match hit {
            Some((source, line, pattern, excluded)) if excluded || self.verbose => {
                self.found += 1;
                if self.quiet {
                    return Ok(());
                }
                if self.verbose {
                    self.field(&mut rec, &source);
                    rec.push(if self.nul { b'\0' } else { b':' });
                    rec.extend_from_slice(line.to_string().as_bytes());
                    rec.push(if self.nul { b'\0' } else { b':' });
                    rec.extend_from_slice(&pattern);
                    rec.push(if self.nul { b'\0' } else { b'\t' });
                }
            }
            _ if self.non_matching => {
                if self.nul {
                    rec.extend_from_slice(b"\0\0\0");
                } else {
                    rec.extend_from_slice(b"::\t");
                }
            }
            _ => return Ok(()),
        }
        self.field(&mut rec, arg);
        rec.push(if self.nul { b'\0' } else { b'\n' });
        self.out
            .write_all(&rec)
            .map_err(|e| Die::Fatal(e.to_string()))
    }

    /// Appends a path field, C-quoted unless `-z` is given.
    fn field(&self, rec: &mut Vec<u8>, s: &[u8]) {
        if self.nul {
            rec.extend_from_slice(s);
        } else {
            quote_c_style(rec, s, self.quote_path);
        }
    }
}

/// git's path quoting: `"..."` with C escapes when the path contains a
/// control character, `"`, `\` or (with `core.quotePath`) a byte >= 0x80.
fn quote_c_style(out: &mut Vec<u8>, name: &[u8], quote_high: bool) {
    let needs = |c: u8| c < 0x20 || c == b'"' || c == b'\\' || c == 0x7f || (quote_high && c >= 0x80);
    if !name.iter().any(|&c| needs(c)) {
        out.extend_from_slice(name);
        return;
    }
    out.push(b'"');
    for &c in name {
        match c {
            0x07 => out.extend_from_slice(b"\\a"),
            0x08 => out.extend_from_slice(b"\\b"),
            b'\t' => out.extend_from_slice(b"\\t"),
            b'\n' => out.extend_from_slice(b"\\n"),
            0x0b => out.extend_from_slice(b"\\v"),
            0x0c => out.extend_from_slice(b"\\f"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            c if needs(c) => out.extend_from_slice(format!("\\{c:03o}").as_bytes()),
            c => out.push(c),
        }
    }
    out.push(b'"');
}

fn global_flags(ctx: &Context) -> GlobalFlags {
    let mut g = GlobalFlags::from_env().unwrap_or_default();
    g.literal |= ctx.literal_pathspecs;
    g.glob |= ctx.glob_pathspecs;
    g.noglob |= ctx.noglob_pathspecs;
    g.icase |= ctx.icase_pathspecs;
    g
}

/// The configuration (all scopes); errors leave what could be read.
fn load_config(ctx: &Context, setup: &rgit_repo::Setup, env: &dyn Environment) -> ConfigSet {
    let facts = StaticRepoFacts {
        git_dir: std::path::absolute(&setup.git_dir).ok(),
        worktree: setup.work_tree.clone(),
        branch: None,
    };
    let opts = LoadOptions {
        env,
        repo: Some(&facts),
        includes: true,
    };
    let repo_cfg = RepoConfig {
        common_dir: setup.common_dir.clone(),
        git_dir: setup.git_dir.clone(),
        worktree_config: None,
    };
    let mut set = ConfigSet::new();
    let _ = sources::load_standard(
        opts,
        Some(&repo_cfg),
        &repo::command_line(ctx),
        ScopeFilter::default(),
        &mut set,
    );
    set
}

#[cfg(unix)]
fn path_bytes(p: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    p.as_os_str().as_bytes().to_vec()
}

#[cfg(not(unix))]
fn path_bytes(p: &Path) -> Vec<u8> {
    p.to_string_lossy().into_owned().into_bytes()
}

#[cfg(unix)]
fn path_from_bytes(b: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    PathBuf::from(std::ffi::OsStr::from_bytes(b))
}

#[cfg(not(unix))]
fn path_from_bytes(b: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(b).into_owned())
}
