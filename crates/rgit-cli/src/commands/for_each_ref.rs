// SPDX-License-Identifier: MIT
//! `git for-each-ref` (Documentation/git-for-each-ref.adoc), basic part:
//! patterns, `--exclude`, `--format` with the atoms `refname`,
//! `objectname`, `objecttype`, `objectsize`, `HEAD`, `symref` (and `*`
//! dereferenced object atoms), `--sort`, `--count`, quoting styles,
//! `--points-at`, `--include-root-refs`, `--start-after`.
//!
//! Not yet: commit/tag field atoms (`subject`, `author`, ...),
//! `upstream`/`push`, `--merged`/`--contains`, `--color`, `--stdin`.

use std::cmp::Ordering;
use std::io::Write;

use rgit_hash::ObjectId;
use rgit_pathspec::wildmatch::{WildmatchFlags, wildmatch};
use rgit_refs::files::FilesStore;
use rgit_refs::{IterItem, IterOptions, RefStore, Target};

use crate::messages::for_each_ref as msg;
use crate::prelude::*;
use crate::refs_support::{self, Objects};
use crate::repo::{self, Mode};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Quote {
    None,
    Shell,
    Perl,
    Python,
    Tcl,
}

/// How a name atom is shortened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NameMod {
    Full,
    Short,
    Lstrip(i64),
    Rstrip(i64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AtomKind {
    Refname(NameMod),
    Symref(NameMod),
    Objectname(Option<usize>),
    Objecttype,
    Objectsize,
    Head,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Atom {
    kind: AtomKind,
    /// `%(*...)`: of the object an annotated tag points to.
    deref: bool,
}

enum Piece {
    Lit(Vec<u8>),
    Atom(Atom),
}

/// One listed reference.
struct Item {
    name: String,
    oid: ObjectId,
    symref: Option<String>,
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let quote = Cell::new(Quote::None);
    let quote_count = Cell::new(0);
    let omit_empty = Cell::new(false);
    let count: RefCell<Option<String>> = RefCell::new(None);
    let format: RefCell<Option<String>> = RefCell::new(None);
    let start_after: RefCell<Option<String>> = RefCell::new(None);
    let excludes: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let sorts: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let points_at: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let ignore_case = Cell::new(false);
    let stdin = Cell::new(false);
    let root_refs = Cell::new(false);
    let unsupported: RefCell<Option<&'static str>> = RefCell::new(None);
    let quote_opt = |short: Option<char>, long: &'static str, q: Quote, help: &'static str| {
        let quote = &quote;
        let quote_count = &quote_count;
        Opt::callback(short, long, None, help, move |_, unset| {
            if unset {
                quote.set(Quote::None);
            } else {
                quote.set(q);
                quote_count.set(quote_count.get() + 1);
            }
            Ok(())
        })
        .noarg()
    };
    let unsup = |long: &'static str, help: &'static str| {
        let unsupported = &unsupported;
        Opt::callback(None, long, Some("commit"), help, move |_, _| {
            *unsupported.borrow_mut() = Some(long);
            Ok(())
        })
        .noneg()
    };
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            quote_opt(Some('s'), "shell", Quote::Shell, msg::HELP_SHELL),
            quote_opt(Some('p'), "perl", Quote::Perl, msg::HELP_PERL),
            quote_opt(None, "python", Quote::Python, msg::HELP_PYTHON),
            quote_opt(None, "tcl", Quote::Tcl, msg::HELP_TCL),
            Opt::flag(None, "omit-empty", &omit_empty, msg::HELP_OMIT_EMPTY),
            Opt::group(""),
            Opt::string(None, "count", &count, "n", msg::HELP_COUNT),
            Opt::string(None, "format", &format, "format", msg::HELP_FORMAT),
            Opt::string(
                None,
                "start-after",
                &start_after,
                "marker",
                msg::HELP_START_AFTER,
            ),
            {
                let unsupported = &unsupported;
                Opt::callback(None, "color", Some("when"), msg::HELP_COLOR, move |_, _| {
                    *unsupported.borrow_mut() = Some("color");
                    Ok(())
                })
                .optional(None)
            },
            Opt::string_list(None, "exclude", &excludes, "pattern", msg::HELP_EXCLUDE),
            Opt::string_list(None, "sort", &sorts, "key", msg::HELP_SORT),
            Opt::string_list(None, "points-at", &points_at, "object", msg::HELP_POINTS_AT),
            unsup("merged", msg::HELP_MERGED),
            unsup("no-merged", msg::HELP_NO_MERGED),
            unsup("contains", msg::HELP_CONTAINS),
            unsup("no-contains", msg::HELP_NO_CONTAINS),
            Opt::flag(None, "ignore-case", &ignore_case, msg::HELP_IGNORE_CASE),
            Opt::flag(None, "stdin", &stdin, msg::HELP_STDIN),
            Opt::flag(
                None,
                "include-root-refs",
                &root_refs,
                msg::HELP_INCLUDE_ROOT_REFS,
            ),
        ],
    );
    let args = parser.parse(argv)?.args;
    if let Some(opt) = *unsupported.borrow() {
        return Err(Die::Fatal(msg::not_supported(&format!("--{opt}"))));
    }
    let max = match count.borrow().as_deref() {
        None => None,
        Some(c) => match c.parse::<i64>() {
            Ok(n) if n >= 0 => Some(n as usize),
            _ => return Err(parser.error_with_usage(&msg::invalid_count(c)).into()),
        },
    };
    if quote_count.get() > 1 {
        return Err(parser.error_with_usage(msg::MORE_THAN_ONE_QUOTING).into());
    }
    let fmt = format
        .borrow()
        .clone()
        .unwrap_or_else(|| msg::DEFAULT_FORMAT.to_owned());
    let pieces = match parse_format(&fmt) {
        Ok(p) => p,
        Err(FormatError::Malformed(rest)) => {
            return Err(parser
                .error_with_usage(&msg::malformed_format(&rest))
                .into());
        }
        Err(FormatError::Fatal(m)) => return Err(Die::Fatal(m)),
    };
    let mut sort_keys = Vec::new();
    for key in sorts.borrow().iter().rev() {
        let (reverse, k) = match key.strip_prefix('-') {
            Some(k) => (true, k),
            None => (false, key.as_str()),
        };
        let atom = parse_atom(k).map_err(|e| match e {
            FormatError::Fatal(m) | FormatError::Malformed(m) => Die::Fatal(m),
        })?;
        sort_keys.push((atom, reverse));
    }

    let mut patterns = args.clone();
    if stdin.get() {
        if !args.is_empty() {
            return Err(Die::Fatal(msg::STDIN_AND_PATTERNS.to_owned()));
        }
        let input = std::io::stdin();
        let mut line = String::new();
        while std::io::BufRead::read_line(&mut input.lock(), &mut line).unwrap_or(0) > 0 {
            patterns.push(line.trim_end_matches('\n').to_owned());
            line.clear();
        }
    }

    let setup = repo::setup(ctx, Mode::Required)?;
    let config = refs_support::load_config(ctx, &setup)?;
    let store = refs_support::ref_store(&setup, &config);
    let objects = Objects::open(&setup);
    let revs = refs_support::Revs::new(ctx, &setup)?;

    let mut points = Vec::new();
    for p in points_at.borrow().iter() {
        let id = revs.resolve(p)
            .ok_or_else(|| Die::Error(msg::malformed_object_name(p), 129))?;
        points.push(id);
    }

    let iter_opts = IterOptions {
        prefix: String::new(),
        include_root_refs: root_refs.get(),
    };
    let listed = store
        .iter(&iter_opts)
        .map_err(|e| Die::Fatal(e.to_string()))?;
    let icase = ignore_case.get();
    let mut items = Vec::new();
    for entry in listed {
        let r = match entry {
            IterItem::Ref(r) => r,
            IterItem::Broken(n) | IterItem::BadName(n) => {
                if matches_any(&patterns, &n, icase) || patterns.is_empty() {
                    eprintln!("warning: {}", msg::ignoring_broken_ref(&n));
                }
                continue;
            }
        };
        let Some(oid) = r.oid else { continue };
        if !patterns.is_empty() && !matches_any(&patterns, &r.name, icase) {
            continue;
        }
        if matches_any(&excludes.borrow(), &r.name, icase) {
            continue;
        }
        if let Some(marker) = start_after.borrow().as_deref() {
            if r.name.as_bytes() <= marker.as_bytes() {
                continue;
            }
        }
        if !points.is_empty() {
            let peeled = objects.peel(&oid);
            if !points.contains(&oid) && !peeled.is_some_and(|p| points.contains(&p)) {
                continue;
            }
        }
        let symref = match &r.target {
            Target::Symbolic(t) => Some(t.clone()),
            Target::Object(_) => None,
        };
        items.push(Item {
            name: r.name,
            oid,
            symref,
        });
    }

    let ctx_vals = Values {
        objects: &objects,
        store: &store,
        head: store.read_raw("HEAD").ok().flatten(),
    };
    if !sort_keys.is_empty() {
        let mut keyed: Vec<(Vec<Value>, Item)> = Vec::new();
        for it in items {
            let mut vals = Vec::new();
            for (atom, _) in &sort_keys {
                vals.push(ctx_vals.value(atom, &it)?);
            }
            keyed.push((vals, it));
        }
        keyed.sort_by(|(a, ia), (b, ib)| {
            for (k, (_, reverse)) in sort_keys.iter().enumerate() {
                let o = compare(&a[k], &b[k], icase);
                let o = if *reverse { o.reverse() } else { o };
                if o != Ordering::Equal {
                    return o;
                }
            }
            ia.name.as_bytes().cmp(ib.name.as_bytes())
        });
        items = keyed.into_iter().map(|(_, it)| it).collect();
    }

    let mut out = std::io::stdout().lock();
    for (n, it) in items.iter().enumerate() {
        if max.is_some_and(|m| m > 0 && n >= m) {
            break;
        }
        let mut line = Vec::new();
        for p in &pieces {
            match p {
                Piece::Lit(b) => line.extend_from_slice(b),
                Piece::Atom(a) => {
                    let v = ctx_vals.value(a, it)?;
                    let s = v.text();
                    line.extend_from_slice(quote_value(&s, quote.get()).as_bytes());
                }
            }
        }
        if omit_empty.get() && line.is_empty() {
            continue;
        }
        line.push(b'\n');
        let _ = out.write_all(&line);
    }
    Ok(0)
}

enum FormatError {
    /// `malformed format string <rest>` (usage error).
    Malformed(String),
    /// Other errors (`fatal:`).
    Fatal(String),
}

fn parse_format(fmt: &str) -> Result<Vec<Piece>, FormatError> {
    let b = fmt.as_bytes();
    let mut pieces = Vec::new();
    let mut lit = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'%' {
            lit.push(b[i]);
            i += 1;
            continue;
        }
        match b.get(i + 1) {
            Some(b'%') => {
                lit.push(b'%');
                i += 2;
            }
            Some(b'(') => {
                let Some(end) = fmt[i..].find(')') else {
                    return Err(FormatError::Malformed(fmt[i..].to_owned()));
                };
                let atom = parse_atom(&fmt[i + 2..i + end])?;
                if !lit.is_empty() {
                    pieces.push(Piece::Lit(std::mem::take(&mut lit)));
                }
                pieces.push(Piece::Atom(atom));
                i += end + 1;
            }
            _ => {
                let hex = b.get(i + 1..i + 3).and_then(|h| {
                    std::str::from_utf8(h)
                        .ok()
                        .and_then(|h| u8::from_str_radix(h, 16).ok())
                });
                match hex {
                    Some(v) => {
                        lit.push(v);
                        i += 3;
                    }
                    None => {
                        lit.push(b'%');
                        i += 1;
                    }
                }
            }
        }
    }
    if !lit.is_empty() {
        pieces.push(Piece::Lit(lit));
    }
    Ok(pieces)
}

fn parse_atom(spec: &str) -> Result<Atom, FormatError> {
    let (deref, spec) = match spec.strip_prefix('*') {
        Some(s) => (true, s),
        None => (false, spec),
    };
    let (name, arg) = match spec.split_once(':') {
        Some((n, a)) => (n, Some(a)),
        None => (spec, None),
    };
    let bad = |a: &str| FormatError::Fatal(msg::unrecognized_arg(name, a));
    let kind = match name {
        "refname" | "symref" => {
            let m = match arg {
                None => NameMod::Full,
                Some("short") => NameMod::Short,
                Some(a) => {
                    if let Some(n) = a
                        .strip_prefix("lstrip=")
                        .or_else(|| a.strip_prefix("strip="))
                    {
                        NameMod::Lstrip(n.parse().map_err(|_| bad(a))?)
                    } else if let Some(n) = a.strip_prefix("rstrip=") {
                        NameMod::Rstrip(n.parse().map_err(|_| bad(a))?)
                    } else {
                        return Err(bad(a));
                    }
                }
            };
            if name == "refname" {
                AtomKind::Refname(m)
            } else {
                AtomKind::Symref(m)
            }
        }
        "objectname" => match arg {
            None => AtomKind::Objectname(None),
            Some("short") => AtomKind::Objectname(Some(7)),
            Some(a) => match a.strip_prefix("short=") {
                Some(n) => AtomKind::Objectname(Some(
                    n.parse::<i64>().map_err(|_| bad(a))?.max(0) as usize,
                )),
                None => return Err(bad(a)),
            },
        },
        "objecttype" | "objectsize" | "HEAD" => {
            if arg.is_some() {
                return Err(FormatError::Fatal(msg::no_arguments(name)));
            }
            match name {
                "objecttype" => AtomKind::Objecttype,
                "objectsize" => AtomKind::Objectsize,
                _ => AtomKind::Head,
            }
        }
        _ => return Err(FormatError::Fatal(msg::unknown_field(spec))),
    };
    Ok(Atom { kind, deref })
}

enum Value {
    Text(String),
    Number(u64),
}

impl Value {
    fn text(&self) -> String {
        match self {
            Value::Text(s) => s.clone(),
            Value::Number(n) => n.to_string(),
        }
    }
}

fn compare(a: &Value, b: &Value, icase: bool) -> Ordering {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.cmp(y),
        _ if icase => a
            .text()
            .to_ascii_lowercase()
            .cmp(&b.text().to_ascii_lowercase()),
        _ => a.text().as_bytes().cmp(b.text().as_bytes()),
    }
}

struct Values<'a> {
    objects: &'a Objects,
    store: &'a FilesStore,
    head: Option<Target>,
}

impl Values<'_> {
    fn value(&self, atom: &Atom, it: &Item) -> Result<Value, Die> {
        let missing = || Die::Fatal(msg::missing_object(&it.oid.to_hex(), &it.name));
        // The object the atom describes.
        let oid = if atom.deref {
            match atom.kind {
                AtomKind::Objectname(_) | AtomKind::Objecttype | AtomKind::Objectsize => {
                    let (kind, _) = self.objects.read(&it.oid).ok_or_else(missing)?;
                    if kind != rgit_object::ObjectKind::Tag {
                        return Ok(Value::Text(String::new()));
                    }
                    let p = self.objects.peel(&it.oid).ok_or_else(missing)?;
                    Some(p)
                }
                _ => return Ok(Value::Text(String::new())),
            }
        } else {
            Some(it.oid)
        };
        let oid = oid.unwrap_or(it.oid);
        Ok(match atom.kind {
            AtomKind::Refname(m) => Value::Text(self.shorten(&it.name, m)),
            AtomKind::Symref(m) => match &it.symref {
                Some(t) => Value::Text(self.shorten(t, m)),
                None => Value::Text(String::new()),
            },
            AtomKind::Objectname(None) => Value::Text(oid.to_hex()),
            AtomKind::Objectname(Some(n)) => Value::Text(self.objects.abbrev(&oid, n)),
            AtomKind::Objecttype => {
                let kind = self.objects.kind(&oid).ok_or_else(missing)?;
                Value::Text(kind.name().to_owned())
            }
            AtomKind::Objectsize => {
                let (_, data) = self.objects.read(&oid).ok_or_else(missing)?;
                Value::Number(data.len() as u64)
            }
            AtomKind::Head => {
                let is_head = matches!(&self.head, Some(Target::Symbolic(t)) if *t == it.name);
                Value::Text(if is_head { "*" } else { " " }.to_owned())
            }
        })
    }

    fn shorten(&self, name: &str, m: NameMod) -> String {
        match m {
            NameMod::Full => name.to_owned(),
            NameMod::Short => shorten_unambiguous(self.store, name),
            NameMod::Lstrip(n) => lstrip(name, n),
            NameMod::Rstrip(n) => rstrip(name, n),
        }
    }
}

/// `:lstrip=<n>`: drop `n` leading components (negative: keep `-n`
/// trailing ones).
fn lstrip(name: &str, n: i64) -> String {
    let parts: Vec<&str> = name.split('/').collect();
    let drop = if n >= 0 {
        n as usize
    } else {
        parts.len().saturating_sub((-n) as usize)
    };
    if drop >= parts.len() {
        return String::new();
    }
    parts[drop..].join("/")
}

/// `:rstrip=<n>`: drop `n` trailing components (negative: keep `-n`
/// leading ones).
fn rstrip(name: &str, n: i64) -> String {
    let parts: Vec<&str> = name.split('/').collect();
    let keep = if n >= 0 {
        parts.len().saturating_sub(n as usize)
    } else {
        ((-n) as usize).min(parts.len())
    };
    parts[..keep].join("/")
}

/// The shortest name that still resolves to `name` with git's lookup
/// rules (`refname:short`).
fn shorten_unambiguous(store: &FilesStore, name: &str) -> String {
    const RULES: [(&str, &str); 6] = [
        ("", ""),
        ("refs/", ""),
        ("refs/tags/", ""),
        ("refs/heads/", ""),
        ("refs/remotes/", ""),
        ("refs/remotes/", "/HEAD"),
    ];
    for i in (1..RULES.len()).rev() {
        let (pre, suf) = RULES[i];
        let Some(short) = name
            .strip_prefix(pre)
            .and_then(|s| s.strip_suffix(suf))
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let ambiguous = RULES[..i].iter().any(|(p, s)| {
            let candidate = format!("{p}{short}{s}");
            candidate != name && matches!(store.read_raw(&candidate), Ok(Some(_)))
        });
        if !ambiguous {
            return short.to_owned();
        }
    }
    name.to_owned()
}

/// Literal prefix match up to a `/`, or a glob with `*` not crossing `/`.
fn matches_any(patterns: &[String], name: &str, icase: bool) -> bool {
    patterns.iter().any(|p| {
        let (p, n) = if icase {
            (p.to_ascii_lowercase(), name.to_ascii_lowercase())
        } else {
            (p.clone(), name.to_owned())
        };
        if let Some(rest) = n.strip_prefix(p.as_str()) {
            if rest.is_empty() || p.ends_with('/') || rest.starts_with('/') {
                return true;
            }
        }
        p.contains(['*', '?', '[', '\\'])
            && wildmatch(p.as_bytes(), n.as_bytes(), WildmatchFlags::PATHNAME)
    })
}

fn quote_value(s: &str, q: Quote) -> String {
    match q {
        Quote::None => s.to_owned(),
        Quote::Shell => crate::context::sq_quote(s),
        Quote::Perl => {
            let mut o = String::from("'");
            for c in s.chars() {
                if c == '\'' || c == '\\' {
                    o.push('\\');
                }
                o.push(c);
            }
            o.push('\'');
            o
        }
        Quote::Python => {
            let mut o = String::from("'");
            for c in s.chars() {
                match c {
                    '\'' | '\\' => {
                        o.push('\\');
                        o.push(c);
                    }
                    '\n' => o.push_str("\\n"),
                    _ => o.push(c),
                }
            }
            o.push('\'');
            o
        }
        Quote::Tcl => {
            let mut o = String::from("\"");
            for c in s.chars() {
                match c {
                    '[' | ']' | '{' | '}' | '$' | '\\' | '"' => {
                        o.push('\\');
                        o.push(c);
                    }
                    '\x0c' => o.push_str("\\f"),
                    '\r' => o.push_str("\\r"),
                    '\n' => o.push_str("\\n"),
                    '\t' => o.push_str("\\t"),
                    '\x0b' => o.push_str("\\v"),
                    _ => o.push(c),
                }
            }
            o.push('"');
            o
        }
    }
}

/// `--format` strings for the other reference listing commands
/// (`tag`), with the atoms implemented here.
pub(crate) struct RefFormat {
    pieces: Vec<Piece>,
}

impl RefFormat {
    /// Parses a format string.
    pub(crate) fn parse(fmt: &str) -> Result<RefFormat, Die> {
        match parse_format(fmt) {
            Ok(pieces) => Ok(RefFormat { pieces }),
            Err(FormatError::Malformed(rest)) => Err(Die::Fatal(msg::malformed_format(&rest))),
            Err(FormatError::Fatal(m)) => Err(Die::Fatal(m)),
        }
    }
}

/// A sort key atom (`--sort=<key>` without `-` and `version:`).
#[derive(Clone, Copy)]
pub(crate) struct SortAtom(Atom);

impl SortAtom {
    pub(crate) fn parse(name: &str) -> Result<SortAtom, Die> {
        parse_atom(name).map(SortAtom).map_err(|e| match e {
            FormatError::Fatal(m) | FormatError::Malformed(m) => Die::Fatal(m),
        })
    }
}

/// A sort value: numbers compare as numbers, everything else as bytes.
pub(crate) struct SortValue(Value);

impl SortValue {
    pub(crate) fn text(&self) -> String {
        self.0.text()
    }

    pub(crate) fn compare(&self, other: &SortValue, icase: bool) -> Ordering {
        compare(&self.0, &other.0, icase)
    }
}

/// Expands atoms for the references of one repository.
pub(crate) struct RefValues<'a>(Values<'a>);

impl<'a> RefValues<'a> {
    pub(crate) fn new(objects: &'a Objects, store: &'a FilesStore) -> Self {
        RefValues(Values {
            objects,
            store,
            head: store.read_raw("HEAD").ok().flatten(),
        })
    }

    fn item(name: &str, oid: ObjectId) -> Item {
        Item {
            name: name.to_owned(),
            oid,
            symref: None,
        }
    }

    /// The value of `atom` for reference `name` at `oid`.
    pub(crate) fn value(&self, atom: SortAtom, name: &str, oid: ObjectId) -> Result<SortValue, Die> {
        self.0.value(&atom.0, &Self::item(name, oid)).map(SortValue)
    }

    /// `format` expanded for reference `name` at `oid`.
    pub(crate) fn render(&self, format: &RefFormat, name: &str, oid: ObjectId) -> Result<Vec<u8>, Die> {
        let it = Self::item(name, oid);
        let mut line = Vec::new();
        for p in &format.pieces {
            match p {
                Piece::Lit(b) => line.extend_from_slice(b),
                Piece::Atom(a) => line.extend_from_slice(self.0.value(a, &it)?.text().as_bytes()),
            }
        }
        Ok(line)
    }
}
