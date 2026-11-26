// SPDX-License-Identifier: MIT
//! Notes ref names.

/// The notes ref used when nothing else is configured.
pub const DEFAULT: &str = "refs/notes/commits";

/// The prefix of all notes refs.
pub const PREFIX: &str = "refs/notes/";

/// The full name of the notes ref `name` as `--ref` and `--notes=` take
/// it: names starting with `refs/notes/` are kept, `notes/<x>` becomes
/// `refs/notes/<x>`, anything else gets `refs/notes/` in front (so
/// `refs/heads/x` becomes `refs/notes/refs/heads/x`).
pub fn expand(name: &str) -> String {
    if name.starts_with(PREFIX) {
        name.to_owned()
    } else if name.starts_with("notes/") {
        format!("refs/{name}")
    } else {
        format!("{PREFIX}{name}")
    }
}

/// The notes ref to read and write: `GIT_NOTES_REF` (`env`, even when
/// empty), else `core.notesRef` (`config`), else [`DEFAULT`].
pub fn default_ref(env: Option<&str>, config: Option<&str>) -> String {
    env.or(config).unwrap_or(DEFAULT).to_owned()
}

/// The name `git log` shows for the notes of `refname`: `None` for
/// [`DEFAULT`] (`Notes:`), else the name without `refs/notes/`
/// (`Notes (<name>):`).
pub fn display_name(refname: &str) -> Option<&str> {
    if refname == DEFAULT {
        return None;
    }
    Some(refname.strip_prefix(PREFIX).unwrap_or(refname))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_like_git() {
        assert_eq!(expand("foo"), "refs/notes/foo");
        assert_eq!(expand("notes/foo"), "refs/notes/foo");
        assert_eq!(expand("refs/notes/foo"), "refs/notes/foo");
        assert_eq!(expand("refs/heads/main"), "refs/notes/refs/heads/main");
        assert_eq!(display_name("refs/notes/commits"), None);
        assert_eq!(display_name("refs/notes/other"), Some("other"));
    }
}
