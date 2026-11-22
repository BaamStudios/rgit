// SPDX-License-Identifier: MIT
//! Messages and usage of `git bisect` (CLEANROOM.md: user-visible strings
//! are reproduced verbatim and collected here).

/// `git bisect -h` (stdout) and the usage after errors (stderr).
pub const USAGE: &str = "\
usage: git bisect start [--term-(bad|new)=<term-new> --term-(good|old)=<term-old>]
                        [--no-checkout] [--first-parent] [--reset-when-found[=<where>]] [<bad> [<good>...]] [--] [<pathspec>...]
   or: git bisect (bad|new|<term-new>) [<rev>]
   or: git bisect (good|old|<term-old>) [<rev>...]
   or: git bisect terms [--term-(good|old) | --term-(bad|new)]
   or: git bisect skip [(<rev>|<range>)...]
   or: git bisect next
   or: git bisect reset [<commit>]
   or: git bisect (visualize|view)
   or: git bisect replay <logfile>
   or: git bisect log
   or: git bisect run [--reset-when-found[=<where>]] <cmd> [<arg>...]
   or: git bisect help

";

pub const NEED_COMMAND: &str = "need a command";
pub const NOT_BISECTING: &str = "We are not bisecting.";
pub const NEED_START: &str = "You need to start by \"git bisect start\"\n";
pub const RESET_ARGS: &str = "'git bisect reset' requires either no argument or a commit";
pub const TERMS_ARGS: &str = "'git bisect terms' requires 0 or 1 argument";
pub const NEXT_ARGS: &str = "'git bisect next' requires 0 arguments";
pub const NO_LOGFILE: &str = "no logfile given";
pub const RUN_NO_COMMAND: &str = "'git bisect run' failed: no command provided.";
pub const NO_TERMS: &str = "no terms defined";
pub const TWO_TERMS: &str = "please use two different terms";
pub const MERGE_BASE_MUST_BE_TESTED: &str = "Bisecting: a merge base must be tested";
pub const NO_TESTABLE: &str =
    "No testable commit found.\nMaybe you started with bad path arguments?";
pub const ONLY_SKIPPED_LOG: &str = "# only skipped commits left to test";
pub const CANNOT_CONTINUE: &str = "bisect run cannot continue any more";
pub const CANNOT_BISECT_MORE: &str = "We cannot bisect more!";
pub const RESET_WHEN_FOUND_NO_CHECKOUT: &str =
    "options '--reset-when-found' and '--no-checkout' cannot be used together";

pub const ONLY_BAD: &str = "bisecting only with a bad commit";

pub fn need_both(new: &str, old: &str) -> String {
    format!(
        "You need to give me at least one {new} and {old} revision.\nYou can use \"git bisect {new}\" and \"git bisect {old}\" for that."
    )
}

pub fn cannot_check_out_original(target: &str) -> String {
    format!("could not check out original HEAD '{target}'. Try 'git bisect reset <commit>'.")
}

pub fn checking_out_failed(start: &str) -> String {
    format!("checking out '{start}' failed. Try 'git bisect start <valid-branch>'.")
}

pub const BAD_HEAD: &str = "bad HEAD - I need a HEAD";

pub fn could_not_write(path: &str, err: &str) -> String {
    format!("could not write '{path}': {err}")
}

pub fn unknown_command(cmd: &str) -> String {
    format!("unknown command: '{cmd}'")
}

pub fn unrecognized_option(opt: &str) -> String {
    format!("unrecognized option: '{opt}'")
}

pub fn not_a_valid_revision(arg: &str) -> String {
    format!("'{arg}' does not appear to be a valid revision")
}

pub fn not_a_valid_commit(arg: &str) -> String {
    format!("'{arg}' is not a valid commit")
}

pub fn bad_rev_input(arg: &str) -> String {
    format!("Bad rev input: {arg}")
}

pub fn only_one_argument(term: &str) -> String {
    format!("'git bisect {term}' can take only one argument.")
}

pub fn invalid_command_terms(new: &str, old: &str) -> String {
    format!("Invalid command: you're currently in a {new}/{old} bisect")
}

pub fn not_a_valid_term(term: &str) -> String {
    format!("'{term}' is not a valid term")
}

pub fn builtin_as_term(term: &str) -> String {
    format!("can't use the builtin command '{term}' as a term")
}

pub fn change_meaning(term: &str) -> String {
    format!("can't change the meaning of the term '{term}'")
}

pub fn invalid_reset_when_found(value: &str) -> String {
    format!("invalid value for '--reset-when-found': '{value}'")
}

pub fn current_terms(old: &str, new: &str) -> String {
    format!("Your current terms are '{old}' for the old state\nand '{new}' for the new state.")
}

pub fn invalid_terms_argument(arg: &str) -> String {
    format!(
        "invalid argument {arg} for 'git bisect terms'.\nSupported options are: --term-good|--term-old and --term-bad|--term-new."
    )
}

pub fn waiting_both(old: &str, new: &str) -> String {
    format!("status: waiting for both '{old}' and '{new}' commits")
}

pub fn waiting_old(old: &str, new: &str) -> String {
    format!("status: waiting for '{old}' commit(s), '{new}' commit known")
}

pub fn waiting_new(new: &str, count: usize, old: &str) -> String {
    let commits = if count == 1 { "commit" } else { "commits" };
    format!("status: waiting for '{new}' commit, {count} '{old}' {commits} known")
}

pub fn bisecting(nr: i64, steps: usize) -> String {
    let revs = if nr == 1 { "revision" } else { "revisions" };
    let step = if steps == 1 { "step" } else { "steps" };
    format!("Bisecting: {nr} {revs} left to test after this (roughly {steps} {step})")
}

pub fn first_bad(hex: &str, term: &str) -> String {
    format!("{hex} is the first '{term}' commit")
}
/// The diffstat of the first new commit failed (`fatal: `).
pub fn unable_to_start_show(hex: &str) -> String {
    format!("unable to start 'show' for object '{hex}'")
}

pub fn first_bad_log(term: &str, hex: &str, subject: &str) -> String {
    format!("# first '{term}' commit: [{hex}] {subject}")
}

pub fn possible_first_bad_log(term: &str, hex: &str, subject: &str) -> String {
    format!("# possible first '{term}' commit: [{hex}] {subject}")
}

pub fn only_skipped(term: &str) -> String {
    format!(
        "There are only 'skip'ped commits left to test.\nThe first '{term}' commit could be any of:"
    )
}

pub fn both_good_and_bad(hex: &str, old: &str, new: &str) -> String {
    format!("{hex} was both '{old}' and '{new}'")
}

pub fn not_ancestors(old: &str, new: &str) -> String {
    format!(
        "Some '{old}' revs are not ancestors of the '{new}' rev.\ngit bisect cannot work properly in this case.\nMaybe you mistook '{old}' and '{new}' revs?"
    )
}

/// The merge base `mb` was found bad (or new): the change happened on the
/// other side.
pub fn merge_base_is_bad(mb: &str, new: &str, old: &str, goods: &str) -> String {
    if new == "bad" {
        format!(
            "The merge base {mb} is bad.\nThis means the bug has been fixed between {mb} and [{goods}]."
        )
    } else if new == "new" {
        format!("The merge base {mb} is new.\nThe property has changed between {mb} and [{goods}].")
    } else {
        format!(
            "The merge base {mb} is '{new}'.\nThis means the first '{old}' commit is between {mb} and [{goods}]."
        )
    }
}

pub fn skipped_merge_base(bad: &str, goods: &str, new: &str, mb: &str) -> String {
    format!(
        "the merge base between {bad} and [{goods}] must be skipped.\nSo we cannot be sure the first '{new}' commit is between {mb} and {bad}.\nWe continue anyway."
    )
}

pub fn running(cmd: &str) -> String {
    format!("running {cmd}")
}

pub fn found_first(term: &str) -> String {
    format!("bisect found first '{term}' commit")
}

pub fn run_bad_exit(code: i32, cmd: &str) -> String {
    format!("bisect run failed: exit code {code} from {cmd} is < 0 or >= 128")
}

pub fn unable_to_verify(cmd: &str, old: &str) -> String {
    format!("unable to verify {cmd} on '{old}' revision")
}

pub fn bogus_exit_code(code: i32, old: &str) -> String {
    format!("bogus exit code {code} for '{old}' revision")
}

pub fn state_failed(state: &str, code: i32) -> String {
    format!("bisect run failed: 'git bisect {state}' exited with error code {code}")
}

pub fn cannot_read_replay(file: &str) -> String {
    format!("cannot read file '{file}' for replaying")
}

pub fn what_are_you_talking_about(word: &str) -> String {
    format!("'{word}'?? what are you talking about?")
}

pub fn path_limiting_not_supported() -> String {
    "rgit bisect: pathspecs are not supported yet".to_string()
}

/// The status banner (`git status`).
pub fn status_banner(start: &str) -> String {
    format!("You are currently bisecting, started from branch '{start}'.")
}

pub fn status_banner_detached() -> &'static str {
    "You are currently bisecting."
}

pub const STATUS_RESET_HINT: &str =
    "  (use \"git bisect reset\" to get back to the original branch)";
