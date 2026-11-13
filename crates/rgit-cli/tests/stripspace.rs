// SPDX-License-Identifier: MIT
//! Differential tests of `stripspace` against the reference git.

use rgit_difftest::{Check, Fixture};

fn fx() -> Fixture {
    Fixture::new().rgit(env!("CARGO_BIN_EXE_rgit"))
}

fn same(fx: &mut Fixture, args: &[&str], input: &str) {
    fx.compare(args)
        .stdin(input.as_bytes().to_vec())
        .checks(Check::OUTPUT)
        .run();
}

const INPUTS: &[&str] = &[
    "",
    "\n\n  \n",
    "a",
    "\n a  \n\n\n\nb\t\n\n",
    "keep # this\n# comment\n\n\n#\tother\nend  ",
    "\tone\n\ntwo\n  \n",
    "a\r\nb\r\n",
];

#[test]
fn modes() {
    let mut fx = fx();
    for input in INPUTS {
        same(&mut fx, &["stripspace"], input);
        same(&mut fx, &["stripspace", "-s"], input);
        same(&mut fx, &["stripspace", "--strip-comments"], input);
        same(&mut fx, &["stripspace", "-c"], input);
        same(&mut fx, &["stripspace", "--comment-lines"], input);
    }
}

#[test]
fn comment_string() {
    let mut fx = fx();
    let input = "; one\n// two\n# three\nfour\n";
    for c in [
        "core.commentChar=;",
        "core.commentChar=//",
        "core.commentString=//",
        "core.commentChar=auto",
    ] {
        same(&mut fx, &["-c", c, "stripspace", "-s"], input);
        same(&mut fx, &["-c", c, "stripspace", "-c"], input);
    }
    // The last of the two variables wins.
    same(
        &mut fx,
        &[
            "-c",
            "core.commentString=xy",
            "-c",
            "core.commentChar=z",
            "stripspace",
            "-c",
        ],
        input,
    );
}

#[test]
fn errors() {
    let mut fx = fx();
    same(
        &mut fx,
        &["-c", "core.commentChar=", "stripspace", "-s"],
        "a\n",
    );
    same(
        &mut fx,
        &["-c", "core.commentString=", "stripspace", "-c"],
        "a\n",
    );
    same(
        &mut fx,
        &["-c", "core.commentChar=a\nb", "stripspace", "-s"],
        "a\n",
    );
    same(
        &mut fx,
        &["-c", "core.commentChar", "stripspace", "-c"],
        "a\n",
    );
    // Without -s or -c the configuration is not read.
    same(&mut fx, &["-c", "core.commentChar=", "stripspace"], "a\n");
    same(&mut fx, &["stripspace", "-s", "-c"], "a\n");
    same(&mut fx, &["stripspace", "x"], "a\n");
    same(&mut fx, &["stripspace", "-h"], "a\n");
}

#[test]
fn repository_config() {
    let mut fx = fx()
        .git(["init", "-q"])
        .git(["config", "core.commentChar", "="])
        .write("sub/.keep", "");
    fx.compare(["stripspace", "-c"])
        .in_dir("sub")
        .stdin(b"foo".to_vec())
        .checks(Check::OUTPUT)
        .run();
    let mut fx = fx.git(["config", "core.commentChar", ""]);
    fx.compare(["stripspace", "-s"])
        .stdin(b"foo\n".to_vec())
        .checks(Check::OUTPUT)
        .run();
}
