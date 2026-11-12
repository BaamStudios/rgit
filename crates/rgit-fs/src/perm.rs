// SPDX-License-Identifier: MIT
//! Permissions of newly created repository files (`core.sharedRepository`).

use crate::messages;

/// Value of `core.sharedRepository`.
///
/// Observed with git 2.56 on lock files (base mode `0666 & !umask`):
///
/// | value   | umask 077 | umask 022 |
/// |---------|-----------|-----------|
/// | `umask` | 600       | 644       |
/// | `group` | 660       | 664       |
/// | `all`   | 664       | 664       |
/// | `0640`  | 640       | 640       |
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SharedRepository {
    /// `umask` / `false`: keep what umask(2) gives.
    #[default]
    Umask,
    /// `group` / `true`: the group gets the owner's permissions.
    Group,
    /// `all` / `world` / `everybody`: like `Group`, others may also read.
    All,
    /// `0xxx`: this exact mode for files (the umask is ignored).
    Mode(u32),
}

/// Rejected `core.sharedRepository` value.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SharedRepositoryError {
    /// An octal mode that does not give the owner read and write access.
    #[error("{}", messages::shared_repository_owner_perms(.0))]
    OwnerPermissions(String),
    /// Neither a keyword, a boolean nor an octal mode.
    #[error("invalid value for core.sharedRepository: '{0}'")]
    Invalid(String),
}

impl SharedRepository {
    /// Parses a `core.sharedRepository` (or `git init --shared=`) value.
    ///
    /// Keywords are matched case-insensitively; the boolean spellings
    /// `true/yes/on` and `false/no/off` mean `group` and `umask`. An octal
    /// mode must start with `0`.
    pub fn parse(value: &str) -> Result<Self, SharedRepositoryError> {
        let lower = value.to_ascii_lowercase();
        match lower.as_str() {
            "umask" | "false" | "no" | "off" => return Ok(Self::Umask),
            "group" | "true" | "yes" | "on" => return Ok(Self::Group),
            "all" | "world" | "everybody" => return Ok(Self::All),
            _ => {}
        }
        let invalid = || SharedRepositoryError::Invalid(value.to_owned());
        if !value.starts_with('0') || value.len() < 2 {
            return Err(invalid());
        }
        let mode = u32::from_str_radix(value, 8).map_err(|_| invalid())?;
        if mode > 0o777 {
            return Err(invalid());
        }
        if mode & 0o600 != 0o600 {
            return Err(SharedRepositoryError::OwnerPermissions(value.to_owned()));
        }
        Ok(Self::Mode(mode))
    }

    /// Mode a new regular file should get, given the mode it was created
    /// with (already reduced by the umask). Executable files keep execute
    /// bits wherever read access is granted.
    pub fn adjust_file_mode(self, mode: u32) -> u32 {
        let perm = mode & 0o777;
        let adjusted = match self {
            Self::Umask => perm,
            Self::Group => perm | ((perm & 0o700) >> 3),
            Self::All => perm | ((perm & 0o700) >> 3) | ((perm & 0o500) >> 6),
            Self::Mode(m) => {
                let mut m = m & 0o666;
                if perm & 0o100 != 0 {
                    m |= (m & 0o444) >> 2;
                }
                m
            }
        };
        (mode & !0o777) | adjusted
    }
}

#[cfg(test)]
mod tests {
    use super::SharedRepository::{self, *};

    #[test]
    fn parse_values() {
        assert_eq!(SharedRepository::parse("group"), Ok(Group));
        assert_eq!(SharedRepository::parse("TRUE"), Ok(Group));
        assert_eq!(SharedRepository::parse("everybody"), Ok(All));
        assert_eq!(SharedRepository::parse("false"), Ok(Umask));
        assert_eq!(SharedRepository::parse("0640"), Ok(Mode(0o640)));
        assert_eq!(
            SharedRepository::parse("0444").unwrap_err().to_string(),
            "problem with core.sharedRepository filemode value (0444).\n\
             The owner of files must always have read and write permissions."
        );
        assert!(SharedRepository::parse("bogus").is_err());
    }

    #[test]
    fn modes_match_git() {
        // Base modes 0666 & !umask for umask 077, 022, 002 (see table above).
        for (base, group, all) in [
            (0o600, 0o660, 0o664),
            (0o644, 0o664, 0o664),
            (0o664, 0o664, 0o664),
        ] {
            assert_eq!(Umask.adjust_file_mode(base), base);
            assert_eq!(Group.adjust_file_mode(base), group);
            assert_eq!(All.adjust_file_mode(base), all);
            assert_eq!(Mode(0o640).adjust_file_mode(base), 0o640);
            assert_eq!(Mode(0o660).adjust_file_mode(base), 0o660);
        }
        assert_eq!(Mode(0o640).adjust_file_mode(0o700), 0o750);
    }
}
