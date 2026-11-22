// SPDX-License-Identifier: MIT
//! Command line of `git format-patch` (Documentation/git-format-patch.adoc):
//! its own options, the diff options and the revision arguments.

use crate::Die;
use crate::diff::options::{DiffOpts, usage_exit};
use crate::messages::format_patch as msg;

/// `--thread` styles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Thread {
    Shallow,
    Deep,
}

/// A change to the recipient and extra header lists, applied in order
/// after the configured ones.
#[derive(Debug, Clone)]
pub enum HeaderOp {
    To(String),
    NoTo,
    Cc(String),
    NoCc,
    Add(String),
    /// `--no-add-header`: drops every list.
    NoAdd,
}

/// What the command line asked for; `None` leaves the configured value.
#[derive(Debug, Default)]
pub struct Options {
    pub numbered: Option<bool>,
    pub start_number: Option<usize>,
    pub numbered_files: bool,
    pub keep_subject: bool,
    pub signoff: Option<bool>,
    pub stdout: bool,
    pub output: Option<String>,
    pub output_dir: Option<String>,
    pub cover_letter: Option<bool>,
    pub suffix: Option<String>,
    pub reroll: Option<String>,
    pub filename_max: Option<usize>,
    /// `--rfc[=<rfc>]`; `Some("")` after `--no-rfc` or `--rfc=`.
    pub rfc: Option<String>,
    pub subject_prefix: Option<String>,
    pub cover_from_description: Option<String>,
    pub description_file: Option<String>,
    pub zero_commit: bool,
    /// `--encoding=<encoding>`: the charset of the patches.
    pub encoding: Option<String>,
    /// `-p` / `--no-stat`.
    pub no_stat: bool,
    /// `--no-binary`: no binary patch data.
    pub no_binary: bool,
    pub headers: Vec<HeaderOp>,
    /// `--from[=<ident>]`: `Some(None)` for the committer, `Some(Some(""))`
    /// after `--no-from`.
    pub from: Option<Option<String>>,
    pub force_in_body_from: Option<bool>,
    pub in_reply_to: Option<String>,
    /// `--thread[=<style>]` / `--no-thread` (`Some(None)`).
    pub thread: Option<Option<Thread>>,
    /// `--signature=<s>` / `--no-signature` (`Some(None)`).
    pub signature: Option<Option<String>>,
    pub signature_file: Option<String>,
    pub quiet: bool,
    pub encode_headers: Option<bool>,
    pub root: bool,
    pub ignore_if_in_upstream: bool,
    /// `--pretty=mboxrd`.
    pub mboxrd: bool,
    pub default_prefix: bool,
    /// `-<n>`, `--max-count=<n>`.
    pub max_count: Option<String>,
    /// Other revision options, passed to the walk.
    pub rev_options: Vec<String>,
    /// Revision arguments.
    pub revs: Vec<String>,
    /// Paths after `--`.
    pub paths: Vec<String>,
    pub diff: DiffOpts,
}

/// Options that take a value, as their long names.
const WITH_VALUE: &[&str] = &[
    "output-directory",
    "output",
    "reroll-count",
    "start-number",
    "subject-prefix",
    "suffix",
    "to",
    "cc",
    "add-header",
    "in-reply-to",
    "signature",
    "signature-file",
    "cover-from-description",
    "description-file",
    "filename-max-length",
    "base",
    "interdiff",
    "range-diff",
    "creation-factor",
    "commit-list-format",
];

fn number(option: &str, value: &str) -> Result<usize, Die> {
    let (num, factor) = match value.as_bytes().last() {
        Some(b'k' | b'K') => (&value[..value.len() - 1], 1024),
        Some(b'm' | b'M') => (&value[..value.len() - 1], 1024 * 1024),
        Some(b'g' | b'G') => (&value[..value.len() - 1], 1024 * 1024 * 1024),
        _ => (value, 1),
    };
    num.parse::<usize>()
        .ok()
        .and_then(|n| n.checked_mul(factor))
        .ok_or_else(|| usage_exit(msg::expects_integer(option)))
}

impl Options {
    /// Parse the arguments after the command name.
    pub fn parse(argv: &[String]) -> Result<Options, Die> {
        let mut o = Options::default();
        let mut it = argv.iter();
        while let Some(arg) = it.next() {
            let a = arg.as_str();
            if a == "--" {
                o.paths.extend(it.by_ref().cloned());
                break;
            }
            if !a.starts_with('-') || a == "-" {
                o.revs.push(arg.clone());
                continue;
            }
            // Long options with a separate value.
            if let Some(name) = a.strip_prefix("--") {
                if WITH_VALUE.contains(&name) {
                    let Some(v) = it.next() else {
                        return Err(usage_exit(msg::requires_value(name)));
                    };
                    o.long(name, Some(v))?;
                    continue;
                }
                if let Some((name, value)) = name.split_once('=') {
                    if o.long(name, Some(value))? {
                        continue;
                    }
                } else if o.long(name, None)? {
                    continue;
                }
            }
            // Short options with a value, attached or separate.
            if let Some(rest) = a.strip_prefix("-o").or_else(|| a.strip_prefix("-v")) {
                let short = a.as_bytes()[1] as char;
                let value = if rest.is_empty() {
                    match it.next() {
                        Some(v) => v.as_str(),
                        None => return Err(usage_exit(msg::requires_value_short(short))),
                    }
                } else {
                    rest
                };
                let name = if short == 'o' {
                    "output-directory"
                } else {
                    "reroll-count"
                };
                o.long(name, Some(value))?;
                continue;
            }
            match a {
                "-n" => o.numbered = Some(true),
                "-N" => o.numbered = Some(false),
                "-k" => o.keep_subject = true,
                "-s" => o.signoff = Some(true),
                "-p" => o.no_stat = true,
                "-q" => o.quiet = true,
                "--name-only" | "--name-status" | "--check" => {
                    return Err(Die::Fatal(msg::does_not_make_sense(a)));
                }
                _ if a.len() > 1 && a[1..].bytes().all(|b| b.is_ascii_digit()) => {
                    o.max_count = Some(a[1..].to_string());
                }
                _ => {
                    if matches!(a, "-S" | "-G" | "-l") {
                        let Some(v) = it.next() else {
                            return Err(usage_exit(crate::messages::diff::requires_value(&a[1..])));
                        };
                        o.diff.parse_arg(&format!("{a}{v}"))?;
                    } else if matches!(
                        a,
                        "--find-object" | "--diff-filter" | "--diff-algorithm" | "--anchored"
                    ) {
                        let Some(v) = it.next() else {
                            return Err(usage_exit(crate::messages::diff::requires_value(&a[2..])));
                        };
                        o.diff.parse_arg(&format!("{a}={v}"))?;
                    } else if !o.diff.parse_arg(a)? {
                        o.rev_options.push(arg.clone());
                    }
                }
            }
        }
        Ok(o)
    }

    /// A long option (without `--`); `false` when it is not one of ours.
    fn long(&mut self, name: &str, value: Option<&str>) -> Result<bool, Die> {
        let v = || value.unwrap_or_default().to_string();
        match (name, value) {
            ("numbered", None) => self.numbered = Some(true),
            ("no-numbered", None) => self.numbered = Some(false),
            ("start-number", Some(n)) => self.start_number = Some(number(name, n)?),
            ("no-start-number", None) => self.start_number = None,
            ("numbered-files", None) => self.numbered_files = true,
            ("no-numbered-files", None) => self.numbered_files = false,
            ("keep-subject", None) => self.keep_subject = true,
            ("signoff", None) => self.signoff = Some(true),
            ("no-signoff", None) => self.signoff = Some(false),
            ("stdout", None) => self.stdout = true,
            ("no-stdout", None) => self.stdout = false,
            ("output", Some(_)) => self.output = Some(v()),
            ("output-directory", Some(_)) => self.output_dir = Some(v()),
            ("no-output-directory", None) => self.output_dir = None,
            ("cover-letter", None) => self.cover_letter = Some(true),
            ("no-cover-letter", None) => self.cover_letter = Some(false),
            ("suffix", Some(_)) => self.suffix = Some(v()),
            ("no-suffix", None) => self.suffix = None,
            ("reroll-count", Some(_)) => self.reroll = Some(v()),
            ("no-reroll-count", None) => self.reroll = None,
            ("filename-max-length", Some(n)) => self.filename_max = Some(number(name, n)?),
            ("no-filename-max-length", None) => self.filename_max = None,
            ("rfc", None) => self.rfc = Some("RFC".to_string()),
            ("rfc", Some(_)) => self.rfc = Some(v()),
            ("no-rfc", None) => self.rfc = Some(String::new()),
            ("subject-prefix", Some(_)) => self.subject_prefix = Some(v()),
            ("cover-from-description", Some(_)) => self.cover_from_description = Some(v()),
            ("no-cover-from-description", None) => self.cover_from_description = None,
            ("description-file", Some(_)) => self.description_file = Some(v()),
            ("no-description-file", None) => self.description_file = None,
            ("encoding", Some(_)) => self.encoding = Some(v()),
            ("zero-commit", None) => self.zero_commit = true,
            ("no-zero-commit", None) => self.zero_commit = false,
            ("no-stat", None) => self.no_stat = true,
            ("binary", None) => self.no_binary = false,
            ("no-binary", None) => self.no_binary = true,
            ("to", Some(_)) => self.headers.push(HeaderOp::To(v())),
            ("no-to", None) => self.headers.push(HeaderOp::NoTo),
            ("cc", Some(_)) => self.headers.push(HeaderOp::Cc(v())),
            ("no-cc", None) => self.headers.push(HeaderOp::NoCc),
            ("add-header", Some(_)) => self.headers.push(HeaderOp::Add(v())),
            ("no-add-header", None) => self.headers.push(HeaderOp::NoAdd),
            ("from", None) => self.from = Some(None),
            ("from", Some(_)) => self.from = Some(Some(v())),
            ("no-from", None) => self.from = Some(Some(String::new())),
            ("force-in-body-from", None) => self.force_in_body_from = Some(true),
            ("no-force-in-body-from", None) => self.force_in_body_from = Some(false),
            ("in-reply-to", Some(_)) => self.in_reply_to = Some(v()),
            ("no-in-reply-to", None) => self.in_reply_to = None,
            ("thread", None) => self.thread = Some(Some(Thread::Shallow)),
            ("thread", Some(style)) => {
                self.thread = Some(Some(match style {
                    "shallow" | "" => Thread::Shallow,
                    "deep" => Thread::Deep,
                    // git exits without a message.
                    _ => return Err(Die::Exit(rgit_options::EXIT_USAGE)),
                }));
            }
            ("no-thread", None) => self.thread = Some(None),
            ("signature", Some(_)) => self.signature = Some(Some(v())),
            ("no-signature", None) => self.signature = Some(None),
            ("signature-file", Some(_)) => self.signature_file = Some(v()),
            ("no-signature-file", None) => self.signature_file = None,
            ("quiet", None) => self.quiet = true,
            ("no-quiet", None) => self.quiet = false,
            ("encode-email-headers", None) => self.encode_headers = Some(true),
            ("no-encode-email-headers", None) => self.encode_headers = Some(false),
            ("root", None) => self.root = true,
            ("pretty" | "format", Some("mboxrd")) => self.mboxrd = true,
            ("pretty" | "format", Some("email")) => self.mboxrd = false,
            ("ignore-if-in-upstream", None) => self.ignore_if_in_upstream = true,
            ("no-ignore-if-in-upstream", None) => self.ignore_if_in_upstream = false,
            ("default-prefix", None) => {
                self.default_prefix = true;
                return Ok(false);
            }
            ("max-count", Some(n)) => self.max_count = Some(n.to_string()),
            // Commits without changes are always included.
            ("always" | "no-always", None) => {}
            ("no-attach" | "no-base" | "no-notes" | "no-progress" | "progress", None) => {}
            (
                "attach" | "inline" | "base" | "notes" | "interdiff" | "range-diff"
                | "creation-factor" | "commit-list-format",
                _,
            ) => return Err(Die::Fatal(msg::not_supported(&format!("--{name}")))),
            _ => return Ok(false),
        }
        Ok(true)
    }
}
