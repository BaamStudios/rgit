// SPDX-License-Identifier: MIT
//! Unknown subcommands run external `git-<cmd>` programs from the exec path
//! and `$PATH` (#229), compared against the reference git.

use rgit_difftest::Fixture;

const FOO: &str = "#!/bin/sh\n\
echo \"path foo: $*\"\n\
test -n \"$GIT_DIR\" && echo \"GIT_DIR=$GIT_DIR\"\n\
test -n \"$GIT_CONFIG_PARAMETERS\" && echo \"cfg=$GIT_CONFIG_PARAMETERS\"\n\
case \"$1\" in\n\
term) kill -TERM $$ ;;\n\
pipe) kill -PIPE $$ ;;\n\
esac\n\
exit 7\n";

const EXEC_FOO: &str = "#!/bin/sh\necho \"exec-path foo: $*\"\nexit 3\n";

fn fixture() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .write_executable("bin/git-foo", FOO)
        .write_executable("exec/git-foo", EXEC_FOO)
        .write("bin/git-plain", "#!/bin/sh\necho not executable\n")
}

/// `PATH` with `<work>/bin` in front.
fn path_with_bin(fx: &Fixture) -> std::ffi::OsString {
    let mut dirs = vec![fx.work_dir().join("bin")];
    if let Some(p) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&p));
    }
    std::env::join_paths(dirs).unwrap()
}

fn compare(args: &[&str], exec_path_env: bool) {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let path = path_with_bin(&fx);
    let work = fx.work_dir().display().to_string();
    let argv: Vec<String> = args.iter().map(|a| a.replace("$W", &work)).collect();
    let mut c = fx.compare(&argv).env("PATH", &path);
    if exec_path_env {
        c = c.env("GIT_EXEC_PATH", format!("{work}/exec"));
    }
    c.run();
}

#[test]
fn runs_program_from_path_with_args_and_exit_code() {
    compare(&["foo", "a", "b c"], false);
}

#[test]
fn passes_global_options_in_environment() {
    compare(&["-c", "x.y=z", "--git-dir=/nowhere", "foo"], false);
}

#[test]
fn exec_path_option_comes_before_path() {
    compare(&["--exec-path=$W/exec", "foo", "1"], false);
}

#[test]
fn exec_path_environment_comes_before_path() {
    compare(&["foo", "1"], true);
}

#[test]
fn killed_by_signal() {
    compare(&["foo", "term"], false);
}

#[test]
fn killed_by_sigpipe_is_quiet() {
    compare(&["foo", "pipe"], false);
}

#[test]
fn unknown_command() {
    compare(&["no-such-command"], false);
}

#[test]
fn non_executable_file_is_not_a_command() {
    compare(&["plain"], false);
}
