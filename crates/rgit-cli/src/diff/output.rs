// SPDX-License-Identifier: MIT
//! Writing a list of file pairs in the requested formats
//! (Documentation/diff-format.adoc, diff-generate-patch.adoc): raw,
//! `--name-only`, `--name-status`, `--numstat`, `--stat`, `--shortstat`,
//! `--summary` and patches, in that order.

use std::io::Write;

use rgit_diff::diffcore::{self, Detect, filter::DiffFilter, pickaxe::Criterion};
use rgit_diff::{
    DiffOptions, PatchExtra, PatchFile, PatchOptions, diff_text, is_binary, quote_path,
    write_file_patch_extra,
};
use rgit_hash::{HashKind, ObjectId};
use rgit_object::Object;

use rgit_config::ConfigSet;

use super::collect::{GITLINK_MODE, Pair, Side, TREE_MODE, type_bits};
use super::color::DiffColors;
use super::options::{DiffOpts, Formats, usage_exit};
use super::stat::{self, FileStat, StatKind, StatLayout};
use crate::Die;
use crate::messages::diff as msg;
use crate::repository::Repo;

/// Output settings, resolved from options and configuration.
pub struct Output<'a> {
    pub repo: Option<&'a Repo>,
    /// Whether work tree files are converted to their blob form (not with
    /// `--no-index`).
    pub convert: bool,
    pub hash: HashKind,
    pub formats: Formats,
    pub nul: bool,
    pub reverse: bool,
    pub quote_high: bool,
    /// Digits of object ids in raw output; 0 for full ids.
    pub raw_abbrev: usize,
    /// Digits of object ids on `index` lines; 0 for full ids.
    pub index_abbrev: usize,
    pub patch: PatchOptions,
    pub stat: StatLayout,
    /// `git diff`: pairs whose sides turn out to be identical are dropped.
    pub porcelain: bool,
    /// Unmerged paths would need a combined diff in patch output.
    pub combined_unmerged: bool,
    /// Colours, when colouring.
    pub colors: Option<DiffColors>,
    /// `--compact-summary`: creations, deletions and mode changes go into
    /// the stat names ("(new)", "(gone)", "(mode +x)") instead of summary
    /// lines.
    pub compact_summary: bool,
    /// Break, rename and copy detection.
    pub core: diffcore::Options,
    /// `-S` / `-G` / `--find-object`.
    pub pickaxe: Option<Pickaxe>,
    pub pickaxe_all: bool,
    pub filter: DiffFilter,
    pub irreversible_delete: bool,
    /// The rename limit that would have been needed (warned about after
    /// the output).
    limit_needed: std::cell::Cell<Option<(usize, bool)>>,
}

/// A compiled pickaxe criterion.
pub enum Pickaxe {
    Literal(Vec<u8>),
    Count(regex::bytes::Regex),
    Grep(regex::bytes::Regex),
    Objects(Vec<ObjectId>),
}

/// Compile a pickaxe pattern (an extended POSIX regular expression).
fn pickaxe_regex(pattern: &str) -> Result<regex::bytes::Regex, Die> {
    let translated = rgit_revwalk::walk::grep::translate(pattern, true)
        .map_err(|reason| Die::Fatal(msg::invalid_regex(pattern, reason)))?;
    regex::bytes::RegexBuilder::new(&translated)
        .unicode(false)
        .build()
        .map_err(|e| {
            let reason = rgit_revwalk::walk::grep::reason(&e.to_string());
            Die::Fatal(msg::invalid_regex(pattern, reason))
        })
}

/// The diffcore settings: `diff.renames` (porcelain only),
/// `diff.renameLimit` and the command line.
fn core_options(
    repo: Option<&Repo>,
    opts: &DiffOpts,
    porcelain: bool,
) -> Result<diffcore::Options, Die> {
    let mut core = diffcore::Options::default();
    if porcelain {
        if let Some(e) = repo.and_then(|r| r.config.get("diff.renames")) {
            let v = e.value.as_ref().map(|v| v.to_string()).unwrap_or_default();
            core.detect = match v.to_ascii_lowercase().as_str() {
                "copies" | "copy" => Detect::Copies,
                _ => {
                    if e.bool().map_err(|e| Die::Fatal(e.to_string()))? {
                        Detect::Renames
                    } else {
                        Detect::Off
                    }
                }
            };
        } else {
            core.detect = Detect::Renames;
        }
    }
    if let Some(r) = repo {
        if let Some(e) = r.config.get("diff.renamelimit") {
            let n = e
                .int(rgit_config::IntWidth::I32)
                .map_err(|e| Die::Fatal(e.to_string()))?;
            core.rename_limit = usize::try_from(n.max(0)).unwrap_or(0);
        }
    }
    if let Some(d) = opts.detect {
        core.detect = d;
    }
    if opts.copies_harder && opts.detect != Some(Detect::Off) {
        core.detect = Detect::Copies;
    }
    core.find_copies_harder = opts.copies_harder;
    if let Some(s) = opts.rename_score {
        core.rename_score = s;
    }
    core.break_rewrites = opts.break_rewrites;
    if let Some(l) = opts.rename_limit {
        core.rename_limit = l;
    }
    if let Some(e) = opts.rename_empty {
        core.rename_empty = e;
    }
    Ok(core)
}

/// The `--compact-summary` note of a pair, if any.
fn compact_note(old: Option<&Side>, new: Option<&Side>) -> Option<String> {
    let flags = |mode: u32| match mode & 0o170000 {
        0o120000 => " +l",
        _ if mode & 0o111 != 0 => " +x",
        _ => "",
    };
    match (old, new) {
        (None, Some(n)) => Some(format!("new{}", flags(n.mode))),
        (Some(o), None) => Some(format!("gone{}", flags(o.mode))),
        (Some(o), Some(n)) if o.mode != n.mode && (o.mode & 0o170000) == (n.mode & 0o170000) => {
            Some(
                if n.mode & 0o111 != 0 {
                    "mode +x"
                } else {
                    "mode -x"
                }
                .to_owned(),
            )
        }
        _ => None,
    }
}

/// A side with its contents loaded.
struct Loaded {
    mode: u32,
    oid: ObjectId,
    data: Vec<u8>,
}

fn is_file_mode(mode: u32) -> bool {
    mode != TREE_MODE && type_bits(mode) != GITLINK_MODE
}

impl<'a> Output<'a> {
    /// Settings from the options and the repository configuration.
    pub fn new(
        repo: Option<&'a Repo>,
        hash: HashKind,
        opts: &DiffOpts,
        formats: Formats,
        porcelain: bool,
    ) -> Result<Output<'a>, Die> {
        let config_bool = |key: &str| repo.and_then(|r| r.config.bool(key).ok().flatten());
        let config_int = |key: &str| {
            repo.and_then(|r| r.config.int(key).ok().flatten())
                .and_then(|v| usize::try_from(v).ok())
        };
        let default_abbrev = repo.map_or(7, |r| r.default_abbrev());
        let abbrev_len = |a: Option<usize>| match a {
            None => default_abbrev,
            Some(0) => 0,
            Some(n) => n.max(crate::repository::MIN_ABBREV),
        };
        let raw_abbrev = match opts.abbrev {
            None if porcelain => default_abbrev,
            None => 0,
            Some(a) => abbrev_len(a),
        };
        let index_abbrev = if opts.full_index {
            0
        } else {
            match opts.abbrev {
                Some(Some(n)) if n > 0 => n.max(crate::repository::MIN_ABBREV),
                _ => default_abbrev,
            }
        };
        let mut diff = DiffOptions::default();
        if porcelain {
            if let Some(c) = config_int("diff.context") {
                diff.context = c;
            }
            if let Some(c) = config_int("diff.interhunkcontext") {
                diff.interhunk_context = c;
            }
        }
        if let Some(h) = config_bool("diff.indentheuristic") {
            diff.indent_heuristic = h;
        }
        if let Some(c) = opts.context {
            diff.context = c;
        }
        if let Some(c) = opts.interhunk {
            diff.interhunk_context = c;
        }
        // GIT_DIFF_OPTS=--unified=<n> or -u<n> wins over the command line
        // (Documentation/git.adoc).
        if let Ok(v) = std::env::var("GIT_DIFF_OPTS") {
            let n = v
                .strip_prefix("--unified=")
                .or_else(|| v.strip_prefix("-u"));
            if let Some(n) = n.and_then(|n| n.parse::<usize>().ok()) {
                diff.context = n;
            }
        }
        if let Some(h) = opts.indent_heuristic {
            diff.indent_heuristic = h;
        }
        // diff.algorithm applies to the porcelain commands only; the
        // command line wins (`--minimal` means minimal Myers).
        if porcelain {
            if let Some(e) = repo.and_then(|r| r.config.get("diff.algorithm")) {
                let v = e.value.as_ref().map(|v| v.to_string()).unwrap_or_default();
                let Some((algorithm, minimal)) = rgit_diff::Algorithm::parse(&v) else {
                    crate::messages::print_error(&msg::unknown_config_value("diff.algorithm", &v));
                    return Err(Die::Fatal(crate::revisions::unable_to_parse(
                        "diff.algorithm",
                        e,
                    )));
                };
                diff.algorithm = algorithm;
                diff.minimal = minimal;
            }
        }
        if let Some(a) = opts.algorithm {
            diff.algorithm = a;
        }
        if let Some(m) = opts.minimal {
            diff.minimal = m;
        }
        diff.anchors = opts.anchors.clone();
        let quote_high = repo.is_none_or(|r| r.quote_path());
        let mut src = b"a/".to_vec();
        let mut dst = b"b/".to_vec();
        if porcelain && !opts.default_prefix && config_bool("diff.noprefix") == Some(true) {
            src.clear();
            dst.clear();
        }
        if opts.no_prefix {
            src.clear();
            dst.clear();
        }
        if let Some(p) = &opts.src_prefix {
            src = p.clone();
        }
        if let Some(p) = &opts.dst_prefix {
            dst = p.clone();
        }
        if opts.reverse {
            std::mem::swap(&mut src, &mut dst);
        }
        let (stat_name_width, stat_graph_width) = if porcelain {
            (
                config_int("diff.statnamewidth"),
                config_int("diff.statgraphwidth"),
            )
        } else {
            (None, None)
        };
        let core = core_options(repo, opts, porcelain)?;
        let pickaxe = if !opts.find_object.is_empty() {
            if opts.found_objects.len() != opts.find_object.len() {
                return Err(usage_exit(msg::unable_to_resolve(&opts.find_object[0])));
            }
            Some(Pickaxe::Objects(opts.found_objects.clone()))
        } else {
            match &opts.pickaxe {
                None => None,
                Some((true, _)) if opts.pickaxe_regex => {
                    return Err(Die::Fatal(msg::G_WITH_PICKAXE_REGEX.to_string()));
                }
                Some((true, p)) => Some(Pickaxe::Grep(pickaxe_regex(p)?)),
                Some((false, p)) if opts.pickaxe_regex => Some(Pickaxe::Count(pickaxe_regex(p)?)),
                Some((false, p)) => Some(Pickaxe::Literal(p.as_bytes().to_vec())),
            }
        };
        Ok(Output {
            core,
            pickaxe,
            pickaxe_all: opts.pickaxe_all,
            filter: opts.diff_filter.clone(),
            irreversible_delete: opts.irreversible_delete,
            limit_needed: std::cell::Cell::new(None),
            repo,
            convert: repo.is_some(),
            hash,
            formats,
            nul: opts.nul,
            reverse: opts.reverse,
            quote_high,
            raw_abbrev,
            index_abbrev,
            patch: PatchOptions {
                diff,
                abbrev: index_abbrev,
                quote_path: quote_high,
                src_prefix: src,
                dst_prefix: dst,
            },
            stat: StatLayout {
                width: opts.stat_width.unwrap_or(80),
                name_width: opts.stat_name_width.or(stat_name_width),
                graph_width: opts.stat_graph_width.or(stat_graph_width),
                count: opts.stat_count,
            },
            porcelain,
            combined_unmerged: false,
            compact_summary: false,
            colors: None,
        })
    }

    /// Decide about colours: `--color` (`opts.color`), and for `git diff`
    /// (`porcelain`) also `color.diff` and `color.ui`.
    pub fn set_color(&mut self, opts: &DiffOpts) -> Result<(), Die> {
        let empty = ConfigSet::new();
        let config = self.repo.map_or(&empty, |r| &r.config);
        let on = if self.porcelain {
            crate::color::want(config, Some("color.diff"), opts.color)?
        } else {
            match opts.color {
                Some(c) => crate::color::want(config, None, Some(c))?,
                None => false,
            }
        };
        self.colors = if on {
            Some(DiffColors::from_config(config)?)
        } else {
            None
        };
        Ok(())
    }

    /// Set the mnemonic prefixes (`diff.mnemonicPrefix`) for `git diff`.
    pub fn mnemonic(&mut self, opts: &DiffOpts, src: &str, dst: &str) {
        let on = self
            .repo
            .and_then(|r| r.config.bool("diff.mnemonicprefix").ok().flatten())
            == Some(true);
        if !on
            || opts.no_prefix
            || opts.default_prefix
            || opts.src_prefix.is_some()
            || opts.dst_prefix.is_some()
        {
            return;
        }
        if self.patch.src_prefix.is_empty() {
            return;
        }
        let (mut s, mut d) = (
            format!("{src}/").into_bytes(),
            format!("{dst}/").into_bytes(),
        );
        if self.reverse {
            std::mem::swap(&mut s, &mut d);
        }
        self.patch.src_prefix = s;
        self.patch.dst_prefix = d;
    }

    fn abbrev(&self, id: &ObjectId, len: usize) -> String {
        if len == 0 {
            return id.to_hex();
        }
        if id.is_null() {
            return "0".repeat(len.min(id.to_hex().len()));
        }
        match self.repo {
            Some(r) => r.abbrev(id, len),
            None => id.to_hex_with_len(len),
        }
    }

    fn read_file(&self, side: &Side) -> Result<Vec<u8>, Die> {
        let path = side.file.as_ref().expect("file side");
        let res = if type_bits(side.mode) == 0o120000 {
            std::fs::read_link(path).map(|t| t.into_os_string().into_encoded_bytes())
        } else {
            std::fs::read(path)
        };
        res.map_err(|_| Die::Error(msg::read_failed(&path.to_string_lossy()), 1))
    }

    fn load(&self, side: &Side, path: &[u8]) -> Result<Loaded, Die> {
        if side.file.is_some() {
            let mut data = self.read_file(side)?;
            if let (Some(repo), true) = (self.repo, self.convert && is_file_mode(side.mode)) {
                let conv = repo.converter()?;
                let opts = rgit_convert::Options::inspect(conv.config());
                data = conv.to_git(path, data, opts)?;
            }
            let oid = rgit_hash::hash_object(self.hash, "blob", &data)
                .map_err(|e| Die::Fatal(e.to_string()))?;
            return Ok(Loaded {
                mode: side.mode,
                oid,
                data,
            });
        }
        let data = if is_file_mode(side.mode) {
            match self.repo.and_then(|r| r.read_object(&side.oid)) {
                Some(Object::Blob(b)) => b.data,
                _ => Vec::new(),
            }
        } else {
            Vec::new()
        };
        Ok(Loaded {
            mode: side.mode,
            oid: side.oid,
            data,
        })
    }

    fn write_path(&self, out: &mut Vec<u8>, path: &[u8]) {
        if self.nul {
            out.extend_from_slice(path);
            out.push(0);
        } else {
            quote_path(out, path, self.quote_high);
            out.push(b'\n');
        }
    }

    /// The status letter (with the score of renames, copies and
    /// rewrites) and the path(s) of a raw or `--name-status` line.
    fn write_status(&self, out: &mut Vec<u8>, p: &Pair) {
        out.push(p.status());
        if p.is_rename() || p.is_rewrite() {
            write!(out, "{:03}", p.percent()).unwrap();
        }
        out.push(if self.nul { 0 } else { b'\t' });
        if p.is_rename() {
            if self.nul {
                out.extend_from_slice(&p.old_path);
                out.push(0);
            } else {
                quote_path(out, &p.old_path, self.quote_high);
                out.push(b'\t');
            }
        }
        self.write_path(out, p.shown_path());
    }

    fn display_name(&self, p: &Pair) -> Vec<u8> {
        let q = |path: &[u8]| {
            let mut o = Vec::new();
            quote_path(&mut o, path, self.quote_high);
            o
        };
        if p.old_path == p.path {
            return q(&p.path);
        }
        let (a, b) = (&p.old_path, &p.path);
        let (pre, suf) = crate::tree_changes::rename_split(a, b);
        let mut out = Vec::new();
        if pre > 0 || suf > 0 {
            let mid = |x: &[u8]| q(&x[pre.min(x.len() - suf)..x.len() - suf]);
            out.extend_from_slice(&q(&a[..pre]));
            out.push(b'{');
            out.extend_from_slice(&mid(a));
            out.extend_from_slice(b" => ");
            out.extend_from_slice(&mid(b));
            out.push(b'}');
            out.extend_from_slice(&q(&a[a.len() - suf..]));
        } else {
            out.extend_from_slice(&q(a));
            out.extend_from_slice(b" => ");
            out.extend_from_slice(&q(b));
        }
        out
    }

    /// The contents of a side for diffcore (unreadable contents are empty;
    /// the output reports the error later).
    fn core_contents(&self, side: &diffcore::Side<(usize, Option<std::path::PathBuf>)>) -> Vec<u8> {
        let s = Side {
            mode: side.mode,
            oid: side.oid,
            file: side.extra.1.clone(),
        };
        self.load(&s, &side.path)
            .map(|l| l.data)
            .unwrap_or_default()
    }

    /// Run the diffcore transformations (break, rename/copy detection,
    /// pickaxe, `--diff-filter`) on the pairs.
    pub fn diffcore(&self, pairs: Vec<Pair>) -> Result<Vec<Pair>, Die> {
        if !self.core.active() && self.pickaxe.is_none() && !self.filter.is_active() {
            return Ok(pairs);
        }
        type Extra = (usize, Option<std::path::PathBuf>);
        let side = |i: usize, path: &[u8], s: &Option<Side>| {
            s.as_ref().map(|s| diffcore::Side {
                path: path.to_vec(),
                mode: s.mode,
                oid: s.oid,
                extra: (i, s.file.clone()),
            })
        };
        let mut unmerged: Vec<(usize, Pair)> = Vec::new();
        let mut input: Vec<diffcore::FilePair<Extra>> = Vec::new();
        // The command's pairs, to keep their names (`--no-index` shows
        // `/dev/null` for a missing side).
        let mut originals: Vec<Option<Pair>> = Vec::with_capacity(pairs.len());
        for (i, p) in pairs.into_iter().enumerate() {
            if p.unmerged {
                unmerged.push((i, p));
                originals.push(None);
                continue;
            }
            let one = side(i, &p.old_path, &p.old);
            let two = side(i, &p.path, &p.new);
            input.push(diffcore::FilePair::new(one, two));
            originals.push(Some(p));
        }
        let contents = |s: &diffcore::Side<Extra>| self.core_contents(s);
        let (mut result, report) = diffcore::run(input, &self.core, &contents);
        if let Some(n) = report.rename_limit_needed {
            self.limit_needed
                .set(Some((n, report.copies_from_modified_only)));
        }
        let mut pickaxe_hit = true;
        if let Some(how) = &self.pickaxe {
            let literal;
            let count_re;
            let grep_re;
            let criterion = match how {
                Pickaxe::Literal(s) => {
                    literal = move |d: &[u8]| diffcore::pickaxe::count_literal(d, s);
                    Criterion::Count(&literal)
                }
                Pickaxe::Count(re) => {
                    count_re = move |d: &[u8]| re.find_iter(d).count();
                    Criterion::Count(&count_re)
                }
                Pickaxe::Grep(re) => {
                    grep_re = move |l: &[u8]| re.is_match(l);
                    Criterion::Grep {
                        matches: &grep_re,
                        text: false,
                    }
                }
                Pickaxe::Objects(ids) => Criterion::Objects(ids),
            };
            result = diffcore::pickaxe::pickaxe(result, &criterion, self.pickaxe_all, &contents);
            pickaxe_hit = !result.is_empty();
        }
        // Back to the command's pairs, in order (unmerged paths where they
        // were).
        let mut out: Vec<(usize, Pair)> = Vec::with_capacity(result.len() + unmerged.len());
        for fp in result {
            let anchor = fp.two.as_ref().or(fp.one.as_ref()).map_or(0, |s| s.extra.0);
            let back = |s: &Option<diffcore::Side<Extra>>| {
                s.as_ref().map(|s| Side {
                    mode: s.mode,
                    oid: s.oid,
                    file: s.extra.1.clone(),
                })
            };
            let path = fp
                .two
                .as_ref()
                .or(fp.one.as_ref())
                .map(|s| s.path.clone())
                .unwrap_or_default();
            let old_path = fp
                .one
                .as_ref()
                .map_or_else(|| path.clone(), |s| s.path.clone());
            let renamed = match fp.status {
                diffcore::Status::Renamed => Some(b'R'),
                diffcore::Status::Copied => Some(b'C'),
                _ => None,
            };
            if renamed.is_none() {
                if let Some(mut p) = originals.get_mut(anchor).and_then(Option::take) {
                    p.score = fp.score;
                    out.push((anchor, p));
                    continue;
                }
            }
            out.push((
                anchor,
                Pair {
                    old_path,
                    path,
                    old: back(&fp.one),
                    new: back(&fp.two),
                    unmerged: false,
                    renamed,
                    score: fp.score,
                },
            ));
        }
        // Unmerged paths never match a pickaxe.
        if self.pickaxe.is_none() || (self.pickaxe_all && pickaxe_hit) {
            for (i, p) in unmerged {
                let pos = out.partition_point(|(a, _)| *a < i);
                out.insert(pos, (i, p));
            }
        }
        let pairs = out.into_iter().map(|(_, p)| p).collect();
        Ok(self
            .filter
            .apply_by(pairs, |p: &Pair| (p.status(), p.is_rewrite())))
    }

    /// Write everything; returns the output and whether there were changes.
    pub fn render(&self, mut pairs: Vec<Pair>) -> Result<(Vec<u8>, bool), Die> {
        if self.reverse {
            for p in &mut pairs {
                if !p.unmerged {
                    std::mem::swap(&mut p.old, &mut p.new);
                    std::mem::swap(&mut p.old_path, &mut p.path);
                }
            }
        }
        let pairs = self.diffcore(pairs)?;
        if self.combined_unmerged && self.formats.patch && pairs.iter().any(|p| p.unmerged) {
            return Err(Die::Fatal(msg::not_supported(
                "combined diff of unmerged paths",
            )));
        }
        let f = self.formats;
        // Load contents where needed.
        let need = f.needs_content() || f.summary;
        let mut loaded: Vec<(Option<Loaded>, Option<Loaded>)> = Vec::with_capacity(pairs.len());
        let mut keep = Vec::with_capacity(pairs.len());
        for p in &pairs {
            let has_file = p.old.iter().chain(p.new.iter()).any(|s| s.file.is_some());
            let load = !p.unmerged && (need || (self.porcelain && has_file));
            let (o, n) = if load {
                (
                    p.old
                        .as_ref()
                        .map(|s| self.load(s, &p.old_path))
                        .transpose()?,
                    p.new.as_ref().map(|s| self.load(s, &p.path)).transpose()?,
                )
            } else {
                (None, None)
            };
            let same = matches!((&o, &n), (Some(o), Some(n)) if o.oid == n.oid && o.mode == n.mode);
            keep.push(!(self.porcelain && same && !p.is_rename()));
            loaded.push((o, n));
        }
        let mut items: Vec<(Pair, Option<Loaded>, Option<Loaded>)> = Vec::new();
        for ((p, (o, n)), k) in pairs.into_iter().zip(loaded).zip(keep) {
            if k {
                items.push((p, o, n));
            }
        }
        let changed = !items.is_empty();
        let mut out = Vec::new();

        if f.raw {
            for (p, _, _) in &items {
                let null = ObjectId::null(self.hash);
                let (om, oo) = p.old.as_ref().map_or((0, null), |s| (s.mode, s.oid));
                let (nm, no) = p.new.as_ref().map_or((0, null), |s| (s.mode, s.oid));
                write!(
                    out,
                    ":{om:06o} {nm:06o} {} {} ",
                    self.abbrev(&oo, self.raw_abbrev),
                    self.abbrev(&no, self.raw_abbrev)
                )
                .unwrap();
                self.write_status(&mut out, p);
            }
        }
        if f.name_only || f.name_status {
            for (p, _, _) in &items {
                if f.name_status {
                    self.write_status(&mut out, p);
                } else {
                    self.write_path(&mut out, p.shown_path());
                }
            }
        }

        // Pairs with a visible content or mode difference.
        let differs = |p: &Pair, o: &Option<Loaded>, n: &Option<Loaded>| -> bool {
            if p.unmerged || p.is_rename() {
                return true;
            }
            !matches!((o, n), (Some(o), Some(n)) if o.oid == n.oid && o.mode == n.mode)
        };
        let mut stats: Vec<FileStat> = Vec::new();
        let mut names: Vec<Vec<u8>> = Vec::new();
        if f.numstat || f.stat || f.shortstat {
            for (p, o, n) in &items {
                if !differs(p, o, n) {
                    continue;
                }
                let is_file = |s: &Option<Side>| s.as_ref().is_none_or(|s| is_file_mode(s.mode));
                if !p.unmerged && (!is_file(&p.old) || !is_file(&p.new)) {
                    continue;
                }
                let kind = if p.unmerged {
                    StatKind::Unmerged
                } else {
                    let od = o.as_ref().map_or(&[][..], |l| &l.data[..]);
                    let nd = n.as_ref().map_or(&[][..], |l| &l.data[..]);
                    let same = matches!((o, n), (Some(o), Some(n)) if o.oid == n.oid);
                    if (is_binary(od) || is_binary(nd)) && same {
                        StatKind::BinarySame
                    } else if is_binary(od) || is_binary(nd) {
                        StatKind::Binary {
                            old_size: od.len(),
                            new_size: nd.len(),
                        }
                    } else {
                        let same = matches!((o, n), (Some(o), Some(n)) if o.oid == n.oid);
                        if p.is_rewrite() {
                            StatKind::Text {
                                added: rgit_diff::split_lines(nd).len(),
                                deleted: rgit_diff::split_lines(od).len(),
                            }
                        } else if same {
                            StatKind::Text {
                                added: 0,
                                deleted: 0,
                            }
                        } else {
                            let d = diff_text(od, nd, &self.patch.diff);
                            StatKind::Text {
                                added: d.stats.insertions,
                                deleted: d.stats.deletions,
                            }
                        }
                    }
                };
                let mut name = self.display_name(p);
                names.push(if self.nul && p.is_rename() {
                    let mut n = vec![0];
                    n.extend_from_slice(&p.old_path);
                    n.push(0);
                    n.extend_from_slice(&p.path);
                    n
                } else if self.nul {
                    p.path.clone()
                } else {
                    name.clone()
                });
                if self.compact_summary {
                    if let Some(note) = compact_note(p.old.as_ref(), p.new.as_ref()) {
                        name.extend_from_slice(format!(" ({note})").as_bytes());
                    }
                }
                stats.push(FileStat { name, kind });
            }
        }
        if f.numstat {
            stat::write_numstat(&mut out, &stats, &names, self.nul);
        }
        if f.stat {
            let colors = self
                .colors
                .as_ref()
                .map(|c| (c.new.as_str(), c.old.as_str()));
            stat::write_stat(&mut out, &stats, &self.stat, colors);
        } else if f.shortstat && !stats.is_empty() {
            stat::write_summary_line(&mut out, &stats);
        }
        if f.summary && !self.compact_summary {
            for (p, o, n) in &items {
                if p.unmerged || !differs(p, o, n) {
                    continue;
                }
                let name = self.display_name(p);
                if let Some(c) = p.renamed {
                    let word = if c == b'C' { "copy" } else { "rename" };
                    write!(out, " {word} ").unwrap();
                    out.extend_from_slice(&name);
                    writeln!(out, " ({}%)", p.percent()).unwrap();
                } else if p.is_rewrite() {
                    out.extend_from_slice(b" rewrite ");
                    out.extend_from_slice(&name);
                    writeln!(out, " ({}%)", p.percent()).unwrap();
                }
                let mut line = |what: &str, mode: u32| {
                    write!(out, " {what} mode {mode:06o} ").unwrap();
                    out.extend_from_slice(&name);
                    out.push(b'\n');
                };
                match (&p.old, &p.new) {
                    (None, Some(s)) => line("create", s.mode),
                    (Some(s), None) => line("delete", s.mode),
                    (Some(a), Some(b)) if a.mode != b.mode && p.is_rename() => {
                        writeln!(out, " mode change {:06o} => {:06o}", a.mode, b.mode).unwrap();
                    }
                    (Some(a), Some(b)) if a.mode != b.mode => {
                        write!(out, " mode change {:06o} => {:06o} ", a.mode, b.mode).unwrap();
                        out.extend_from_slice(&name);
                        out.push(b'\n');
                    }
                    _ => {}
                }
            }
        }
        if f.patch {
            if changed && (f.raw || f.numstat || f.stat || f.shortstat || f.summary) {
                out.push(b'\n');
            }
            let abbrev = |id: &ObjectId| self.abbrev(id, self.index_abbrev);
            for (p, o, n) in &items {
                if p.unmerged {
                    out.extend_from_slice(msg::UNMERGED_PATH.as_bytes());
                    let mut q = Vec::new();
                    quote_path(&mut q, &p.path, self.quote_high);
                    out.extend_from_slice(&q);
                    out.push(b'\n');
                    continue;
                }
                if !differs(p, o, n) {
                    continue;
                }
                let is_file = |s: &Option<Side>| s.as_ref().is_none_or(|s| is_file_mode(s.mode));
                if !is_file(&p.old) || !is_file(&p.new) {
                    continue;
                }
                let of = o.as_ref().map(|l| PatchFile {
                    path: &p.old_path,
                    mode: l.mode,
                    oid: l.oid,
                    data: &l.data,
                });
                let nf = n.as_ref().map(|l| PatchFile {
                    path: &p.path,
                    mode: l.mode,
                    oid: l.oid,
                    data: &l.data,
                });
                let new_data = nf.as_ref().map_or(&[][..], |f| f.data);
                let extra = PatchExtra {
                    rename: p.renamed.map(|c| (c == b'C', p.percent())),
                    rewrite: p.is_rewrite().then(|| p.percent()),
                    irreversible_delete: self.irreversible_delete,
                };
                let mut emit =
                    |old: Option<&PatchFile<'_>>, new: Option<&PatchFile<'_>>| match &self.colors {
                        None => {
                            write_file_patch_extra(
                                &mut out,
                                old,
                                new,
                                &self.patch,
                                &abbrev,
                                &extra,
                            );
                        }
                        Some(c) => {
                            let mut plain = Vec::new();
                            write_file_patch_extra(
                                &mut plain,
                                old,
                                new,
                                &self.patch,
                                &abbrev,
                                &extra,
                            );
                            let data = if new.is_some() { new_data } else { &[][..] };
                            super::color::patch(&mut out, c, &plain, data);
                        }
                    };
                if p.status() == b'T' {
                    emit(of.as_ref(), None);
                    emit(None, nf.as_ref());
                } else {
                    emit(of.as_ref(), nf.as_ref());
                }
            }
        }
        Ok((out, changed))
    }
}

impl Output<'_> {
    /// Write the output to stdout, then the warnings, and compute the
    /// exit code.
    pub fn finish(&self, out: &[u8], changed: bool, opts: &DiffOpts) -> i32 {
        let code = finish(out, changed, opts);
        self.warn();
        code
    }

    /// Warn that the rename limit was exceeded.
    pub fn warn(&self) {
        if let Some((n, degraded)) = self.limit_needed.take() {
            let mut e = std::io::stderr().lock();
            let _ = e.write_all(if degraded {
                msg::RENAME_LIMIT_DEGRADED.as_bytes()
            } else {
                msg::RENAME_LIMIT_SKIPPED.as_bytes()
            });
            let _ = e.write_all(msg::rename_limit_hint(n).as_bytes());
        }
    }
}

/// Write the output to stdout and compute the exit code.
pub fn finish(out: &[u8], changed: bool, opts: &DiffOpts) -> i32 {
    let mut o = std::io::stdout().lock();
    let _ = o.write_all(out);
    let _ = o.flush();
    i32::from(opts.wants_exit_code() && changed)
}
