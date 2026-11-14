// SPDX-License-Identifier: MIT

//! Content conversion between the work tree and the repository
//! (gitattributes(5) "Checking-out and checking-in"): line endings
//! ([`eol`]: `text`, `eol`, legacy `crlf`, `core.autocrlf`, `core.eol`,
//! `core.safecrlf`), keyword expansion ([`ident`]) and
//! `working-tree-encoding` ([`encoding`]). Filter drivers (`filter`) are
//! not applied; [`Attrs::filter`] lets callers report them.
//!
//! On the way into the repository ([`to_git`]) the content is re-encoded
//! to UTF-8, line endings are normalized, then `$Id$` collapsed; checkout
//! ([`to_worktree`]) does the reverse in the opposite order.
//!
//! ```
//! use rgit_convert::{Attrs, Config, Conversion, Options, to_git, to_worktree};
//! use rgit_convert::eol::{AutoCrlf, Text};
//!
//! let cfg = Config { autocrlf: AutoCrlf::True, ..Config::default() };
//! let conv = Conversion::new(Attrs::default(), &cfg);
//! let blob = to_git("a.txt", b"one\r\ntwo\r\n".to_vec(), &conv, &cfg,
//!     Options::QUIET, || false, &mut |_| {}).unwrap();
//! assert_eq!(blob, b"one\ntwo\n");
//! let file = to_worktree("a.txt", blob, &conv, "", &mut |_| {});
//! assert_eq!(file, b"one\r\ntwo\r\n");
//! ```
//!
//! Behaviour follows Documentation/gitattributes.adoc,
//! Documentation/config/core.adoc and the `git` binary (see the module
//! documentation of each part).

pub mod encoding;
pub mod eol;
pub mod ident;
pub mod messages;
pub mod stats;

use rgit_attr::State;

pub use encoding::Encoding;
pub use eol::{AutoCrlf, Eol, Irreversible, Mode, Plan, SafeCrlf, Text};
pub use stats::Stats;

/// The attributes conversion reads, in the order of [`Attrs::from_states`].
pub const ATTRIBUTES: [&[u8]; 6] = [
    b"text",
    b"crlf",
    b"eol",
    b"ident",
    b"working-tree-encoding",
    b"filter",
];

/// The configuration that conversion reads.
#[derive(Debug, Clone)]
pub struct Config {
    /// `core.autocrlf`.
    pub autocrlf: AutoCrlf,
    /// `core.eol` (native when unset or invalid).
    pub eol: Eol,
    /// `core.safecrlf` (default warn).
    pub safecrlf: SafeCrlf,
    /// `core.checkRoundtripEncoding` (default `SHIFT-JIS`).
    pub roundtrip: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            autocrlf: AutoCrlf::False,
            eol: Eol::native(),
            safecrlf: SafeCrlf::Warn,
            roundtrip: vec!["SHIFT-JIS".to_string()],
        }
    }
}

/// The `working-tree-encoding` attribute.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum EncodingAttr {
    /// Unspecified, unset, empty or UTF-8: no re-encoding.
    #[default]
    None,
    /// Set without a value, which is an error.
    Invalid,
    /// The work tree encoding.
    Name(String),
}

/// The conversion attributes of one path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attrs {
    /// `text`, with `crlf` folded in.
    pub text: Text,
    /// `eol`.
    pub eol: Option<Eol>,
    /// `ident`.
    pub ident: bool,
    /// `working-tree-encoding`.
    pub encoding: EncodingAttr,
    /// `filter=<driver>` (not applied by this crate).
    pub filter: Option<Vec<u8>>,
}

impl Default for Attrs {
    fn default() -> Self {
        Attrs {
            text: Text::Unspecified,
            eol: None,
            ident: false,
            encoding: EncodingAttr::None,
            filter: None,
        }
    }
}

impl Attrs {
    /// From the states of the attributes in [`ATTRIBUTES`].
    #[must_use]
    pub fn from_states(states: [&State; 6]) -> Attrs {
        let [text, crlf, eol, ident, wte, filter] = states;
        let text = match text {
            State::Set => Text::Set,
            State::Unset => Text::Unset,
            State::Value(v) if v.as_slice() == b"auto" => Text::Auto,
            _ => match crlf {
                State::Set => Text::Set,
                State::Unset => Text::Unset,
                State::Value(v) if v.as_slice() == b"input" => Text::Input,
                _ => Text::Unspecified,
            },
        };
        let encoding = match wte {
            State::Set => EncodingAttr::Invalid,
            State::Value(v) if !v.is_empty() => {
                let name = String::from_utf8_lossy(v).into_owned();
                if Encoding::new(&name).is_utf8() {
                    EncodingAttr::None
                } else {
                    EncodingAttr::Name(name)
                }
            }
            _ => EncodingAttr::None,
        };
        Attrs {
            text,
            eol: eol.value().and_then(Eol::parse),
            ident: ident.is_set(),
            encoding,
            filter: filter.value().map(<[u8]>::to_vec),
        }
    }

    /// The `attr/` column of `git ls-files --eol`.
    #[must_use]
    pub fn describe(&self) -> &'static str {
        let eol = match self.text {
            Text::Input => Some(self.eol.unwrap_or(Eol::Lf)),
            _ => self.eol,
        };
        match (self.text, eol) {
            (Text::Unset, _) => "-text",
            (Text::Set | Text::Input | Text::Unspecified, Some(Eol::Lf)) => "text eol=lf",
            (Text::Set | Text::Input | Text::Unspecified, Some(Eol::Crlf)) => "text eol=crlf",
            (Text::Set | Text::Input, None) => "text",
            (Text::Auto, None) => "text=auto",
            (Text::Auto, Some(Eol::Lf)) => "text=auto eol=lf",
            (Text::Auto, Some(Eol::Crlf)) => "text=auto eol=crlf",
            (Text::Unspecified, None) => "",
        }
    }
}

/// Everything that is done to one path.
#[derive(Debug, Clone)]
pub struct Conversion {
    /// The attributes.
    pub attrs: Attrs,
    /// The line ending conversion.
    pub eol: Plan,
}

impl Conversion {
    /// The conversion of a path with `attrs` under `cfg`.
    #[must_use]
    pub fn new(attrs: Attrs, cfg: &Config) -> Conversion {
        let eol = Plan::new(attrs.text, attrs.eol, cfg.autocrlf, cfg.eol);
        Conversion { attrs, eol }
    }

    /// Whether content passes unchanged (filters aside).
    #[must_use]
    pub fn is_identity(&self) -> bool {
        self.eol.mode == Mode::Off && !self.attrs.ident && self.attrs.encoding == EncodingAttr::None
    }
}

/// How [`to_git`] treats problems.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// The `core.safecrlf` behaviour to apply.
    pub safecrlf: SafeCrlf,
    /// Encoding problems are fatal ([`to_git`] fails) instead of being
    /// reported as errors with the content left as it is.
    pub die: bool,
}

impl Options {
    /// No safety checks, problems reported as errors (comparing the work
    /// tree with the index).
    pub const QUIET: Options = Options {
        safecrlf: SafeCrlf::False,
        die: false,
    };

    /// Storing content (`add`, `hash-object -w`): `core.safecrlf` as
    /// configured, problems fatal.
    #[must_use]
    pub fn store(cfg: &Config) -> Options {
        Options {
            safecrlf: cfg.safecrlf,
            die: true,
        }
    }

    /// Inspecting content (`diff`): `core.safecrlf=true` only warns.
    #[must_use]
    pub fn inspect(cfg: &Config) -> Options {
        Options {
            safecrlf: match cfg.safecrlf {
                SafeCrlf::Fail => SafeCrlf::Warn,
                s => s,
            },
            die: false,
        }
    }
}

/// A message for the user (without its prefix).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    /// `warning: `.
    Warning(String),
    /// `hint: `.
    Hint(String),
    /// `error: `.
    Error(String),
}

/// A fatal conversion problem (`fatal: <message>`, after `hint`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fatal {
    /// A `hint: ` line to print first.
    pub hint: Option<String>,
    /// The message.
    pub message: String,
}

impl std::fmt::Display for Fatal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// The repository form of the work tree content `data` of `path`.
/// `index_has_crlf` answers whether the path's index blob is text with
/// CR LF ([`Stats::is_text_with_crlf`]); it is only asked when it
/// matters. Warnings and non-fatal errors go to `notice`.
///
/// # Errors
/// With [`Options::die`]: encoding problems; with `core.safecrlf=true`
/// semantics: an irreversible line ending conversion.
pub fn to_git(
    path: &str,
    mut data: Vec<u8>,
    conv: &Conversion,
    cfg: &Config,
    opts: Options,
    index_has_crlf: impl FnOnce() -> bool,
    notice: &mut dyn FnMut(Notice),
) -> Result<Vec<u8>, Fatal> {
    match &conv.attrs.encoding {
        EncodingAttr::None => {}
        EncodingAttr::Invalid => {
            let message = messages::ENCODING_NOT_BOOL.to_string();
            if opts.die {
                return Err(Fatal {
                    hint: None,
                    message,
                });
            }
            notice(Notice::Error(message));
        }
        EncodingAttr::Name(name) => {
            let enc = Encoding::new(name);
            let roundtrip = enc.in_list(&cfg.roundtrip);
            if roundtrip {
                rgit_trace::trace!(rgit_trace::TRACE, "{}", messages::checking_roundtrip(name));
            }
            match enc.decode(path, &data, roundtrip) {
                Ok(d) => data = d,
                Err(e) if opts.die => {
                    return Err(Fatal {
                        hint: e.hint,
                        message: e.message,
                    });
                }
                Err(e) => {
                    if let Some(h) = e.hint {
                        notice(Notice::Hint(h));
                    }
                    notice(Notice::Error(e.message));
                }
            }
        }
    }
    let (converted, irreversible) = eol::to_git(&data, conv.eol, index_has_crlf);
    if let (Some(kind), true) = (irreversible, opts.safecrlf != SafeCrlf::False) {
        let fail = opts.safecrlf == SafeCrlf::Fail;
        let message = match (kind, fail) {
            (Irreversible::CrlfToLf, false) => messages::crlf_to_lf_warning(path),
            (Irreversible::LfToCrlf, false) => messages::lf_to_crlf_warning(path),
            (Irreversible::CrlfToLf, true) => messages::crlf_to_lf_fatal(path),
            (Irreversible::LfToCrlf, true) => messages::lf_to_crlf_fatal(path),
        };
        if fail {
            return Err(Fatal {
                hint: None,
                message,
            });
        }
        notice(Notice::Warning(message));
    }
    if let Some(c) = converted {
        data = c;
    }
    if conv.attrs.ident {
        if let Some(c) = ident::collapse(&data) {
            data = c;
        }
    }
    Ok(data)
}

/// The work tree form of the blob `data` of `path`, whose object id is
/// `hex` (for `ident`). Content that cannot be re-encoded is reported to
/// `notice` and written in UTF-8.
pub fn to_worktree(
    path: &str,
    mut data: Vec<u8>,
    conv: &Conversion,
    hex: &str,
    notice: &mut dyn FnMut(Notice),
) -> Vec<u8> {
    if conv.attrs.ident {
        if let Some(c) = ident::expand(&data, hex) {
            data = c;
        }
    }
    if let Some(c) = eol::to_worktree(&data, conv.eol) {
        data = c;
    }
    if let EncodingAttr::Name(name) = &conv.attrs.encoding {
        match Encoding::new(name).encode(path, &data) {
            Ok(d) => data = d,
            Err(e) => notice(Notice::Error(e.message)),
        }
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attrs(text: &State, eol: &State) -> Attrs {
        let u = State::Unspecified;
        Attrs::from_states([text, &u, eol, &u, &u, &u])
    }

    #[test]
    fn describe() {
        let v = |s: &str| State::Value(s.as_bytes().to_vec());
        let u = State::Unspecified;
        assert_eq!(attrs(&State::Set, &u).describe(), "text");
        assert_eq!(attrs(&State::Unset, &v("crlf")).describe(), "-text");
        assert_eq!(attrs(&v("auto"), &v("lf")).describe(), "text=auto eol=lf");
        assert_eq!(attrs(&u, &v("crlf")).describe(), "text eol=crlf");
        assert_eq!(attrs(&v("foo"), &u).describe(), "");
        assert_eq!(attrs(&State::Set, &v("foo")).describe(), "text");
        let input = Attrs::from_states([&u, &v("input"), &u, &u, &u, &u]);
        assert_eq!(input.describe(), "text eol=lf");
        let auto_input = Attrs::from_states([&v("auto"), &v("input"), &u, &u, &u, &u]);
        assert_eq!(auto_input.describe(), "text=auto");
    }

    #[test]
    fn safecrlf() {
        let cfg = Config {
            autocrlf: AutoCrlf::True,
            safecrlf: SafeCrlf::Fail,
            ..Config::default()
        };
        let conv = Conversion::new(Attrs::default(), &cfg);
        let r = to_git(
            "f",
            b"a\nb\n".to_vec(),
            &conv,
            &cfg,
            Options::store(&cfg),
            || false,
            &mut |_| {},
        );
        assert_eq!(r.unwrap_err().message, "LF would be replaced by CRLF in f");
        let mut seen = Vec::new();
        let r = to_git(
            "f",
            b"a\nb\n".to_vec(),
            &conv,
            &cfg,
            Options::inspect(&cfg),
            || false,
            &mut |n| seen.push(n),
        );
        assert!(r.is_ok());
        assert_eq!(seen.len(), 1);
    }

    #[test]
    fn ident_and_eol() {
        let cfg = Config::default();
        let u = State::Unspecified;
        let a = Attrs::from_states([
            &State::Set,
            &u,
            &State::Value(b"crlf".to_vec()),
            &State::Set,
            &u,
            &u,
        ]);
        let conv = Conversion::new(a, &cfg);
        let wt = to_worktree("f", b"$Id$\nx\n".to_vec(), &conv, "abc", &mut |_| {});
        assert_eq!(wt, b"$Id: abc $\r\nx\r\n");
        let back = to_git("f", wt, &conv, &cfg, Options::QUIET, || false, &mut |_| {}).unwrap();
        assert_eq!(back, b"$Id$\nx\n");
    }
}
