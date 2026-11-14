// SPDX-License-Identifier: MIT

//! End-of-line conversion (gitattributes(5) `text`, `eol`, `crlf`;
//! git-config(1) `core.autocrlf`, `core.eol`, `core.safecrlf`).
//!
//! A path is converted in one of two ways ([`Mode`]): as text (`text`
//! set, or `eol` given while `text` is unspecified), where CR LF always
//! becomes LF on the way into the repository, or automatically
//! (`text=auto`, or `core.autocrlf` while `text` is unspecified), where
//! content that looks binary ([`Stats::is_binary`]) is left alone, and so
//! is a file whose index blob already is text with CR LF. On checkout,
//! LF becomes CR LF when the work tree uses CR LF ([`Plan::crlf`]); in
//! automatic mode only for blobs that are text without any CR.

use crate::stats::Stats;

/// A line ending.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Eol {
    /// `\n`.
    Lf,
    /// `\r\n`.
    Crlf,
}

impl Eol {
    /// `lf` or `crlf` (case-sensitive, as git reads the `eol` attribute
    /// and `core.eol`); `None` for anything else.
    #[must_use]
    pub fn parse(v: &[u8]) -> Option<Eol> {
        match v {
            b"lf" => Some(Eol::Lf),
            b"crlf" => Some(Eol::Crlf),
            _ => None,
        }
    }

    /// The platform's line ending (`core.eol=native`).
    #[must_use]
    pub fn native() -> Eol {
        if cfg!(windows) { Eol::Crlf } else { Eol::Lf }
    }
}

/// `core.autocrlf`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AutoCrlf {
    /// false (the default).
    #[default]
    False,
    /// true: `text=auto` for every path with CR LF in the work tree.
    True,
    /// input: `text=auto` without conversion on checkout.
    Input,
}

/// `core.safecrlf`: what to do about a conversion that a checkout would
/// not undo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SafeCrlf {
    /// Nothing.
    False,
    /// Print a warning (the default).
    Warn,
    /// Refuse (fatal).
    Fail,
}

/// The `text` attribute, with the legacy `crlf` attribute folded in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Text {
    /// `text` (or `crlf`).
    Set,
    /// `-text` (or `-crlf`).
    Unset,
    /// `text=auto`.
    Auto,
    /// `crlf=input` with `text` unspecified: text with LF in the work tree.
    Input,
    /// Not specified (or any other value).
    Unspecified,
}

/// How a path is converted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Not at all.
    Off,
    /// Always, as text.
    Text,
    /// Only content that looks like text.
    Auto,
}

/// The line ending conversion of one path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plan {
    /// How.
    pub mode: Mode,
    /// Whether checkout writes CR LF.
    pub crlf: bool,
}

impl Plan {
    /// No conversion.
    pub const OFF: Plan = Plan {
        mode: Mode::Off,
        crlf: false,
    };

    /// The conversion for the attributes `text` and `eol` with the
    /// configuration `autocrlf` and `core_eol` (`core.eol`, native when
    /// unset or invalid).
    #[must_use]
    pub fn new(text: Text, eol: Option<Eol>, autocrlf: AutoCrlf, core_eol: Eol) -> Plan {
        let default_crlf = match autocrlf {
            AutoCrlf::True => true,
            AutoCrlf::Input => false,
            AutoCrlf::False => core_eol == Eol::Crlf,
        };
        let out = |default: bool| eol.map_or(default, |e| e == Eol::Crlf);
        match text {
            Text::Unset => Plan::OFF,
            Text::Set => Plan {
                mode: Mode::Text,
                crlf: out(default_crlf),
            },
            Text::Input => Plan {
                mode: Mode::Text,
                crlf: out(false),
            },
            Text::Auto => Plan {
                mode: Mode::Auto,
                crlf: out(default_crlf),
            },
            Text::Unspecified => match (eol, autocrlf) {
                (Some(e), _) => Plan {
                    mode: Mode::Text,
                    crlf: e == Eol::Crlf,
                },
                (None, AutoCrlf::False) => Plan::OFF,
                (None, a) => Plan {
                    mode: Mode::Auto,
                    crlf: a == AutoCrlf::True,
                },
            },
        }
    }
}

/// A conversion that a checkout with the current settings would not undo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Irreversible {
    /// CR LF in the file would become LF.
    CrlfToLf,
    /// LF in the file would become CR LF.
    LfToCrlf,
}

/// The repository form of the work tree content `data` under `plan`:
/// `Some` with the converted content when CR LF had to become LF.
/// `index_has_crlf` tells whether the path's index blob is text with
/// CR LF (asked in automatic mode only, and only when `data` has CR LF).
/// The second value names a change the next checkout would not undo.
pub fn to_git(
    data: &[u8],
    plan: Plan,
    index_has_crlf: impl FnOnce() -> bool,
) -> (Option<Vec<u8>>, Option<Irreversible>) {
    if plan.mode == Mode::Off {
        return (None, None);
    }
    let s = Stats::new(data);
    if plan.mode == Mode::Auto && s.is_binary() {
        return (None, None);
    }
    let convert = s.crlf > 0 && (plan.mode == Mode::Text || !index_has_crlf());
    // What a checkout of the result would write.
    let (lone_lf, crlf) = if convert {
        (s.lone_lf + s.crlf, 0)
    } else {
        (s.lone_lf, s.crlf)
    };
    let checkout_converts = plan.crlf && lone_lf > 0 && (plan.mode == Mode::Text || crlf == 0);
    let (final_lf, final_crlf) = if checkout_converts {
        (0, crlf + lone_lf)
    } else {
        (lone_lf, crlf)
    };
    let irreversible = if s.lone_lf > 0 && final_lf == 0 {
        Some(Irreversible::LfToCrlf)
    } else if s.crlf > 0 && final_crlf == 0 {
        Some(Irreversible::CrlfToLf)
    } else {
        None
    };
    let out = convert.then(|| crlf_to_lf(data));
    (out, irreversible)
}

/// The work tree form of the blob `data` under `plan`: `Some` when LF had
/// to become CR LF.
#[must_use]
pub fn to_worktree(data: &[u8], plan: Plan) -> Option<Vec<u8>> {
    if plan.mode == Mode::Off || !plan.crlf {
        return None;
    }
    let s = Stats::new(data);
    if s.lone_lf == 0 {
        return None;
    }
    if plan.mode == Mode::Auto && (s.crlf > 0 || s.is_binary()) {
        return None;
    }
    let mut out = Vec::with_capacity(data.len() + s.lone_lf);
    let mut prev = 0u8;
    for &b in data {
        if b == b'\n' && prev != b'\r' {
            out.push(b'\r');
        }
        out.push(b);
        prev = b;
    }
    Some(out)
}

/// `data` with every CR LF replaced by LF.
fn crlf_to_lf(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        if data[i] == b'\r' && data.get(i + 1) == Some(&b'\n') {
            i += 1;
            continue;
        }
        out.push(data[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const LF: Eol = Eol::Lf;

    #[test]
    fn plans() {
        let p = |t, e, a| Plan::new(t, e, a, LF);
        assert_eq!(p(Text::Unspecified, None, AutoCrlf::False), Plan::OFF);
        assert_eq!(
            p(Text::Unspecified, None, AutoCrlf::True),
            Plan {
                mode: Mode::Auto,
                crlf: true
            }
        );
        assert_eq!(
            p(Text::Unspecified, Some(Eol::Crlf), AutoCrlf::Input),
            Plan {
                mode: Mode::Text,
                crlf: true
            }
        );
        assert_eq!(
            p(Text::Set, None, AutoCrlf::Input),
            Plan {
                mode: Mode::Text,
                crlf: false
            }
        );
        assert_eq!(p(Text::Unset, Some(Eol::Crlf), AutoCrlf::True), Plan::OFF);
        assert_eq!(
            Plan::new(Text::Set, None, AutoCrlf::False, Eol::Crlf),
            Plan {
                mode: Mode::Text,
                crlf: true
            }
        );
    }

    #[test]
    fn checkin() {
        let text = Plan {
            mode: Mode::Text,
            crlf: false,
        };
        let auto = Plan {
            mode: Mode::Auto,
            crlf: true,
        };
        assert_eq!(
            to_git(b"a\r\nb\n", text, || unreachable!()),
            (Some(b"a\nb\n".to_vec()), Some(Irreversible::CrlfToLf))
        );
        assert_eq!(
            to_git(b"a\r\nb\n", auto, || false),
            (Some(b"a\nb\n".to_vec()), Some(Irreversible::LfToCrlf))
        );
        assert_eq!(to_git(b"a\r\nb\n", auto, || true), (None, None));
        assert_eq!(to_git(b"a\r\n\0", auto, || false), (None, None));
        assert_eq!(to_git(b"a\r\nb\r\n", auto, || false).1, None);
        assert_eq!(
            to_git(b"a\nb\n", auto, || false),
            (None, Some(Irreversible::LfToCrlf))
        );
    }

    #[test]
    fn checkout() {
        let text = Plan {
            mode: Mode::Text,
            crlf: true,
        };
        let auto = Plan {
            mode: Mode::Auto,
            crlf: true,
        };
        assert_eq!(
            to_worktree(b"a\r\nb\nc\rd\n", text),
            Some(b"a\r\nb\r\nc\rd\r\n".to_vec())
        );
        assert_eq!(to_worktree(b"a\r\nb\n", auto), None);
        assert_eq!(to_worktree(b"a\nb\n", auto), Some(b"a\r\nb\r\n".to_vec()));
        assert_eq!(to_worktree(b"a\0\nb\n", auto), None);
    }
}
