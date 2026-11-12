// SPDX-License-Identifier: MIT

//! `sha1`, `sha256`, `sha1-is-sha1dc` against the reference `test-tool`.

mod common;

use common::{fixture, same, same_stdin};

/// The SHAttered collision PDF from git's test data, if the upstream checkout
/// is present (it is test data, not implementation code).
fn shattered() -> Option<Vec<u8>> {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../upstream/git/t/t0013/shattered-1.pdf");
    std::fs::read(p).ok()
}

#[test]
fn hashes_match_reference() {
    let Some(mut fx) = fixture() else { return };
    let big: Vec<u8> = (0..200_000u32).map(|i| (i * 31 % 251) as u8).collect();
    for kind in ["sha1", "sha256"] {
        for input in [&b""[..], b"abc\n", &big] {
            same_stdin(&mut fx, &[kind], input);
            same_stdin(&mut fx, &[kind, "-b"], input);
        }
        // Extra arguments are ignored; only a lone `-b` switches to binary.
        for extra in [
            &["-h"][..],
            &["-b", "-b"],
            &["3", "-b"],
            &["-b", "0"],
            &["x"],
        ] {
            let mut args = vec![kind];
            args.extend_from_slice(extra);
            same_stdin(&mut fx, &args, b"abc\n");
        }
    }
}

#[test]
fn sha1dc_prerequisite() {
    let Some(mut fx) = fixture() else { return };
    same(&mut fx, &["sha1-is-sha1dc"]);
}

#[test]
fn shattered_pdf_is_refused() {
    let Some(mut fx) = fixture() else { return };
    let Some(pdf) = shattered() else { return };
    same_stdin(&mut fx, &["sha1"], &pdf);
    same_stdin(&mut fx, &["sha256"], &pdf);
}

#[test]
fn chdir_failure() {
    let Some(mut fx) = fixture() else { return };
    same(
        &mut fx,
        &["-C", "/nonexistent-rgit-test-tool", "sha1-is-sha1dc"],
    );
}
