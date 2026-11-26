// SPDX-License-Identifier: MIT
//! `git unpack-file <blob>` (Documentation/git-unpack-file.adoc): writes the
//! blob to a new `.merge_file_XXXXXX` file at the top of the work tree (in
//! the current directory of a bare repository) and prints its name. The
//! messages were taken from the behaviour of the `git` binary.

use std::io::Write as _;
use std::path::Path;

use rgit_object::ObjectKind;

use crate::messages::merge_plumbing as msg;
use crate::prelude::*;
use crate::repo::Mode;
use crate::repository::Repo;

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let args = &argv[1..];
    if args.len() == 1 && matches!(args[0].as_str(), "-h" | "--help-all") {
        println!("{}", msg::UNPACK_FILE_USAGE);
        return Ok(0);
    }
    if args.len() != 1 || args[0] == "--" {
        eprintln!("{}", msg::UNPACK_FILE_USAGE);
        return Ok(129);
    }
    let mut repo = Repo::open(ctx, Mode::Required)?;
    let name = &args[0];
    let id = repo
        .resolve(name)
        .ok_or_else(|| Die::Fatal(msg::not_a_valid_object(name)))?;
    let data = match repo.odb().read(&id) {
        Ok(Some(o)) if o.kind == ObjectKind::Blob => o.data,
        _ => return Err(Die::Fatal(msg::unable_to_read_blob(&id.to_hex()))),
    };
    let dir = match &repo.setup.work_tree {
        Some(w) => w.clone(),
        None => std::env::current_dir().map_err(|e| Die::Fatal(e.to_string()))?,
    };
    let file = temp_file(&dir, &data).map_err(|e| Die::Fatal(e.to_string()))?;
    println!("{file}");
    Ok(0)
}

/// Writes `data` to a new file `.merge_file_XXXXXX` (mode 0600) in `dir`
/// and returns its name.
pub(crate) fn temp_file(dir: &Path, data: &[u8]) -> std::io::Result<String> {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(1)
        ^ (u64::from(std::process::id()) << 32)
        | 1;
    loop {
        let mut name = String::from(".merge_file_");
        for _ in 0..6 {
            // xorshift
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            name.push(CHARS[(seed % CHARS.len() as u64) as usize] as char);
        }
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        match opts.open(dir.join(&name)) {
            Ok(mut f) => {
                f.write_all(data)?;
                return Ok(name);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
}
