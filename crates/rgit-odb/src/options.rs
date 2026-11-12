// SPDX-License-Identifier: MIT

//! Settings for writing loose objects and how they are read from the
//! configuration.

use rgit_config::ConfigSet;
use rgit_fs::{FsyncComponents, FsyncMethod, SharedRepository};

use crate::{Error, messages};

/// The default `core.looseCompression` when neither it nor
/// `core.compression` is set: 1 (best speed).
pub const DEFAULT_LOOSE_COMPRESSION: u32 = 1;

/// zlib's own default level, used for the configured value `-1`.
pub const ZLIB_DEFAULT_COMPRESSION: u32 = 6;

/// How loose objects are written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LooseOptions {
    /// zlib level 0..=9 (`core.looseCompression`, falling back to
    /// `core.compression`; `-1` maps to zlib's default 6).
    pub compression: u32,
    /// `core.sharedRepository`: permissions of new files and directories.
    pub shared: SharedRepository,
    /// Whether new loose objects are flushed to disk, and how
    /// (`core.fsync` contains `loose-object`, `core.fsyncMethod`).
    /// `None` means no flush (the platform default).
    pub fsync: Option<FsyncMethod>,
    /// Verify that the content of every object read hashes to its id.
    /// Off by default, as in git; `fsck` uses [`crate::LooseStore::verify`].
    pub check_hash: bool,
}

impl Default for LooseOptions {
    fn default() -> LooseOptions {
        LooseOptions {
            compression: DEFAULT_LOOSE_COMPRESSION,
            shared: SharedRepository::Umask,
            fsync: None,
            check_hash: false,
        }
    }
}

impl LooseOptions {
    /// Read the settings from `config`.
    ///
    /// Invalid compression levels and malformed numbers are errors (git
    /// dies); unknown `core.fsync` components and `core.fsyncMethod` values
    /// are ignored with a warning, returned in `warnings` (each to be
    /// printed as `warning: <text>`).
    pub fn from_config(
        config: &ConfigSet,
        warnings: &mut Vec<String>,
    ) -> Result<LooseOptions, Error> {
        let int = |name: &str| config.int(name).map_err(|e| Error::Config(e.to_string()));
        let level = |v: i64| match v {
            -1 => Ok(ZLIB_DEFAULT_COMPRESSION),
            0..=9 => Ok(v as u32),
            _ => Err(Error::Config(messages::bad_compression_level(v))),
        };
        let core = int("core.compression")?.map(level).transpose()?;
        let loose = int("core.looseCompression")?.map(level).transpose()?;
        let compression = loose.or(core).unwrap_or(DEFAULT_LOOSE_COMPRESSION);

        let shared = match config.string("core.sharedRepository") {
            Ok(Some(v)) => SharedRepository::parse(&String::from_utf8_lossy(v))
                .map_err(|e| Error::Config(e.to_string()))?,
            Ok(None) => SharedRepository::Umask,
            Err(e) => return Err(Error::Config(e.to_string())),
        };

        let mut components = FsyncComponents::PLATFORM_DEFAULT;
        let mut fsync_set = false;
        for entry in config.get_all("core.fsync") {
            let value = entry.string().map_err(|e| Error::Config(e.to_string()))?;
            components = parse_fsync(&String::from_utf8_lossy(value), warnings);
            fsync_set = true;
        }
        if !fsync_set
            && config
                .bool("core.fsyncObjectFiles")
                .map_err(|e| Error::Config(e.to_string()))?
                .unwrap_or(false)
        {
            components |= FsyncComponents::LOOSE_OBJECT;
        }
        let mut method = FsyncMethod::default();
        if let Some(v) = config
            .string("core.fsyncMethod")
            .map_err(|e| Error::Config(e.to_string()))?
        {
            let v = String::from_utf8_lossy(v);
            match FsyncMethod::from_name(&v) {
                Some(m) => method = m,
                None => warnings.push(messages::unknown_fsync_method(&v)),
            }
        }
        let fsync = components
            .contains(FsyncComponents::LOOSE_OBJECT)
            .then_some(method);

        Ok(LooseOptions {
            compression,
            shared,
            fsync,
            check_hash: false,
        })
    }
}

/// Parse one `core.fsync` value: start from the platform default, `-x`
/// removes a component, `none` clears the set, the empty string restores
/// the default.
pub fn parse_fsync(value: &str, warnings: &mut Vec<String>) -> FsyncComponents {
    let mut set = FsyncComponents::PLATFORM_DEFAULT;
    for item in value.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let (remove, name) = match item.strip_prefix('-') {
            Some(n) => (true, n),
            None => (false, item),
        };
        match FsyncComponents::from_name(name) {
            Some(FsyncComponents::NONE) if !remove => set = FsyncComponents::NONE,
            Some(c) if remove => set = set - c,
            Some(c) => set |= c,
            None => warnings.push(messages::unknown_fsync_component(name)),
        }
    }
    set
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fsync_values() {
        let mut w = Vec::new();
        assert_eq!(parse_fsync("", &mut w), FsyncComponents::PLATFORM_DEFAULT);
        assert!(parse_fsync("loose-object", &mut w).contains(FsyncComponents::LOOSE_OBJECT));
        assert!(!parse_fsync("all,-loose-object", &mut w).contains(FsyncComponents::LOOSE_OBJECT));
        assert_eq!(parse_fsync("none", &mut w), FsyncComponents::NONE);
        assert!(w.is_empty());
        parse_fsync("bogus", &mut w);
        assert_eq!(w, ["ignoring unknown core.fsync component 'bogus'"]);
    }
}
