// SPDX-License-Identifier: MIT

//! Stand-in for the `test-tool sha1` / `test-tool sha256` helpers used by the
//! conformance suite: hashes stdin and prints the hex digest.
//!
//! ```text
//! hash-tool sha1|sha256 [-b]       # -b: print the raw digest bytes
//! hash-tool sha1-is-sha1dc         # exits 0: SHA-1 detects collisions
//! hash-tool object sha1|sha256 <type>   # like `git hash-object --stdin -t <type>`
//! ```
//!
//! A detected SHA-1 collision prints `fatal: ...` and exits with 128.

use std::io::{self, Read, Write};
use std::process::exit;

use rgit_hash::{CollisionError, HashKind, Hasher, object_hasher};

fn usage() -> ! {
    eprintln!("usage: hash-tool (sha1|sha256) [-b] | sha1-is-sha1dc | object (sha1|sha256) <type>");
    exit(129);
}

fn kind(arg: Option<&str>) -> HashKind {
    arg.and_then(HashKind::from_name).unwrap_or_else(|| usage())
}

fn finish(h: Hasher) -> rgit_hash::ObjectId {
    h.finalize().unwrap_or_else(|e: CollisionError| {
        eprintln!("fatal: {e}");
        exit(128);
    })
}

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut input = Vec::new();
    match args.as_slice() {
        ["sha1-is-sha1dc"] => return Ok(()),
        ["object", k, ty] => {
            let kind = kind(Some(k));
            io::stdin().lock().read_to_end(&mut input)?;
            let mut h = object_hasher(kind, ty, input.len() as u64);
            h.update(&input);
            println!("{}", finish(h));
        }
        [k, rest @ ..] if rest.is_empty() || rest == ["-b"] => {
            let mut h = Hasher::new(kind(Some(k)));
            io::copy(&mut io::stdin().lock(), &mut h)?;
            let id = finish(h);
            if rest.is_empty() {
                println!("{id}");
            } else {
                io::stdout().write_all(id.as_bytes())?;
            }
        }
        _ => usage(),
    }
    Ok(())
}
