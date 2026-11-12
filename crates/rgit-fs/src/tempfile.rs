// SPDX-License-Identifier: MIT
//! Temporary files that are removed unless they are persisted.
//!
//! Create the temp file in the directory of its final destination, so that
//! the final rename stays on one filesystem and is atomic.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::registry::{self, Registration};

/// Prefix of temporary loose objects in `objects/` (`tmp_obj_XXXXXX`).
pub const OBJECT_PREFIX: &str = "tmp_obj_";
/// Prefix of temporary pack files in `objects/pack/` (`tmp_pack_XXXXXX`).
pub const PACK_PREFIX: &str = "tmp_pack_";

const SUFFIX_LEN: usize = 6;
const ATTEMPTS: usize = 100;
const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

/// A file named `<prefix>XXXXXX` (six random characters), deleted when
/// dropped, on `exit` and on fatal signals, unless it was persisted.
#[derive(Debug)]
pub struct TempFile {
    path: PathBuf,
    file: Option<File>,
    reg: Option<Registration>,
}

impl TempFile {
    /// Creates `dir/<prefix>XXXXXX` with mode 0600 (like mkstemp(3)).
    pub fn new_in(dir: impl AsRef<Path>, prefix: &str) -> io::Result<Self> {
        Self::with_mode(dir, prefix, 0o600)
    }

    /// Like [`new_in`](Self::new_in) with an explicit creation mode (reduced
    /// by the umask; ignored on non-Unix systems).
    pub fn with_mode(dir: impl AsRef<Path>, prefix: &str, mode: u32) -> io::Result<Self> {
        let dir = dir.as_ref();
        let mut seed = seed();
        let mut last_err = None;
        for _ in 0..ATTEMPTS {
            let mut name = String::with_capacity(prefix.len() + SUFFIX_LEN);
            name.push_str(prefix);
            for _ in 0..SUFFIX_LEN {
                seed = seed
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                name.push(ALPHABET[((seed >> 33) % ALPHABET.len() as u64) as usize] as char);
            }
            let path = dir.join(name);
            let mut opts = OpenOptions::new();
            opts.read(true).write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(mode);
            }
            #[cfg(not(unix))]
            let _ = mode;
            match opts.open(&path) {
                Ok(file) => {
                    let reg = registry::register(&path);
                    return Ok(Self {
                        path,
                        file: Some(file),
                        reg: Some(reg),
                    });
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => last_err = Some(e),
                Err(e) => return Err(e),
            }
        }
        Err(last_err.unwrap_or_else(|| io::Error::from(io::ErrorKind::AlreadyExists)))
    }

    /// Current location of the temp file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The open file.
    pub fn file_mut(&mut self) -> &mut File {
        self.file
            .as_mut()
            .expect("temp file is open until persisted")
    }

    /// Closes the file and renames it to `dest`, replacing `dest` if it
    /// exists. On failure the temp file is deleted.
    pub fn persist(mut self, dest: impl AsRef<Path>) -> io::Result<()> {
        self.file = None;
        let result = std::fs::rename(&self.path, dest.as_ref());
        if result.is_ok() {
            if let Some(reg) = self.reg.take() {
                registry::unregister(reg);
            }
        }
        // On error Drop deletes the temp file.
        result
    }

    /// Stops tracking the file: it is neither deleted on drop nor on exit.
    /// Returns the open file and its path.
    pub fn keep(mut self) -> (File, PathBuf) {
        if let Some(reg) = self.reg.take() {
            registry::unregister(reg);
        }
        let file = self.file.take().expect("temp file is open until persisted");
        (file, std::mem::take(&mut self.path))
    }
}

impl Write for TempFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.file_mut().write(buf)
    }
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.file_mut().write_all(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file_mut().flush()
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        self.file = None;
        if let Some(reg) = self.reg.take() {
            let _ = std::fs::remove_file(&self.path);
            registry::unregister(reg);
        }
    }
}

fn seed() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u32(std::process::id());
    if let Ok(t) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        h.write_u128(t.as_nanos());
    }
    h.finish()
}
