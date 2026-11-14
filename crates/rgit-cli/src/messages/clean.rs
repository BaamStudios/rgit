// SPDX-License-Identifier: MIT
//! Messages, usage and option help of `git clean` (CLEANROOM.md:
//! user-visible strings are reproduced verbatim and collected here).

pub const USAGE: &[&str] =
    &["git clean [-d] [-f] [-i] [-n] [-q] [-e <pattern>] [-x | -X] [--] [<pathspec>...]"];

pub const HELP_QUIET: &str = "do not print names of files removed";
pub const HELP_DRY_RUN: &str = "dry run";
pub const HELP_FORCE: &str = "force";
pub const HELP_INTERACTIVE: &str = "interactive cleaning";
pub const HELP_D: &str = "remove whole directories";
pub const HELP_EXCLUDE: &str = "add <pattern> to ignore rules";
pub const HELP_X: &str = "remove ignored files, too";
pub const HELP_ONLY_IGNORED: &str = "remove only ignored files";

pub const REQUIRE_FORCE: &str = "clean.requireForce is true and -f not given: refusing to clean";
pub const X_AND_ONLY_IGNORED: &str = "options '-x' and '-X' cannot be used together";

// Interactive mode.

pub fn would_remove_header(n: usize) -> &'static str {
    if n == 1 {
        "Would remove the following item:"
    } else {
        "Would remove the following items:"
    }
}

pub const COMMANDS_HEADER: &str = "*** Commands ***";
pub const MENU: [&str; 6] = [
    "clean",
    "filter by pattern",
    "select by numbers",
    "ask each",
    "quit",
    "help",
];
pub const PROMPT_MENU: &str = "What now> ";
pub const PROMPT_FILTER: &str = "Input ignore patterns>> ";
pub const PROMPT_SELECT: &str = "Select items to delete>> ";
pub const BYE: &str = "Bye.";
pub const NO_MORE_FILES: &str = "No more files to clean, exiting.";

pub fn huh(input: &str) -> String {
    format!("Huh ({input})?")
}

pub fn no_items_matched(pattern: &str) -> String {
    format!("WARNING: Cannot find items matched by: {pattern}")
}

pub fn ask_remove(item: &str) -> String {
    format!("Remove {item} [y/N]? ")
}

pub const MENU_HELP: &str = "\
clean               - start cleaning
filter by pattern   - exclude items from deletion
select by numbers   - select items to be deleted by numbers
ask each            - confirm each deletion (like \"rm -i\")
quit                - stop cleaning
help                - this screen
?                   - help for prompt selection";

pub const PROMPT_HELP_SINGLE: &str = "\
Prompt help:
1          - select a numbered item
foo        - select item based on unique prefix
           - (empty) select nothing";

pub const PROMPT_HELP_MULTI: &str = "\
Prompt help:
1          - select a single item
3-5        - select a range of items
2-3,6-9    - select multiple ranges
foo        - select item based on unique prefix
-...       - unselect specified items
*          - choose all items
           - (empty) finish selecting";

pub fn removing(path: &str) -> String {
    format!("Removing {path}")
}

pub fn would_remove(path: &str) -> String {
    format!("Would remove {path}")
}

pub fn skipping_repo(path: &str) -> String {
    format!("Skipping repository {path}")
}

pub fn would_skip_repo(path: &str) -> String {
    format!("Would skip repository {path}")
}

pub fn failed_to_remove(path: &str, err: &str) -> String {
    format!("failed to remove {path}: {err}")
}

pub fn could_not_open_dir(path: &str, err: &str) -> String {
    format!("could not open directory '{path}': {err}")
}
