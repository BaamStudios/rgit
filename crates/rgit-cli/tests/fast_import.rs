// SPDX-License-Identifier: MIT
//! Differential tests of `rgit fast-import` against the reference git:
//! the same stream imported by both must give the same refs, reflogs,
//! objects, marks files and responses (`ls`, `cat-blob`, `get-mark`), and
//! invalid streams the same messages.

use rgit_difftest::{Check, Fixture, Normalizer};

const C: &str = "committer C O Mitter <c@example.com> 1112911993 -0700";

fn repo() -> Fixture {
    Fixture::new()
        .rgit(env!("CARGO_BIN_EXE_rgit"))
        .git(["init", "-q", "-b", "main"])
}

/// The crash report names the process id.
fn crash() -> Normalizer {
    Normalizer::regex(r"fast_import_crash_[0-9]+", "fast_import_crash_PID")
}

fn import(fx: &mut Fixture, args: &[&str], stream: &str) {
    let mut argv = vec!["fast-import"];
    argv.extend_from_slice(args);
    fx.compare(&argv)
        .stdin(stream.as_bytes().to_vec())
        .normalize(crash())
        .run();
}

/// A history with blobs, inline data, copies, renames, deletes, merges,
/// tags, notes, resets and marks.
fn main_stream() -> String {
    format!(
        "blob\nmark :1\ndata 6\nhello\n\
         blob\nmark :2\ndata <<EOF\nsecond\nfile\nEOF\n\n\
         commit refs/heads/main\nmark :10\n{C}\ndata 8\ninitial\n\
         M 100644 :1 a.txt\nM 755 :2 bin/run\nM 644 inline \"q u\\\"o\"\ndata 2\nq\n\n\
         commit refs/heads/main\nmark :11\nauthor A U Thor <a@example.com> 1112911000 +0100\n{C}\ndata 7\nsecond\n\
         C a.txt dir/copy.txt\nR bin dir/bin\nM 120000 inline link\ndata 5\na.txt\n\n\
         commit refs/heads/topic\nmark :12\n{C}\ndata 6\ntopic\nfrom :10\nD a.txt\nM 100644 inline new\ndata 0\n\n\
         commit refs/heads/main\nmark :13\n{C}\ndata 6\nmerge\nmerge :12\nM 100644 :1 dir/sub/deep\nls \"dir\"\ncat-blob :1\n\n\
         tag v1\nfrom :13\ntagger T Agger <t@example.com> 1112911993 -0700\ndata 4\nv1!\n\n\
         tag v0\nmark :14\nfrom :10\ndata 7\nno tagger\n\
         reset refs/tags/light\nfrom :11\n\n\
         commit refs/notes/commits\n{C}\ndata 5\nnote\nN inline :10\ndata 3\nn1\nN :1 :13\n\n\
         reset refs/heads/gone\nfrom :12\n\
         reset refs/heads/gone\nfrom 0000000000000000000000000000000000000000\n\n\
         alias\nmark :20\nto :11\n\n\
         get-mark :20\n\
         ls :13 dir/sub\n\
         progress all done\n\
         checkpoint\n\
         commit refs/heads/main\n{C}\ndata 5\nlast\ndeleteall\nM 040000 4b825dc642cb6eb9a060e54bf8d69288fbee4904 empty\nM 100644 :2 kept\n\n"
    )
}

#[test]
fn import_history() {
    let mut fx = repo();
    if fx.is_skipped() {
        return;
    }
    let stream = main_stream();
    import(&mut fx, &["--quiet", "--export-marks=marks"], &stream);
    import(
        &mut fx,
        &["--quiet", "--import-marks=marks", "--export-marks=marks2"],
        &format!("commit refs/heads/main\n{C}\ndata 6\nagain\nfrom :13\nM 644 :1 x\n"),
    );
    // Packs instead of loose objects.
    fx = fx.git(["config", "fastimport.unpackLimit", "0"]);
    import(&mut fx, &["--quiet"], &stream);
}

#[test]
fn import_options_and_features() {
    let mut fx = repo();
    if fx.is_skipped() {
        return;
    }
    let raw = "commit refs/heads/d\ncommitter N <e> 1234567890 +051800\ndata 0\n".to_string();
    import(&mut fx, &["--quiet"], &raw);
    import(&mut fx, &["--quiet", "--date-format=raw-permissive"], &raw);
    import(
        &mut fx,
        &["--quiet", "--date-format=rfc2822"],
        "commit refs/heads/r\ncommitter N <e> Tue Feb 6 11:22:18 2007 -0500\ndata 0\n",
    );
    import(
        &mut fx,
        &["--quiet"],
        "feature date-format=rfc2822\nfeature done\ncommit refs/heads/f\ncommitter N <e> Tue, 6 Feb 2007 11:22:18 +0000\ndata 0\ndone\n",
    );
    import(&mut fx, &["--quiet"], "feature done\n");
    import(&mut fx, &["--quiet"], "feature no-such\n");
    import(&mut fx, &["--quiet"], "feature export-marks=x\n");
    import(&mut fx, &["--quiet"], "option git depth=x\n");
    import(&mut fx, &["--quiet"], "option nothing-for-git\n");
    import(&mut fx, &["--quiet"], "blob\ndata 0\noption git quiet\n");
    import(&mut fx, &["--quiet", "--cat-blob-fd=-1"], "");
    import(&mut fx, &["--quiet", "--import-marks=missing"], "");
    import(
        &mut fx,
        &["--quiet", "--import-marks-if-exists=missing"],
        "",
    );
    // Not a fast-forward, then forced.
    let nonff = format!("commit refs/heads/r\n{C}\ndata 1\nx\n");
    import(&mut fx, &["--quiet"], &nonff);
    import(&mut fx, &["--quiet", "--force"], &nonff);
    fx.compare(["fast-import", "-h"])
        .checks(Check::OUTPUT)
        .run();
}

#[test]
fn import_errors() {
    let mut fx = repo();
    if fx.is_skipped() {
        return;
    }
    let cases = [
        "tag foo\n".to_string(),
        "frob\n".to_string(),
        "\n".to_string(),
        "blob\nmark :1\n".to_string(),
        "blob\ndata 10\nabc".to_string(),
        "blob\ndata <<EOF\nabc\n".to_string(),
        "commit refs/heads/x\ndata 0\n".to_string(),
        "commit refs/heads/x\ncommitter Name email> 1 +0000\ndata 0\n".to_string(),
        "commit refs/heads/x\ncommitter Name <e<mail> 1 +0000\ndata 0\n".to_string(),
        "commit refs/heads/x\ncommitter Name <email>> 1 +0000\ndata 0\n".to_string(),
        "commit refs/heads/x\ncommitter Name<email> 1 +0000\ndata 0\n".to_string(),
        "commit refs/heads/x\ncommitter N <e> 1 +1401\ndata 0\n".to_string(),
        format!("commit refs/heads/x..y\n{C}\ndata 0\n"),
        format!("commit refs/heads/x\n{C}\ndata 0\nM 777 inline bob\ndata 0\n"),
        format!("blob\nmark :1\ndata 0\ncommit refs/heads/x\n{C}\ndata 0\nM 100644 :1 ../a\n"),
        format!(
            "blob\nmark :1\ndata 0\ncommit refs/heads/x\n{C}\ndata 0\nM 120000 :1 .gitmodules\n"
        ),
        format!(
            "commit refs/heads/x\n{C}\ndata 0\nM 100644 0000000000000000000000000000000000000001 a\n"
        ),
        format!("commit refs/heads/x\n{C}\ndata 0\nM 100644 :5 a\n"),
        format!("blob\nmark :1\ndata 0\ncommit refs/heads/x\n{C}\ndata 0\nM 100644 :1x a\n"),
        format!("commit refs/heads/x\n{C}\ndata 0\nM 100644 inlineX a\n"),
        format!("commit refs/heads/x\n{C}\ndata 0\nfrom :1x\n"),
        format!("commit refs/heads/x\n{C}\ndata 0\nfrom nosuch\n"),
        format!(
            "commit refs/heads/x\n{C}\ndata 0\ncommit refs/heads/x\n{C}\ndata 0\nfrom refs/heads/x\n"
        ),
        "reset refs/heads/J2\n\ntag w\nfrom refs/heads/J2\ndata 0\n".to_string(),
        format!("commit refs/heads/x\n{C}\ndata 0\nD \"hello.c\n"),
        format!("commit refs/heads/x\n{C}\ndata 0\nD \"hello\\000\"\n"),
        format!("commit refs/heads/x\n{C}\ndata 0\nD \"hello.c\"x\n"),
        format!("commit refs/heads/x\n{C}\ndata 0\nC hello.c\n"),
        format!("commit refs/heads/x\n{C}\ndata 0\nC a b\n"),
        "ls \"a\"\n".to_string(),
        "ls :1x a\n".to_string(),
        "cat-blob 0000000000000000000000000000000000000001\n".to_string(),
        "get-mark 3\n".to_string(),
        "alias\nto :1\n".to_string(),
        format!("commit refs/heads/x\n{C}\ndata 0\nM 160000 inline sub\ndata 0\n"),
        format!("blob\nmark :1\ndata 0\ncommit refs/heads/x\n{C}\ndata 0\nM 040000 :1 sub\n"),
        format!("commit refs/heads/x\n{C}\ngpgsig sha1\ndata 0\ndata 0\n"),
        format!("blob\nmark :1\ndata 0\ncommit refs/heads/x\n{C}\ndata 0\nM 100644 :1 \n"),
    ];
    for case in &cases {
        import(&mut fx, &["--quiet"], case);
    }
}

#[test]
fn import_notes_fanout() {
    let mut fx = repo();
    if fx.is_skipped() {
        return;
    }
    let mut s = String::new();
    for i in 1..=300 {
        s.push_str(&format!(
            "commit refs/heads/many\nmark :{i}\ncommitter C <c@e> {i} +0000\ndata 2\nc\nM 644 inline f\ndata {}\n{i}\n\n",
            i.to_string().len()
        ));
    }
    s.push_str("commit refs/notes/many\ncommitter C <c@e> 1 +0000\ndata 1\nn\n");
    for i in 1..=300 {
        s.push_str(&format!("N inline :{i}\ndata 1\nn\n"));
    }
    s.push_str("M 644 inline de/adbeef\ndata 0\n\n");
    import(&mut fx, &["--quiet", "--export-marks=marks"], &s);
    // Back below the fan-out threshold.
    let mut d = String::from("commit refs/notes/many\ncommitter C <c@e> 2 +0000\ndata 1\nd\n");
    for i in 1..=100 {
        d.push_str(&format!(
            "N 0000000000000000000000000000000000000000 :{i}\n"
        ));
    }
    import(&mut fx, &["--quiet", "--import-marks=marks"], &d);
}
