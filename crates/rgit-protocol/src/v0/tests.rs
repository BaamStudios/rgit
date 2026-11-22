// SPDX-License-Identifier: MIT
//! Scripted-server tests of the v0/v1 client.

use super::*;
use crate::pktline::encode;
use crate::sideband::ProgressStyle;
use crate::v2::client::Pipe;

fn id(n: u8) -> ObjectId {
    ObjectId::from_bytes(HashKind::Sha1, &[n; 20]).unwrap()
}

fn pkt(out: &mut Vec<u8>, line: &str) {
    out.extend(encode(format!("{line}\n").as_bytes()).unwrap());
}

/// Offers the given commits in order; ACKs are reported as new.
struct List(Vec<ObjectId>);

impl Negotiator for List {
    fn next_have(&mut self) -> Option<ObjectId> {
        if self.0.is_empty() {
            None
        } else {
            Some(self.0.remove(0))
        }
    }
    fn ack(&mut self, _id: &ObjectId) -> bool {
        true
    }
}

/// The capabilities of the scripted server (a typical set).
const CAPS: &[&str] = &[
    "multi_ack_detailed",
    "multi_ack",
    "side-band",
    "side-band-64k",
    "ofs-delta",
    "thin-pack",
    "no-progress",
    "include-tag",
    "shallow",
    "deepen-since",
    "deepen-not",
    "deepen-relative",
    "symref=HEAD:refs/heads/main",
    "agent=test/1",
];

fn advertisement(extra_caps: &str) -> Vec<u8> {
    let mut s = Vec::new();
    let caps = CAPS.join(" ");
    s.extend(encode(format!("{} HEAD\0{caps}{extra_caps}\n", id(1)).as_bytes()).unwrap());
    pkt(&mut s, &format!("{} refs/heads/main", id(1)));
    pkt(&mut s, &format!("{} refs/tags/v1", id(2)));
    pkt(&mut s, &format!("{} refs/tags/v1^{{}}", id(1)));
    s.extend_from_slice(b"0000");
    s
}

fn sideband_pack(out: &mut Vec<u8>, data: &[u8]) {
    let mut p = vec![1u8];
    p.extend_from_slice(data);
    out.extend(encode(&p).unwrap());
    out.extend_from_slice(b"0000");
}

/// Run a fetch against a scripted server: `server` is the advertisement
/// followed by every response. Returns the outcome, the pack bytes and
/// what the client sent.
fn run(
    server: Vec<u8>,
    stateless: bool,
    args: &FetchArgs,
    haves: Vec<ObjectId>,
) -> (Result<FetchOutcome>, Vec<u8>, String) {
    let pipe = Pipe::new(&server[..], Vec::new(), stateless);
    let mut reader = Reader::new(pipe, "fetch");
    let v = ProtocolVersion::read(&mut reader).unwrap();
    let adv = read_advertisement(&mut reader, v).unwrap();
    let mut client = Client::new(reader, &adv);
    let mut progress = Progress::new(Vec::new(), ProgressStyle::Spaces);
    let mut pack = Vec::new();
    let r = client.fetch(args, &mut List(haves), &mut progress, |d| {
        pack.extend_from_slice(d);
        Ok(())
    });
    let (_, sent) = client.into_inner().into_parts();
    (r, pack, String::from_utf8_lossy(&sent).into_owned())
}

#[test]
fn advertisement_refs_and_caps() {
    let mut s = Vec::new();
    pkt(&mut s, "version 1");
    s.extend(advertisement(""));
    let mut r = Reader::new(&s[..], "t");
    let v = ProtocolVersion::read(&mut r).unwrap();
    assert_eq!(v, ProtocolVersion::V1);
    let adv = read_advertisement(&mut r, v).unwrap();
    assert_eq!(adv.refs.len(), 3);
    assert_eq!(
        adv.refs[0].symref_target.as_deref(),
        Some(&b"refs/heads/main"[..])
    );
    assert_eq!(adv.refs[2].name, b"refs/tags/v1");
    assert_eq!(adv.refs[2].peeled, Some(id(1)));
    assert_eq!(adv.head_target(), Some("refs/heads/main"));
    assert!(adv.caps.contains("multi_ack_detailed"));
}

#[test]
fn empty_repository_advertisement() {
    let mut s = Vec::new();
    let zero = "0".repeat(64);
    s.extend(
        encode(
            format!("{zero} capabilities^{{}}\0symref=HEAD:refs/heads/x object-format=sha256\n")
                .as_bytes(),
        )
        .unwrap(),
    );
    s.extend_from_slice(b"0000");
    let mut r = Reader::new(&s[..], "t");
    let adv = read_advertisement(&mut r, ProtocolVersion::V0).unwrap();
    assert!(adv.refs.is_empty());
    assert_eq!(adv.format, HashKind::Sha256);
    assert_eq!(adv.head_target(), Some("refs/heads/x"));
}

#[test]
fn bad_ref_line() {
    let mut s = Vec::new();
    pkt(&mut s, "zzz HEAD");
    let mut r = Reader::new(&s[..], "t");
    let e = read_advertisement(&mut r, ProtocolVersion::V0).unwrap_err();
    assert_eq!(e.to_string(), "protocol error: unexpected 'zzz HEAD'");
}

fn args(wants: &[ObjectId]) -> FetchArgs {
    FetchArgs {
        wants: wants.to_vec(),
        thin_pack: true,
        no_progress: true,
        include_tag: true,
        ofs_delta: true,
        ..FetchArgs::default()
    }
}

#[test]
fn clone_request_and_pack() {
    let mut s = advertisement("");
    pkt(&mut s, "NAK");
    sideband_pack(&mut s, b"PACKDATA");
    let (r, pack, sent) = run(s, false, &args(&[id(1), id(1)]), vec![]);
    let out = r.unwrap();
    assert!(out.packfile);
    assert_eq!(pack, b"PACKDATA");
    let caps = format!(
        "multi_ack_detailed side-band-64k thin-pack no-progress include-tag ofs-delta \
deepen-since deepen-not agent={}",
        agent::agent()
    );
    let mut expect = Vec::new();
    pkt(&mut expect, &format!("want {} {caps}", id(1)));
    pkt(&mut expect, &format!("want {}", id(1)));
    expect.extend_from_slice(b"0000");
    pkt(&mut expect, "done");
    assert_eq!(sent, String::from_utf8(expect).unwrap());
}

#[test]
fn stateful_batches_are_pipelined() {
    let haves: Vec<ObjectId> = (10..50).map(id).collect();
    let mut s = advertisement("");
    // Response to batch 1 (16 haves), to batch 2 (16), then to `done`
    // after the last 8.
    pkt(&mut s, "NAK");
    pkt(&mut s, &format!("ACK {} common", id(30)));
    pkt(&mut s, "NAK");
    pkt(&mut s, &format!("ACK {}", id(30)));
    sideband_pack(&mut s, b"P");
    let (r, _, sent) = run(s, false, &args(&[id(1)]), haves);
    let out = r.unwrap();
    assert_eq!(out.common, vec![id(30)]);
    let lines: Vec<&str> = sent.split("0000").collect();
    // wants | 16 haves | 16 haves | 8 haves + done
    assert_eq!(lines.len(), 4);
    assert_eq!(lines[1].matches("have ").count(), 16);
    assert_eq!(lines[2].matches("have ").count(), 16);
    assert_eq!(lines[3].matches("have ").count(), 8);
    assert!(lines[3].ends_with("0009done\n"));
}

#[test]
fn stateful_ready_ends_negotiation() {
    let haves: Vec<ObjectId> = (10..100).map(id).collect();
    let mut s = advertisement("");
    pkt(&mut s, &format!("ACK {} common", id(12)));
    pkt(&mut s, &format!("ACK {} ready", id(13)));
    pkt(&mut s, "NAK");
    pkt(&mut s, "NAK");
    pkt(&mut s, &format!("ACK {}", id(13)));
    sideband_pack(&mut s, b"P");
    let (r, _, sent) = run(s, false, &args(&[id(1)]), haves);
    let out = r.unwrap();
    assert_eq!(out.common, vec![id(12), id(13)]);
    // Two batches went out before the first answer, then `done`.
    assert_eq!(sent.matches("have ").count(), 32);
    assert!(sent.ends_with("0009done\n"));
}

#[test]
fn stateless_repeats_wants_and_common() {
    let haves: Vec<ObjectId> = (10..30).map(id).collect();
    let mut s = advertisement(" no-done");
    pkt(&mut s, &format!("ACK {} common", id(11)));
    pkt(&mut s, "NAK");
    pkt(&mut s, &format!("ACK {}", id(11)));
    sideband_pack(&mut s, b"P");
    let (r, _, sent) = run(s, true, &args(&[id(1)]), haves);
    r.unwrap();
    assert!(sent.contains("multi_ack_detailed no-done side-band-64k"));
    assert_eq!(sent.matches("want ").count(), 2);
    // 16 haves, then the common one again and the last 4 with done.
    assert_eq!(sent.matches("have ").count(), 21);
    assert!(sent.ends_with("0009done\n"));
}

#[test]
fn stateless_no_done_after_ready() {
    let haves: Vec<ObjectId> = (10..60).map(id).collect();
    let mut s = advertisement(" no-done");
    pkt(&mut s, &format!("ACK {} ready", id(11)));
    pkt(&mut s, "NAK");
    pkt(&mut s, &format!("ACK {}", id(11)));
    sideband_pack(&mut s, b"PK");
    let (r, pack, sent) = run(s, true, &args(&[id(1)]), haves);
    assert!(r.unwrap().packfile);
    assert_eq!(pack, b"PK");
    assert!(!sent.contains("done\n"));
}

#[test]
fn bad_acknowledgment() {
    let mut s = advertisement("");
    pkt(&mut s, "bogus");
    let (r, _, _) = run(s, false, &args(&[id(1)]), vec![id(9)]);
    assert_eq!(
        r.unwrap_err().to_string(),
        "git fetch-pack: expected ACK/NAK, got 'bogus'"
    );
}

#[test]
fn raw_pack_without_sideband() {
    let mut s = Vec::new();
    s.extend(encode(format!("{} HEAD\0multi_ack_detailed\n", id(1)).as_bytes()).unwrap());
    s.extend_from_slice(b"0000");
    pkt(&mut s, "NAK");
    s.extend_from_slice(b"RAWPACK");
    let (r, pack, sent) = run(s, false, &args(&[id(1)]), vec![]);
    assert!(r.unwrap().packfile);
    assert_eq!(pack, b"RAWPACK");
    assert!(sent.contains(&format!("want {} multi_ack_detailed\n", id(1))));
}
