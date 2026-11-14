// SPDX-License-Identifier: MIT
//! Writing a list of file pairs in the requested formats
//! (Documentation/diff-format.adoc, diff-generate-patch.adoc): raw,
//! `--name-only`, `--name-status`, `--numstat`, `--stat`, `--shortstat`,
//! `--summary` and patches, in that order.

use std::io::Write;

use rgit_diff::{
    DiffOptions, PatchFile, PatchOptions, diff_text, is_binary, quote_path, write_file_patch_with,
};
use rgit_hash::{HashKind, ObjectId};
use rgit_object::Object;

use rgit_config::ConfigSet;

use super::collect::{GITLINK_MODE, Pair, Side, TREE_MODE, type_bits};
use super::color::DiffColors;
use super::options::{DiffOpts, Formats};
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
        Ok(Output {
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
        // Common leading directories and trailing components.
        let mut pre = 0;
        for (i, (x, y)) in a.iter().zip(b.iter()).enumerate() {
            if x != y {
                break;
            }
            if *x == b'/' {
                pre = i + 1;
            }
        }
        let mut suf = 0;
        let max_suf = a.len().min(b.len()) - pre;
        for i in 1..=max_suf {
            if a[a.len() - i] != b[b.len() - i] {
                break;
            }
            if a[a.len() - i] == b'/' {
                suf = i;
            }
        }
        let mut out = Vec::new();
        if pre > 0 || suf > 0 {
            out.extend_from_slice(&q(&a[..pre]));
            out.push(b'{');
            out.extend_from_slice(&q(&a[pre..a.len() - suf]));
            out.extend_from_slice(b" => ");
            out.extend_from_slice(&q(&b[pre..b.len() - suf]));
            out.push(b'}');
            out.extend_from_slice(&q(&a[a.len() - suf..]));
        } else {
            out.extend_from_slice(&q(a));
            out.extend_from_slice(b" => ");
            out.extend_from_slice(&q(b));
        }
        out
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
            keep.push(!(self.porcelain && same));
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
                out.push(p.status());
                out.push(if self.nul { 0 } else { b'\t' });
                self.write_path(&mut out, p.shown_path());
            }
        }
        if f.name_only || f.name_status {
            for (p, _, _) in &items {
                if f.name_status {
                    out.push(p.status());
                    out.push(if self.nul { 0 } else { b'\t' });
                }
                self.write_path(&mut out, p.shown_path());
            }
        }

        // Pairs with a visible content or mode difference.
        let differs = |p: &Pair, o: &Option<Loaded>, n: &Option<Loaded>| -> bool {
            if p.unmerged {
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
                    if is_binary(od) || is_binary(nd) {
                        StatKind::Binary {
                            old_size: od.len(),
                            new_size: nd.len(),
                        }
                    } else {
                        let same = matches!((o, n), (Some(o), Some(n)) if o.oid == n.oid);
                        if same {
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
                let name = self.display_name(p);
                names.push(if self.nul {
                    p.path.clone()
                } else {
                    name.clone()
                });
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
        if f.summary {
            for (p, o, n) in &items {
                if p.unmerged || !differs(p, o, n) {
                    continue;
                }
                let name = self.display_name(p);
                let mut line = |what: &str, mode: u32| {
                    write!(out, " {what} mode {mode:06o} ").unwrap();
                    out.extend_from_slice(&name);
                    out.push(b'\n');
                };
                match (&p.old, &p.new) {
                    (None, Some(s)) => line("create", s.mode),
                    (Some(s), None) => line("delete", s.mode),
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
                let mut emit =
                    |old: Option<&PatchFile<'_>>, new: Option<&PatchFile<'_>>| match &self.colors {
                        None => {
                            write_file_patch_with(&mut out, old, new, &self.patch, &abbrev);
                        }
                        Some(c) => {
                            let mut plain = Vec::new();
                            write_file_patch_with(&mut plain, old, new, &self.patch, &abbrev);
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

/// Write the output to stdout and compute the exit code.
pub fn finish(out: &[u8], changed: bool, opts: &DiffOpts) -> i32 {
    let mut o = std::io::stdout().lock();
    let _ = o.write_all(out);
    let _ = o.flush();
    i32::from(opts.wants_exit_code() && changed)
}
