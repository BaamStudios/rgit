// SPDX-License-Identifier: MIT
//! Advice: hints the user can switch off (`Documentation/config/advice.adoc`).
//!
//! Observed from git 2.56 (`t0018-advice.sh` and the commands that advise):
//!
//! - Advice `X` is shown unless `advice.X` is false, `GIT_ADVICE` is false
//!   or `git --no-advice` was given. `--no-advice` wins over `GIT_ADVICE`.
//! - While `advice.X` is not set at all, the advice ends with the line
//!   `Disable this message with "git config set advice.X false"`
//!   ([`footer`]); `advice.X=true` shows the advice without that line.
//! - Every `advice.*` value of a known name must be a boolean: an invalid
//!   one ends the command with `fatal: bad boolean config value` as soon as
//!   the configuration is read. An invalid `GIT_ADVICE` is reported only
//!   when an advice is about to be shown
//!   (`fatal: bad boolean environment value '<v>' for 'GIT_ADVICE'`).
//! - The `hint: ` lines are colored with `color.advice.hint` (default
//!   yellow) when `color.advice` (falling back to `color.ui`, default
//!   `auto`) says so. Some commands (`init`, `push`) do not read `color.ui`
//!   and look at `color.advice` only ([`Advisor::without_color_ui`]).
//! - `advice.pushUpdateRejected=false` also switches off `pushNonFFCurrent`,
//!   `pushNonFFMatching`, `pushAlreadyExists`, `pushFetchFirst`,
//!   `pushNeedsForce` and `pushRefNeedsUpdate`.
//!
//! Some advice is not printed as `hint: ` lines but as plain text (for
//! example `rmHints`, `objectNameWarning`); those callers only ask
//! [`Advisor::enabled`].

use std::io::{IsTerminal, Write};

use rgit_config::color::{ColorBool, parse_color, parse_colorbool};
use rgit_config::value::parse_bool;
use rgit_config::{ConfigSet, Environment};

macro_rules! advice_table {
    ($($variant:ident => $name:literal,)*) => {
        /// An advice name (`advice.<name>`).
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Advice {
            $($variant,)*
        }

        impl Advice {
            /// Every advice, in the order of `advice.adoc`.
            pub const ALL: &'static [Advice] = &[$(Advice::$variant,)*];

            /// The name as documented (camelCase), e.g. `addIgnoredFile`.
            #[must_use]
            pub const fn name(self) -> &'static str {
                match self {
                    $(Advice::$variant => $name,)*
                }
            }
        }
    };
}

advice_table! {
    AddEmbeddedRepo => "addEmbeddedRepo",
    AddEmptyPathspec => "addEmptyPathspec",
    AddIgnoredFile => "addIgnoredFile",
    AmWorkDir => "amWorkDir",
    AmbiguousFetchRefspec => "ambiguousFetchRefspec",
    CheckoutAmbiguousRemoteBranchName => "checkoutAmbiguousRemoteBranchName",
    CommitBeforeMerge => "commitBeforeMerge",
    DefaultBranchName => "defaultBranchName",
    DetachedHead => "detachedHead",
    Diverging => "diverging",
    FetchRemoteHeadWarn => "fetchRemoteHEADWarn",
    FetchShowForcedUpdates => "fetchShowForcedUpdates",
    ForceDeleteBranch => "forceDeleteBranch",
    IgnoredHook => "ignoredHook",
    ImplicitIdentity => "implicitIdentity",
    MergeConflict => "mergeConflict",
    NestedTag => "nestedTag",
    ObjectNameWarning => "objectNameWarning",
    PushAlreadyExists => "pushAlreadyExists",
    PushFetchFirst => "pushFetchFirst",
    PushNeedsForce => "pushNeedsForce",
    PushNonFfCurrent => "pushNonFFCurrent",
    PushNonFfMatching => "pushNonFFMatching",
    PushRefNeedsUpdate => "pushRefNeedsUpdate",
    PushRepoLooksLikeRef => "pushRepoLooksLikeRef",
    PushUnqualifiedRefname => "pushUnqualifiedRefname",
    PushUpdateRejected => "pushUpdateRejected",
    RebaseTodoError => "rebaseTodoError",
    RefSyntax => "refSyntax",
    ResetNoRefresh => "resetNoRefresh",
    ResolveConflict => "resolveConflict",
    RmHints => "rmHints",
    SequencerInUse => "sequencerInUse",
    SetUpstreamFailure => "setUpstreamFailure",
    SkippedCherryPicks => "skippedCherryPicks",
    SparseIndexExpanded => "sparseIndexExpanded",
    StatusAheadBehind => "statusAheadBehind",
    StatusHints => "statusHints",
    StatusUoption => "statusUoption",
    SubmoduleAlternateErrorStrategyDie => "submoduleAlternateErrorStrategyDie",
    SubmoduleMergeConflict => "submoduleMergeConflict",
    SubmodulesNotUpdated => "submodulesNotUpdated",
    SuggestDetachingHead => "suggestDetachingHead",
    UpdateSparsePath => "updateSparsePath",
    WaitingForEditor => "waitingForEditor",
    WorktreeAddOrphan => "worktreeAddOrphan",
}

impl Advice {
    /// The configuration key, `advice.<name>`.
    #[must_use]
    pub fn key(self) -> String {
        format!("advice.{}", self.name())
    }

    /// The advice whose `false` also switches this one off.
    #[must_use]
    pub const fn umbrella(self) -> Option<Advice> {
        match self {
            Advice::PushNonFfCurrent
            | Advice::PushNonFfMatching
            | Advice::PushAlreadyExists
            | Advice::PushFetchFirst
            | Advice::PushNeedsForce
            | Advice::PushRefNeedsUpdate => Some(Advice::PushUpdateRejected),
            _ => None,
        }
    }

    /// Look up a name (case-insensitive, like configuration keys).
    #[must_use]
    pub fn from_name(name: &str) -> Option<Advice> {
        Advice::ALL
            .iter()
            .copied()
            .find(|a| a.name().eq_ignore_ascii_case(name))
    }
}

/// The last line of an advice whose `advice.<name>` is not configured.
#[must_use]
pub fn footer(advice: Advice) -> String {
    format!(
        "Disable this message with \"git config set advice.{} false\"",
        advice.name()
    )
}

/// The default `color.advice.hint`: yellow.
pub const DEFAULT_HINT_COLOR: &str = "\x1b[33m";

/// Decides whether advice is shown and renders it.
#[derive(Debug, Clone, Default)]
pub struct Advisor {
    /// `--no-advice`, or advice switched off by the caller (`--quiet`).
    off: bool,
    /// The raw value of `GIT_ADVICE`, checked when advice is about to be
    /// shown.
    env: Option<String>,
    /// Configured `advice.*` values.
    values: Vec<(Advice, bool)>,
    /// Color of the `hint: ` lines, `None` for plain output.
    color: Option<String>,
    /// The color when `color.ui` is not consulted.
    color_without_ui: Option<String>,
}

impl Advisor {
    /// Read the advice settings. `no_advice` is `git --no-advice`. Fails
    /// with the `fatal:` message for an `advice.*` value that is not a
    /// boolean.
    pub fn new(config: &ConfigSet, env: &dyn Environment, no_advice: bool) -> Result<Self, String> {
        let mut values = Vec::new();
        for &a in Advice::ALL {
            if let Some(v) = config.bool(a.key()).map_err(|e| e.to_string())? {
                values.push((a, v));
            }
        }
        let color = hint_color(config, env, true);
        let color_without_ui = hint_color(config, env, false);
        Ok(Advisor {
            off: no_advice,
            env: env
                .var("GIT_ADVICE")
                .map(|v| v.to_string_lossy().into_owned()),
            values,
            color,
            color_without_ui,
        })
    }

    /// An advisor that never advises.
    #[must_use]
    pub fn silent() -> Self {
        Advisor {
            off: true,
            ..Advisor::default()
        }
    }

    /// Switch all advice off (e.g. for `--quiet`) when `off` is true.
    #[must_use]
    pub fn silenced(mut self, off: bool) -> Self {
        self.off |= off;
        self
    }

    /// Ignore `color.ui` for the color of the `hint: ` lines (for commands
    /// that do not read it).
    #[must_use]
    pub fn without_color_ui(mut self) -> Self {
        self.color = self.color_without_ui.clone();
        self
    }

    /// Override the color of the `hint: ` lines (`None`: plain).
    #[must_use]
    pub fn with_color(mut self, color: Option<String>) -> Self {
        self.color.clone_from(&color);
        self.color_without_ui = color;
        self
    }

    /// The configured value of `advice.<name>`, if set.
    fn value(&self, a: Advice) -> Option<bool> {
        self.values.iter().find(|(x, _)| *x == a).map(|(_, v)| *v)
    }

    /// Whether `advice` is shown. Fails with the `fatal:` message when
    /// `GIT_ADVICE` is not a boolean.
    pub fn enabled(&self, advice: Advice) -> Result<bool, String> {
        if self.off {
            return Ok(false);
        }
        if let Some(v) = &self.env {
            match parse_bool(Some(v.as_bytes())) {
                Some(false) => return Ok(false),
                Some(true) => {}
                None => return Err(crate::bad_bool_env("GIT_ADVICE", v)),
            }
        }
        if advice.umbrella().and_then(|u| self.value(u)) == Some(false) {
            return Ok(false);
        }
        Ok(self.value(advice).unwrap_or(true))
    }

    /// The text written to stderr for `advice` with message `text` (one
    /// `hint: ` line per line of `text`, plus the [`footer`] while
    /// `advice.<name>` is unset), or `None` when the advice is off.
    pub fn render(&self, advice: Advice, text: &str) -> Result<Option<String>, String> {
        if !self.enabled(advice)? {
            return Ok(None);
        }
        let full = if self.value(advice).is_none() {
            format!("{text}\n{}", footer(advice))
        } else {
            text.to_owned()
        };
        Ok(Some(crate::render_hint(&full, self.color.as_deref())))
    }

    /// Write `advice` to stderr if it is enabled (see [`Advisor::render`]).
    pub fn advise(&self, advice: Advice, text: &str) -> Result<(), String> {
        if let Some(s) = self.render(advice, text)? {
            write_stderr(&s);
        }
        Ok(())
    }

    /// Like [`Advisor::advise`], but never with the "Disable this message"
    /// line (git's `push` rejection hints are shown this way).
    pub fn advise_without_footer(&self, advice: Advice, text: &str) -> Result<(), String> {
        if self.enabled(advice)? {
            write_stderr(&self.hint(text));
        }
        Ok(())
    }

    /// `text` as `hint: ` lines in the advice color, whatever the
    /// settings say.
    #[must_use]
    pub fn hint(&self, text: &str) -> String {
        crate::render_hint(text, self.color.as_deref())
    }

    /// Fails with the `fatal:` message when `GIT_ADVICE` is set but not a
    /// boolean, for commands that look at the advice settings up front.
    pub fn check_env(&self) -> Result<(), String> {
        if self.off {
            return Ok(());
        }
        match &self.env {
            Some(v) if parse_bool(Some(v.as_bytes())).is_none() => {
                Err(crate::bad_bool_env("GIT_ADVICE", v))
            }
            _ => Ok(()),
        }
    }
}

fn write_stderr(s: &str) {
    let mut err = std::io::stderr().lock();
    let _ = err.write_all(s.as_bytes());
}

/// The color of `hint: ` lines from `color.advice` (or `color.ui` when
/// `use_ui`) and `color.advice.hint`; `auto` colors when stderr is a
/// terminal and `TERM` is not `dumb`.
fn hint_color(config: &ConfigSet, env: &dyn Environment, use_ui: bool) -> Option<String> {
    let colorbool = config
        .get("color.advice")
        .or_else(|| use_ui.then(|| config.get("color.ui")).flatten())
        .and_then(|e| parse_colorbool(e.value.as_ref().map(|v| v.as_slice())))
        .unwrap_or(ColorBool::Auto);
    let use_color = match colorbool {
        ColorBool::Always => true,
        ColorBool::Never => false,
        ColorBool::Auto => {
            std::io::stderr().is_terminal() && env.var("TERM").is_none_or(|t| t != "dumb")
        }
    };
    use_color.then(|| {
        config
            .string("color.advice.hint")
            .ok()
            .flatten()
            .and_then(|v| parse_color(&String::from_utf8_lossy(v)))
            .unwrap_or_else(|| DEFAULT_HINT_COLOR.to_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgit_config::{MapEnv, Origin, Scope, parse_bytes};

    fn config(text: &str) -> ConfigSet {
        parse_bytes(text.as_bytes(), Origin::CommandLine, Scope::Command).expect("parse")
    }

    fn advisor(text: &str, env: &[(&str, &str)], no_advice: bool) -> Result<Advisor, String> {
        let env = MapEnv::new(env.iter().copied());
        Advisor::new(&config(text), &env, no_advice).map(|a| a.with_color(None))
    }

    #[test]
    fn names_round_trip() {
        for &a in Advice::ALL {
            assert_eq!(Advice::from_name(&a.name().to_ascii_lowercase()), Some(a));
        }
        assert_eq!(Advice::NestedTag.key(), "advice.nestedTag");
    }

    #[test]
    fn footer_only_while_unset() {
        let a = advisor("", &[], false).unwrap();
        assert_eq!(
            a.render(Advice::NestedTag, "piece").unwrap().unwrap(),
            "hint: piece\nhint: Disable this message with \"git config set advice.nestedTag false\"\n"
        );
        let a = advisor("[advice]\nnestedTag = true\n", &[], false).unwrap();
        assert_eq!(
            a.render(Advice::NestedTag, "piece").unwrap().unwrap(),
            "hint: piece\n"
        );
        // A trailing newline in the text leaves an empty hint line before the footer.
        let a = advisor("", &[], false).unwrap();
        assert!(
            a.render(Advice::RefSyntax, "x\n")
                .unwrap()
                .unwrap()
                .starts_with("hint: x\nhint:\nhint: Disable")
        );
    }

    #[test]
    fn switches() {
        let off = |cfg: &str, env: &[(&str, &str)], no: bool| {
            !advisor(cfg, env, no)
                .unwrap()
                .enabled(Advice::AddIgnoredFile)
                .unwrap()
        };
        assert!(!off("", &[], false));
        assert!(off("[advice]\naddIgnoredFile = false\n", &[], false));
        assert!(!off("[advice]\naddIgnoredFile\n", &[], false));
        assert!(off("", &[("GIT_ADVICE", "0")], false));
        assert!(off("", &[("GIT_ADVICE", "")], false));
        assert!(!off("", &[("GIT_ADVICE", "yes")], false));
        assert!(off("", &[], true));
        // --no-advice wins over an invalid GIT_ADVICE.
        assert!(off("", &[("GIT_ADVICE", "bogus")], true));
        assert_eq!(
            advisor("", &[("GIT_ADVICE", "bogus")], false)
                .unwrap()
                .enabled(Advice::AddIgnoredFile),
            Err("bad boolean environment value 'bogus' for 'GIT_ADVICE'".to_owned())
        );
        // The umbrella switch.
        let a = advisor("[advice]\npushUpdateRejected = false\n", &[], false).unwrap();
        assert!(!a.enabled(Advice::PushFetchFirst).unwrap());
        assert!(a.enabled(Advice::PushRepoLooksLikeRef).unwrap());
    }

    #[test]
    fn invalid_config_value_is_fatal() {
        let e = advisor("[advice]\nnestedTag = bogus\n", &[], false).unwrap_err();
        assert!(
            e.starts_with("bad boolean config value 'bogus' for 'advice.nestedtag'"),
            "{e}"
        );
    }
}
