// SPDX-License-Identifier: MIT
//! Lock files, temp files and atomic file replacement for rgit.
//!
//! * [`LockFile`]: git's `<path>.lock` protocol. Create the lock
//!   exclusively, write the new content, then [`commit`](LockFile::commit)
//!   (rename over the original) or [`rollback`](LockFile::rollback). Error
//!   messages match git's ([`LockError`]).
//! * [`TempFile`]: `<prefix>XXXXXX` files such as `tmp_obj_*` and
//!   `tmp_pack_*`, persisted by rename.
//! * Knobs for `core.sharedRepository` ([`SharedRepository`]), `core.fsync`
//!   and `core.fsyncMethod` ([`FsyncComponents`], [`FsyncMethod`]) and the
//!   ref/config lock timeouts ([`Timeout`]). Reading these from the
//!   configuration is the caller's job.
//!
//! Every lock, PID and temp file still open is removed when the value is
//! dropped, when the process calls `exit` (also via
//! `std::process::exit`) and, on Unix, when it receives SIGINT, SIGTERM,
//! SIGHUP, SIGQUIT or SIGPIPE. After cleaning up, the signal is re-raised
//! with its previous disposition so the exit status reports it. Signals the
//! process ignores stay ignored.
//!
//! ```no_run
//! use std::io::Write;
//! use rgit_fs::{LockFile, LockOptions, Timeout};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let opts = LockOptions::new().timeout(Timeout::CONFIG_DEFAULT);
//! let mut lock = LockFile::acquire(".git/config", &opts)?;
//! lock.write_all(b"[core]\n\tbare = false\n")?;
//! lock.commit()?;
//! # Ok(())
//! # }
//! ```

mod fsync;
mod lock;
pub mod messages;
mod perm;
mod registry;
mod tempfile;

pub use fsync::{FsyncComponents, FsyncMethod, harden};
pub use lock::{
    LOCK_SUFFIX, LockError, LockFile, LockHolder, LockOptions, PID_LOCK_SUFFIX, Timeout,
};
pub use perm::{SharedRepository, SharedRepositoryError};
pub use registry::remove_registered_files;
pub use tempfile::{OBJECT_PREFIX, PACK_PREFIX, TempFile};
