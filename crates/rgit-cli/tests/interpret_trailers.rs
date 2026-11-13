// SPDX-License-Identifier: MIT
//! Differential tests of `interpret-trailers` against the reference git:
//! trailer block detection, placement rules, configured keys and commands,
//! options and errors.

use rgit_difftest::{Check, Fixture};

fn fx() -> Fixture {
    Fixture::new().rgit(env!("CARGO_BIN_EXE_rgit"))
}

fn it(fx: &mut Fixture, config: &[&str], args: &[&str], input: &str) {
    let mut full: Vec<&str> = Vec::new();
    for c in config {
        full.push("-c");
        full.push(c);
    }
    full.push("interpret-trailers");
    full.extend_from_slice(args);
    fx.compare(&full)
        .stdin(input.as_bytes().to_vec())
        .checks(Check::OUTPUT)
        .run();
}

const MESSAGES: &[&str] = &[
    "",
    "\n",
    "  \n\t\n",
    "subject\n",
    "area: change",
    "area: change\n\nReviewed-by: x",
    "subject\nA: b\n\nB: c\n",
    "\n\nfoo\n\nA: b\n",
    "subject\n\nbody\n\n\n",
    "subject\n\nbody\n  \n",
    "subject\n\nA: b\n  \n",
    "subject\n\nA: b\n# c\n\n# d\n\n",
    "subject\n\nA: b\nConflicts:\n\tfoo\n\tbar\n",
    "subject\n\nA: b\n\nConflicts:\n\tfoo\nnot\n",
    "subject\n\nA: b\n---\nB: c\n",
    "subject\n\nA: b\n--- x\nB: c\n",
    "subject\n\nA: b\n------ dashes\n",
    "subject\n---\nA: b\n",
    "subject\n\n# c\nA: b\n",
    "subject\n\n#A: b\n",
    "# only comment\n",
    "subject\n\nA: b\n# ------------------------ >8 ------------------------\nB: c\n",
    "subject\n\nA: b\n#  ------------------------ >8 ------------------------\nB: c\n",
    "subject\n\nSigned-off-by: b\nfoo\nbar\nbaz\n",
    "subject\n\nSigned-off-by: b\nfoo\nbar\nbaz\nqux\n",
    "subject\n\nSigned-off-by:b\nfoo\nbar\nbaz\n",
    "subject\n\nfoo\n(cherry picked from commit x)\nbar\n",
    "subject\n\nA: b\n  \tcont\nfoo\n",
    "subject\n\n  cont\nA: b\n",
    "subject\n\nA: b\nfoo\n  bar\nSigned-off-by: x\n",
    "subject\n\nSigned-off-by: a\nB: c\n# c\n  bar\n",
    "subject\n\nfoo: a  \n  b\t\n\tc\n",
    "subject\n\nA_b: c\n",
    "subject\n\n:c\n",
    "subject\n\nA-1 \t: c\n",
    "subject\n\nA b: c\n",
    "subject\n\nhttps://x.org\nhttps ://y.org\nB: //c\n",
    "subject\n\nA: b\r\nC: d\r\n",
    "subject\n\nAcked-by: \nReviewed-by: x\nAcked-by: y\n",
];

#[test]
fn block_detection() {
    let mut fx = fx();
    for m in MESSAGES {
        it(&mut fx, &[], &[], m);
        it(&mut fx, &[], &["--trailer", "x=y"], m);
        it(&mut fx, &[], &["--parse"], m);
        it(&mut fx, &[], &["--only-trailers", "--trim-empty"], m);
        it(
            &mut fx,
            &[],
            &["--no-divider", "--trailer", "Acked-by: z"],
            m,
        );
    }
}

#[test]
fn comments() {
    let mut fx = fx();
    for c in [
        "core.commentChar=;",
        "core.commentString=//",
        "core.commentChar=auto",
    ] {
        it(
            &mut fx,
            &[c],
            &["--trailer", "x=y"],
            "subj\n\n; c\n// d\n# e\nA: b\n;x\n",
        );
        it(
            &mut fx,
            &[c],
            &["--trailer", "x=y"],
            "subj\n\nA: b\n; ------------------------ >8 ------------------------\nB: c\n",
        );
    }
    it(&mut fx, &["core.commentChar="], &[], "a\n");
}

const EXISTING: &str = "subj\n\nfoo: a\nbar: x\nfoo: b\n";

#[test]
fn placement() {
    let mut fx = fx();
    for w in ["end", "start", "after", "before"] {
        for e in [
            "addIfDifferentNeighbor",
            "addIfDifferent",
            "add",
            "replace",
            "doNothing",
        ] {
            for t in ["foo: a", "foo: b", "FOO: c", "foobar: d", "fo: a", "baz: e"] {
                it(
                    &mut fx,
                    &[],
                    &["--where", w, "--if-exists", e, "--trailer", t],
                    EXISTING,
                );
            }
        }
        it(
            &mut fx,
            &[],
            &[
                "--where",
                w,
                "--if-missing",
                "doNothing",
                "--trailer",
                "baz: e",
                "--no-if-missing",
                "--trailer",
                "q: r",
            ],
            EXISTING,
        );
    }
    // Options apply to the --trailer options after them.
    it(
        &mut fx,
        &[],
        &[
            "--trailer",
            "foo: a",
            "--where",
            "start",
            "--trailer",
            "foo: a",
            "--no-where",
            "--trailer",
            "foo: c",
        ],
        EXISTING,
    );
    it(
        &mut fx,
        &[],
        &["--trailer", "a: b", "--no-trailer", "--trailer", "c=d"],
        EXISTING,
    );
}

#[test]
fn configuration() {
    let mut fx = fx();
    let conf: &[&str] = &[
        "trailer.separators=:=#",
        "trailer.ack.key=Acked-by= ",
        "trailer.ack.where=after",
        "trailer.bug.key=Bug #",
        "trailer.bug.where=before",
        "trailer.review.key=Reviewed-by:",
        "trailer.ifexists=addIfDifferent",
    ];
    let msg = "my subject\n\nmy body\n\nFixes: \nAcked-by: \nReviewed-by: \nSigned-off-by: \n";
    it(
        &mut fx,
        conf,
        &[
            "--trailer",
            "ack: Peff",
            "--trailer",
            "review:",
            "--trailer",
            "bug: 42",
            "--trailer",
            "Ack = Junio",
            "--trailer",
            "acked-b: x",
        ],
        msg,
    );
    it(&mut fx, conf, &["--trim-empty", "--trailer", "bug: 1"], msg);
    it(&mut fx, conf, &["--parse"], msg);
    it(
        &mut fx,
        &["trailer.separators=%"],
        &[
            "--trailer",
            "bug = 42",
            "--trailer",
            "count%10",
            "--trailer",
            "test: stuff",
        ],
        "",
    );
    it(
        &mut fx,
        &["trailer.separators==:"],
        &[],
        "subj\n\nA= c\nB: d\n",
    );
    it(
        &mut fx,
        &["trailer.separators="],
        &["--trailer", "x=y"],
        "subj\n\nA: b\n",
    );
    // Keys and aliases match by prefix, ignoring case.
    it(
        &mut fx,
        &["trailer.zz.key=Acked-by"],
        &[
            "--trailer",
            "z: 1",
            "--trailer",
            "Ack: 2",
            "--trailer",
            "ZZ: 3",
            "--trailer",
            "zzz: 4",
            "--if-exists",
            "add",
            "--trailer",
            "acked-b: 6",
        ],
        "subj\n\nz: 1\nzzz: 4\n",
    );
    // An alias without key keeps the spelling of the argument.
    it(
        &mut fx,
        &["trailer.a.where=start"],
        &["--trailer", "A: z", "--trailer", "aa: y"],
        "subj\n\nA: b\n",
    );
    // A configured key lets a block count with a quarter of trailers.
    it(
        &mut fx,
        &["trailer.my.key=My-trailer"],
        &["--trailer", "x=y"],
        "subj\n\nfoo\nbar\nbaz\nmy: x\n",
    );
    it(
        &mut fx,
        &["trailer.my.key=My-trailer:"],
        &["--trailer", "x=y"],
        "subj\n\nfoo\nbar\nbaz\nMy-trailer : x\n",
    );
    // Unknown values warn, value-less ones are errors.
    it(
        &mut fx,
        &["trailer.where=foo", "trailer.x.ifmissing=bar"],
        &["--trailer", "a: b"],
        "subj\n",
    );
    it(&mut fx, &["trailer.a.key"], &[], "subj\n");
    it(&mut fx, &["trailer.a.cmd"], &[], "subj\n");
    it(
        &mut fx,
        &["trailer.where"],
        &["--trailer", "A=y"],
        "subj\n\nC: d\nA: b\n",
    );
}

#[test]
fn commands() {
    let mut fx = fx();
    it(
        &mut fx,
        &[
            "trailer.bug.key=Bug-maker: ",
            "trailer.bug.ifExists=add",
            "trailer.bug.cmd=echo \"$1\" is",
        ],
        &["--trailer", "bug: him", "--trailer", "bug:me"],
        "",
    );
    it(&mut fx, &["trailer.bug.cmd=echo X"], &[], "subj\n");
    it(
        &mut fx,
        &["trailer.bug.cmd=echo X", "trailer.bug.command=echo Y"],
        &["--trailer", "bug=1"],
        "subj\n",
    );
    it(
        &mut fx,
        &[
            "trailer.bug.command=echo \"[$ARG]\" $ARG",
            "trailer.bug.key=Bug",
        ],
        &["--trailer", "bug=a b", "--trailer", "bug"],
        "subj\n\nBug: old\nbar: x\n",
    );
    it(
        &mut fx,
        &[
            "trailer.bug.cmd=echo \"[$1]\" $#",
            "trailer.bug.key=Bug",
            "trailer.bug.where=before",
        ],
        &["--trailer", "bug", "--trailer", "bug="],
        "subj\n\nBug: old\nbar: x\n",
    );
    it(
        &mut fx,
        &["trailer.sign.command=echo \"$GIT_AUTHOR_NAME <$GIT_AUTHOR_EMAIL>\""],
        &["--trailer", "review:Junio"],
        "",
    );
    it(
        &mut fx,
        &["trailer.bug.key=B", "trailer.bug.cmd=exit 3"],
        &["--trailer", "bug=1"],
        "subj\n",
    );
    it(
        &mut fx,
        &["trailer.bug.cmd=echo \"  a b  \"; echo; echo c"],
        &["--trailer", "bug=x"],
        "subj\n",
    );
    // Not run when nothing would be added.
    it(
        &mut fx,
        &[
            "trailer.bug.cmd=echo run >&2",
            "trailer.bug.ifexists=doNothing",
        ],
        &["--trailer", "bug=x"],
        "s\n\nbug: y\n",
    );
    it(
        &mut fx,
        &["trailer.bug.command=echo run"],
        &["--only-input"],
        "s\n\nbug: y\n",
    );
}

#[test]
fn options_and_errors() {
    let mut fx = fx();
    it(
        &mut fx,
        &[],
        &[
            "--trailer",
            ":",
            "--trailer",
            ":x",
            "--trailer",
            " : y",
            "--trailer",
            "x://y",
            "--trailer",
            "foo bar: baz",
        ],
        "subj\n",
    );
    it(
        &mut fx,
        &[],
        &["--where", "foo", "--trailer", "a: b"],
        "subj\n",
    );
    it(&mut fx, &[], &["--if-exists", "foo"], "subj\n");
    it(
        &mut fx,
        &[],
        &[
            "--where=Before",
            "--if-missing=DOnothing",
            "--trailer",
            "a: b",
        ],
        "subj\n",
    );
    it(
        &mut fx,
        &[],
        &["--trail", "a: b", "--only-t", "--unf"],
        "subj\n",
    );
    it(&mut fx, &[], &["--only"], "subj\n");
    it(&mut fx, &[], &["--bogus"], "subj\n");
    it(&mut fx, &[], &["--where"], "subj\n");
    it(
        &mut fx,
        &[],
        &["--parse", "--no-only-trailers", "--trailer=x=y"],
        "subj\n",
    );
    it(&mut fx, &[], &["-h"], "subj\n");
    it(&mut fx, &[], &["--in-place"], "subj\n");
    it(&mut fx, &[], &["nonexisting"], "subj\n");
    it(
        &mut fx,
        &[],
        &["--unfold", "--only-trailers"],
        "s\n\nfoo: \n  continued\n   across\n",
    );
}

#[test]
fn files() {
    let mut fx = fx()
        .write("one", "subject\n\nbody\n")
        .write("two", "subject\n\nbody\n\nSigned-off-by: a\n---\npatch\n");
    fx.compare([
        "interpret-trailers",
        "--trailer",
        "Acked-by: b",
        "one",
        "two",
    ])
    .run();
    let mut fx = fx.same([
        "interpret-trailers",
        "--in-place",
        "--trailer",
        "Acked-by: b",
        "one",
        "two",
    ]);
    fx.compare(["interpret-trailers", "--in-place", "one", "nonexisting"])
        .run();
}
