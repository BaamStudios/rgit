// SPDX-License-Identifier: MIT

//! `sha1`, `sha256` and `sha1-is-sha1dc` (t0013, lib-loose.sh, lib-pack.sh).
//!
//! Observed behaviour of the reference binary:
//! - `sha1` / `sha256` hash all of stdin and print the lowercase hex digest
//!   and a newline. A single argument `-b` prints the raw digest bytes
//!   instead; any other arguments are ignored.
//! - SHA-1 input crafted for a collision attack (SHAttered) is refused with
//!   `fatal: SHA-1 appears to be part of a collision attack: <hex>`, exit 128.
//! - `sha1-is-sha1dc` exits 0 (arguments ignored): rgit's SHA-1 always detects
//!   collisions.

use std::io::{self, Read, Write};

use rgit_hash::{HashKind, Hasher};

use crate::messages;

pub fn sha1(args: &[String]) -> i32 {
    hash_stdin(HashKind::Sha1, args)
}

pub fn sha256(args: &[String]) -> i32 {
    hash_stdin(HashKind::Sha256, args)
}

pub fn sha1_is_sha1dc(_args: &[String]) -> i32 {
    0
}

fn hash_stdin(kind: HashKind, args: &[String]) -> i32 {
    let binary = args.len() == 1 && args[0] == "-b";
    let mut hasher = Hasher::new(kind);
    let mut stdin = io::stdin().lock();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match stdin.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buf[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => {
                eprintln!("{}", messages::fatal(e));
                return 128;
            }
        }
    }
    let id = match hasher.finalize() {
        Ok(id) => id,
        Err(collision) => {
            eprintln!("{}", messages::fatal(collision));
            return 128;
        }
    };
    let mut out = io::stdout().lock();
    let written = if binary {
        out.write_all(id.as_bytes())
    } else {
        writeln!(out, "{id}")
    };
    if written.and_then(|()| out.flush()).is_err() {
        return 128;
    }
    0
}
