// SPDX-License-Identifier: MIT
//! Messages of the interactive modes: hunk selection (`add -p` and the
//! patch modes of reset, checkout, restore, stash and commit), `add -e`
//! and the `add -i` menu (Documentation/git-add.adoc, "INTERACTIVE MODE"),
//! reproduced verbatim as the `git` binary prints them.

pub const NO_CHANGES: &str = "No changes.";
pub const ONLY_BINARY: &str = "Only binary files changed.";
pub const NO_OTHER_UNDECIDED: &str = "No other undecided hunk";
pub const NO_OTHER_HUNK: &str = "No other hunk";
pub const NO_GOTO: &str = "No other hunks to goto";
pub const NO_SEARCH: &str = "No other hunks to search";
pub const GOTO_PROMPT: &str = "go to which hunk? ";
pub const SEARCH_PROMPT: &str = "search for regex? ";
pub const NO_MATCH: &str = "No hunk matches the given pattern";
pub const CANNOT_SPLIT: &str = "Sorry, cannot split this hunk";
pub const CANNOT_EDIT: &str = "Sorry, cannot edit this hunk";
pub const EDIT_AGAIN: &str =
    "Your edited hunk does not apply. Edit again (saying \"no\" discards!) [y/n]? ";
pub const NOT_TO_INDEX: &str = "The selected hunks do not apply to the index!";
pub const APPLY_ANYWAY: &str = "Apply them to the worktree anyway? ";
pub const NOTHING_APPLIED: &str = "Nothing was applied.\n";
pub const COULD_NOT_PARSE_DIFF: &str = "could not parse diff";
pub const FILTER_MISMATCH: &str = "mismatched output from interactive.diffFilter";
pub const FILTER_HINT: &str =
    "Your filter must maintain a one-to-one correspondence\nbetween its input and output lines.";
pub const EDITING_PATCH_FAILED: &str = "editing patch failed";
pub const EMPTY_PATCH: &str = "empty patch. aborted";
pub const CONTEXT_NEGATIVE: &str = "diff.context cannot be negative";
pub const INTERHUNK_NEGATIVE: &str = "diff.interHunkContext cannot be negative";

pub const HELP_J: &str = "j - go to the next undecided hunk, roll over at the bottom";
pub const HELP_BIG_J: &str = "J - go to the next hunk, roll over at the bottom";
pub const HELP_K: &str = "k - go to the previous undecided hunk, roll over at the top";
pub const HELP_BIG_K: &str = "K - go to the previous hunk, roll over at the top";
pub const HELP_G: &str = "g - select a hunk to go to";
pub const HELP_SEARCH: &str = "/ - search for a hunk matching the given regex";
pub const HELP_S: &str = "s - split the current hunk into smaller hunks";
pub const HELP_E: &str = "e - manually edit the current hunk";
pub const HELP_P: &str = "p - print the current hunk";
pub const HELP_BIG_P: &str = "P - print the current hunk using the pager";
pub const HELP_HELP: &str = "? - print help";
pub const HELP_NEXT_FILE: &str = "> - go to the next file, roll over at the bottom";
pub const HELP_PREVIOUS_FILE: &str = "< - go to the previous file, roll over at the top";
pub const NO_NEXT_FILE: &str = "No next file";
pub const NO_PREVIOUS_FILE: &str = "No previous file";

pub fn hunks_summary(hunks: usize, used: usize, skipped: usize) -> String {
    format!("HUNKS SUMMARY - Hunks: {hunks}, USE: {used}, SKIP: {skipped}")
}

pub fn unknown_command(cmd: &str) -> String {
    format!("Unknown command '{cmd}' (use '?' for help)")
}
pub fn one_letter(got: &str) -> String {
    format!("Only one letter is expected, got '{got}'")
}
pub fn invalid_number(got: &str) -> String {
    format!("Invalid number: '{got}'")
}
pub fn only_hunks(n: usize) -> String {
    if n == 1 {
        format!("Sorry, only {n} hunk available.")
    } else {
        format!("Sorry, only {n} hunks available.")
    }
}
pub fn malformed_regex(pattern: &str, err: &str) -> String {
    format!("Malformed search regexp {pattern}: {err}")
}
pub fn child_failed(cmd: &str) -> String {
    format!("'{cmd}' failed")
}
pub fn split_into(n: usize) -> String {
    format!("Split into {n} hunks.")
}
pub fn could_not_write(path: &str, err: &str) -> String {
    format!("could not write '{path}': {err}")
}
pub fn requires(option: &str, needs: &str) -> String {
    format!("the option '{option}' requires '{needs}'")
}
pub fn cannot_be_negative(option: &str) -> String {
    format!("'{option}' cannot be negative")
}

/// What a patch mode does with a hunk, for its prompts and help.
#[derive(Clone, Copy)]
pub struct Verb {
    /// "Stage", "Discard", ...
    pub cap: &'static str,
    /// "stage", "discard", ...
    pub low: &'static str,
    /// "", " to index", " from worktree", ...
    pub target: &'static str,
    /// For the editing guide: "staging", "discarding", ...
    pub gerund: &'static str,
}

pub const STAGE: Verb = Verb {
    cap: "Stage",
    low: "stage",
    target: "",
    gerund: "staging",
};
pub const STASH: Verb = Verb {
    cap: "Stash",
    low: "stash",
    target: "",
    gerund: "stashing",
};
pub const UNSTAGE: Verb = Verb {
    cap: "Unstage",
    low: "unstage",
    target: "",
    gerund: "unstaging",
};
pub const APPLY_INDEX: Verb = Verb {
    cap: "Apply",
    low: "apply",
    target: " to index",
    gerund: "applying",
};
pub const DISCARD_WORKTREE: Verb = Verb {
    cap: "Discard",
    low: "discard",
    target: " from worktree",
    gerund: "discarding",
};
pub const DISCARD_BOTH: Verb = Verb {
    cap: "Discard",
    low: "discard",
    target: " from index and worktree",
    gerund: "discarding",
};
pub const APPLY_BOTH: Verb = Verb {
    cap: "Apply",
    low: "apply",
    target: " to index and worktree",
    gerund: "applying",
};
pub const APPLY_WORKTREE: Verb = Verb {
    cap: "Apply",
    low: "apply",
    target: " to worktree",
    gerund: "applying",
};

/// What a hunk is, for the prompt.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum What {
    ModeChange,
    Deletion,
    Addition,
    Hunk,
}

pub fn prompt(v: Verb, what: What) -> String {
    let what = match what {
        What::ModeChange => "mode change",
        What::Deletion => "deletion",
        What::Addition => "addition",
        What::Hunk => "this hunk",
    };
    format!("{} {what}{}", v.cap, v.target)
}

/// The first block of the help (`y`, `n`, `q`, `a`, `d`).
pub fn help_head(v: Verb) -> String {
    let (l, t) = (v.low, v.target);
    format!(
        "y - {l} this hunk{t}\n\
         n - do not {l} this hunk{t}\n\
         q - quit; do not {l} this hunk or any of the remaining ones\n\
         a - {l} this hunk and all later hunks in the file\n\
         d - do not {l} this hunk or any of the later hunks in the file\n"
    )
}

/// The lines of the hunk editing file around the hunk, with the comment
/// string `c`. `reverse`: the mode applies the diff in reverse.
pub fn edit_intro(c: &str) -> String {
    format!("{c} Manual hunk edit mode -- see bottom for a quick guide.\n")
}
pub fn edit_guide(c: &str, v: Verb, reverse: bool) -> String {
    let (minus, plus) = if reverse { ('+', '-') } else { ('-', '+') };
    let g = v.gerund;
    format!(
        "{c} ---\n\
         {c} To remove '{minus}' lines, make them ' ' lines (context).\n\
         {c} To remove '{plus}' lines, delete them.\n\
         {c} Lines starting with {c} will be removed.\n\
         {c} If the patch applies cleanly, the edited hunk will immediately be marked for {g}.\n\
         {c} If it does not apply cleanly, you will be given an opportunity to\n\
         {c} edit again.  If all lines of the hunk are removed, then the edit is\n\
         {c} aborted and the hunk is left unchanged.\n"
    )
}

pub const STATUS_HEADER: &str = "           staged     unstaged path";
pub const COMMANDS_HEADER: &str = "*** Commands ***";
pub const WHAT_NOW: &str = "What now";
pub const BYE: &str = "Bye.";
pub const UNCHANGED: &str = "unchanged";
pub const NOTHING: &str = "nothing";
pub const BINARY: &str = "binary";
pub const NO_UNTRACKED: &str = "No untracked files.";
pub const UPDATE_PROMPT: &str = "Update";
pub const REVERT_PROMPT: &str = "Revert";
pub const ADD_UNTRACKED_PROMPT: &str = "Add untracked";
pub const PATCH_PROMPT: &str = "Patch update";
pub const DIFF_PROMPT: &str = "Review diff";
/// The commands of the main menu, in menu order.
pub const COMMANDS: [&str; 8] = [
    "status",
    "update",
    "revert",
    "add untracked",
    "patch",
    "diff",
    "quit",
    "help",
];
pub const MENU_HELP: [&str; 6] = [
    "status        - show paths with changes",
    "update        - add working tree state to the staged set of changes",
    "revert        - revert staged set of changes back to the HEAD version",
    "patch         - pick hunks and update selectively",
    "diff          - view diff between HEAD and index",
    "add untracked - add contents of untracked files to the staged set of changes",
];
pub const SINGLE_HELP: [&str; 4] = [
    "Prompt help:",
    "1          - select a numbered item",
    "foo        - select item based on unique prefix",
    "           - (empty) select nothing",
];
pub const MULTI_HELP: [&str; 8] = [
    "Prompt help:",
    "1          - select a single item",
    "3-5        - select a range of items",
    "2-3,6-9    - select multiple ranges",
    "foo        - select item based on unique prefix",
    "-...       - unselect specified items",
    "*          - choose all items",
    "           - (empty) finish selecting",
];

pub fn huh(input: &str) -> String {
    format!("Huh ({input})?")
}
pub fn paths_done(verb: &str, n: usize) -> String {
    if n == 1 {
        format!("{verb} {n} path")
    } else {
        format!("{verb} {n} paths")
    }
}
pub fn untracked_now(path: &str) -> String {
    format!("note: {path} is untracked now.")
}
