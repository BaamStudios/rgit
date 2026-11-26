// SPDX-License-Identifier: MIT
//! `git archive` (Documentation/git-archive.adoc,
//! Documentation/config/tar.adoc): an archive of a tree as tar, zip or a
//! filtered tar, made locally or fetched from `git upload-archive`.

#[path = "archive/client.rs"]
mod client;
#[path = "archive/engine.rs"]
pub mod engine;

use rgit_config::ConfigSet;
use rgit_object::ObjectKind;
use rgit_pathspec::{ParseOptions, Pathspec};

use crate::messages::archive as msg;
use crate::prelude::*;
use crate::repo::Mode;
use crate::repository::Repo;
use engine::{Backend, Extra, INTERNAL_GZIP, Job};

/// An archive format: built in or `tar.<name>.command`.
#[derive(Debug, Clone)]
pub struct Format {
    pub name: String,
    pub backend: Backend,
    /// Offered to remote clients (`tar.<name>.remote`).
    pub remote: bool,
}

/// The formats in the order `--list` shows them: `tar`, the tar filters
/// (`tgz` and `tar.gz` first, then those configured), `zip`.
pub fn formats(config: &ConfigSet) -> Vec<Format> {
    let mut filters: Vec<(String, Option<String>, Option<bool>)> = vec![
        ("tgz".into(), Some(INTERNAL_GZIP.into()), Some(true)),
        ("tar.gz".into(), Some(INTERNAL_GZIP.into()), Some(true)),
    ];
    for e in config.entries() {
        let name = String::from_utf8_lossy(&e.name).into_owned();
        let Some(rest) = name.strip_prefix("tar.") else {
            continue;
        };
        let Some((sub, key)) = rest.rsplit_once('.') else {
            continue;
        };
        if key != "command" && key != "remote" {
            continue;
        }
        let slot = match filters.iter().position(|f| f.0 == sub) {
            Some(i) => i,
            None => {
                filters.push((sub.to_string(), None, None));
                filters.len() - 1
            }
        };
        let value = e.value.as_ref().map(|v| String::from_utf8_lossy(v).into_owned());
        if key == "command" {
            filters[slot].1 = value;
        } else {
            let b = rgit_config::value::parse_bool_text(value.as_deref().map(str::as_bytes))
                .unwrap_or(false);
            filters[slot].2 = Some(b);
        }
    }
    let mut out = vec![Format {
        name: "tar".into(),
        backend: Backend::Tar,
        remote: true,
    }];
    for (name, command, remote) in filters {
        let Some(command) = command else { continue };
        out.push(Format {
            name,
            backend: Backend::Filter(command),
            remote: remote.unwrap_or(false),
        });
    }
    out.push(Format {
        name: "zip".into(),
        backend: Backend::Zip,
        remote: true,
    });
    out
}

/// The format `-o <file>` implies: the first whose name is the file's
/// extension (after a dot).
pub fn format_from_filename<'a>(formats: &'a [Format], file: &str) -> Option<&'a Format> {
    formats.iter().find(|f| {
        file.len() > f.name.len()
            && file.ends_with(&f.name)
            && file.as_bytes()[file.len() - f.name.len() - 1] == b'.'
    })
}

/// Split `--add-virtual-file=<path>:<content>`.
fn parse_virtual(arg: &str) -> Result<Extra, Die> {
    let bytes = arg.as_bytes();
    let (path, rest) = if bytes.first() == Some(&b'"') {
        let mut i = 1;
        let mut end = None;
        while i < bytes.len() {
            match bytes[i] {
                b'\\' => i += 2,
                b'"' => {
                    end = Some(i);
                    break;
                }
                _ => i += 1,
            }
        }
        let end = end.ok_or_else(|| Die::Fatal(msg::unclosed_quote(arg)))?;
        let path = rgit_pathspec::unquote_c_style(&bytes[..=end])
            .ok_or_else(|| Die::Fatal(msg::unclosed_quote(arg)))?;
        (path, &bytes[end + 1..])
    } else {
        match bytes.iter().position(|&b| b == b':') {
            Some(i) => (bytes[..i].to_vec(), &bytes[i..]),
            None => return Err(Die::Fatal(msg::missing_colon(arg))),
        }
    };
    let Some(content) = rest.strip_prefix(b":") else {
        return Err(Die::Fatal(msg::missing_colon(arg)));
    };
    Ok(Extra::Virtual {
        path,
        content: content.to_vec(),
    })
}

/// `-h` / `--help-all` of the commands that only have a usage line:
/// the line on standard output. `None` for any other arguments.
pub fn usage_line_help(argv: &[String], usage: &str) -> Option<CmdResult> {
    let args = &argv[1..];
    (args.len() == 1 && (args[0] == "-h" || args[0] == "--help-all")).then(|| {
        println!("{usage}");
        Ok(0)
    })
}

/// The usage line on standard error, exit code 129.
pub fn usage_line_error(usage: &str) -> Die {
    eprintln!("{usage}");
    Die::Exit(rgit_options::EXIT_USAGE)
}

/// How the archiver was started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// `git archive` on the command line.
    Local,
    /// By `upload-archive` for a remote client.
    Remote,
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    // First the options that decide where the archive is made.
    let remote = RefCell::new(None);
    let exec = RefCell::new(None);
    let output = RefCell::new(None);
    let rest = {
        let mut parser = Parser::new(
            msg::USAGE,
            vec![
                Opt::string(None, "remote", &remote, "repo", msg::HELP_REMOTE),
                Opt::string(None, "exec", &exec, "command", msg::HELP_EXEC),
                Opt::string('o', "output", &output, "file", msg::HELP_OUTPUT),
            ],
        )
        .flags(ParseFlags {
            keep_unknown_opt: true,
            keep_dashdash: true,
            no_internal_help: true,
            ..Default::default()
        });
        parser.parse(argv)?.args
    };
    // Relative to where we started (setting up a repository may change
    // the directory).
    let output = output.into_inner().map(|o| match std::env::current_dir() {
        Ok(dir) if !std::path::Path::new(&o).is_absolute() => {
            dir.join(&o).to_string_lossy().into_owned()
        }
        _ => o,
    });
    if let Some(remote) = remote.into_inner() {
        return client::run(ctx, &remote, exec.into_inner(), output, rest);
    }
    if exec.borrow().is_some() {
        return Err(Die::Fatal(msg::EXEC_REQUIRES_REMOTE.into()));
    }
    let mut args = vec![argv[0].clone()];
    args.extend(rest);
    archive(ctx, &args, output, Origin::Local)
}

/// Make the archive `argv` describes, writing it to `output` (standard
/// output when `None`).
pub fn archive(ctx: &Context, argv: &[String], output: Option<String>, origin: Origin) -> CmdResult {
    let format = RefCell::new(None);
    let events: RefCell<Vec<(u8, String)>> = RefCell::new(Vec::new());
    let worktree_attributes = Cell::new(false);
    let verbose = Cell::new(false);
    let mtime = RefCell::new(None);
    let level = RefCell::new(None);
    let list = Cell::new(false);
    let remote = RefCell::new(None);
    let exec = RefCell::new(None);
    let out_opt = RefCell::new(None);
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::string(None, "format", &format, "fmt", msg::HELP_FORMAT),
            Opt::callback(None, "prefix", Some("prefix"), msg::HELP_PREFIX, |v, unset| {
                let v = if unset { String::new() } else { v.unwrap_or("").to_string() };
                events.borrow_mut().push((b'p', v));
                Ok(())
            }),
            Opt::callback(None, "add-file", Some("file"), msg::HELP_ADD_FILE, |v, unset| {
                if !unset {
                    events.borrow_mut().push((b'f', v.unwrap_or("").to_string()));
                }
                Ok(())
            }),
            Opt::callback(
                None,
                "add-virtual-file",
                Some("path:content"),
                msg::HELP_ADD_FILE,
                |v, unset| {
                    if !unset {
                        events.borrow_mut().push((b'v', v.unwrap_or("").to_string()));
                    }
                    Ok(())
                },
            ),
            Opt::string('o', "output", &out_opt, "file", msg::HELP_OUTPUT),
            Opt::flag(
                None,
                "worktree-attributes",
                &worktree_attributes,
                msg::HELP_WORKTREE_ATTRIBUTES,
            ),
            Opt::flag('v', "verbose", &verbose, msg::HELP_VERBOSE),
            Opt::string(None, "mtime", &mtime, "time", msg::HELP_MTIME).noneg(),
            Opt::number(msg::HELP_LEVEL, |n| {
                *level.borrow_mut() = Some(n.to_string());
                Ok(())
            }),
            Opt::group(""),
            Opt::flag('l', "list", &list, msg::HELP_LIST),
            Opt::group(""),
            Opt::string(None, "remote", &remote, "repo", msg::HELP_REMOTE),
            Opt::string(None, "exec", &exec, "command", msg::HELP_EXEC),
        ],
    );
    let args = parser.parse(argv)?.args;
    let usage = parser.usage_error();
    drop(parser);
    let format = format.into_inner();
    let level = level.into_inner();
    let events = events.into_inner();
    if origin == Origin::Remote {
        if out_opt.borrow().is_some() {
            return Err(Die::Fatal(msg::unexpected_option("--output")));
        }
        if remote.borrow().is_some() {
            return Err(Die::Fatal(msg::unexpected_option("--remote")));
        }
    }
    if exec.borrow().is_some() && remote.borrow().is_none() {
        return Err(Die::Fatal(msg::EXEC_REQUIRES_REMOTE.into()));
    }
    // Paths on the command line are relative to where we started; setting
    // up the repository may change the directory.
    let start = std::env::current_dir().ok();
    let absolute = |p: &str| -> String {
        match &start {
            Some(dir) if !std::path::Path::new(p).is_absolute() => {
                dir.join(p).to_string_lossy().into_owned()
            }
            _ => p.to_string(),
        }
    };
    let output = output.or_else(|| out_opt.into_inner()).map(|o| absolute(&o));

    let mut repo = if list.get() {
        Repo::open_gently(ctx)?
    } else {
        Some(Repo::open(ctx, Mode::Required)?)
    };
    let config = match &repo {
        Some(r) => r.config.clone(),
        None => crate::repo::config_outside_repository(ctx),
    };
    let all = formats(&config);
    let available: Vec<&Format> = all
        .iter()
        .filter(|f| origin == Origin::Local || f.remote)
        .collect();
    if list.get() {
        if let Some(extra) = args.first() {
            return Err(Die::Fatal(msg::extra_parameter(extra)));
        }
        let mut out = String::new();
        for f in &available {
            out.push_str(&f.name);
            out.push('\n');
        }
        print!("{out}");
        return Ok(0);
    }
    let Some(repo) = repo.as_mut() else {
        return Err(Die::Fatal(msg::NOT_A_REPOSITORY.into()));
    };
    let Some(name) = args.first() else {
        return Err(usage.into());
    };

    let fmt_name = format.clone().or_else(|| {
        output
            .as_deref()
            .and_then(|o| format_from_filename(&all, o))
            .map(|f| f.name.clone())
    });
    let fmt = match &fmt_name {
        None => &all[0],
        Some(n) => *available
            .iter()
            .find(|f| &f.name == n)
            .ok_or_else(|| Die::Fatal(msg::unknown_format(n)))?,
    };
    if let Some(l) = &level {
        let ok = match fmt.backend {
            Backend::Tar => false,
            Backend::Zip => l.parse::<u32>().is_ok_and(|n| n <= 9),
            Backend::Filter(_) => true,
        };
        if !ok {
            return Err(Die::Fatal(msg::argument_not_supported(&fmt.name, l)));
        }
    }

    // The tree-ish; remote clients may only name refs.
    if origin == Origin::Remote {
        let allow = repo
            .config
            .bool("uploadarchive.allowunreachable")
            .ok()
            .flatten()
            .unwrap_or(false);
        if !allow {
            let refpart = name.split(':').next().unwrap_or(name);
            if !is_ref(repo, refpart) {
                return Err(Die::Fatal(msg::no_such_ref(refpart)));
            }
        }
    }
    let id = repo
        .resolve(name)
        .ok_or_else(|| Die::Fatal(msg::not_valid_object_name(name)))?;
    let commit = repo.peel_to(id, ObjectKind::Commit);
    let tree = repo
        .peel_to(id, ObjectKind::Tree)
        .ok_or_else(|| Die::Fatal(msg::not_a_tree(&id.to_hex())))?;
    let date = rgit_date::DateContext::from_env();
    let mut time = match commit {
        Some(c) => commit_time(repo, &c).unwrap_or(0),
        // The current time; git does not take GIT_TEST_DATE_NOW here.
        None => std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
    };
    if let Some(m) = mtime.borrow().as_deref() {
        time = rgit_date::approxidate(m, &date);
    }

    // Paths: below the current directory unless remote.
    let base = if origin == Origin::Remote {
        Vec::new()
    } else {
        repo.prefix()
    };
    let root = repo
        .setup
        .work_tree
        .as_ref()
        .map(|w| w.as_os_str().as_encoded_bytes().to_vec());
    let opts = ParseOptions {
        prefix: base.clone(),
        root,
        globals: crate::repository::pathspec_globals(ctx),
        ..Default::default()
    };
    let pathspec =
        Pathspec::parse(&args[1..], &opts).map_err(|e| Die::Fatal(e.to_string()))?;
    if !base.is_empty() {
        for item in pathspec.items().iter().filter(|i| !i.is_exclude()) {
            let p = item.pattern();
            let inside = p.starts_with(&base) || [p, b"/"].concat() == base;
            if !inside {
                return Err(Die::Fatal(msg::pathspec_outside(&String::from_utf8_lossy(
                    item.original(),
                ))));
            }
        }
    }

    // Untracked files and their prefixes, in command line order.
    let mut prefix = Vec::new();
    let mut extras = Vec::new();
    for (kind, value) in &events {
        match kind {
            b'p' => prefix = value.as_bytes().to_vec(),
            b'f' => extras.push(Extra::File {
                prefix: prefix.clone(),
                path: value.clone(),
                on_disk: absolute(value),
            }),
            _ => extras.push(parse_virtual(value)?),
        }
    }

    let umask = match repo.config.get("tar.umask").and_then(|e| e.value.as_ref()) {
        Some(v) if v.as_slice() == b"user" => user_umask(),
        _ => repo
            .config
            .int("tar.umask")
            .map_err(|e| Die::Fatal(e.to_string()))?
            .map_or(0o002, |v| v as u32),
    };
    let big_file_threshold = repo
        .config
        .int64("core.bigfilethreshold")
        .map_err(|e| Die::Fatal(e.to_string()))?
        .map_or(512 << 20, |v| v.max(0) as u64);

    let tree_source = if worktree_attributes.get() {
        crate::attributes::tree_source(ctx, repo, None)?
    } else {
        None
    };
    let index = if worktree_attributes.get() && repo.setup.work_tree.is_some() {
        repo.read_index().ok().map(|(i, _)| i)
    } else {
        None
    };
    let job = Job {
        repo,
        ctx,
        tree,
        commit,
        mtime: time,
        base,
        prefix,
        pathspec,
        backend: fmt.backend.clone(),
        level,
        verbose: verbose.get(),
        worktree_attributes: worktree_attributes.get(),
        extras,
        umask,
        big_file_threshold,
        output,
    };
    engine::run(&job, index.as_ref(), tree_source)?;
    Ok(0)
}

/// Whether `name` is a reference by git's lookup rules.
fn is_ref(repo: &Repo, name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let candidates = [
        name.to_string(),
        format!("refs/{name}"),
        format!("refs/tags/{name}"),
        format!("refs/heads/{name}"),
        format!("refs/remotes/{name}"),
        format!("refs/remotes/{name}/HEAD"),
    ];
    candidates.iter().any(|c| {
        rgit_refs::name::is_valid_refname(c.as_bytes(), rgit_refs::name::RefNameFlags::ONELEVEL)
            && repo.refs.resolve(c).is_some()
    })
}

fn commit_time(repo: &Repo, id: &rgit_hash::ObjectId) -> Option<u64> {
    let raw = repo.odb().read(id).ok()??;
    let commit = rgit_object::Commit::parse(repo.hash, &raw.data).ok()?;
    let t = commit.committer()?.time()?;
    Some(t.seconds.max(0) as u64)
}

/// The process umask (`tar.umask = user`).
// `mode_t` is `u32` on Linux but not everywhere.
#[allow(clippy::useless_conversion)]
fn user_umask() -> u32 {
    #[cfg(unix)]
    {
        // SAFETY: umask only swaps the process file mode mask; the old
        // value is restored right away.
        unsafe {
            let old = libc::umask(0);
            libc::umask(old);
            u32::from(old)
        }
    }
    #[cfg(not(unix))]
    {
        0o002
    }
}
