// SPDX-License-Identifier: MIT
//! Messages of the deprecated commands (`whatchanged`, `pack-redundant`),
//! which refuse to run without `--i-still-use-this`; taken verbatim from the
//! behaviour of the `git` binary.

/// The option that makes a deprecated command run anyway.
pub const STILL_USE_THIS: &str = "--i-still-use-this";

/// The notice before the refusal; `hint` is the advice block of commands
/// that have a replacement (with its trailing blank line).
pub fn removal_notice(cmd: &str, hint: &str) -> String {
    format!(
        "'git {cmd}' will be removed soon.\n\
         {hint}\
         If you need a replacement:\n\
         \n\
         - Read https://git-scm.com/docs/BreakingChanges.html.\n\
         \n\
         - Check what others on the mailing list suggest as a replacement:\n\
         \x20 https://lore.kernel.org/git/?q=git%20{cmd}\n\
         \n\
         - Send an email to <git@vger.kernel.org> asking for help, only if\n\
         \x20 suggestions by others do not work for you.\n\
         \n"
    )
}

/// The advice `whatchanged` prints before the rest of the notice.
pub const WHATCHANGED_HINT: &str = "\n\
hint: You can replace 'git whatchanged <opts>' with:\n\
hint:\tgit log <opts> --raw --no-merges\n\
hint: Or make an alias:\n\
hint:\tgit config set --global alias.whatchanged 'log --raw --no-merges'\n\
\n";

pub const REFUSING_TO_RUN: &str = "refusing to run without --i-still-use-this";
