// SPDX-License-Identifier: MIT

//! `pkt-line` against the reference `test-tool`: the invocations of the
//! upstream scripts (packing requests, unpacking answers, the side-band
//! checks) and the error cases around them.

mod common;

use common::{fixture, same, same_stdin};

/// A data packet.
fn pkt(payload: &[u8]) -> Vec<u8> {
    let mut out = format!("{:04x}", payload.len() + 4).into_bytes();
    out.extend_from_slice(payload);
    out
}

/// The stream `send-split-sideband` writes.
fn split_sideband() -> Vec<u8> {
    let mut s = Vec::new();
    for p in [
        &b"\x02Foo.\n"[..],
        b"\x02Bar.\n",
        b"\x02Hello,",
        b"\x01primary: regular output\n",
        b"\x02 world!\n",
    ] {
        s.extend(pkt(p));
    }
    s.extend_from_slice(b"00020000");
    s
}

#[test]
fn pack_and_unpack() {
    let Some(mut fx) = fixture() else { return };
    let long = vec![b'a'; 70_000];
    for input in [
        &b"command=ls-refs\n0001\nsymrefs\npeel\n0000\n"[..],
        b"a\n0000\n0001\n0002\n\nlast-no-newline",
        b"0000 \n0000\r\n0001",
        b"",
        &long,
    ] {
        same_stdin(&mut fx, &["pkt-line", "pack"], input);
        same_stdin(&mut fx, &["pkt-line", "pack-raw-stdin"], input);
    }
    let mut stream = Vec::new();
    for p in [
        &b"version 2\n"[..],
        b"agent=x",
        b"",
        b"a\r\n",
        b"a\n\n",
        b"ERR oops\n",
    ] {
        stream.extend(pkt(p));
    }
    stream.extend_from_slice(b"000100020000");
    for input in [
        stream.clone(),
        b"0009hel".to_vec(),
        b"zzzz".to_vec(),
        b"0005a0003".to_vec(),
        b"00".to_vec(),
        Vec::new(),
    ] {
        same_stdin(&mut fx, &["pkt-line", "unpack"], &input);
    }
}

#[test]
fn sideband() {
    let Some(mut fx) = fixture() else { return };
    // The fixture runs with TERM=dumb: progress lines end in spaces.
    let fx = &mut fx;
    same(fx, &["pkt-line", "send-split-sideband"]);
    let split = split_sideband();
    for opts in [
        &[][..],
        &["--no-chomp-newline"],
        &["--chomp-newline"],
        &["--reader-use-sideband", "--no-chomp-newline"],
        &["--reader-use-sideband", "--chomp-newline"],
        &["--bogus"],
    ] {
        let mut args = vec!["pkt-line", "unpack-sideband"];
        args.extend_from_slice(opts);
        same_stdin(fx, &args, &split);
    }
    // A fetch answer: pack data on band 1, progress on band 2.
    let mut answer = pkt(b"packfile\n");
    answer.extend(pkt(b"\x02Enumerating objects: 3, done.\n"));
    answer.extend(pkt(b"\x01PACK\x00\x00\x00\x02"));
    answer.extend(pkt(b"\x01\x00\x00\x00\x00rest"));
    answer.extend_from_slice(b"0000");
    same_stdin(fx, &["pkt-line", "unpack-sideband"], &answer);
    for input in [
        split.clone(),
        b"1234".to_vec(),
        b"0004".to_vec(),
        [pkt(b"\x01abc"), b"0000".to_vec()].concat(),
        [pkt(b"\x05a"), b"0000".to_vec()].concat(),
        [pkt(b"\x02a"), b"0000".to_vec()].concat(),
        b"0001".to_vec(),
        b"zzzz".to_vec(),
        Vec::new(),
    ] {
        same_stdin(fx, &["pkt-line", "receive-sideband"], &input);
    }
}

#[test]
fn bad_actions() {
    let Some(mut fx) = fixture() else { return };
    same(&mut fx, &["pkt-line"]);
    same(&mut fx, &["pkt-line", "bogus"]);
}
