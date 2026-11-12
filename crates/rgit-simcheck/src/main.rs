// SPDX-License-Identifier: MIT
//! rgit-simcheck: license guard for rgit (issue #85).
//!
//! Compares rgit's Rust sources with upstream git's implementation sources
//! (`*.c`, `*.h`, `*.rs` outside `t/`) and reports regions that look copied or
//! transliterated. Three independent checks:
//!
//! * code: token-normalised k-gram fingerprints with winnowing,
//! * comments: the same on comment words,
//! * strings: long string literals that occur verbatim upstream.
//!
//! The report names rgit file:lines, upstream file:lines and a score only.
//! It never prints upstream text, so reading it does not break the
//! clean-room rules in CLEANROOM.md.

mod allow;
mod engine;
mod lex;
mod norm;
mod selftest;
mod strings;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use allow::{Allowlist, Check, Reason};
use engine::{Corpus, Doc, Params};
use lex::{Lang, Tok, lex};
use norm::{Interner, Seq, distinct_trigrams, normalise};
use strings::{Haystack, literals, visible_len};

const USAGE: &str = "\
usage: rgit-simcheck [options]

Compares rgit's sources with upstream git's implementation sources and
reports suspicious similarity. Prints locations and scores only, never
upstream source text.

options:
  --rgit <dir>              rgit sources to check (default: crates)
  --upstream <dir>          upstream git checkout (default: upstream/git)
  --allowlist <file>        accepted findings (default: crates/rgit-simcheck/allowlist.txt)
  --report <file>           also write the report to this file
  --threshold <n>           code: matched normalised tokens for a finding (default: 30)
  --rust-threshold <n>      code: threshold against upstream Rust files, which share
                            many idioms with any Rust code (default: 60)
  --comment-threshold <n>   comments: matched words for a finding (default: 16)
  --string-min-len <n>      strings: minimum literal length to compare (default: 32)
  --k <n>                   code: k-gram length (default: 10)
  --window <n>              code: winnowing window (default: 6)
  --self-test [<n>]         plant <n> (default 50) transliterated upstream fragments
                            and check that every one is reported
  --plant-tokens <n>        self-test: raw C tokens per fragment (default: 40)
  --min-detection <pct>     self-test: required share of reported fragments (default: 95)
  --seed <n>                self-test: random seed (default: 1)
  -v, --verbose             also list allowed findings
  -h, --help                show this help

exit status: 0 no findings, 1 findings (or self-test misses), 2 usage/IO error
";

struct Opts {
    rgit: PathBuf,
    upstream: PathBuf,
    allowlist: Option<PathBuf>,
    report: Option<PathBuf>,
    code: Params,
    comment: Params,
    rust_threshold: usize,
    string_min: usize,
    self_test: Option<usize>,
    plant_tokens: usize,
    min_detection: usize,
    seed: u64,
    verbose: bool,
}

/// Defaults for the code check, calibrated against an independent Rust code
/// base of about 2 million normalised tokens (gitoxide): at these values it
/// yields a handful of findings there, while planted fragments are reported.
const CODE_PARAMS: Params = Params {
    k: 10,
    w: 6,
    gap: 24,
    max_occ: 20,
    threshold: 30,
    min_distinct: 15,
    max_df: 10,
};

const COMMENT_PARAMS: Params = Params {
    k: 6,
    w: 4,
    gap: 10,
    max_occ: 20,
    threshold: 16,
    min_distinct: 10,
    max_df: 10,
};

fn parse_args() -> Result<Opts, String> {
    let mut o = Opts {
        rgit: "crates".into(),
        upstream: "upstream/git".into(),
        allowlist: None,
        report: None,
        code: CODE_PARAMS,
        comment: COMMENT_PARAMS,
        rust_threshold: 60,
        string_min: 32,
        self_test: None,
        plant_tokens: 40,
        min_detection: 95,
        seed: 1,
        verbose: false,
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    let num = |i: usize, name: &str| -> Result<usize, String> {
        args.get(i)
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| format!("{name} needs a number"))
    };
    while i < args.len() {
        let a = args[i].as_str();
        let val = |i: usize| -> Result<String, String> {
            args.get(i)
                .cloned()
                .ok_or_else(|| format!("{a} needs a value"))
        };
        match a {
            "--rgit" => o.rgit = val(i + 1)?.into(),
            "--upstream" => o.upstream = val(i + 1)?.into(),
            "--allowlist" => o.allowlist = Some(val(i + 1)?.into()),
            "--report" => o.report = Some(val(i + 1)?.into()),
            "--threshold" => o.code.threshold = num(i + 1, a)?,
            "--rust-threshold" => o.rust_threshold = num(i + 1, a)?,
            "--comment-threshold" => o.comment.threshold = num(i + 1, a)?,
            "--string-min-len" => o.string_min = num(i + 1, a)?,
            "--k" => o.code.k = num(i + 1, a)?.max(2),
            "--window" => o.code.w = num(i + 1, a)?.max(1),
            "--max-df" => o.code.max_df = num(i + 1, a)?,
            "--gap" => o.code.gap = num(i + 1, a)?,
            "--min-distinct" => o.code.min_distinct = num(i + 1, a)?,
            "--plant-tokens" => o.plant_tokens = num(i + 1, a)?,
            "--min-detection" => o.min_detection = num(i + 1, a)?,
            "--seed" => o.seed = num(i + 1, a)? as u64,
            "--self-test" => match args.get(i + 1).and_then(|v| v.parse().ok()) {
                Some(n) => o.self_test = Some(n),
                None => {
                    o.self_test = Some(50);
                    i += 1;
                    continue;
                }
            },
            "-v" | "--verbose" => {
                o.verbose = true;
                i += 1;
                continue;
            }
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            _ => return Err(format!("unknown option {a}")),
        }
        i += 2;
    }
    if o.code.gap < o.code.k {
        o.code.gap = o.code.k * 2;
    }
    Ok(o)
}

/// Recursively list files with one of `exts`, sorted, skipping symlinks,
/// `.git`, `target` and the given top-level directories.
fn walk(root: &Path, exts: &[&str], skip_top: &[&str]) -> std::io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        for e in std::fs::read_dir(&dir)? {
            let e = e?;
            let ft = e.file_type()?;
            let name = e.file_name().to_string_lossy().into_owned();
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                if name == ".git"
                    || name == "target"
                    || (depth == 0 && skip_top.contains(&name.as_str()))
                {
                    continue;
                }
                stack.push((e.path(), depth + 1));
            } else if ft.is_file() {
                let p = e.path();
                if p.extension().is_some_and(|x| exts.iter().any(|e| x == *e)) {
                    out.push(p);
                }
            }
        }
    }
    out.sort();
    Ok(out)
}

fn read_lossy(p: &Path) -> std::io::Result<String> {
    Ok(String::from_utf8_lossy(&std::fs::read(p)?).into_owned())
}

fn lang_of(p: &Path) -> Lang {
    if p.extension().is_some_and(|x| x == "rs") {
        Lang::Rust
    } else {
        Lang::C
    }
}

fn display(p: &Path, root: &Path, prefix: &str) -> String {
    let rel = p.strip_prefix(root).unwrap_or(p);
    let rel = rel.to_string_lossy().replace('\\', "/");
    if prefix.is_empty() {
        rel
    } else {
        format!("{prefix}/{rel}")
    }
}

fn word_seq(words: &[(String, u32)], it: &mut Interner) -> Seq {
    Seq {
        ids: words.iter().map(|w| it.id(&w.0)).collect(),
        lines: words.iter().map(|w| w.1).collect(),
    }
}

struct Upstream {
    code: Corpus,
    comments: Corpus,
    literals: Haystack,
    long_literals: Vec<(String, usize, u32)>,
    paths: Vec<String>,
    /// Raw tokens of `.c` files (self-test only), by document index.
    c_toks: Vec<(usize, Vec<Tok>)>,
    tokens: usize,
}

fn load_upstream(
    o: &Opts,
    code_it: &mut Interner,
    word_it: &mut Interner,
) -> Result<Upstream, String> {
    if !o.upstream.join("Makefile").exists() {
        return Err(format!(
            "{} does not look like a git checkout; run scripts/fetch-upstream.sh",
            o.upstream.display()
        ));
    }
    let files = walk(&o.upstream, &["c", "h", "rs"], &["t"]).map_err(|e| e.to_string())?;
    let mut code_docs = Vec::new();
    let mut comment_docs = Vec::new();
    let mut hay = Haystack::new();
    let mut long_literals = Vec::new();
    let mut paths = Vec::new();
    let mut c_toks = Vec::new();
    let mut tokens = 0;
    for (d, f) in files.iter().enumerate() {
        let src = read_lossy(f).map_err(|e| format!("{}: {e}", f.display()))?;
        let lang = lang_of(f);
        let lexed = lex(&src, lang);
        let path = display(f, &o.upstream, "");
        let seq = normalise(&lexed.toks, lang, code_it);
        tokens += seq.ids.len();
        code_docs.push(Doc {
            path: path.clone(),
            seq,
            threshold: (lang == Lang::Rust).then_some(o.rust_threshold),
        });
        comment_docs.push(Doc {
            path: path.clone(),
            seq: word_seq(&lexed.comment_words, word_it),
            threshold: None,
        });
        for (lit, line) in literals(&lexed.toks, lang) {
            hay.add(&lit, d, line);
            if strings::comparable(&lit, o.string_min) {
                long_literals.push((lit, d, line));
            }
        }
        if o.self_test.is_some() && lang == Lang::C && f.extension().is_some_and(|x| x == "c") {
            c_toks.push((d, lexed.toks));
        }
        paths.push(path);
    }
    Ok(Upstream {
        code: Corpus::new(code_docs, o.code),
        comments: Corpus::new(comment_docs, o.comment),
        literals: hay,
        long_literals,
        paths,
        c_toks,
        tokens,
    })
}

struct Finding {
    check: Check,
    path: String,
    lines: (u32, u32),
    upstream: String,
    score: String,
    allowed: Option<Reason>,
}

fn line_range(seq: &Seq, r: (usize, usize)) -> (u32, u32) {
    (seq.lines[r.0], seq.lines[r.1 - 1])
}

fn fmt_lines(l: (u32, u32)) -> String {
    if l.0 == l.1 {
        format!("{}", l.0)
    } else {
        format!("{}-{}", l.0, l.1)
    }
}

fn scan(
    o: &Opts,
    up: &Upstream,
    code_it: &mut Interner,
    word_it: &mut Interner,
    allowlist: &Allowlist,
) -> Result<(Vec<Finding>, usize, usize), String> {
    let files = walk(&o.rgit, &["rs"], &[]).map_err(|e| format!("{}: {e}", o.rgit.display()))?;
    let prefix = o.rgit.to_string_lossy().trim_end_matches('/').to_string();
    let mut findings = Vec::new();
    let mut rgit_hay = Haystack::new();
    let mut rgit_paths = Vec::new();
    let mut gitoxide = Vec::new();
    let mut tokens = 0;
    for (fi, f) in files.iter().enumerate() {
        let src = read_lossy(f).map_err(|e| format!("{}: {e}", f.display()))?;
        let path = display(f, &o.rgit, &prefix);
        let lexed = lex(&src, Lang::Rust);
        let gx = allow::has_gitoxide_header(&src);
        gitoxide.push(gx);
        let seq = normalise(&lexed.toks, Lang::Rust, code_it);
        tokens += seq.ids.len();
        for (check, corpus, seq) in [
            (Check::Code, &up.code, seq),
            (
                Check::Comment,
                &up.comments,
                word_seq(&lexed.comment_words, word_it),
            ),
        ] {
            for h in corpus.find(&seq) {
                let lines = line_range(&seq, h.r);
                let udoc = &corpus.docs[h.up_doc];
                let unit = if check == Check::Code {
                    "tokens"
                } else {
                    "words"
                };
                findings.push(Finding {
                    check,
                    path: path.clone(),
                    lines,
                    upstream: format!("{}:{}", udoc.path, fmt_lines(line_range(&udoc.seq, h.u))),
                    score: format!("{} {unit}, coverage {:.0}%", h.score, h.coverage() * 100.0),
                    allowed: None,
                });
            }
        }
        for (lit, line) in literals(&lexed.toks, Lang::Rust) {
            if strings::comparable(&lit, o.string_min) {
                let locs = up.literals.find_all(&lit, 4);
                if let Some(&(d, uline, _)) = locs.first() {
                    let more = if locs.len() > 1 { " (and more)" } else { "" };
                    findings.push(string_finding(
                        &path,
                        line,
                        &lit,
                        &up.paths[d],
                        uline,
                        more,
                        allowlist,
                    ));
                }
            }
            rgit_hay.add(&lit, fi, line);
        }
        rgit_paths.push(path);
    }
    // Long upstream literals contained in a (longer) rgit literal.
    for (lit, d, uline) in &up.long_literals {
        for (fi, line, _) in rgit_hay.find_all(lit, 16) {
            let path = &rgit_paths[fi];
            if findings
                .iter()
                .any(|f| f.check == Check::String && &f.path == path && f.lines.0 == line)
            {
                continue;
            }
            findings.push(string_finding(
                path,
                line,
                lit,
                &up.paths[*d],
                *uline,
                "",
                allowlist,
            ));
        }
    }
    for f in &mut findings {
        let gx = rgit_paths
            .iter()
            .position(|p| *p == f.path)
            .is_some_and(|i| gitoxide[i]);
        if f.allowed.is_none() {
            if gx {
                f.allowed = Some(Reason::Gitoxide);
            } else if allowlist.allows(f.check, &f.path, f.lines) {
                f.allowed = Some(Reason::Allowlist);
            }
        }
    }
    findings
        .sort_by(|a, b| (a.check as u8, &a.path, a.lines).cmp(&(b.check as u8, &b.path, b.lines)));
    Ok((findings, files.len(), tokens))
}

fn string_finding(
    path: &str,
    line: u32,
    lit: &str,
    up_path: &str,
    up_line: u32,
    more: &str,
    allowlist: &Allowlist,
) -> Finding {
    let allowed = if allow::is_message_file(path) || allow::is_message_literal(lit) {
        Some(Reason::Message)
    } else if allowlist.allows(Check::String, path, (line, line)) {
        Some(Reason::Allowlist)
    } else {
        None
    };
    Finding {
        check: Check::String,
        path: path.to_string(),
        lines: (line, line),
        upstream: format!("{up_path}:{up_line}{more}"),
        score: format!("{} chars verbatim", visible_len(lit)),
        allowed,
    }
}

fn self_test(o: &Opts, up: &Upstream, code_it: &mut Interner, n: usize) -> (String, bool) {
    let mut rep = String::new();
    let p = up.code.params();
    let mut rng = selftest::Rng::new(o.seed);
    let pool: Vec<&(usize, Vec<Tok>)> = up
        .c_toks
        .iter()
        .filter(|(_, t)| t.len() > o.plant_tokens * 4)
        .collect();
    let (mut planted, mut detected, mut generic, mut attempts) = (0, 0, 0, 0);
    let _ = writeln!(
        rep,
        "self-test: planting {n} fragments of {} raw C tokens, transliterated to Rust (seed {})",
        o.plant_tokens, o.seed
    );
    while planted < n && attempts < n * 200 {
        attempts += 1;
        let (d, toks) = pool[rng.below(pool.len())];
        let Some(start) = selftest::statement_start(toks, rng.below(toks.len() - o.plant_tokens))
        else {
            continue;
        };
        let Some(win) = toks.get(start..start + o.plant_tokens) else {
            continue;
        };
        if !selftest::suitable(win) {
            continue;
        }
        let src = selftest::transliterate(win, &mut rng);
        let seq = normalise(&lex(&src, Lang::Rust).toks, Lang::Rust, code_it);
        // Fragments made of idioms found all over upstream cannot reach the
        // threshold by design (they would match independent code just as
        // well, see CLEANROOM.md). Skip them and count them separately.
        if distinct_trigrams(&seq.ids) < p.min_distinct
            || up.code.specific_coverage(&normalise(win, Lang::C, code_it)) < p.threshold
            || up.code.specific_coverage(&seq) < p.threshold
        {
            generic += 1;
            continue;
        }
        planted += 1;
        let ulines = (win[0].line, win[win.len() - 1].line);
        // Upstream often contains the same code in several places, so a
        // report against another location counts as well.
        let hits = up.code.find(&seq);
        let at_source = hits.iter().any(|h| {
            let l = line_range(&up.code.docs[h.up_doc].seq, h.u);
            h.up_doc == *d && l.0 <= ulines.1 && ulines.0 <= l.1
        });
        match hits.iter().map(|h| h.score).max() {
            Some(score) => {
                detected += 1;
                let _ = writeln!(
                    rep,
                    "  ok    {}:{} -> {} normalised tokens, score {score}{}",
                    up.paths[*d],
                    fmt_lines(ulines),
                    seq.ids.len(),
                    if at_source {
                        ""
                    } else {
                        " (reported against another upstream location)"
                    }
                );
            }
            None => {
                let best = up
                    .code
                    .find_with(&seq, Some(1))
                    .into_iter()
                    .map(|h| h.score)
                    .max()
                    .unwrap_or(0);
                let _ = writeln!(
                    rep,
                    "  MISS  {}:{} -> {} normalised tokens, best score {best}, reachable {}",
                    up.paths[*d],
                    fmt_lines(ulines),
                    seq.ids.len(),
                    up.code.specific_coverage(&seq)
                );
            }
        }
    }
    let _ = writeln!(
        rep,
        "self-test: {detected}/{planted} planted fragments reported ({generic} skipped as too generic: fewer than {} tokens covered by k-grams found in at most {} upstream files)",
        p.threshold, p.max_df
    );
    let ok = planted > 0 && detected * 100 >= planted * o.min_detection;
    let _ = writeln!(
        rep,
        "self-test: {} (required: {}% of planted fragments)",
        if ok { "passed" } else { "FAILED" },
        o.min_detection
    );
    (rep, ok)
}

fn run() -> Result<bool, String> {
    let o = parse_args()?;
    let allowlist_path = o.allowlist.clone().or_else(|| {
        let p = PathBuf::from("crates/rgit-simcheck/allowlist.txt");
        p.exists().then_some(p)
    });
    let allowlist = match &allowlist_path {
        Some(p) => Allowlist::parse(&read_lossy(p).map_err(|e| format!("{}: {e}", p.display()))?)
            .map_err(|e| format!("{}: {e}", p.display()))?,
        None => Allowlist::default(),
    };
    let mut code_it = Interner::default();
    let mut word_it = Interner::default();
    let up = load_upstream(&o, &mut code_it, &mut word_it)?;
    let mut rep = String::new();
    let _ = writeln!(
        rep,
        "rgit-simcheck: upstream {} ({} files, {} normalised tokens)",
        o.upstream.display(),
        up.paths.len(),
        up.tokens
    );

    if let Some(n) = o.self_test {
        let (text, ok) = self_test(&o, &up, &mut code_it, n);
        rep.push_str(&text);
        emit(&o, &rep)?;
        return Ok(ok);
    }

    let (findings, nfiles, ntokens) = scan(&o, &up, &mut code_it, &mut word_it, &allowlist)?;
    let _ = writeln!(
        rep,
        "rgit-simcheck: checked {} ({nfiles} files, {ntokens} normalised tokens)",
        o.rgit.display()
    );
    let _ = writeln!(
        rep,
        "thresholds: code {} tokens ({} against upstream Rust; k={}, w={}), comments {} words, strings {} chars",
        o.code.threshold, o.rust_threshold, o.code.k, o.code.w, o.comment.threshold, o.string_min
    );
    let mut failing = 0;
    for check in [Check::Code, Check::Comment, Check::String] {
        let list: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.check == check && f.allowed.is_none())
            .collect();
        let allowed = findings
            .iter()
            .filter(|f| f.check == check && f.allowed.is_some())
            .count();
        failing += list.len();
        let _ = writeln!(
            rep,
            "\n{check} findings: {} (allowed: {allowed})",
            list.len()
        );
        for f in list {
            let _ = writeln!(
                rep,
                "  {}:{}  <->  git:{}  [{}]",
                f.path,
                fmt_lines(f.lines),
                f.upstream,
                f.score
            );
        }
        if o.verbose {
            for f in findings
                .iter()
                .filter(|f| f.check == check && f.allowed.is_some())
            {
                let _ = writeln!(
                    rep,
                    "  allowed ({}): {}:{}  <->  git:{}  [{}]",
                    f.allowed.unwrap_or(Reason::Allowlist),
                    f.path,
                    fmt_lines(f.lines),
                    f.upstream,
                    f.score
                );
            }
        }
    }
    let _ = writeln!(
        rep,
        "\n{}",
        if failing == 0 {
            "result: no findings above the thresholds".to_string()
        } else {
            format!("result: {failing} finding(s) need review, see CLEANROOM.md \"License guard\"")
        }
    );
    emit(&o, &rep)?;
    Ok(failing == 0)
}

fn emit(o: &Opts, rep: &str) -> Result<(), String> {
    print!("{rep}");
    if let Some(p) = &o.report {
        std::fs::write(p, rep).map_err(|e| format!("{}: {e}", p.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("rgit-simcheck: {e}");
            ExitCode::from(2)
        }
    }
}
