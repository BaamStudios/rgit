// SPDX-License-Identifier: MIT
//! Permissions of newly created repository files (`core.sharedRepository`).

use std::io;
use std::path::Path;

use crate::messages;

/// Value of `core.sharedRepository`.
///
/// Observed with git 2.56 (base mode `0666 & !umask` for files, `0777 &
/// !umask` for directories):
///
/// | value   | umask 077 | umask 022 | directories (umask 022) |
/// |---------|-----------|-----------|-------------------------|
/// | `umask` | 600       | 644       | 755                     |
/// | `group` | 660       | 664       | 2775                    |
/// | `all`   | 664       | 664       | 2775                    |
/// | `0640`  | 640       | 640       | 2750                    |
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SharedRepository {
    /// `umask` / `false`: keep what umask(2) gives.
    #[default]
    Umask,
    /// `group` / `true`: the group gets the owner's permissions.
    Group,
    /// `all` / `world` / `everybody`: like `Group`, others may also read.
    All,
    /// `0xxx`: this exact mode for files (the umask is ignored). Only the
    /// read and write bits are kept (`0777` means `0666`).
    Mode(u32),
}

/// Rejected `core.sharedRepository` value.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SharedRepositoryError {
    /// An octal mode that does not give the owner read and write access;
    /// the parsed number (shown as git does: `0` and at least three octal
    /// digits of its 32-bit value).
    #[error("{}", messages::shared_repository_owner_perms(&format!("0{:03o}", .0)))]
    OwnerPermissions(u32),
    /// Neither a keyword, an octal mode nor a boolean; the value and the
    /// name it was given as (`core.sharedrepository`, or `arg` for
    /// `git init --shared=`).
    #[error("{}", messages::bad_boolean(.value, .name))]
    Invalid {
        /// The value as given.
        value: String,
        /// The variable name used in the message.
        name: String,
    },
}

impl SharedRepository {
    /// Parses a `core.sharedRepository` value, naming the variable
    /// `core.sharedrepository` in errors.
    pub fn parse(value: &str) -> Result<Self, SharedRepositoryError> {
        Self::parse_named(value, "core.sharedrepository")
    }

    /// Parses a `core.sharedRepository` (or `git init --shared=`) value;
    /// `name` is used in the error for a value that is not understood.
    ///
    /// Behaviour of git 2.56, checked against the binary:
    /// - the keywords `umask`, `group`, `all`, `world`, `everybody`
    ///   (exact case);
    /// - otherwise an octal number (C `strtol` in base 8: leading blanks
    ///   and a sign are allowed, nothing may follow). `0`, `1` and `2` mean
    ///   `umask`, `group` and `all`; any other number is a mode that must
    ///   give the owner read and write access;
    /// - otherwise a boolean (`true`/`yes`/`on`, `false`/`no`/`off`, any
    ///   case, the empty string, or a decimal/hex/octal integer with an
    ///   optional `k`/`m`/`g` suffix): true is `group`, false `umask`.
    pub fn parse_named(value: &str, name: &str) -> Result<Self, SharedRepositoryError> {
        match value {
            "umask" => return Ok(Self::Umask),
            "group" => return Ok(Self::Group),
            "all" | "world" | "everybody" => return Ok(Self::All),
            _ => {}
        }
        if let Some(n) = strtol_octal(value) {
            return match n {
                0 => Ok(Self::Umask),
                1 => Ok(Self::Group),
                2 => Ok(Self::All),
                _ => {
                    // git keeps the number in a C `int`.
                    let bits = n as i32 as u32;
                    if bits & 0o600 != 0o600 {
                        Err(SharedRepositoryError::OwnerPermissions(bits))
                    } else {
                        Ok(Self::Mode(bits & 0o666))
                    }
                }
            };
        }
        match parse_bool(value) {
            Some(true) => Ok(Self::Group),
            Some(false) => Ok(Self::Umask),
            None => Err(SharedRepositoryError::Invalid {
                value: value.to_owned(),
                name: name.to_owned(),
            }),
        }
    }

    /// The value as git writes it into `core.sharedRepository`: `1` for
    /// `group`, `2` for `all`, the mode as four octal digits. `None` for
    /// `umask` (nothing is written).
    pub fn config_value(self) -> Option<String> {
        match self {
            Self::Umask => None,
            Self::Group => Some("1".into()),
            Self::All => Some("2".into()),
            Self::Mode(m) => Some(format!("{m:04o}")),
        }
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

    /// Mode a new directory should get, given the mode it was created with
    /// (already reduced by the umask): like an executable file, plus the
    /// set-group-id bit when the group gets any access, so that files in it
    /// inherit the directory's group.
    pub fn adjust_dir_mode(self, mode: u32) -> u32 {
        if self == Self::Umask {
            return mode;
        }
        let adjusted = self.adjust_file_mode(mode | 0o100);
        let setgid = if adjusted & 0o070 != 0 { 0o2000 } else { 0 };
        (adjusted & !0o7000) | (mode & 0o5000) | setgid
    }

    /// Adjust the permissions of the new file or directory `path` (git's
    /// `adjust_shared_perm`). Symbolic links are left alone; nothing is done
    /// for `umask`.
    pub fn apply(self, path: &Path) -> io::Result<()> {
        if self == Self::Umask {
            return Ok(());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let meta = std::fs::symlink_metadata(path)?;
            if meta.file_type().is_symlink() {
                return Ok(());
            }
            let mode = meta.permissions().mode() & 0o7777;
            let wanted = if meta.is_dir() {
                self.adjust_dir_mode(mode)
            } else {
                self.adjust_file_mode(mode)
            };
            if wanted != mode {
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(wanted))?;
            }
        }
        #[cfg(not(unix))]
        let _ = path;
        Ok(())
    }
}

fn c_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// `strtol(s, &end, 8)` that must consume the whole string. Overflow
/// saturates like C does.
fn strtol_octal(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && c_space(b[i]) {
        i += 1;
    }
    let mut neg = false;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        neg = b[i] == b'-';
        i += 1;
    }
    let start = i;
    let mut acc: i128 = 0;
    while i < b.len() && (b'0'..=b'7').contains(&b[i]) {
        acc = (acc * 8 + i128::from(b[i] - b'0')).min(i128::from(i64::MAX) + 1);
        i += 1;
    }
    if i == start || i != b.len() {
        return None;
    }
    let v = if neg { -acc } else { acc };
    Some(v.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64)
}

/// git's boolean parsing for configuration values: words, the empty
/// string, or a 32-bit integer (base prefix `0x`/`0`, unit `k`/`m`/`g`).
fn parse_bool(s: &str) -> Option<bool> {
    let lower = s.to_ascii_lowercase();
    match lower.as_str() {
        "true" | "yes" | "on" => return Some(true),
        "false" | "no" | "off" | "" => return Some(false),
        _ => {}
    }
    parse_int32(s).map(|n| n != 0)
}

/// C `strtoimax(s, &end, 0)` plus an optional unit suffix, limited to the
/// range of a 32-bit `int`.
fn parse_int32(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && c_space(b[i]) {
        i += 1;
    }
    let mut neg = false;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        neg = b[i] == b'-';
        i += 1;
    }
    let (radix, start) = if b.len() > i + 2
        && b[i] == b'0'
        && (b[i + 1] | 0x20) == b'x'
        && b[i + 2].is_ascii_hexdigit()
    {
        (16, i + 2)
    } else if i < b.len() && b[i] == b'0' {
        (8, i)
    } else {
        (10, i)
    };
    let mut j = start;
    let mut acc: i128 = 0;
    while j < b.len() {
        let Some(d) = (b[j] as char).to_digit(radix) else {
            break;
        };
        acc = (acc * i128::from(radix) + i128::from(d)).min(1 << 70);
        j += 1;
    }
    if j == start {
        return None;
    }
    let factor: i128 = match &b[j..] {
        [] => 1,
        [c] => match c.to_ascii_lowercase() {
            b'k' => 1 << 10,
            b'm' => 1 << 20,
            b'g' => 1 << 30,
            _ => return None,
        },
        _ => return None,
    };
    let v = if neg { -acc } else { acc } * factor;
    (i128::from(i32::MIN)..=i128::from(i32::MAX))
        .contains(&v)
        .then_some(v as i64)
}

#[cfg(test)]
mod tests {
    use super::SharedRepository::{self, *};
    use super::SharedRepositoryError;

    fn arg(v: &str) -> Result<SharedRepository, SharedRepositoryError> {
        SharedRepository::parse_named(v, "arg")
    }

    #[test]
    fn parse_values() {
        assert_eq!(arg("group"), Ok(Group));
        assert_eq!(arg("TRUE"), Ok(Group));
        assert_eq!(arg("yes"), Ok(Group));
        assert_eq!(arg(" 1"), Ok(Group));
        assert_eq!(arg("0x1"), Ok(Group));
        assert_eq!(arg("2"), Ok(All));
        assert_eq!(arg("everybody"), Ok(All));
        assert_eq!(arg("false"), Ok(Umask));
        assert_eq!(arg(""), Ok(Umask));
        assert_eq!(arg("0"), Ok(Umask));
        assert_eq!(arg("off"), Ok(Umask));
        assert_eq!(arg("0640"), Ok(Mode(0o640)));
        assert_eq!(arg("640"), Ok(Mode(0o640)));
        assert_eq!(arg("+0640"), Ok(Mode(0o640)));
        assert_eq!(arg("01640"), Ok(Mode(0o640)));
        assert_eq!(arg("0777"), Ok(Mode(0o666)));
        assert_eq!(arg("0007700"), Ok(Mode(0o600)));
        assert_eq!(arg("-1"), Ok(Mode(0o666)));
        assert_eq!(Mode(0o666).config_value().as_deref(), Some("0666"));
        assert_eq!(
            arg("0444").unwrap_err().to_string(),
            "problem with core.sharedRepository filemode value (0444).\n\
             The owner of files must always have read and write permissions."
        );
        assert_eq!(
            arg("3").unwrap_err(),
            SharedRepositoryError::OwnerPermissions(3)
        );
        assert!(
            arg("-0640")
                .unwrap_err()
                .to_string()
                .contains("(037777777140)")
        );
        for bad in ["Group", "bogus", "0o640", "08", "1x", "1 ", "0640a"] {
            assert_eq!(
                arg(bad).unwrap_err().to_string(),
                format!("bad boolean config value '{bad}' for 'arg'"),
                "{bad}"
            );
        }
        assert_eq!(
            SharedRepository::parse("bogus").unwrap_err().to_string(),
            "bad boolean config value 'bogus' for 'core.sharedrepository'"
        );
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
        assert_eq!(Mode(0o666).adjust_file_mode(0o755), 0o777);
    }

    #[test]
    fn directory_modes_match_git() {
        // Directories created with 0777 & !umask for umask 077, 022, 002.
        for (base, group, all, m660) in [
            (0o700, 0o2770, 0o2775, 0o2770),
            (0o755, 0o2775, 0o2775, 0o2770),
            (0o775, 0o2775, 0o2775, 0o2770),
        ] {
            assert_eq!(Umask.adjust_dir_mode(base), base);
            assert_eq!(Group.adjust_dir_mode(base), group);
            assert_eq!(All.adjust_dir_mode(base), all);
            assert_eq!(Mode(0o660).adjust_dir_mode(base), m660);
        }
        assert_eq!(Mode(0o640).adjust_dir_mode(0o755), 0o2750);
        assert_eq!(Mode(0o600).adjust_dir_mode(0o755), 0o700);
        assert_eq!(Mode(0o606).adjust_dir_mode(0o755), 0o707);
        assert_eq!(Mode(0o666).adjust_dir_mode(0o755), 0o2777);
    }
}
