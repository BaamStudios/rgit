// SPDX-License-Identifier: MIT

//! User-visible messages of the patch parser and applier, verbatim as the
//! reference git prints them (see CLEANROOM.md, "User-visible messages";
//! taken from the output of `git apply`). Callers add the `error: `
//! prefix.

/// Input that is not a patch at all.
pub fn corrupt_patch(source: &str, line: usize) -> String {
    format!("corrupt patch at {source}:{line}")
}

pub fn fragment_without_header(source: &str, line: usize, text: &str) -> String {
    format!("patch fragment without header at {source}:{line}: {text}")
}

pub fn lacks_filename(source: &str, line: usize) -> String {
    format!("git diff header lacks filename information at {source}:{line}")
}

pub fn lacks_filename_removing(p: usize, source: &str, line: usize) -> String {
    let s = if p == 1 { "" } else { "s" };
    format!(
        "git diff header lacks filename information when removing {p} leading pathname component{s} at {source}:{line}"
    )
}

/// `text` keeps its line feed, as git prints it.
pub fn invalid_mode(source: &str, line: usize, text: &str) -> String {
    format!("invalid mode at {source}:{line}: {text}")
}

pub fn only_garbage(source: &str, line: usize) -> String {
    format!("patch with only garbage at {source}:{line}")
}

pub fn no_filename(source: &str, line: usize) -> String {
    format!("unable to find filename in patch at {source}:{line}")
}

pub fn expected_dev_null(source: &str, line: usize) -> String {
    format!("git apply: bad git-diff - expected /dev/null at {source}:{line}")
}

pub fn inconsistent_new(source: &str, line: usize) -> String {
    format!("git apply: bad git-diff - inconsistent new filename at {source}:{line}")
}

pub fn inconsistent_old(source: &str, line: usize) -> String {
    format!("git apply: bad git-diff - inconsistent old filename at {source}:{line}")
}

pub fn new_depends_on_old(name: &str) -> String {
    format!("new file {name} depends on old contents")
}

pub fn deleted_has_contents(name: &str) -> String {
    format!("deleted file {name} still has contents")
}

pub fn recount_unexpected(text: &str) -> String {
    format!("recount: unexpected line: {text}")
}

pub fn inconsistent_lines(first: usize, second: usize) -> String {
    format!("inconsistent header lines {first} and {second}")
}
