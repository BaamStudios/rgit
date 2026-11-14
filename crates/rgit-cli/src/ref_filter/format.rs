// SPDX-License-Identifier: MIT
//! Parsing `--format` strings and sort keys into atoms
//! (Documentation/git-for-each-ref.adoc, "FIELD NAMES"; argument errors
//! checked against the `git` binary).

use rgit_date::DateMode;

use crate::messages::ref_filter as msg;

/// How a reference name is shortened (`:short`, `:lstrip=<n>`,
/// `:rstrip=<n>`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NameMod {
    Full,
    Short,
    Lstrip(i64),
    Rstrip(i64),
}

/// The variants of `%(upstream)` and `%(push)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RemoteMod {
    Name(NameMod),
    Track { brackets: bool },
    TrackShort,
    RemoteName,
    RemoteRef,
}

/// Length of an object name: full, the default abbreviation, or at least
/// `n` digits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Abbrev {
    Full,
    Default,
    Len(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Who {
    Author,
    Committer,
    Tagger,
    /// Committer of a commit, tagger of a tag.
    Creator,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PersonPart {
    /// `Name <email> <time> <tz>`.
    Whole,
    /// `%(author:<anything>)`: git prints nothing.
    Nothing,
    Name {
        mailmap: bool,
    },
    Email {
        mailmap: bool,
        trim: bool,
        localpart: bool,
    },
    /// The date, in the given format (`None`: the default format, sorting
    /// by the timestamp).
    Date(Option<DateMode>),
}

/// Options of `%(trailers)` (Documentation/pretty-formats.adoc).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TrailerOpts {
    /// Any option was given (otherwise the block is shown as written).
    pub(crate) formatted: bool,
    pub(crate) keys: Vec<Vec<u8>>,
    pub(crate) only: Option<bool>,
    pub(crate) unfold: bool,
    pub(crate) keyonly: bool,
    pub(crate) valueonly: bool,
    pub(crate) separator: Option<Vec<u8>>,
    pub(crate) kv_separator: Option<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Contents {
    All,
    Subject,
    Body,
    Signature,
    Size,
    Lines(usize),
    Trailers(TrailerOpts),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum IfCond {
    NotEmpty,
    Equals(Vec<u8>),
    NotEquals(Vec<u8>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Position {
    Left,
    Middle,
    Right,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Refname(NameMod),
    Symref(NameMod),
    Upstream(RemoteMod),
    Push(RemoteMod),
    Objectname(Abbrev),
    Objecttype,
    Objectsize {
        disk: bool,
    },
    Deltabase,
    Tree(Abbrev),
    Parent(Abbrev),
    Numparent,
    Object,
    Type,
    Tag,
    Person(Who, PersonPart),
    Subject {
        sanitize: bool,
    },
    Body,
    Contents(Contents),
    Raw {
        size: bool,
    },
    Head,
    Color(String),
    Align {
        width: usize,
        position: Position,
    },
    If(IfCond),
    Then,
    Else,
    End,
    AheadBehind(String),
    Worktreepath,
    Rest,
    /// Atoms rgit does not implement yet (`describe`, `is-base`,
    /// `signature`): an error when used.
    Unsupported(&'static str),
}

/// One `%(...)` placeholder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Atom {
    pub(crate) kind: Kind,
    /// `%(*...)`: of the object an annotated tag points to.
    pub(crate) deref: bool,
}

/// A piece of a format string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Piece {
    Lit(Vec<u8>),
    Atom(Atom),
}

/// Errors while parsing a format.
#[derive(Debug)]
pub(crate) enum FormatError {
    /// `malformed format string <rest>` (a usage error in git).
    Malformed(String),
    /// Other errors (`fatal:`).
    Fatal(String),
}

impl FormatError {
    pub(crate) fn message(self) -> String {
        match self {
            FormatError::Malformed(rest) => msg::malformed_format(&rest),
            FormatError::Fatal(m) => m,
        }
    }
}

/// Decodes `%%` and `%xx` in literal text.
pub(crate) fn unescape(text: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        if text[i] == b'%' {
            if text.get(i + 1) == Some(&b'%') {
                out.push(b'%');
                i += 2;
                continue;
            }
            let hex = text
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok())
                .filter(|h| h.bytes().all(|c| c.is_ascii_hexdigit()))
                .and_then(|h| u8::from_str_radix(h, 16).ok());
            if let Some(v) = hex {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(text[i]);
        i += 1;
    }
    out
}

/// Decodes the `git log` placeholders `%n`, `%%` and `%x<hh>` (the
/// trailer separators of Documentation/pretty-formats.adoc).
fn pretty_unescape(text: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        if text[i] == b'%' {
            match text.get(i + 1) {
                Some(b'n') => {
                    out.push(b'\n');
                    i += 2;
                    continue;
                }
                Some(b'%') => {
                    out.push(b'%');
                    i += 2;
                    continue;
                }
                Some(b'x') => {
                    let hex = text
                        .get(i + 2..i + 4)
                        .and_then(|h| std::str::from_utf8(h).ok())
                        .filter(|h| h.bytes().all(|c| c.is_ascii_hexdigit()))
                        .and_then(|h| u8::from_str_radix(h, 16).ok());
                    if let Some(v) = hex {
                        out.push(v);
                        i += 4;
                        continue;
                    }
                }
                _ => {}
            }
        }
        out.push(text[i]);
        i += 1;
    }
    out
}

/// Splits a format string into literal text and atoms.
pub(crate) fn parse_format(fmt: &str) -> Result<Vec<Piece>, FormatError> {
    let b = fmt.as_bytes();
    let mut pieces = Vec::new();
    let mut lit_start = 0;
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && b.get(i + 1) == Some(&b'%') {
            i += 2;
            continue;
        }
        if b[i] == b'%' && b.get(i + 1) == Some(&b'(') {
            let Some(end) = fmt[i..].find(')') else {
                return Err(FormatError::Malformed(fmt[i..].to_owned()));
            };
            if lit_start < i {
                pieces.push(Piece::Lit(unescape(&b[lit_start..i])));
            }
            pieces.push(Piece::Atom(parse_atom(&fmt[i + 2..i + end])?));
            i += end + 1;
            lit_start = i;
            continue;
        }
        i += 1;
    }
    if lit_start < b.len() {
        pieces.push(Piece::Lit(unescape(&b[lit_start..])));
    }
    Ok(pieces)
}

fn fatal(m: String) -> FormatError {
    FormatError::Fatal(m)
}

/// `:short`, `:lstrip=<n>`, `:strip=<n>`, `:rstrip=<n>`.
fn parse_name_mod(atom: &str, arg: Option<&str>) -> Result<NameMod, FormatError> {
    let Some(a) = arg.filter(|a| !a.is_empty()) else {
        return Ok(NameMod::Full);
    };
    if a == "short" {
        return Ok(NameMod::Short);
    }
    let int = |n: &str, shown: &str| {
        n.parse::<i64>()
            .map_err(|_| fatal(msg::integer_expected(shown)))
    };
    if let Some(n) = a.strip_prefix("lstrip=") {
        return Ok(NameMod::Lstrip(int(n, a)?));
    }
    if let Some(n) = a.strip_prefix("strip=") {
        return Ok(NameMod::Lstrip(int(n, &format!("lstrip={n}"))?));
    }
    if let Some(n) = a.strip_prefix("rstrip=") {
        return Ok(NameMod::Rstrip(int(n, a)?));
    }
    Err(fatal(msg::unrecognized_arg(atom, a)))
}

fn parse_remote_mod(atom: &str, arg: Option<&str>) -> Result<RemoteMod, FormatError> {
    let Some(a) = arg.filter(|a| !a.is_empty()) else {
        return Ok(RemoteMod::Name(NameMod::Full));
    };
    let mut result = None;
    let mut brackets = true;
    for item in a.split(',') {
        match item {
            "track" => result = Some(RemoteMod::Track { brackets: true }),
            "trackshort" => result = Some(RemoteMod::TrackShort),
            "nobracket" => brackets = false,
            "remotename" => result = Some(RemoteMod::RemoteName),
            "remoteref" => result = Some(RemoteMod::RemoteRef),
            _ if !a.contains(',') => return Ok(RemoteMod::Name(parse_name_mod(atom, arg)?)),
            _ => return Err(fatal(msg::unrecognized_arg(atom, a))),
        }
    }
    Ok(match result {
        Some(RemoteMod::Track { .. }) => RemoteMod::Track { brackets },
        Some(r) => r,
        None => RemoteMod::Name(NameMod::Full),
    })
}

/// `:short`, `:short=<n>` of `objectname`, `tree` and `parent`.
fn parse_abbrev(atom: &str, arg: Option<&str>) -> Result<Abbrev, FormatError> {
    match arg {
        None | Some("") => Ok(Abbrev::Full),
        Some("short") => Ok(Abbrev::Default),
        Some(a) => match a.strip_prefix("short=") {
            Some(n) => match n.parse::<i64>() {
                Ok(v) if v > 0 => Ok(Abbrev::Len(v as usize)),
                _ => Err(fatal(msg::positive_in(n, &format!("{atom}:{a}")))),
            },
            None => Err(fatal(msg::unrecognized_arg(atom, a))),
        },
    }
}

/// The comma-separated options of an email or name atom.
fn parse_person_opts(
    atom: &str,
    arg: &str,
    allowed: &[&str],
) -> Result<Vec<&'static str>, FormatError> {
    const ALL: [&str; 3] = ["mailmap", "trim", "localpart"];
    let mut found = Vec::new();
    if arg.is_empty() {
        return Ok(found);
    }
    let mut rest = arg;
    loop {
        let Some(opt) = ALL
            .iter()
            .find(|o| allowed.contains(o) && rest.starts_with(**o))
        else {
            return Err(fatal(msg::unrecognized_arg(atom, rest)));
        };
        found.push(*opt);
        rest = &rest[opt.len()..];
        if rest.is_empty() {
            return Ok(found);
        }
        match rest.strip_prefix(',') {
            Some(r) => rest = r,
            None => return Err(fatal(msg::unrecognized_arg(atom, rest))),
        }
    }
}

fn parse_person(name: &str, arg: Option<&str>) -> Result<Option<Kind>, FormatError> {
    let (who, part) = [
        ("author", Who::Author),
        ("committer", Who::Committer),
        ("tagger", Who::Tagger),
        ("creator", Who::Creator),
    ]
    .into_iter()
    .find_map(|(p, w)| name.strip_prefix(p).map(|rest| (w, rest)))
    .unwrap_or((Who::Author, "?"));
    let part = match (who, part) {
        (Who::Creator, "") => PersonPart::Whole,
        (Who::Creator, "date") => PersonPart::Date(parse_date_arg(arg)?),
        (Who::Creator, _) => return Ok(None),
        (_, "") if arg.is_some() => PersonPart::Nothing,
        (_, "") => PersonPart::Whole,
        (_, "name") => {
            let opts = parse_person_opts(name, arg.unwrap_or(""), &["mailmap"])
                .map_err(|_| fatal(msg::unrecognized_arg(name, arg.unwrap_or(""))))?;
            if opts.len() > 1 {
                return Err(fatal(msg::unrecognized_arg(name, arg.unwrap_or(""))));
            }
            PersonPart::Name {
                mailmap: !opts.is_empty(),
            }
        }
        (_, "email") => {
            let opts =
                parse_person_opts(name, arg.unwrap_or(""), &["mailmap", "trim", "localpart"])?;
            PersonPart::Email {
                mailmap: opts.contains(&"mailmap"),
                trim: opts.contains(&"trim"),
                localpart: opts.contains(&"localpart"),
            }
        }
        (_, "date") => PersonPart::Date(parse_date_arg(arg)?),
        _ => return Ok(None),
    };
    Ok(Some(Kind::Person(who, part)))
}

fn parse_date_arg(arg: Option<&str>) -> Result<Option<DateMode>, FormatError> {
    match arg {
        None => Ok(None),
        Some(a) => {
            rgit_date::parse_date_mode(a, std::io::IsTerminal::is_terminal(&std::io::stdout()))
                .map(Some)
                .map_err(|e| fatal(e.to_string()))
        }
    }
}

/// A boolean option value (`git config --type=bool`).
fn parse_bool(v: &str) -> Option<bool> {
    match v.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Some(true),
        "false" | "no" | "off" | "0" | "" => Some(false),
        _ => v.parse::<i64>().ok().map(|n| n != 0),
    }
}

/// The options of `%(trailers:...)`.
fn parse_trailers(arg: Option<&str>) -> Result<TrailerOpts, FormatError> {
    let mut o = TrailerOpts::default();
    let Some(arg) = arg else {
        return Ok(o);
    };
    o.formatted = true;
    let mut rest = arg;
    while !rest.is_empty() {
        let (item, next) = match rest.find(',') {
            Some(p) => (&rest[..p], &rest[p + 1..]),
            None => (rest, ""),
        };
        let (key, value) = match item.split_once('=') {
            Some((k, v)) => (k, Some(v)),
            None => (item, None),
        };
        let boolean = |v: Option<&str>| match v {
            None => Ok(true),
            Some(v) => parse_bool(v).ok_or_else(|| fatal(msg::unknown_trailers_arg(""))),
        };
        match key {
            "key" => match value {
                Some(v) => {
                    let v = v.strip_suffix(':').unwrap_or(v);
                    o.keys.push(v.as_bytes().to_vec());
                }
                None => return Err(fatal(msg::TRAILERS_KEY.to_owned())),
            },
            "only" => o.only = Some(boolean(value)?),
            "unfold" => o.unfold = boolean(value)?,
            "keyonly" => o.keyonly = boolean(value)?,
            "valueonly" => o.valueonly = boolean(value)?,
            "separator" if value.is_some() => {
                o.separator = Some(pretty_unescape(value.unwrap_or_default().as_bytes()));
            }
            "key_value_separator" if value.is_some() => {
                o.kv_separator = Some(pretty_unescape(value.unwrap_or_default().as_bytes()));
            }
            _ => return Err(fatal(msg::unknown_trailers_arg(rest))),
        }
        rest = next;
    }
    Ok(o)
}

fn parse_contents(arg: Option<&str>) -> Result<Contents, FormatError> {
    let Some(a) = arg.filter(|a| !a.is_empty()) else {
        return Ok(Contents::All);
    };
    Ok(match a {
        "subject" => Contents::Subject,
        "body" => Contents::Body,
        "signature" => Contents::Signature,
        "size" => Contents::Size,
        "trailers" => Contents::Trailers(parse_trailers(None)?),
        _ => {
            if let Some(t) = a.strip_prefix("trailers:") {
                Contents::Trailers(parse_trailers(Some(t))?)
            } else if let Some(n) = a.strip_prefix("lines=") {
                match n.parse::<i64>() {
                    Ok(v) if v >= 0 => Contents::Lines(v as usize),
                    _ => return Err(fatal(msg::positive_lines(n))),
                }
            } else {
                return Err(fatal(msg::unrecognized_arg("contents", a)));
            }
        }
    })
}

fn parse_align(arg: Option<&str>) -> Result<Kind, FormatError> {
    let Some(arg) = arg else {
        return Err(fatal(msg::ALIGN_EXPECTED.to_owned()));
    };
    let mut width = None;
    let mut position = Position::Left;
    let pos = |p: &str| match p {
        "left" => Some(Position::Left),
        "middle" => Some(Position::Middle),
        "right" => Some(Position::Right),
        _ => None,
    };
    for item in arg.split(',') {
        if let Some(p) = item.strip_prefix("position=") {
            position = pos(p).ok_or_else(|| fatal(msg::unrecognized_position(p)))?;
        } else if let Some(w) = item.strip_prefix("width=") {
            width = Some(
                w.parse::<usize>()
                    .map_err(|_| fatal(msg::unrecognized_width(w)))?,
            );
        } else if let Some(p) = pos(item) {
            position = p;
        } else if let Ok(w) = item.parse::<usize>() {
            width = Some(w);
        } else {
            return Err(fatal(msg::unrecognized_arg("align", item)));
        }
    }
    match width {
        Some(width) => Ok(Kind::Align { width, position }),
        None => Err(fatal(msg::ALIGN_WIDTH.to_owned())),
    }
}

fn parse_if(arg: Option<&str>) -> Result<Kind, FormatError> {
    Ok(Kind::If(match arg {
        None => IfCond::NotEmpty,
        Some(a) => {
            if let Some(s) = a.strip_prefix("equals=") {
                IfCond::Equals(s.as_bytes().to_vec())
            } else if let Some(s) = a.strip_prefix("notequals=") {
                IfCond::NotEquals(s.as_bytes().to_vec())
            } else {
                return Err(fatal(msg::unrecognized_arg("if", a)));
            }
        }
    }))
}

/// Parses the text between `%(` and `)`.
pub(crate) fn parse_atom(spec: &str) -> Result<Atom, FormatError> {
    let (deref, body) = match spec.strip_prefix('*') {
        Some(s) => (true, s),
        None => (false, spec),
    };
    let (name, arg) = match body.split_once(':') {
        Some((n, a)) => (n, Some(a)),
        None => (body, None),
    };
    if name.is_empty() {
        return Err(fatal(msg::malformed_field(spec)));
    }
    let no_args = |k: Kind| match arg {
        Some(_) => Err(fatal(msg::no_arguments(name))),
        None => Ok(k),
    };
    let kind = match name {
        "refname" => Kind::Refname(parse_name_mod(name, arg)?),
        "symref" => Kind::Symref(parse_name_mod(name, arg)?),
        "upstream" => Kind::Upstream(parse_remote_mod(name, arg)?),
        "push" => Kind::Push(parse_remote_mod(name, arg)?),
        "objectname" => Kind::Objectname(parse_abbrev(name, arg)?),
        "tree" => Kind::Tree(parse_abbrev(name, arg)?),
        "parent" => Kind::Parent(parse_abbrev(name, arg)?),
        "objecttype" => no_args(Kind::Objecttype)?,
        "objectsize" => match arg {
            None => Kind::Objectsize { disk: false },
            Some("disk") => Kind::Objectsize { disk: true },
            Some(a) => return Err(fatal(msg::unrecognized_arg(name, a))),
        },
        "deltabase" => no_args(Kind::Deltabase)?,
        "numparent" => Kind::Numparent,
        "object" => Kind::Object,
        "type" => Kind::Type,
        "tag" => Kind::Tag,
        "subject" => match arg {
            None => Kind::Subject { sanitize: false },
            Some("sanitize") => Kind::Subject { sanitize: true },
            Some(a) => return Err(fatal(msg::unrecognized_arg(name, a))),
        },
        "body" => no_args(Kind::Body)?,
        "contents" => Kind::Contents(parse_contents(arg)?),
        "trailers" => Kind::Contents(Contents::Trailers(parse_trailers(arg)?)),
        "raw" => match arg {
            None => Kind::Raw { size: false },
            Some("size") => Kind::Raw { size: true },
            Some(a) => return Err(fatal(msg::unrecognized_arg(name, a))),
        },
        "HEAD" => no_args(Kind::Head)?,
        "color" => match arg {
            None | Some("") => return Err(fatal(msg::COLOR_EXPECTED.to_owned())),
            Some(c) => match rgit_config::color::parse_color(c) {
                Some(seq) => Kind::Color(seq),
                None => {
                    eprintln!("error: {}", crate::messages::config::invalid_color(c));
                    return Err(fatal(msg::unrecognized_color(c)));
                }
            },
        },
        "align" => parse_align(arg)?,
        "if" => parse_if(arg)?,
        "then" => no_args(Kind::Then)?,
        "else" => no_args(Kind::Else)?,
        "end" => no_args(Kind::End)?,
        "ahead-behind" => match arg {
            Some(a) => Kind::AheadBehind(a.to_owned()),
            None => return Err(fatal(msg::expected_committish(name))),
        },
        "is-base" => match arg {
            Some(_) => Kind::Unsupported("is-base"),
            None => return Err(fatal(msg::expected_committish(name))),
        },
        "worktreepath" => Kind::Worktreepath,
        "rest" => no_args(Kind::Rest)?,
        "describe" => Kind::Unsupported("describe"),
        "signature" => Kind::Unsupported("signature"),
        _ => match parse_person(name, arg)? {
            Some(k) => k,
            None => return Err(fatal(msg::unknown_field(body))),
        },
    };
    Ok(Atom { kind, deref })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(spec: &str) -> String {
        match parse_atom(spec) {
            Ok(a) => panic!("{spec} parsed as {a:?}"),
            Err(e) => e.message(),
        }
    }

    #[test]
    fn person_options() {
        assert_eq!(
            err("authoremail:mailmaptrim"),
            "unrecognized %(authoremail) argument: trim"
        );
        assert_eq!(
            err("committeremail:localpart,,,trim"),
            "unrecognized %(committeremail) argument: ,,trim"
        );
        assert_eq!(
            err("authoremail:trim,"),
            "unrecognized %(authoremail) argument: "
        );
        assert_eq!(
            err("taggeremail:mailmap,trim, foo "),
            "unrecognized %(taggeremail) argument:  foo "
        );
        assert_eq!(
            parse_atom("authoremail:trim,mailmap").unwrap().kind,
            Kind::Person(
                Who::Author,
                PersonPart::Email {
                    mailmap: true,
                    trim: true,
                    localpart: false
                }
            )
        );
        assert_eq!(err("creatorname"), "unknown field name: creatorname");
    }

    #[test]
    fn remote_options() {
        assert_eq!(
            parse_atom("upstream:nobracket,track").unwrap().kind,
            Kind::Upstream(RemoteMod::Track { brackets: false })
        );
        assert_eq!(
            err("upstream:short,track"),
            "unrecognized %(upstream) argument: short,track"
        );
        assert_eq!(
            err("refname:strip=1x"),
            "Integer value expected refname:lstrip=1x"
        );
    }

    #[test]
    fn align_and_literals() {
        assert_eq!(
            parse_atom("align:width=42,position=middle").unwrap().kind,
            Kind::Align {
                width: 42,
                position: Position::Middle
            }
        );
        assert_eq!(err("align:left,right"), msg::ALIGN_WIDTH);
        assert_eq!(unescape(b"a%%b%41%zz%"), b"a%bA%zz%");
        assert!(matches!(
            parse_format("x%(refname"),
            Err(FormatError::Malformed(r)) if r == "%(refname"
        ));
    }
}
