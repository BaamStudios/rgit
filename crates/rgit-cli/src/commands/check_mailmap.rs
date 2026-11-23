// SPDX-License-Identifier: MIT
//! `git check-mailmap` (Documentation/git-check-mailmap.adoc): print the
//! canonical identity of each contact (`Name <email>`, `<email>` or a bare
//! email) from the arguments and, with `--stdin`, standard input.

use std::io::{BufRead, Write};

use crate::history::mailmap::{BlobLookup, Mailmap};
use crate::messages::check_mailmap as msg;
use crate::prelude::*;

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let stdin = Cell::new(false);
    let file: RefCell<Option<String>> = RefCell::new(None);
    let blob: RefCell<Option<String>> = RefCell::new(None);
    let mut parser = Parser::new(
        msg::USAGE,
        vec![
            Opt::flag(None, "stdin", &stdin, msg::HELP_STDIN),
            Opt::string(None, "mailmap-file", &file, "file", msg::HELP_FILE),
            Opt::string(None, "mailmap-blob", &blob, "blob", msg::HELP_BLOB),
        ],
    );
    let contacts = parser.parse(argv)?.args;
    drop(parser);
    let setup = crate::repo::setup(ctx, crate::repo::Mode::Required)?;
    if contacts.is_empty() && !stdin.get() {
        return Err(Die::Fatal(msg::NO_CONTACTS.to_owned()));
    }
    let mut mailmap = Mailmap::load_for(ctx)?;
    let _ = setup;
    if let Some(spec) = blob.borrow().as_deref() {
        match crate::revisions::read_blob(ctx, spec, "") {
            BlobLookup::Blob(data) => mailmap.add(&data),
            BlobLookup::NotBlob => {
                eprintln!(
                    "error: {}",
                    crate::messages::rev_list::not_a_blob(spec)
                );
            }
            BlobLookup::Unresolved => {}
        }
    }
    if let Some(path) = file.borrow().as_deref()
        && let Ok(text) = std::fs::read(path)
    {
        mailmap.add(&text);
    }
    let mut out = std::io::stdout().lock();
    for c in &contacts {
        let _ = out.write_all(&check(&mailmap, c.as_bytes()));
    }
    if stdin.get() {
        let _ = out.flush();
        for line in std::io::stdin().lock().split(b'\n') {
            let Ok(line) = line else { break };
            let _ = out.write_all(&check(&mailmap, &line));
            let _ = out.flush();
        }
    }
    let _ = out.flush();
    Ok(0)
}

/// The mapped identity of one contact, as a line.
fn check(mailmap: &Mailmap, contact: &[u8]) -> Vec<u8> {
    let (name, email) = match contact.iter().position(|&b| b == b'<') {
        Some(lt) => {
            let end = contact[lt..]
                .iter()
                .position(|&b| b == b'>')
                .map_or(contact.len(), |i| lt + i);
            (contact[..lt].trim_ascii(), &contact[lt + 1..end])
        }
        None => (&b""[..], contact),
    };
    let (name, email) = mailmap.map(name, email);
    let mut out = Vec::new();
    if !name.is_empty() {
        out.extend_from_slice(name);
        out.push(b' ');
    }
    out.push(b'<');
    out.extend_from_slice(email);
    out.extend_from_slice(b">\n");
    out
}
