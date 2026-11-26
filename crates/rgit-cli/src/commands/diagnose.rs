// SPDX-License-Identifier: MIT
//! `git diagnose` (Documentation/git-diagnose.adoc): a zip archive with
//! the build options, the repository root and free space
//! (`diagnostics.log`), the pack files (`packs-local.txt`) and loose object
//! counts (`objects-local.txt`); `--mode=all` adds the top-level files of
//! the git directory and its `hooks`, `info`, `logs` and `objects/info`
//! directories. The archive code is shared with `git bugreport
//! --diagnose`.
//!
//! Layout and messages observed from git: the log goes to standard output
//! (and into the archive), the closing lines to standard error; an archive
//! of the same name is overwritten.

use std::path::{Path, PathBuf};

use rgit_repo::Setup;

use rgit_archive::zip::{Kind, ZipWriter, needs_utf8_flag};

use crate::messages::bugreport as msg;
use crate::prelude::*;
use crate::repo::{self, Mode};

/// What the archive holds (`--mode`, `bugreport --diagnose=<mode>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnoseMode {
    Stats,
    All,
}

impl DiagnoseMode {
    pub fn parse(v: &str) -> Option<DiagnoseMode> {
        match v {
            "stats" => Some(DiagnoseMode::Stats),
            "all" => Some(DiagnoseMode::All),
            _ => None,
        }
    }
}

/// The current local time formatted with strftime `fmt`, and the time
/// itself (seconds since the epoch) for the archive entries.
pub fn local_now(fmt: &str) -> (String, u64) {
    let dctx = rgit_date::DateContext::from_env();
    let now = u64::try_from(dctx.now).unwrap_or(0);
    let mode = rgit_date::DateMode {
        style: rgit_date::DateStyle::Strftime(fmt.to_string()),
        local: true,
    };
    let text = rgit_date::show_date(now, 0, &mode, &dctx).unwrap_or_default();
    (text, now)
}

/// `statvfs`: available bytes and mount flags of the file system of `path`.
#[cfg(unix)]
fn free_space(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a valid C string and `st` a writable statvfs.
    let rc = unsafe { libc::statvfs(c.as_ptr(), &mut st) };
    if rc != 0 {
        return None;
    }
    #[allow(clippy::unnecessary_cast)]
    Some((st.f_bsize as u64 * st.f_bavail as u64, st.f_flag as u64))
}

#[cfg(not(unix))]
fn free_space(_path: &Path) -> Option<(u64, u64)> {
    None
}

/// The files of `dir` in directory order, recursively, as paths relative
/// to `dir`.
fn walk(dir: &Path, rel: &str, out: &mut Vec<(String, PathBuf)>, recursive: bool) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut subdirs = Vec::new();
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let path = e.path();
        let Ok(ft) = e.file_type() else { continue };
        let r = if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        };
        if ft.is_dir() {
            if recursive {
                subdirs.push((r, path));
            }
        } else if ft.is_file() {
            out.push((r, path));
        }
    }
    for (r, p) in subdirs {
        walk(&p, &r, out, recursive);
    }
}

/// `packs-local.txt`.
fn pack_listing(objects_shown: &str, objects: &Path) -> String {
    let mut s = format!("{}\n", msg::contents_of(objects_shown));
    let mut files = Vec::new();
    walk(&objects.join("pack"), "", &mut files, false);
    for (name, path) in files {
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        s += &format!("{name:<70} {size:>16}\n");
    }
    s
}

/// `objects-local.txt`.
fn loose_counts(objects: &Path) -> String {
    let cwd = std::env::current_dir().unwrap_or_default();
    let abs = rgit_repo::path::real_path_lossy(objects, &cwd);
    let mut s = format!("{}\n", msg::object_dir_stats(&abs.display().to_string()));
    let mut total = 0;
    if let Ok(rd) = std::fs::read_dir(objects) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.len() != 2 || !name.bytes().all(|b| b.is_ascii_hexdigit()) {
                continue;
            }
            let n = std::fs::read_dir(e.path())
                .map(|d| d.flatten().filter(|f| f.path().is_file()).count())
                .unwrap_or(0);
            if n > 0 {
                s += &format!("{name} : {n:>7} files\n");
                total += n;
            }
        }
    }
    s += &msg::total_loose(total);
    s
}

/// Write the archive `zip_path` (shown as `shown`) for the repository of
/// `setup` and print the log and closing lines like git.
pub fn create_archive(
    setup: &Setup,
    zip_path: &Path,
    shown: &str,
    mode: DiagnoseMode,
    now: u64,
) -> Result<(), Die> {
    let root = setup
        .work_tree
        .clone()
        .unwrap_or_else(|| setup.absolute_git_dir());
    let mut log = format!("{}\n\n", msg::COLLECTING);
    log += &format!("{}\n", rgit_version::version_line());
    log += &crate::commands::version::build_options_text();
    log += &format!("{}\n", msg::repository_root(&root.display().to_string()));
    if let Some((avail, flags)) = free_space(&root) {
        log += &format!(
            "{}\n",
            msg::available_space(
                &root.display().to_string(),
                &rgit_pack::index_pack::messages::humanise_bytes(avail),
                flags
            )
        );
    }
    print!("{log}");

    let objects = std::env::var_os("GIT_OBJECT_DIRECTORY")
        .map(PathBuf::from)
        .unwrap_or_else(|| setup.common_dir.join("objects"));
    let objects_shown = objects.display().to_string();

    let file = std::fs::File::create(zip_path).map_err(|e| {
        Die::Fatal(msg::could_not_open_for_writing(
            shown,
            &crate::global::strerror(&e),
        ))
    })?;
    // The zip writer of `archive` (rgit-archive), every entry a regular
    // file with the current time.
    let date = rgit_date::DateContext::from_env();
    let t = i64::try_from(now).unwrap_or(i64::MAX);
    let local = t + rgit_date::tz_to_minutes(date.local_tz(t)) * 60;
    let mut zip = ZipWriter::new(std::io::BufWriter::new(file), 6, now, local);
    let werr = |e: std::io::Error| Die::Fatal(e.to_string());
    let add = |zip: &mut ZipWriter<_>, name: &str, data: &[u8]| {
        let utf8 = needs_utf8_flag(name.as_bytes()).unwrap_or(false);
        zip.add(name.as_bytes(), Kind::File { executable: false }, data, utf8, false)
    };
    add(&mut zip, "diagnostics.log", log.as_bytes()).map_err(werr)?;
    add(
        &mut zip,
        "packs-local.txt",
        pack_listing(&objects_shown, &objects).as_bytes(),
    )
    .map_err(werr)?;
    add(&mut zip, "objects-local.txt", loose_counts(&objects).as_bytes()).map_err(werr)?;
    if mode == DiagnoseMode::All {
        let git_dir = &setup.git_dir;
        let base = setup
            .absolute_git_dir()
            .file_name()
            .map_or_else(|| ".git".to_string(), |n| n.to_string_lossy().into_owned());
        let mut files = Vec::new();
        walk(git_dir, "", &mut files, false);
        for sub in ["hooks", "info", "logs", "objects/info"] {
            walk(&git_dir.join(sub), sub, &mut files, true);
        }
        for (rel, path) in files {
            if let Ok(data) = std::fs::read(&path) {
                add(&mut zip, &format!("{base}/{rel}"), &data).map_err(werr)?;
            }
        }
    }
    zip.finish(b"").map_err(werr)?;
    eprintln!(
        "\n{}\n{}",
        msg::DIAGNOSTICS_COMPLETE,
        msg::captured_in(shown)
    );
    Ok(())
}

/// The output path for `name` in directory `dir` (as given, relative to
/// the original directory): the path to open and the path to show.
pub fn output_path(setup: Option<&Setup>, dir: &str, name: &str) -> (PathBuf, String) {
    let mut shown = String::new();
    if let Some(p) = setup.and_then(|s| s.prefix.as_ref()) {
        if !Path::new(dir).is_absolute() {
            shown.push_str(&p.to_string_lossy());
        }
    }
    shown.push_str(dir);
    if !shown.is_empty() && !shown.ends_with('/') {
        shown.push('/');
    }
    shown.push_str(name);
    (PathBuf::from(&shown), shown)
}

/// Create the directories leading to `path`.
pub fn create_leading(path: &Path, shown: &str) -> Result<(), Die> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .map_err(|_| Die::Fatal(msg::could_not_create_leading_directories(shown)))?;
    }
    Ok(())
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let output: RefCell<Option<String>> = RefCell::new(None);
    let suffix: RefCell<Option<String>> = RefCell::new(None);
    let mode = Cell::new(DiagnoseMode::Stats);
    let mut parser = Parser::new(
        &[msg::DIAGNOSE_USAGE],
        vec![
            Opt::string(
                'o',
                "output-directory",
                &output,
                "path",
                msg::HELP_DIAGNOSE_OUTPUT_DIRECTORY,
            ),
            Opt::string('s', "suffix", &suffix, "format", msg::HELP_DIAGNOSE_SUFFIX),
            Opt::callback(None, "mode", Some("(stats|all)"), msg::HELP_MODE, |v, _| {
                let v = v.unwrap_or("");
                mode.set(
                    DiagnoseMode::parse(v)
                        .ok_or_else(|| CallbackError::Error(msg::invalid_mode_value(v)))?,
                );
                Ok(())
            })
            .literal_argh()
            .noneg(),
        ],
    );
    parser.parse(argv)?;
    drop(parser);
    let setup = repo::setup(ctx, Mode::Required)?;
    let fmt = suffix
        .into_inner()
        .unwrap_or_else(|| "%Y-%m-%d-%H%M".to_string());
    let (stamp, now) = local_now(&fmt);
    let dir = output.into_inner().unwrap_or_default();
    let (path, shown) = output_path(Some(&setup), &dir, &format!("git-diagnostics-{stamp}.zip"));
    // git goes on when the directories cannot be made; opening the
    // archive then reports the problem.
    let _ = create_leading(&path, &shown);
    create_archive(&setup, &path, &shown, mode.get(), now)?;
    Ok(0)
}
