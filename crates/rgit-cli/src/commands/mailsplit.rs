// SPDX-License-Identifier: MIT
//! `git mailsplit` (Documentation/git-mailsplit.adoc): split mboxes and
//! Maildirs into numbered files. The splitting is in
//! [`rgit_mailinfo::split`].
//!
//! Behaviour determined with the `git` binary (the options are parsed by
//! hand, as git does):
//!
//! - Options must be stuck to their values (`-o<dir>`, `-d<prec>`,
//!   `-f<nn>`); the precision must be 3 to 9. Anything else starting with
//!   `-` is fatal. Without `-o` the last argument is the directory.
//! - Without mailboxes standard input is read; empty input is no error.
//! - Maildirs: the files of `cur` and then `new`, each sorted by name.
//! - The number of messages written is printed. The first mailbox that
//!   fails stops the command (exit 1).

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use rgit_mailinfo::split::{self, Error as SplitError, Options};

use crate::messages::mailinfo as msg;
use crate::prelude::*;

fn usage() -> Die {
    Die::Output(rgit_options::Exit {
        code: rgit_options::EXIT_USAGE,
        stdout: String::new(),
        stderr: msg::MAILSPLIT_USAGE.to_owned(),
    })
}

/// `strtol` on the digits of an option value (0 when there are none).
fn number(s: &str) -> i64 {
    let s = s.trim_start();
    let (neg, digits) = match s.strip_prefix('-') {
        Some(d) => (true, d),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let end = digits
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(digits.len());
    let n: i64 = digits[..end].parse().unwrap_or(0);
    if neg { -n } else { n }
}

struct Writer {
    dir: PathBuf,
    prec: usize,
    next: i64,
    written: i64,
}

impl Writer {
    fn write(&mut self, mail: &[u8]) -> Result<(), Die> {
        self.next += 1;
        let name = if self.next < 0 {
            format!("-{:0width$}", -self.next, width = self.prec.saturating_sub(1))
        } else {
            format!("{:0width$}", self.next, width = self.prec)
        };
        let path = self.dir.join(&name);
        // Existing files are not overwritten.
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .and_then(|mut f| f.write_all(mail))
            .map_err(|e| {
            Die::Fatal(msg::unable_to_create(
                &path.to_string_lossy(),
                &crate::global::strerror(&e),
            ))
        })?;
        self.written += 1;
        Ok(())
    }
}

/// Splits one mbox; prints the error and returns false on failure.
fn split_mbox(
    data: &[u8],
    name: &str,
    opts: &Options,
    out: &mut Writer,
    stdin: bool,
) -> Result<bool, Die> {
    match split::split(data, opts) {
        Ok(mails) => {
            for m in &mails {
                out.write(m)?;
            }
            Ok(true)
        }
        Err(SplitError::Empty) if stdin => Ok(true),
        Err(SplitError::Empty) => {
            eprintln!("error: {}", msg::empty_mbox(name));
            eprintln!("error: {}", msg::cannot_split(name));
            Ok(false)
        }
        Err(SplitError::Corrupt) => {
            eprintln!("{}", msg::CORRUPT_MAILBOX);
            Ok(false)
        }
    }
}

/// The message files of a Maildir, in order.
pub(crate) fn maildir_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for sub in ["cur", "new"] {
        let Ok(rd) = std::fs::read_dir(dir.join(sub)) else {
            continue;
        };
        let mut names: Vec<_> = rd
            .filter_map(Result::ok)
            .map(|e| e.file_name())
            .filter(|n| !n.to_string_lossy().starts_with('.'))
            .collect();
        names.sort();
        out.extend(names.into_iter().map(|n| dir.join(sub).join(n)));
    }
    out
}

fn split_maildir(dir: &Path, name: &str, opts: &Options, out: &mut Writer) -> Result<bool, Die> {
    for file in maildir_files(dir) {
        let shown = file.to_string_lossy().into_owned();
        let data = match std::fs::read(&file) {
            Ok(d) => d,
            Err(e) => {
                eprintln!(
                    "error: {}",
                    msg::cannot_open_mail(&shown, &crate::global::strerror(&e))
                );
                eprintln!("error: {}", msg::cannot_split(name));
                return Ok(false);
            }
        };
        if data.is_empty() {
            eprintln!("error: {}", msg::cannot_read_mail(&shown, "Success"));
            eprintln!("error: {}", msg::cannot_split(name));
            return Ok(false);
        }
        let bare = Options {
            allow_bare: true,
            ..*opts
        };
        match split::split(&data, &bare) {
            Ok(mails) => {
                // A Maildir file is one message.
                let joined: Vec<u8> = mails.concat();
                out.write(&joined)?;
            }
            Err(_) => {
                out.write(&data)?;
            }
        }
    }
    Ok(true)
}

pub fn run(_ctx: &mut Context, argv: &[String]) -> CmdResult {
    let args = &argv[1..];
    if args.len() == 1 && (args[0] == "-h" || args[0] == "--help-all") {
        let _ = std::io::stdout().write_all(msg::MAILSPLIT_USAGE.as_bytes());
        return Ok(0);
    }
    let mut opts = Options::default();
    let mut dir: Option<String> = None;
    let mut prec: i64 = 4;
    let mut skip: i64 = 0;
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if !a.starts_with('-') {
            break;
        }
        i += 1;
        if a == "--" {
            break;
        }
        if let Some(v) = a.strip_prefix("-d") {
            prec = number(v);
            if !(3..=9).contains(&prec) {
                return Err(usage());
            }
        } else if let Some(v) = a.strip_prefix("-f") {
            skip = number(v);
        } else if a == "-b" {
            opts.allow_bare = true;
        } else if a == "--keep-cr" {
            opts.keep_cr = true;
        } else if a == "--mboxrd" {
            opts.mboxrd = true;
        } else if let Some(v) = a.strip_prefix("-o").filter(|v| !v.is_empty()) {
            dir = Some(v.to_owned());
        } else {
            return Err(Die::Fatal(msg::unknown_option(a)));
        }
    }
    let mut files: Vec<String> = args[i..].to_vec();
    let dir = match dir {
        Some(d) => d,
        None => {
            if files.is_empty() {
                return Err(usage());
            }
            files.pop().expect("non-empty")
        }
    };
    let mut out = Writer {
        dir: PathBuf::from(&dir),
        prec: usize::try_from(prec).unwrap_or(4),
        next: skip,
        written: 0,
    };
    let mut ok = true;
    if files.is_empty() {
        let mut data = Vec::new();
        std::io::stdin()
            .lock()
            .read_to_end(&mut data)
            .map_err(|e| Die::Fatal(crate::global::strerror(&e)))?;
        ok = split_mbox(&data, "-", &opts, &mut out, true)?;
    } else {
        for f in &files {
            let path = Path::new(f);
            let meta = match std::fs::metadata(path) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!(
                        "error: {}",
                        msg::cannot_stat(f, &crate::global::strerror(&e))
                    );
                    ok = false;
                    break;
                }
            };
            let fine = if meta.is_dir() {
                split_maildir(path, f, &opts, &mut out)?
            } else {
                let data = std::fs::read(path).map_err(|e| {
                    Die::Fatal(msg::cannot_read(f, &crate::global::strerror(&e)))
                })?;
                split_mbox(&data, f, &opts, &mut out, false)?
            };
            if !fine {
                ok = false;
                break;
            }
        }
    }
    if !ok {
        return Ok(1);
    }
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{}", out.written);
    let _ = stdout.flush();
    Ok(0)
}
