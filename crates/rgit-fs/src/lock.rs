// SPDX-License-Identifier: MIT
//! git's `<path>.lock` protocol.
//!
//! To change `path`, a writer creates `path.lock` exclusively, writes the
//! complete new content into it and renames it over `path` ("commit"). If
//! anything goes wrong, it deletes `path.lock` instead ("rollback"). While
//! the lock exists no other writer can start, and readers always see either
//! the old or the new content.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::fsync::{self, FsyncMethod};
use crate::messages;
use crate::perm::SharedRepository;
use crate::registry::{self, Registration};

/// Suffix git appends to the locked path.
pub const LOCK_SUFFIX: &str = ".lock";

/// Suffix of the PID file next to a lock (`index.lock` -> `index~pid.lock`).
pub const PID_LOCK_SUFFIX: &str = "~pid.lock";

/// Maximum number of symlinks followed when resolving the locked path.
const MAX_SYMLINK_DEPTH: usize = 5;

/// Longest single wait between two attempts to take a busy lock.
const MAX_BACKOFF: Duration = Duration::from_secs(1);

/// How long [`LockFile::acquire`] keeps retrying while the lock exists.
///
/// The config values `core.filesRefLockTimeout`, `core.packedRefsTimeout`
/// and `core.configLockTimeout` map to this with [`Timeout::from_millis`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Timeout {
    /// Fail at once (config value `0`).
    #[default]
    Immediate,
    /// Retry with randomized exponential backoff for at most this long.
    After(Duration),
    /// Retry until the lock is free (config value `-1`, or any negative).
    Forever,
}

impl Timeout {
    /// Default of `core.filesRefLockTimeout` (100 ms).
    pub const FILES_REF_DEFAULT: Self = Self::After(Duration::from_millis(100));
    /// Default of `core.packedRefsTimeout` (1 s).
    pub const PACKED_REFS_DEFAULT: Self = Self::After(Duration::from_millis(1000));
    /// Default of `core.configLockTimeout` (1 s).
    pub const CONFIG_DEFAULT: Self = Self::After(Duration::from_millis(1000));

    /// Interprets a timeout config value in milliseconds: `0` means no
    /// retry, a negative value means forever.
    pub fn from_millis(ms: i64) -> Self {
        match ms {
            0 => Self::Immediate,
            n if n < 0 => Self::Forever,
            n => Self::After(Duration::from_millis(n as u64)),
        }
    }
}

/// Options for [`LockFile::acquire`]. All default to git's behaviour without
/// configuration.
#[derive(Clone, Copy, Debug)]
pub struct LockOptions {
    timeout: Timeout,
    pid_file: bool,
    follow_symlinks: bool,
    shared: SharedRepository,
    fsync: Option<FsyncMethod>,
}

impl Default for LockOptions {
    fn default() -> Self {
        Self {
            timeout: Timeout::Immediate,
            pid_file: false,
            follow_symlinks: true,
            shared: SharedRepository::Umask,
            fsync: None,
        }
    }
}

impl LockOptions {
    /// Same as `Default::default()`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Retry while the lock exists (default: [`Timeout::Immediate`]).
    pub fn timeout(mut self, timeout: Timeout) -> Self {
        self.timeout = timeout;
        self
    }

    /// Write `<name>~pid.lock` with `pid <pid>` next to the lock
    /// (`core.lockfilePid`, default off). Existing PID files of other
    /// processes are read for the error message regardless of this setting.
    pub fn pid_file(mut self, enabled: bool) -> Self {
        self.pid_file = enabled;
        self
    }

    /// Lock the final target if the path is a symlink (default on). With
    /// `false` the symlink itself gets replaced on commit.
    pub fn follow_symlinks(mut self, follow: bool) -> Self {
        self.follow_symlinks = follow;
        self
    }

    /// Adjust the lock file's permissions (`core.sharedRepository`).
    pub fn shared(mut self, shared: SharedRepository) -> Self {
        self.shared = shared;
        self
    }

    /// Flush the content with this method before the rename on commit
    /// (`None`, the default, skips the flush). Callers decide from
    /// `core.fsync` whether their component is hardened.
    pub fn fsync(mut self, method: Option<FsyncMethod>) -> Self {
        self.fsync = method;
        self
    }
}

/// What is known about the process holding an existing lock, from its PID
/// file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LockHolder {
    /// No (or an empty) PID file.
    Unknown,
    /// The PID file could not be parsed; git warns about it.
    Malformed {
        /// The PID file.
        pid_file: PathBuf,
    },
    /// The recorded process no longer exists.
    Stale {
        /// Recorded PID.
        pid: u64,
    },
    /// The recorded process (or one that reused its PID) is running.
    Alive {
        /// Recorded PID.
        pid: u64,
    },
}

impl LockHolder {
    /// The explanation printed after "File exists.".
    pub fn hint(&self) -> String {
        match self {
            Self::Unknown | Self::Malformed { .. } => messages::LOCK_EXISTS_HINT.to_owned(),
            Self::Stale { pid } => messages::holder_stale(*pid),
            Self::Alive { pid } => messages::holder_alive(*pid),
        }
    }
}

/// Failure to take a lock.
///
/// `Display` gives git's message without the `fatal: ` prefix, for example
///
/// ```text
/// Unable to create '/repo/.git/index.lock': File exists.
///
/// Another git process seems to be running in this repository, or the lock file may be stale
/// ```
///
/// git exits with 128 after printing it. Print [`LockError::warning`] (as
/// `warning: ...`) before the error if it returns something.
#[derive(Debug, thiserror::Error)]
pub enum LockError {
    /// The lock file exists (after retrying for the configured timeout).
    #[error("{}", messages::lock_exists(lock_path, &holder.hint()))]
    Exists {
        /// The `.lock` path.
        lock_path: PathBuf,
        /// PID file information.
        holder: LockHolder,
    },
    /// Any other error while creating the lock file.
    #[error("{}", messages::unable_to_create(lock_path, source))]
    Io {
        /// The `.lock` path.
        lock_path: PathBuf,
        /// The underlying error.
        #[source]
        source: io::Error,
    },
}

impl LockError {
    /// The `.lock` path that could not be created.
    pub fn lock_path(&self) -> &Path {
        match self {
            Self::Exists { lock_path, .. } | Self::Io { lock_path, .. } => lock_path,
        }
    }

    /// Warning git prints before the error (malformed PID file), without the
    /// `warning: ` prefix.
    pub fn warning(&self) -> Option<String> {
        match self {
            Self::Exists {
                holder: LockHolder::Malformed { pid_file },
                ..
            } => Some(messages::malformed_pid_file(pid_file)),
            _ => None,
        }
    }
}

/// An acquired `<path>.lock`.
///
/// Write the new content through [`Write`] or [`LockFile::file_mut`], then
/// call [`LockFile::commit`]. Dropping the value without committing rolls
/// back. Until then the lock (and PID) file is also removed on `exit` and on
/// SIGINT, SIGTERM, SIGHUP, SIGQUIT and SIGPIPE.
#[derive(Debug)]
pub struct LockFile {
    target: PathBuf,
    lock_path: PathBuf,
    file: Option<File>,
    lock_reg: Option<Registration>,
    pid: Option<(PathBuf, Registration)>,
    fsync: Option<FsyncMethod>,
}

impl LockFile {
    /// Locks `path` by creating `path.lock` exclusively.
    ///
    /// If `path` is a symlink (and `follow_symlinks` is on) the lock is
    /// taken on its target, which is what the commit replaces.
    pub fn acquire(path: impl AsRef<Path>, options: &LockOptions) -> Result<Self, LockError> {
        let target = if options.follow_symlinks {
            resolve_symlinks(path.as_ref())
        } else {
            path.as_ref().to_owned()
        };
        let lock_path = append_suffix(&target, LOCK_SUFFIX);
        let deadline = match options.timeout {
            Timeout::After(d) => Some(Instant::now() + d),
            _ => None,
        };
        let mut backoff = Duration::from_millis(1);
        let mut rng = Rng::new();
        loop {
            match try_create(&lock_path, options.shared) {
                Ok((file, reg)) => {
                    let pid = if options.pid_file {
                        create_pid_file(&lock_path)
                    } else {
                        None
                    };
                    return Ok(Self {
                        target,
                        lock_path,
                        file: Some(file),
                        lock_reg: Some(reg),
                        pid,
                        fsync: options.fsync,
                    });
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    let wait = match (options.timeout, deadline) {
                        (Timeout::Immediate, _) => None,
                        (Timeout::Forever, _) => Some(rng.jitter(backoff)),
                        (Timeout::After(_), Some(deadline)) => deadline
                            .checked_duration_since(Instant::now())
                            .filter(|left| !left.is_zero())
                            .map(|left| rng.jitter(backoff).min(left)),
                        (Timeout::After(_), None) => None,
                    };
                    match wait {
                        Some(wait) => {
                            std::thread::sleep(wait);
                            backoff = (backoff * 2).min(MAX_BACKOFF);
                        }
                        None => {
                            let holder = read_holder(&pid_path_for(&lock_path));
                            return Err(LockError::Exists { lock_path, holder });
                        }
                    }
                }
                Err(source) => return Err(LockError::Io { lock_path, source }),
            }
        }
    }

    /// The path that [`commit`](Self::commit) replaces (symlinks resolved).
    pub fn target(&self) -> &Path {
        &self.target
    }

    /// The `.lock` file being written.
    pub fn lock_path(&self) -> &Path {
        &self.lock_path
    }

    /// The open lock file, positioned where the last write ended.
    pub fn file_mut(&mut self) -> &mut File {
        self.file
            .as_mut()
            .expect("lock file is open until commit/rollback")
    }

    /// Renames the lock file over the target.
    ///
    /// Flushes first if the options asked for it. On failure the lock is
    /// rolled back and the target is unchanged.
    pub fn commit(self) -> io::Result<()> {
        let target = self.target.clone();
        self.commit_to(target)
    }

    /// Like [`commit`](Self::commit), but renames the lock file to `dest`
    /// instead of the locked path.
    pub fn commit_to(mut self, dest: impl AsRef<Path>) -> io::Result<()> {
        let result = self.finish_write().and_then(|()| {
            // Unregister before the rename: after it, the `.lock` name may
            // belong to the next writer, whose lock our signal handler must
            // never delete.
            if let Some(reg) = self.lock_reg.take() {
                registry::unregister(reg);
            }
            std::fs::rename(&self.lock_path, dest.as_ref())
        });
        if result.is_err() {
            let _ = std::fs::remove_file(&self.lock_path);
        }
        self.remove_pid_file();
        result
    }

    /// Deletes the lock file and leaves the target untouched.
    pub fn rollback(mut self) {
        self.rollback_in_place();
    }

    fn finish_write(&mut self) -> io::Result<()> {
        let file = self
            .file
            .take()
            .expect("lock file is open until commit/rollback");
        if let Some(method) = self.fsync {
            fsync::harden(&file, method)?;
        }
        drop(file);
        Ok(())
    }

    fn rollback_in_place(&mut self) {
        self.file = None;
        if let Some(reg) = self.lock_reg.take() {
            let _ = std::fs::remove_file(&self.lock_path);
            registry::unregister(reg);
        }
        self.remove_pid_file();
    }

    fn remove_pid_file(&mut self) {
        if let Some((path, reg)) = self.pid.take() {
            let _ = std::fs::remove_file(&path);
            registry::unregister(reg);
        }
    }
}

impl Write for LockFile {
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

impl Drop for LockFile {
    fn drop(&mut self) {
        self.rollback_in_place();
    }
}

fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// `dir/name.lock` -> `dir/name~pid.lock`.
pub(crate) fn pid_path_for(lock_path: &Path) -> PathBuf {
    let s = lock_path.as_os_str();
    #[cfg(unix)]
    {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        let bytes = s.as_bytes();
        let stem = bytes.strip_suffix(LOCK_SUFFIX.as_bytes()).unwrap_or(bytes);
        let mut out = stem.to_vec();
        out.extend_from_slice(PID_LOCK_SUFFIX.as_bytes());
        PathBuf::from(std::ffi::OsString::from_vec(out))
    }
    #[cfg(not(unix))]
    {
        let text = s.to_string_lossy();
        let stem = text.strip_suffix(LOCK_SUFFIX).unwrap_or(&text);
        PathBuf::from(format!("{stem}{PID_LOCK_SUFFIX}"))
    }
}

/// Follows a chain of symlinks at the final path component.
fn resolve_symlinks(path: &Path) -> PathBuf {
    let mut current = path.to_owned();
    for _ in 0..MAX_SYMLINK_DEPTH {
        match std::fs::read_link(&current) {
            Ok(link) => {
                current = if link.is_absolute() {
                    link
                } else {
                    match current.parent() {
                        Some(dir) => dir.join(link),
                        None => link,
                    }
                };
            }
            Err(_) => break,
        }
    }
    current
}

fn try_create(lock_path: &Path, shared: SharedRepository) -> io::Result<(File, Registration)> {
    let mut opts = OpenOptions::new();
    opts.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o666);
    }
    let file = opts.open(lock_path)?;
    let reg = registry::register(lock_path);
    #[cfg(unix)]
    if shared != SharedRepository::Umask {
        use std::os::unix::fs::PermissionsExt;
        let adjust = || -> io::Result<()> {
            let mode = file.metadata()?.permissions().mode();
            let wanted = shared.adjust_file_mode(mode);
            if wanted != mode {
                file.set_permissions(std::fs::Permissions::from_mode(wanted))?;
            }
            Ok(())
        };
        if let Err(e) = adjust() {
            drop(file);
            let _ = std::fs::remove_file(lock_path);
            registry::unregister(reg);
            return Err(e);
        }
    }
    #[cfg(not(unix))]
    let _ = shared;
    Ok((file, reg))
}

/// Writes `pid <pid>\n` into a new PID file. An existing PID file (left by a
/// crashed process) is kept and not taken over, as git does.
fn create_pid_file(lock_path: &Path) -> Option<(PathBuf, Registration)> {
    let path = pid_path_for(lock_path);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .ok()?;
    let reg = registry::register(&path);
    if file
        .write_all(format!("pid {}\n", std::process::id()).as_bytes())
        .is_err()
    {
        drop(file);
        let _ = std::fs::remove_file(&path);
        registry::unregister(reg);
        return None;
    }
    Some((path, reg))
}

fn read_holder(pid_path: &Path) -> LockHolder {
    let Ok(content) = std::fs::read(pid_path) else {
        return LockHolder::Unknown;
    };
    if content.is_empty() {
        return LockHolder::Unknown;
    }
    match parse_pid_file(&content) {
        Some(pid) if process_exists(pid) => LockHolder::Alive { pid },
        Some(pid) => LockHolder::Stale { pid },
        None => LockHolder::Malformed {
            pid_file: pid_path.to_owned(),
        },
    }
}

/// Parses `pid <n>` as git 2.56 accepts it: whitespace may precede the
/// number and follow it, a sign is allowed (negative values wrap like
/// `strtoull`), overflow saturates, zero is rejected.
pub(crate) fn parse_pid_file(content: &[u8]) -> Option<u64> {
    let rest = content.strip_prefix(b"pid ")?;
    let rest = trim_start_ws(rest);
    let (negative, rest) = match rest.first() {
        Some(b'-') => (true, &rest[1..]),
        Some(b'+') => (false, &rest[1..]),
        _ => (false, rest),
    };
    let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 {
        return None;
    }
    if !trim_start_ws(&rest[digits..]).is_empty() {
        return None;
    }
    let mut value: u64 = 0;
    let mut overflow = false;
    for &b in &rest[..digits] {
        match value
            .checked_mul(10)
            .and_then(|v| v.checked_add(u64::from(b - b'0')))
        {
            Some(v) => value = v,
            None => {
                overflow = true;
                break;
            }
        }
    }
    let value = if overflow {
        u64::MAX
    } else if negative {
        value.wrapping_neg()
    } else {
        value
    };
    (value != 0).then_some(value)
}

fn trim_start_ws(s: &[u8]) -> &[u8] {
    let n = s
        .iter()
        .take_while(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c))
        .count();
    &s[n..]
}

#[cfg(unix)]
fn process_exists(pid: u64) -> bool {
    // Truncation to pid_t mirrors the reference implementation's observable
    // behaviour for out-of-range values.
    let pid = pid as libc::pid_t;
    // SAFETY: signal 0 only checks for existence and permission.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn process_exists(_pid: u64) -> bool {
    // Without a cheap check, do not call the lock stale.
    true
}

/// Small xorshift generator for backoff jitter (no crypto needed).
struct Rng(u64);

impl Rng {
    fn new() -> Self {
        use std::hash::{BuildHasher, Hasher};
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u32(std::process::id());
        Self(h.finish() | 1)
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// A random duration in `[d/2, 3d/2)`.
    fn jitter(&mut self, d: Duration) -> Duration {
        let nanos = d.as_nanos().max(2) as u64;
        Duration::from_nanos(nanos / 2 + self.next() % nanos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pid_path() {
        assert_eq!(
            pid_path_for(Path::new("/r/.git/index.lock")),
            Path::new("/r/.git/index~pid.lock")
        );
        assert_eq!(
            pid_path_for(Path::new("refs/heads/main.lock")),
            Path::new("refs/heads/main~pid.lock")
        );
    }

    #[test]
    fn pid_file_parsing_matches_git() {
        // Cases observed with git 2.56 (see crate tests for the live check).
        let ok: &[(&[u8], u64)] = &[
            (b"pid 99999", 99999),
            (b"pid 99999\n", 99999),
            (b"pid  99999", 99999),
            (b"pid 99999 \t\n", 99999),
            (b"pid +99999", 99999),
            (b"pid 99999\n\n", 99999),
            (b"pid -5", 18446744073709551611),
            (b"pid 99999999999999999999999", u64::MAX),
        ];
        for (input, pid) in ok {
            assert_eq!(parse_pid_file(input), Some(*pid), "{input:?}");
        }
        let bad: &[&[u8]] = &[
            b"pid 0",
            b"pid 99999 x",
            b"PID 99999",
            b"pid 99999\nmore",
            b" pid 99999",
            b"pid 0x10",
            b"pid",
            b"pid \n",
            b"   \n",
        ];
        for input in bad {
            assert_eq!(parse_pid_file(input), None, "{input:?}");
        }
    }

    #[test]
    fn timeout_from_millis() {
        assert_eq!(Timeout::from_millis(0), Timeout::Immediate);
        assert_eq!(Timeout::from_millis(-1), Timeout::Forever);
        assert_eq!(
            Timeout::from_millis(100),
            Timeout::After(Duration::from_millis(100))
        );
    }
}
