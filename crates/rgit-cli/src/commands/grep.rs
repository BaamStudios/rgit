// SPDX-License-Identifier: MIT
//! `git grep` (Documentation/git-grep.adoc, Documentation/config/grep.adoc):
//! search the tracked files of the work tree, the index (`--cached`),
//! trees and blobs (`<rev>...`), untracked files too (`--untracked`) or a
//! plain directory (`--no-index`, `grep.fallbackToNoIndex`).
//!
//! Matching and printing are in [`crate::grep`]. Behaviour checked against
//! the `git` binary:
//!
//! - The work tree search reads the tracked regular files (symbolic links
//!   and submodules are skipped); entries marked assume-unchanged or
//!   skip-worktree are read from the index instead, unless they are
//!   intent-to-add. `--cached` skips intent-to-add entries.
//! - Without `--`, the arguments after the pattern are revisions as long
//!   as they resolve, then paths that must exist. Before `--` every
//!   argument must be a revision. Files of a tree are shown as
//!   `<rev>:<path>`, a blob as `<rev>`.
//! - Paths are shown relative to the current directory (`--full-name`:
//!   to the top) and quoted unless `-z`; without pathspecs, only the
//!   current directory is searched.
//! - `--max-depth` counts the directories below the pathspec item a path
//!   matched; items with wildcards do not limit the depth.
//! - The `diff` attribute decides about binary files (`-diff` binary,
//!   `diff` text, `diff=<driver>` by `diff.<driver>.binary`); the driver's
//!   `funcname`/`xfuncname` define function lines for `-p`/`-W`, its
//!   `textconv` is used with `--textconv`.
//!
//! Not implemented yet: `--recurse-submodules`; the built-in function
//! name patterns of `diff=<language>` drivers (#147); threads (`--threads`
//! is validated and otherwise ignored).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use rgit_attr::State;
use rgit_config::ConfigSet;
use rgit_index::Index;
use rgit_object::{Object, ObjectKind};
use rgit_pathspec::{DirInterest, ParseOptions, Pathspec};

use crate::grep::pattern::{Atom, Matcher, Syntax, Token};
use crate::grep::{Colors, FuncName, Options, Printer, Source};
use crate::messages::grep as msg;
use crate::prelude::*;
use crate::quote::{relative_to, write_quoted};
use crate::repo::Mode;
use crate::repository::{self, Repo};
use crate::revision::{Failure, Resolver};

const TYPE_UNSET: i32 = 0;
const TYPE_BASIC: i32 = 1;
const TYPE_EXTENDED: i32 = 2;
const TYPE_FIXED: i32 = 3;
const TYPE_PERL: i32 = 4;

/// Parse a context length (`-A`, `-B`, `-C`, `-<n>`).
fn context_len(v: Option<&str>, switch: char) -> Result<usize, CallbackError> {
    v.unwrap_or_default()
        .parse::<usize>()
        .map_err(|_| CallbackError::Error(msg::expects_numerical(switch)))
}

/// Read the patterns of `-f <file>` (`-` is standard input), one per
/// line; empty lines are skipped.
fn pattern_file(file: &str) -> Result<Vec<String>, CallbackError> {
    let data = if file == "-" {
        let mut d = Vec::new();
        std::io::stdin()
            .read_to_end(&mut d)
            .map_err(|e| CallbackError::Fatal(msg::cannot_open(file, &rgit_messages::strerror(&e))))?;
        d
    } else {
        std::fs::read(file)
            .map_err(|e| CallbackError::Fatal(msg::cannot_open(file, &rgit_messages::strerror(&e))))?
    };
    Ok(data
        .split(|&b| b == b'\n')
        .map(|l| l.strip_suffix(b"\r").unwrap_or(l))
        .filter(|l| !l.is_empty())
        .map(|l| String::from_utf8_lossy(l).into_owned())
        .collect())
}

/// The command line, parsed.
struct Args {
    tokens: Vec<Token>,
    rest: Vec<String>,
    dashdash: Option<usize>,
    cached: bool,
    no_index: bool,
    untracked: bool,
    exclude_standard: i32,
    recurse_submodules: bool,
    invert: bool,
    icase: bool,
    word: bool,
    textconv: bool,
    max_depth: i32,
    pattern_type: i32,
    line_number: i32,
    column: i32,
    with_filename: bool,
    full_name: i32,
    color: Option<rgit_config::color::ColorBool>,
    threads: Option<i32>,
    all_match: bool,
    open_pager: Option<Option<String>>,
    opts: Options,
}

fn parse_args(argv: &[String]) -> Result<Args, Die> {
    let tokens: RefCell<Vec<Token>> = RefCell::new(Vec::new());
    let cached = Cell::new(false);
    let no_index = Cell::new(0);
    let untracked = Cell::new(false);
    let exclude_standard = Cell::new(-1);
    let recurse = Cell::new(false);
    let invert = Cell::new(false);
    let icase = Cell::new(false);
    let word = Cell::new(false);
    let text = Cell::new(false);
    let skip_binary = Cell::new(false);
    let textconv = Cell::new(false);
    let max_depth = Cell::new(-1);
    let pattern_type = Cell::new(TYPE_UNSET);
    let line_number = Cell::new(-1);
    let column = Cell::new(-1);
    let with_filename = Cell::new(1);
    let full_name = Cell::new(-1);
    let files_with = Cell::new(false);
    let files_without = Cell::new(false);
    let null = Cell::new(false);
    let only_matching = Cell::new(false);
    let count = Cell::new(false);
    let color = Cell::new(None);
    let file_break = Cell::new(false);
    let heading = Cell::new(false);
    let before = Cell::new(0usize);
    let after = Cell::new(0usize);
    let threads = Cell::new(i32::MIN);
    let show_function = Cell::new(false);
    let function_context = Cell::new(false);
    let quiet = Cell::new(false);
    let all_match = Cell::new(false);
    let open_pager: RefCell<Option<Option<String>>> = RefCell::new(None);
    let ext_grep = Cell::new(false);
    let max_count = Cell::new(-1);
    let push = |t: Token| {
        tokens.borrow_mut().push(t);
        Ok(())
    };

    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::flag(None, "cached", &cached, msg::HELP_CACHED),
            Opt::set_int(None, "no-index", &no_index, msg::HELP_NO_INDEX, 1).noneg(),
            Opt::set_int(None, "index", &no_index, msg::HELP_INDEX, 0).noneg(),
            Opt::flag(None, "untracked", &untracked, msg::HELP_UNTRACKED),
            Opt::bool(
                None,
                "exclude-standard",
                &exclude_standard,
                msg::HELP_EXCLUDE_STANDARD,
            ),
            Opt::flag(
                None,
                "recurse-submodules",
                &recurse,
                msg::HELP_RECURSE_SUBMODULES,
            ),
            Opt::group(""),
            Opt::flag('v', "invert-match", &invert, msg::HELP_INVERT),
            Opt::flag('i', "ignore-case", &icase, msg::HELP_ICASE),
            Opt::flag('w', "word-regexp", &word, msg::HELP_WORD),
            Opt::flag('a', "text", &text, msg::HELP_TEXT),
            Opt::flag('I', None, &skip_binary, msg::HELP_I),
            Opt::flag(None, "textconv", &textconv, msg::HELP_TEXTCONV),
            Opt::callback('r', "recursive", None, msg::HELP_RECURSIVE, |_, neg| {
                max_depth.set(if neg { 0 } else { -1 });
                Ok(())
            })
            .noarg(),
            Opt::integer(None, "max-depth", &max_depth, msg::HELP_MAX_DEPTH).noneg(),
            Opt::group(""),
            Opt::set_int('E', "extended-regexp", &pattern_type, msg::HELP_ERE, TYPE_EXTENDED),
            Opt::set_int('G', "basic-regexp", &pattern_type, msg::HELP_BRE, TYPE_BASIC),
            Opt::set_int('F', "fixed-strings", &pattern_type, msg::HELP_FIXED, TYPE_FIXED),
            Opt::set_int('P', "perl-regexp", &pattern_type, msg::HELP_PERL, TYPE_PERL),
            Opt::group(""),
            Opt::bool('n', "line-number", &line_number, msg::HELP_LINE_NUMBER),
            Opt::bool(None, "column", &column, msg::HELP_COLUMN),
            Opt::set_int('h', None, &with_filename, msg::HELP_NO_FILENAME, 0),
            Opt::set_int('H', None, &with_filename, msg::HELP_FILENAME, 1),
            Opt::bool(None, "full-name", &full_name, msg::HELP_FULL_NAME),
            Opt::flag(
                'l',
                "files-with-matches",
                &files_with,
                msg::HELP_FILES_WITH_MATCHES,
            ),
            Opt::flag(None, "name-only", &files_with, msg::HELP_NAME_ONLY),
            Opt::flag(
                'L',
                "files-without-match",
                &files_without,
                msg::HELP_FILES_WITHOUT_MATCH,
            ),
            Opt::flag('z', "null", &null, msg::HELP_NULL),
            Opt::flag('o', "only-matching", &only_matching, msg::HELP_ONLY_MATCHING),
            Opt::flag('c', "count", &count, msg::HELP_COUNT),
            Opt::callback(None, "color", Some("when"), msg::HELP_COLOR, |v, neg| {
                color.set(Some(crate::color::parse_when(v, neg)?));
                Ok(())
            })
            .optional(None),
            Opt::flag(None, "break", &file_break, msg::HELP_BREAK),
            Opt::flag(None, "heading", &heading, msg::HELP_HEADING),
            Opt::group(""),
            Opt::callback('C', "context", Some("n"), msg::HELP_CONTEXT, |v, neg| {
                let n = if neg { 0 } else { context_len(v, 'C')? };
                before.set(n);
                after.set(n);
                Ok(())
            }),
            Opt::callback('B', "before-context", Some("n"), msg::HELP_BEFORE, |v, _| {
                before.set(context_len(v, 'B')?);
                Ok(())
            })
            .noneg(),
            Opt::callback('A', "after-context", Some("n"), msg::HELP_AFTER, |v, _| {
                after.set(context_len(v, 'A')?);
                Ok(())
            })
            .noneg(),
            Opt::integer(None, "threads", &threads, msg::HELP_THREADS),
            Opt::number(msg::HELP_NUM, |digits| {
                let n = context_len(Some(digits), 'C')?;
                before.set(n);
                after.set(n);
                Ok(())
            }),
            Opt::flag('p', "show-function", &show_function, msg::HELP_SHOW_FUNCTION),
            Opt::flag(
                'W',
                "function-context",
                &function_context,
                msg::HELP_FUNCTION_CONTEXT,
            ),
            Opt::group(""),
            Opt::callback('f', None, Some("file"), msg::HELP_FILE, |v, _| {
                for p in pattern_file(v.unwrap_or_default())? {
                    tokens.borrow_mut().push(Token::Pattern(p));
                }
                Ok(())
            })
            .noneg(),
            Opt::callback('e', None, Some("pattern"), msg::HELP_PATTERN, |v, _| {
                push(Token::Pattern(v.unwrap_or_default().to_string()))
            })
            .noneg(),
            Opt::callback(None, "and", None, msg::HELP_AND, |_, _| push(Token::And))
                .noarg()
                .noneg(),
            Opt::callback(None, "or", None, "", |_, _| push(Token::Or))
                .noarg()
                .noneg(),
            Opt::callback(None, "not", None, "", |_, _| push(Token::Not))
                .noarg()
                .noneg(),
            Opt::callback('(', None, None, "", |_, _| push(Token::Open))
                .noarg()
                .noneg()
                .nodash(),
            Opt::callback(')', None, None, "", |_, _| push(Token::Close))
                .noarg()
                .noneg()
                .nodash(),
            Opt::flag('q', "quiet", &quiet, msg::HELP_QUIET),
            Opt::flag(None, "all-match", &all_match, msg::HELP_ALL_MATCH),
            Opt::group(""),
            Opt::callback(
                'O',
                "open-files-in-pager",
                Some("pager"),
                msg::HELP_OPEN_PAGER,
                |v, neg| {
                    *open_pager.borrow_mut() = if neg {
                        None
                    } else {
                        Some(v.filter(|s| !s.is_empty()).map(str::to_string))
                    };
                    Ok(())
                },
            )
            .optional(None),
            Opt::flag(None, "ext-grep", &ext_grep, msg::HELP_EXT_GREP),
            Opt::integer('m', "max-count", &max_count, msg::HELP_MAX_COUNT),
        ],
    )
    .flags(ParseFlags {
        keep_dashdash: true,
        stop_at_non_option: true,
        ..Default::default()
    });
    if argv.len() == 2 && argv[1] == "-h" {
        print!("{}", parser.usage_text(false));
        return Err(Die::Exit(0));
    }
    let rest = parser.parse(argv)?.args;
    drop(parser);
    let threads = threads.get();
    Ok(Args {
        tokens: tokens.into_inner(),
        rest,
        dashdash: None,
        cached: cached.get(),
        no_index: no_index.get() == 1,
        untracked: untracked.get(),
        exclude_standard: exclude_standard.get(),
        recurse_submodules: recurse.get(),
        invert: invert.get(),
        icase: icase.get(),
        word: word.get(),
        textconv: textconv.get(),
        max_depth: max_depth.get(),
        pattern_type: pattern_type.get(),
        line_number: line_number.get(),
        column: column.get(),
        with_filename: with_filename.get() != 0,
        full_name: full_name.get(),
        color: color.get(),
        threads: (threads != i32::MIN).then_some(threads),
        all_match: all_match.get(),
        open_pager: open_pager.into_inner(),
        opts: Options {
            files_with_matches: files_with.get(),
            files_without_match: files_without.get(),
            count: count.get(),
            quiet: quiet.get(),
            only_matching: only_matching.get(),
            null: null.get(),
            heading: heading.get(),
            file_break: file_break.get(),
            before: before.get(),
            after: after.get(),
            show_function: show_function.get(),
            function_context: function_context.get(),
            max_count: usize::try_from(max_count.get()).ok(),
            text: text.get(),
            skip_binary: skip_binary.get(),
            ..Default::default()
        },
    })
}

/// The pattern syntax from the options and `grep.patternType` /
/// `grep.extendedRegexp`.
fn syntax(args: &Args, config: &ConfigSet) -> Syntax {
    match args.pattern_type {
        TYPE_BASIC => return Syntax::Basic,
        TYPE_EXTENDED => return Syntax::Extended,
        TYPE_FIXED => return Syntax::Fixed,
        TYPE_PERL => return Syntax::Perl,
        _ => {}
    }
    let configured = config
        .string("grep.patterntype")
        .ok()
        .flatten()
        .and_then(|v| Syntax::from_config(&v.to_string()))
        .flatten();
    configured.unwrap_or_else(|| {
        if config.bool("grep.extendedregexp").ok().flatten() == Some(true) {
            Syntax::Extended
        } else {
            Syntax::Basic
        }
    })
}

/// A setting from the command line (`-1`: not given) or the configuration.
fn tristate(cli: i32, config: &ConfigSet, key: &str) -> bool {
    if cli >= 0 {
        return cli == 1;
    }
    config.bool(key).ok().flatten().unwrap_or(false)
}

fn colors(config: &ConfigSet) -> Result<Colors, Die> {
    let mut c = Colors {
        filename: "\x1b[35m".into(),
        separator: "\x1b[36m".into(),
        line_number: "\x1b[32m".into(),
        column: "\x1b[32m".into(),
        match_selected: "\x1b[1;31m".into(),
        match_context: "\x1b[1;31m".into(),
        ..Default::default()
    };
    for e in config.entries() {
        let name = e.name.to_string();
        let Some(slot) = name.strip_prefix("color.grep.") else {
            continue;
        };
        let target: Vec<&mut String> = match slot {
            "context" => vec![&mut c.context],
            "filename" => vec![&mut c.filename],
            "function" => vec![&mut c.function],
            "linenumber" => vec![&mut c.line_number],
            "column" => vec![&mut c.column],
            "match" => vec![&mut c.match_context, &mut c.match_selected],
            "matchcontext" => vec![&mut c.match_context],
            "matchselected" => vec![&mut c.match_selected],
            "selected" => vec![&mut c.selected],
            "separator" => vec![&mut c.separator],
            _ => continue,
        };
        let seq = crate::color::slot(config, &name, "")?;
        for t in target {
            t.clone_from(&seq);
        }
    }
    Ok(c)
}

/// Where to search.
enum Target {
    /// Tracked files of the work tree (and untracked ones).
    WorkTree { untracked: bool },
    Cached,
    /// Trees and blobs: the name as given, the object, whether it is a
    /// blob.
    Revs(Vec<(String, rgit_hash::ObjectId, bool)>),
}

/// Per-path attributes: binary, function names, textconv.
struct Attrs<'a> {
    attrs: Option<rgit_attr::Attributes<'a>>,
    diff: Option<rgit_attr::AttrId>,
    config: &'a ConfigSet,
    funcnames: HashMap<Vec<u8>, FuncName>,
    default_funcname: FuncName,
    textconv: bool,
}

/// What the attributes say about one path.
struct PathInfo {
    binary: Option<bool>,
    driver: Option<Vec<u8>>,
}

impl Attrs<'_> {
    fn info(&mut self, path: &[u8]) -> PathInfo {
        let (Some(attrs), Some(id)) = (self.attrs.as_mut(), self.diff) else {
            return PathInfo {
                binary: None,
                driver: None,
            };
        };
        match attrs.lookup(path, false).get(id).clone() {
            State::Unset => PathInfo {
                binary: Some(true),
                driver: None,
            },
            State::Set => PathInfo {
                binary: Some(false),
                driver: None,
            },
            State::Value(drv) => {
                let key = format!("diff.{}.binary", String::from_utf8_lossy(&drv));
                PathInfo {
                    binary: self.config.bool(&key).ok().flatten(),
                    driver: Some(drv),
                }
            }
            _ => PathInfo {
                binary: None,
                driver: None,
            },
        }
    }

    fn funcname(&mut self, driver: Option<&[u8]>) -> &FuncName {
        let Some(drv) = driver else {
            return &self.default_funcname;
        };
        let config = self.config;
        self.funcnames.entry(drv.to_vec()).or_insert_with(|| {
            let name = String::from_utf8_lossy(drv);
            let get = |k: &str| {
                config
                    .string(format!("diff.{name}.{k}"))
                    .ok()
                    .flatten()
                    .map(|v| v.to_string())
            };
            if let Some(v) = get("xfuncname") {
                FuncName::custom(&v, true)
            } else if let Some(v) = get("funcname") {
                FuncName::custom(&v, false)
            } else {
                FuncName::Default
            }
        })
    }

    /// The textconv output for `data` (`--textconv` and a driver with a
    /// `textconv` command).
    fn textconv(&self, driver: Option<&[u8]>, data: &[u8]) -> Result<Option<Vec<u8>>, Die> {
        let Some(drv) = driver.filter(|_| self.textconv) else {
            return Ok(None);
        };
        let key = format!("diff.{}.textconv", String::from_utf8_lossy(drv));
        let Some(cmd) = self.config.string(&key).ok().flatten().map(|v| v.to_string()) else {
            return Ok(None);
        };
        run_textconv(&cmd, data).map(Some)
    }
}

fn run_textconv(cmd: &str, data: &[u8]) -> Result<Vec<u8>, Die> {
    static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let failed = || Die::Fatal(msg::textconv_failed(cmd));
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = std::env::temp_dir().join(format!("rgit-textconv-{}-{n}", std::process::id()));
    std::fs::write(&tmp, data).map_err(|_| failed())?;
    let out = rgit_ident::editor::shell_command(cmd, &[tmp.as_os_str()])
        .stdin(std::process::Stdio::null())
        .output();
    let _ = std::fs::remove_file(&tmp);
    match out {
        Ok(o) if o.status.success() => Ok(o.stdout),
        _ => Err(failed()),
    }
}

/// `--max-depth`: whether `path` lies at most `depth` directories below
/// a pathspec item without wildcards (or matches one with wildcards).
fn within_depth(pathspec: &Pathspec, path: &[u8], depth: i32) -> bool {
    if depth < 0 {
        return true;
    }
    let depth = depth as usize;
    pathspec.items().iter().filter(|i| !i.is_exclude()).any(|item| {
        if item.match_path(path).is_none() {
            return false;
        }
        if item.has_wildcard() {
            return true;
        }
        let pat = item.pattern();
        let rest = path.get(pat.len()..).unwrap_or_default();
        let rest = rest.strip_prefix(b"/").unwrap_or(rest);
        rest.iter().filter(|&&b| b == b'/').count() <= depth
    })
}

struct Grepper<'a> {
    printer: Printer,
    matcher: &'a Matcher,
    pathspec: &'a Pathspec,
    max_depth: i32,
    prefix: Vec<u8>,
    full_name: bool,
    quote_high: bool,
    attrs: Attrs<'a>,
}

impl Grepper<'_> {
    fn wanted(&self, path: &[u8]) -> bool {
        self.pathspec.match_path(path).is_some() && within_depth(self.pathspec, path, self.max_depth)
    }

    /// The name shown for `path` (relative to the top), after `label`.
    fn shown(&self, label: &[u8], path: &[u8]) -> Vec<u8> {
        let rel = if self.full_name {
            path.to_vec()
        } else {
            relative_to(path, &self.prefix)
        };
        let mut name = label.to_vec();
        if self.printer.opts.null || self.printer.collect.is_some() {
            name.extend_from_slice(&rel);
        } else {
            write_quoted(&mut name, &rel, self.quote_high);
        }
        name
    }

    /// Search `data` of `path` shown as `name`.
    fn search(&mut self, name: &[u8], path: Option<&[u8]>, data: &[u8]) -> Result<(), Die> {
        let info = match path {
            Some(p) => self.attrs.info(p),
            None => PathInfo {
                binary: None,
                driver: None,
            },
        };
        let converted = self.attrs.textconv(info.driver.as_deref(), data)?;
        let (data, binary) = match &converted {
            Some(d) => (d.as_slice(), Some(false)),
            None => (data, info.binary),
        };
        let funcname = self.attrs.funcname(info.driver.as_deref());
        let src = Source {
            name,
            data,
            binary,
            funcname,
        };
        self.printer.grep(self.matcher, &src);
        if self.printer.out.len() >= 64 * 1024 {
            self.flush();
        }
        Ok(())
    }

    fn flush(&mut self) {
        let mut o = std::io::stdout().lock();
        let _ = o.write_all(&self.printer.out);
        let _ = o.flush();
        self.printer.out.clear();
    }

    /// `-q`: the answer is known.
    fn done(&self) -> bool {
        self.printer.opts.quiet && self.printer.hit
    }
}

fn os_path(root: &Path, rel: &[u8]) -> PathBuf {
    crate::untracked::os_path(root, rel)
}

/// Regular files below `dir` (relative to `root`), sorted, without
/// `.git`; directories excluded by `ex` are skipped, and so are files it
/// excludes.
fn walk_dir(
    root: &Path,
    dir: &[u8],
    ex: &mut Option<rgit_worktree::ignore::Excludes>,
    pathspec: &Pathspec,
    out: &mut Vec<Vec<u8>>,
) {
    let Ok(rd) = std::fs::read_dir(os_path(root, dir)) else {
        return;
    };
    let mut entries: Vec<(Vec<u8>, std::fs::FileType)> = rd
        .flatten()
        .filter_map(|e| Some((e.file_name().as_encoded_bytes().to_vec(), e.file_type().ok()?)))
        .filter(|(n, _)| n != b".git")
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, ft) in entries {
        let mut path = dir.to_vec();
        path.extend_from_slice(&name);
        if ft.is_dir() {
            if ex.as_mut().is_some_and(|e| e.is_excluded(&path, true)) {
                continue;
            }
            if pathspec.dir_interest(&path) == DirInterest::None {
                continue;
            }
            path.push(b'/');
            walk_dir(root, &path, ex, pathspec, out);
        } else if ft.is_file() {
            if ex.as_mut().is_some_and(|e| e.is_excluded(&path, false)) {
                continue;
            }
            out.push(path);
        }
    }
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let mut args = parse_args(argv)?;
    if args.tokens.is_empty() {
        if args.rest.first().is_some_and(|a| a == "--") {
            args.rest.remove(0);
        }
        if args.rest.is_empty() {
            return Err(Die::Fatal(msg::NO_PATTERN.into()));
        }
        let p = args.rest.remove(0);
        args.tokens.push(Token::Pattern(p));
    }
    args.dashdash = args.rest.iter().position(|a| a == "--");
    if let Some(i) = args.dashdash {
        args.rest.remove(i);
    }
    let (expr, patterns) = crate::grep::pattern::parse(&args.tokens).map_err(Die::Fatal)?;

    let repo = Repo::open_gently(ctx)?;
    let config = match &repo {
        Some(r) => r.config.clone(),
        None => crate::pager::config(ctx),
    };
    let fallback = repo.is_none()
        && config
            .bool("grep.fallbacktonoindex")
            .ok()
            .flatten()
            .unwrap_or(false);
    if repo.is_none() && !args.no_index && !fallback {
        Repo::open(ctx, Mode::RequiredQuiet)?;
    }
    let no_index = args.no_index || repo.is_none();
    if no_index && args.cached {
        return Err(Die::Fatal(msg::options_together("--no-index", "--cached")));
    }
    if no_index && args.untracked {
        return Err(Die::Fatal(msg::options_together("--no-index", "--untracked")));
    }
    if args.untracked && args.cached {
        return Err(Die::Fatal(msg::options_together("--untracked", "--cached")));
    }
    if args.recurse_submodules && args.untracked {
        return Err(Die::Fatal(msg::UNTRACKED_WITH_SUBMODULES.into()));
    }
    if let Some(n) = args.threads.filter(|n| *n < 0) {
        return Err(Die::Fatal(msg::bad_threads(i64::from(n))));
    }
    if let Ok(Some(n)) = config.int("grep.threads") {
        if n < 0 {
            return Err(Die::Fatal(msg::bad_config_threads(n)));
        }
    }

    let syn = syntax(&args, &config);
    let utf8 = crate::grep::pattern::utf8_locale();
    let mut atoms = Vec::new();
    for p in &patterns {
        let atom = Atom::new(p, syn, args.icase, args.word, utf8)
            .map_err(|reason| Die::Fatal(msg::bad_pattern(p, &reason)))?;
        atoms.push(atom);
    }
    let mut matcher = Matcher::new(expr, atoms, args.all_match, args.invert);
    matcher.empty_line_at_end = !args.invert
        && args.tokens.len() == 1
        && patterns.len() == 1
        && patterns[0] == "^$"
        && matches!(syn, Syntax::Basic | Syntax::Extended);
    if args.opts.max_count == Some(0) {
        return Ok(1);
    }

    let mut opts = args.opts.clone();
    opts.line_number = tristate(args.line_number, &config, "grep.linenumber");
    opts.column = tristate(args.column, &config, "grep.column");
    opts.with_filename = args.with_filename;
    let full_name = tristate(args.full_name, &config, "grep.fullname");
    if opts.files_with_matches && opts.files_without_match {
        // The later of -l/-L is not known; git lets -L win over -l.
        opts.files_with_matches = false;
    }

    // `-O`: the pager to open the files with, unless it is `cat`.
    let open_pager = match &args.open_pager {
        Some(Some(p)) => Some(p.clone()).filter(|p| p != "cat"),
        Some(None) => {
            let env = rgit_config::SystemEnv::default();
            crate::pager::command(&config, &env, None)
        }
        None => None,
    };
    let colors = if open_pager.is_some() || opts.quiet {
        None
    } else {
        crate::color::want(&config, Some("color.grep"), args.color)?
            .then(|| colors(&config))
            .transpose()?
    };
    if open_pager.is_some() {
        opts.files_with_matches = true;
        opts.files_without_match = false;
        opts.count = false;
    }

    if no_index {
        return run_no_index(ctx, &args, &config, opts, colors, &matcher, open_pager);
    }
    let mut repo = repo.expect("repository");
    let prefix = repo.prefix();

    // Revisions, then paths.
    let mut revs: Vec<(String, rgit_hash::ObjectId, bool)> = Vec::new();
    let mut paths: Vec<String> = Vec::new();
    let split = args.dashdash.unwrap_or(args.rest.len());
    let work_tree = repo.setup.work_tree.clone();
    let exists = |repo: &Repo, arg: &str| -> bool {
        let Some(root) = &work_tree else {
            return false;
        };
        match repo.prefix_path(arg.as_bytes()) {
            Ok(p) => std::fs::symlink_metadata(os_path(root, &p)).is_ok(),
            Err(()) => false,
        }
    };
    let paths_only = args.untracked || args.no_index;
    for arg in &args.rest[..split] {
        if paths_only {
            if args.dashdash.is_some() {
                return Err(Die::Fatal(msg::NO_INDEX_WITH_REVS.into()));
            }
            if !exists(&repo, arg) {
                return Err(Die::Fatal(crate::messages::no_such_path(arg)));
            }
            paths.push(arg.clone());
            continue;
        }
        if paths.is_empty() {
            match Resolver::new(&mut repo).resolve(arg) {
                Ok(r) => {
                    let kind = repo.odb().header(&r.oid).ok().flatten().map(|h| h.kind);
                    match kind {
                        Some(ObjectKind::Blob) => {
                            revs.push((arg.clone(), r.oid, true));
                            continue;
                        }
                        Some(_) => {
                            if let Some(t) = repo.peel_to(r.oid, ObjectKind::Tree) {
                                revs.push((arg.clone(), t, false));
                                continue;
                            }
                        }
                        None => {}
                    }
                    if args.dashdash.is_some() {
                        return Err(Die::Fatal(msg::unresolved_revision(arg)));
                    }
                }
                Err(Failure::Missing { diagnosis }) => {
                    if args.dashdash.is_some() {
                        return Err(Die::Fatal(msg::unresolved_revision(arg)));
                    }
                    if !exists(&repo, arg) {
                        return Err(Die::Fatal(
                            diagnosis.unwrap_or_else(|| crate::messages::ambiguous_argument(arg)),
                        ));
                    }
                }
                Err(Failure::Ambiguous) => return Err(Die::Exit(128)),
                Err(Failure::Fatal(m)) => return Err(Die::Fatal(m)),
            }
        } else if !exists(&repo, arg) {
            return Err(Die::Fatal(crate::messages::no_such_path(arg)));
        }
        paths.push(arg.clone());
    }
    paths.extend(args.rest[split..].iter().cloned());

    if args.cached && !revs.is_empty() {
        return Err(Die::Fatal(msg::CACHED_AND_TREES.into()));
    }
    if open_pager.is_some() && (args.cached || !revs.is_empty()) {
        return Err(Die::Fatal(msg::OPEN_ONLY_WORKTREE.into()));
    }
    if args.recurse_submodules {
        return Err(Die::Fatal(msg::not_supported("--recurse-submodules")));
    }

    let root = work_tree
        .as_ref()
        .map(|w| w.as_os_str().as_encoded_bytes().to_vec());
    let popts = ParseOptions {
        prefix: prefix.clone(),
        root,
        globals: repository::pathspec_globals(ctx),
        ..Default::default()
    };
    let spec_args: Vec<String> = if paths.is_empty() {
        vec![".".to_string()]
    } else {
        paths
    };
    let pathspec = Pathspec::parse(&spec_args, &popts).map_err(|e| Die::Fatal(e.to_string()))?;

    let target = if !revs.is_empty() {
        Target::Revs(revs)
    } else if args.cached {
        Target::Cached
    } else {
        Target::WorkTree {
            untracked: args.untracked,
        }
    };
    if matches!(target, Target::WorkTree { .. }) {
        repo.work_tree()?;
    }

    if !opts.quiet && open_pager.is_none() {
        crate::pager::setup_for(ctx, &config, "grep", true)?;
    }

    let (index, _) = repo.read_index()?;
    let tree_source = crate::attributes::tree_source(ctx, &mut repo, None)?;
    let repo = repo;
    let mut attrs = crate::attributes::open(&repo, &index, tree_source, false);
    let diff = attrs.id(b"diff");
    let mut printer = Printer::new(opts, colors);
    if open_pager.is_some() {
        printer.collect = Some(Vec::new());
    }
    let mut g = Grepper {
        printer,
        matcher: &matcher,
        pathspec: &pathspec,
        max_depth: args.max_depth,
        prefix: prefix.clone(),
        full_name,
        quote_high: repo.quote_path(),
        attrs: Attrs {
            attrs: Some(attrs),
            diff: Some(diff),
            config: &config,
            funcnames: HashMap::new(),
            default_funcname: FuncName::Default,
            textconv: args.textconv,
        },
    };

    match target {
        Target::WorkTree { untracked } => {
            let root = work_tree.clone().expect("work tree");
            grep_index(&mut g, &repo, &index, Some(&root))?;
            if untracked && !g.done() {
                let mut ex = if args.exclude_standard != 0 {
                    Some(super::ls_files::build_excludes(
                        &repo,
                        &[super::ls_files::ExcludeArg::Standard],
                    )?)
                } else {
                    None
                };
                let tracked: std::collections::HashSet<&[u8]> =
                    index.entries().iter().map(|e| e.path()).collect();
                let mut files = Vec::new();
                let start = pathspec.common_prefix().to_vec();
                let start = match start.iter().rposition(|&b| b == b'/') {
                    Some(i) => start[..=i].to_vec(),
                    None => Vec::new(),
                };
                walk_dir(&root, &start, &mut ex, &pathspec, &mut files);
                for path in files {
                    if g.done() {
                        break;
                    }
                    if tracked.contains(path.as_slice()) || !g.wanted(&path)
                    {
                        continue;
                    }
                    let Ok(data) = std::fs::read(os_path(&root, &path)) else {
                        continue;
                    };
                    let name = g.shown(b"", &path);
                    g.search(&name, Some(&path), &data)?;
                }
            }
        }
        Target::Cached => grep_index(&mut g, &repo, &index, None)?,
        Target::Revs(revs) => {
            for (name, id, blob) in revs {
                if g.done() {
                    break;
                }
                if blob {
                    let Some(obj) = repo.odb().read(&id).ok().flatten() else {
                        continue;
                    };
                    let path = name.split_once(':').map(|(_, p)| p.as_bytes().to_vec());
                    g.search(name.as_bytes(), path.as_deref(), &obj.data)?;
                    continue;
                }
                let mut label = name.into_bytes();
                label.push(b':');
                grep_tree(&mut g, &repo, &id, b"", &label)?;
            }
        }
    }
    g.flush();
    if let Some(names) = g.printer.collect.take() {
        let hit = g.printer.hit;
        if !names.is_empty() {
            let pager = open_pager.expect("pager");
            let one_pattern = patterns.len() == 1;
            let cwd = work_tree.map(|w| os_path(&w, &prefix));
            open_files(&pager, &names, one_pattern.then(|| patterns[0].as_str()), cwd)?;
        }
        return Ok(if hit { 0 } else { 1 });
    }
    Ok(if g.printer.hit { 0 } else { 1 })
}

/// Search the index entries: the work tree files when `root` is given
/// (assume-unchanged and skip-worktree entries from the index), else the
/// blobs.
fn grep_index(
    g: &mut Grepper<'_>,
    repo: &Repo,
    index: &Index,
    root: Option<&Path>,
) -> Result<(), Die> {
    let mut last: Option<&[u8]> = None;
    for e in index.entries() {
        if g.done() {
            break;
        }
        let path = e.path();
        if last == Some(path) || !e.mode.is_file() {
            continue;
        }
        if root.is_none() && e.stage().is_conflict() {
            continue;
        }
        if !g.wanted(path) {
            continue;
        }
        last = Some(path);
        if e.flags.intent_to_add && (root.is_none() || e.flags.assume_valid || e.flags.skip_worktree)
        {
            continue;
        }
        let data = match root {
            Some(r) if !(e.flags.assume_valid || e.flags.skip_worktree) => {
                match std::fs::symlink_metadata(os_path(r, path)) {
                    Ok(m) if m.is_file() => match std::fs::read(os_path(r, path)) {
                        Ok(d) => d,
                        Err(_) => continue,
                    },
                    _ => continue,
                }
            }
            _ => match repo.odb().read(&e.id).ok().flatten() {
                Some(o) => o.data,
                None => continue,
            },
        };
        let name = g.shown(b"", path);
        g.search(&name, Some(path), &data)?;
    }
    Ok(())
}

/// Search a tree; `base` is the path of the tree relative to the top of
/// the searched tree, `label` the `<rev>:` prefix.
fn grep_tree(
    g: &mut Grepper<'_>,
    repo: &Repo,
    tree: &rgit_hash::ObjectId,
    base: &[u8],
    label: &[u8],
) -> Result<(), Die> {
    let Some(Object::Tree(t)) = repo.read_object(tree) else {
        return Err(Die::Fatal(msg::unable_to_read(&tree.to_hex())));
    };
    for e in t.entries {
        if g.done() {
            break;
        }
        let mut path = base.to_vec();
        path.extend_from_slice(&e.name);
        let mode = e.mode.canonical().bits();
        if mode & 0o170000 == 0o040000 {
            if g.pathspec.dir_interest(&path) == DirInterest::None {
                continue;
            }
            path.push(b'/');
            grep_tree(g, repo, &e.oid, &path, label)?;
        } else if mode & 0o170000 == 0o100000 {
            if !g.wanted(&path) {
                continue;
            }
            let Some(obj) = repo.odb().read(&e.oid).ok().flatten() else {
                continue;
            };
            let name = g.shown(label, &path);
            g.search(&name, Some(&path), &obj.data)?;
        }
    }
    Ok(())
}

/// Normalise a path relative to the current directory; `None` when it
/// leaves it.
fn normalise_inside(arg: &str) -> Option<Vec<u8>> {
    if arg.starts_with('/') {
        let cwd = std::env::current_dir().ok()?;
        let rest = Path::new(arg).strip_prefix(&cwd).ok()?;
        return normalise_inside(rest.to_str()?).or(Some(Vec::new()));
    }
    let mut parts: Vec<&str> = Vec::new();
    for c in arg.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            c => parts.push(c),
        }
    }
    let mut out = parts.join("/").into_bytes();
    if arg.ends_with('/') && !out.is_empty() {
        out.push(b'/');
    }
    Some(out)
}

#[allow(clippy::too_many_arguments)]
fn run_no_index(
    ctx: &mut Context,
    args: &Args,
    config: &ConfigSet,
    mut opts: Options,
    colors: Option<Colors>,
    matcher: &Matcher,
    open_pager: Option<String>,
) -> CmdResult {
    let split = args.dashdash.unwrap_or(args.rest.len());
    if args.dashdash.is_some() && split > 0 {
        return Err(Die::Fatal(msg::NO_INDEX_WITH_REVS.into()));
    }
    let mut specs = Vec::new();
    for (i, arg) in args.rest.iter().enumerate() {
        if i < split && std::fs::symlink_metadata(arg).is_err() {
            return Err(Die::Fatal(crate::messages::no_such_path(arg)));
        }
        if normalise_inside(arg).is_none() {
            return Err(Die::Fatal(msg::outside_directory(arg)));
        }
        specs.push(arg.clone());
    }
    let cwd = std::env::current_dir().map_err(|e| Die::Fatal(e.to_string()))?;
    let popts = ParseOptions {
        prefix: Vec::new(),
        root: Some(cwd.as_os_str().as_encoded_bytes().to_vec()),
        globals: repository::pathspec_globals(ctx),
        ..Default::default()
    };
    if specs.is_empty() {
        specs.push(".".into());
    }
    let pathspec = Pathspec::parse(&specs, &popts).map_err(|e| Die::Fatal(e.to_string()))?;
    if !opts.quiet && open_pager.is_none() {
        crate::pager::setup_for(ctx, config, "grep", true)?;
    }
    let mut ex = if args.exclude_standard == 1 {
        let mut ex = rgit_worktree::ignore::Excludes::new(cwd.clone());
        ex.set_per_directory(b".gitignore".to_vec());
        let env = rgit_config::SystemEnv::default();
        let file = match config.path("core.excludesfile", &env) {
            Ok(Some(p)) => Some(p),
            _ => rgit_worktree::ignore::default_excludes_file(|k| std::env::var_os(k)),
        };
        if let Some(f) = file {
            let shown = f.as_os_str().as_encoded_bytes().to_vec();
            let _ = ex.add_file(&f, shown);
        }
        Some(ex)
    } else {
        None
    };
    let mut files = Vec::new();
    walk_dir(&cwd, b"", &mut ex, &pathspec, &mut files);
    opts.with_filename = args.with_filename;
    let mut printer = Printer::new(opts, colors);
    if open_pager.is_some() {
        printer.collect = Some(Vec::new());
    }
    let mut g = Grepper {
        printer,
        matcher,
        pathspec: &pathspec,
        max_depth: args.max_depth,
        prefix: Vec::new(),
        full_name: true,
        quote_high: config.bool("core.quotepath").ok().flatten().unwrap_or(true),
        attrs: Attrs {
            attrs: None,
            diff: None,
            config,
            funcnames: HashMap::new(),
            default_funcname: FuncName::Default,
            textconv: false,
        },
    };
    for path in files {
        if g.done() {
            break;
        }
        if !g.wanted(&path) {
            continue;
        }
        let Ok(data) = std::fs::read(os_path(&cwd, &path)) else {
            continue;
        };
        let name = g.shown(b"", &path);
        g.search(&name, Some(&path), &data)?;
    }
    g.flush();
    if let Some(names) = g.printer.collect.take() {
        if !names.is_empty() {
            let pager = open_pager.expect("pager");
            open_files(&pager, &names, None, Some(cwd))?;
        }
    }
    Ok(if g.printer.hit { 0 } else { 1 })
}

/// `-O`: run the pager on the matching files; `less` and `vi` are told to
/// jump to the only pattern.
fn open_files(
    pager: &str,
    names: &[Vec<u8>],
    pattern: Option<&str>,
    cwd: Option<PathBuf>,
) -> Result<(), Die> {
    let mut args: Vec<std::ffi::OsString> = Vec::new();
    let base = pager.rsplit('/').next().unwrap_or(pager);
    if let Some(p) = pattern {
        if base == "less" || base == "vi" {
            let star = if base == "less" { "*" } else { "" };
            args.push(format!("+/{star}{p}").into());
        }
    }
    for n in names {
        args.push(crate::untracked::os_path(Path::new(""), n).into_os_string());
    }
    let refs: Vec<&std::ffi::OsStr> = args.iter().map(|a| a.as_os_str()).collect();
    let mut cmd = rgit_ident::editor::shell_command(pager, &refs);
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    let _ = std::io::stdout().flush();
    cmd.status()
        .map_err(|e| Die::Fatal(crate::messages::pager::cannot_run(pager, &rgit_messages::strerror(&e))))?;
    Ok(())
}
