// SPDX-License-Identifier: MIT
//! Author and committer identities.
//!
//! Sources, from `git-commit(1)` "COMMIT INFORMATION" and
//! `git-config(1)` `user.*`, with details checked against the `git`
//! binary:
//!
//! * `GIT_{AUTHOR,COMMITTER}_{NAME,EMAIL}` win when set, even when empty.
//! * Then `{author,committer}.{name,email}` when not empty.
//! * Then `user.name` / `user.email` (an empty value counts: it is not
//!   guessed).
//! * Then the guess: the full name from the password database; `$EMAIL`
//!   (when not empty) or `<login>@<mail domain>` (see [`crate::system`]).
//!   Setting any `*.name` (or `*.email`) variable turns guessing off for
//!   the name (email); what is not set is empty then.
//!
//! A *strict* identity (for new commits, `git var GIT_AUTHOR_IDENT`)
//! refuses a made-up email address, an empty name and — with
//! `user.useConfigOnly` — anything guessed. A *lenient* one (reflogs,
//! `git var -l`) takes what it gets and uses the login name for an empty
//! name.
//!
//! `<`, `>` and newlines are removed from names and addresses, and
//! control characters, spaces and `"',:;<>\` are trimmed from both ends.
//! A trailing `.` is kept.

use std::fmt;

use bstr::{BString, ByteSlice, ByteVec};
use rgit_config::{ConfigSet, Environment};
use rgit_date::DateContext;

use crate::config::{self, Problem};
use crate::messages as msg;
use crate::system::UserInfo;

/// Whose identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    Author,
    Committer,
}

impl Role {
    /// `GIT_AUTHOR_NAME` / `GIT_COMMITTER_NAME`.
    pub fn name_var(self) -> &'static str {
        match self {
            Role::Author => "GIT_AUTHOR_NAME",
            Role::Committer => "GIT_COMMITTER_NAME",
        }
    }

    /// `GIT_AUTHOR_EMAIL` / `GIT_COMMITTER_EMAIL`.
    pub fn email_var(self) -> &'static str {
        match self {
            Role::Author => "GIT_AUTHOR_EMAIL",
            Role::Committer => "GIT_COMMITTER_EMAIL",
        }
    }

    /// `GIT_AUTHOR_DATE` / `GIT_COMMITTER_DATE`.
    pub fn date_var(self) -> &'static str {
        match self {
            Role::Author => "GIT_AUTHOR_DATE",
            Role::Committer => "GIT_COMMITTER_DATE",
        }
    }

    /// First line of the "Please tell me who you are" advice.
    pub fn unknown(self) -> &'static str {
        match self {
            Role::Author => msg::AUTHOR_UNKNOWN,
            Role::Committer => msg::COMMITTER_UNKNOWN,
        }
    }
}

/// Strict or lenient resolution (see the module documentation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strictness {
    Strict,
    Lenient,
}

/// The identity-related configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IdentConfig {
    pub user_name: Option<BString>,
    pub user_email: Option<BString>,
    pub author_name: Option<BString>,
    pub author_email: Option<BString>,
    pub committer_name: Option<BString>,
    pub committer_email: Option<BString>,
    /// `user.useConfigOnly`.
    pub use_config_only: bool,
}

impl IdentConfig {
    /// Read the variables; a value-less entry or a bad boolean is an error
    /// (git reports them even when the identity is not needed).
    pub fn from_config(set: &ConfigSet) -> Result<Self, Problem> {
        Ok(IdentConfig {
            user_name: config::string_all(set, "user.name")?,
            user_email: config::string_all(set, "user.email")?,
            author_name: config::string_all(set, "author.name")?,
            author_email: config::string_all(set, "author.email")?,
            committer_name: config::string_all(set, "committer.name")?,
            committer_email: config::string_all(set, "committer.email")?,
            use_config_only: config::bool(set, "user.useConfigOnly")?.unwrap_or(false),
        })
    }

    /// Some `*.name` variable is set: the name is not guessed.
    pub fn name_given(&self) -> bool {
        self.user_name.is_some() || self.author_name.is_some() || self.committer_name.is_some()
    }

    /// Some `*.email` variable is set: the address is not guessed.
    pub fn email_given(&self) -> bool {
        self.user_email.is_some() || self.author_email.is_some() || self.committer_email.is_some()
    }

    fn role_name(&self, role: Role) -> Option<&BString> {
        match role {
            Role::Author => self.author_name.as_ref(),
            Role::Committer => self.committer_name.as_ref(),
        }
        .filter(|v| !v.is_empty())
    }

    fn role_email(&self, role: Role) -> Option<&BString> {
        match role {
            Role::Author => self.author_email.as_ref(),
            Role::Committer => self.committer_email.as_ref(),
        }
        .filter(|v| !v.is_empty())
    }
}

/// A resolved identity with its time stamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ident {
    pub name: BString,
    pub email: BString,
    /// Seconds since the epoch.
    pub time: u64,
    /// Offset in git's `±hhmm` integer form (`+0200` is `200`).
    pub tz: i32,
}

impl Ident {
    /// `Name <email>`.
    pub fn name_email(&self) -> BString {
        let mut out = self.name.clone();
        out.push_str(" <");
        out.push_str(&self.email);
        out.push_str(">");
        out
    }

    /// `Name <email> <time> <±hhmm>`, as in commit headers.
    pub fn to_bstring(&self) -> BString {
        let mut out = self.name_email();
        out.push_str(" ");
        out.push_str(rgit_date::format_raw(self.time, self.tz));
        out
    }
}

impl fmt::Display for Ident {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_bstring())
    }
}

/// Why no identity could be determined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentError {
    /// Print the "Please tell me who you are" advice for this role first.
    pub advice: Option<Role>,
    /// The `fatal:` message.
    pub message: String,
}

impl IdentError {
    fn plain(message: String) -> Self {
        IdentError {
            advice: None,
            message,
        }
    }

    fn advised(role: Role, message: String) -> Self {
        IdentError {
            advice: Some(role),
            message,
        }
    }

    /// Everything git prints to stderr: the advice (if any) and
    /// `fatal: <message>`.
    pub fn stderr_text(&self) -> String {
        let mut out = String::new();
        if let Some(role) = self.advice {
            out.push_str(role.unknown());
            out.push('\n');
            out.push_str(msg::PLEASE_TELL_ME);
            out.push('\n');
        }
        out.push_str("fatal: ");
        out.push_str(&self.message);
        out.push('\n');
        out
    }
}

impl fmt::Display for IdentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for IdentError {}

/// Bytes removed everywhere in names and addresses.
fn is_removed(b: u8) -> bool {
    matches!(b, b'<' | b'>' | b'\n')
}

/// Bytes trimmed from both ends.
fn is_crud(b: u8) -> bool {
    b <= b' ' || matches!(b, b'"' | b'\'' | b',' | b':' | b';' | b'<' | b'>' | b'\\')
}

/// Clean a name or address the way git writes it into an identity.
pub fn sanitize(raw: &[u8]) -> BString {
    let start = raw.iter().position(|&b| !is_crud(b));
    let Some(start) = start else {
        return BString::default();
    };
    let end = raw.iter().rposition(|&b| !is_crud(b)).unwrap_or(start) + 1;
    raw[start..end]
        .iter()
        .copied()
        .filter(|&b| !is_removed(b))
        .collect::<Vec<u8>>()
        .into()
}

/// Resolves identities from configuration, environment and system facts.
pub struct Resolver<'a> {
    config: &'a IdentConfig,
    env: &'a dyn Environment,
    user_cache: std::cell::OnceCell<UserInfo>,
    now: Option<(u64, i32)>,
}

impl<'a> Resolver<'a> {
    /// A resolver that asks the system about the user when it has to guess.
    pub fn new(config: &'a IdentConfig, env: &'a dyn Environment) -> Self {
        Resolver {
            config,
            env,
            user_cache: std::cell::OnceCell::new(),
            now: None,
        }
    }

    /// Use `user` instead of the system's facts (for tests).
    pub fn with_user(mut self, user: UserInfo) -> Self {
        self.user_cache = std::cell::OnceCell::from(user);
        self
    }

    /// Use a fixed "now" (seconds, `±hhmm`) instead of the clock.
    pub fn with_now(mut self, time: u64, tz: i32) -> Self {
        self.now = Some((time, tz));
        self
    }

    fn user(&self) -> &UserInfo {
        self.user_cache.get_or_init(UserInfo::from_system)
    }

    /// The name used when none is given: `user.name`, or the full name
    /// from the password database unless some `*.name` is configured.
    pub fn default_name(&self) -> BString {
        match &self.config.user_name {
            Some(n) => n.clone(),
            None if self.config.name_given() => BString::default(),
            None => self.user().full_name.clone(),
        }
    }

    /// The address used when none is given and whether it was made up
    /// (bogus): `user.email`, `$EMAIL` or `<login>@<mail domain>`.
    pub fn default_email(&self) -> Result<(BString, bool), IdentError> {
        if let Some(e) = &self.config.user_email {
            return Ok((e.clone(), false));
        }
        if self.config.email_given() {
            return Ok((BString::default(), false));
        }
        if let Some(e) = config::env_bytes(self.env, "EMAIL").filter(|e| !e.is_empty()) {
            return Ok((e.trim_ascii().into(), false));
        }
        let user = self.user();
        let Some(login) = &user.login else {
            return Err(IdentError::plain(msg::passwd_lookup(msg::NO_SUCH_USER)));
        };
        let mut email = login.clone();
        email.push(b'@');
        email.push_str(&user.domain);
        Ok((email, user.domain_bogus))
    }

    /// The cleaned name and address of `role`, or why there are none.
    pub fn name_email(
        &self,
        role: Role,
        strictness: Strictness,
    ) -> Result<(BString, BString), IdentError> {
        let strict = strictness == Strictness::Strict;
        let name_arg = config::env_bytes(self.env, role.name_var())
            .or_else(|| self.config.role_name(role).cloned());
        let email_arg = config::env_bytes(self.env, role.email_var())
            .or_else(|| self.config.role_email(role).cloned());

        let email = match email_arg {
            Some(e) => e,
            None => {
                if strict && self.config.use_config_only && !self.config.email_given() {
                    return Err(IdentError::advised(role, msg::NO_EMAIL_GIVEN.into()));
                }
                let (email, bogus) = self.default_email()?;
                if strict && bogus {
                    return Err(IdentError::advised(
                        role,
                        msg::unable_to_auto_detect_email(&email.to_str_lossy()),
                    ));
                }
                email
            }
        };

        let (mut name, guessed) = match name_arg {
            Some(n) => (n, false),
            None => {
                if strict && self.config.use_config_only && !self.config.name_given() {
                    return Err(IdentError::advised(role, msg::NO_NAME_GIVEN.into()));
                }
                (self.default_name(), true)
            }
        };
        if name.is_empty() {
            if strict {
                // git quotes the address as given, before cleaning it.
                let message = msg::empty_ident_name(&email.to_str_lossy());
                return Err(if guessed {
                    IdentError::advised(role, message)
                } else {
                    IdentError::plain(message)
                });
            }
            name = match &self.user().login {
                Some(l) => l.clone(),
                None => return Err(IdentError::plain(msg::passwd_lookup(msg::NO_SUCH_USER))),
            };
        }
        let clean = sanitize(&name);
        if clean.is_empty() && strict {
            return Err(IdentError::plain(msg::only_disallowed(
                &name.to_str_lossy(),
            )));
        }
        Ok((clean, sanitize(&email)))
    }

    /// The time stamp of `role`: `GIT_*_DATE` when set and not empty,
    /// else now.
    pub fn date(&self, role: Role) -> Result<(u64, i32), IdentError> {
        if let Some(d) = config::env_string(self.env, role.date_var()).filter(|d| !d.is_empty()) {
            let ctx = DateContext::from_env();
            return rgit_date::parse_date(&d, &ctx)
                .ok_or_else(|| IdentError::plain(msg::invalid_date(&d)));
        }
        Ok(self.now.unwrap_or_else(now))
    }

    /// The full identity of `role`.
    pub fn ident(&self, role: Role, strictness: Strictness) -> Result<Ident, IdentError> {
        let (name, email) = self.name_email(role, strictness)?;
        let (time, tz) = self.date(role)?;
        Ok(Ident {
            name,
            email,
            time,
            tz,
        })
    }
}

/// The current time and local offset.
pub fn now() -> (u64, i32) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let zone = jiff::tz::TimeZone::system();
    let offset = jiff::Timestamp::from_second(secs as i64)
        .map(|ts| zone.to_offset(ts).seconds())
        .unwrap_or(0);
    (secs, rgit_date::secs_to_tz(offset as i64))
}

/// Convenience: the strict identity of `role` from a configuration set and
/// the process environment.
pub fn ident_from(
    set: &ConfigSet,
    env: &dyn Environment,
    role: Role,
    strictness: Strictness,
) -> Result<Ident, IdentResolveError> {
    let cfg = IdentConfig::from_config(set).map_err(IdentResolveError::Config)?;
    Resolver::new(&cfg, env)
        .ident(role, strictness)
        .map_err(IdentResolveError::Ident)
}

/// Error of [`ident_from`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentResolveError {
    Config(Problem),
    Ident(IdentError),
}

impl IdentResolveError {
    /// Print the error to stderr the way git does.
    pub fn print(&self) {
        match self {
            IdentResolveError::Config(p) => p.print(),
            IdentResolveError::Ident(e) => eprint!("{}", e.stderr_text()),
        }
    }
}

impl fmt::Display for IdentResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IdentResolveError::Config(p) => p.fmt(f),
            IdentResolveError::Ident(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for IdentResolveError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizing() {
        assert_eq!(sanitize(b" .Pat <Doe> Jr.. "), ".Pat Doe Jr..");
        assert_eq!(sanitize(b" <x@y>. "), "x@y.");
        assert_eq!(sanitize(b" ,;<>"), "");
        assert_eq!(sanitize(b"a\nb"), "ab");
        assert_eq!(sanitize(b"\ta\x01b\t"), "a\x01b");
        assert_eq!(sanitize("éaé".as_bytes()), "éaé");
        assert_eq!(sanitize(b"\x7fa\x7f"), "\x7fa\x7f");
        assert_eq!(sanitize(b"Pat Doe Jr."), "Pat Doe Jr.");
    }
}
