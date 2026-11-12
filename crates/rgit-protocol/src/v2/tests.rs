// SPDX-License-Identifier: MIT
//! Client tests against canned server responses. The expected error texts
//! were observed from git 2.56.0 against a scripted server.

use std::io::Cursor;

use rgit_hash::{HashKind, ObjectId};

use super::client::*;
use super::negotiate::{Negotiator, NoopNegotiator};
use crate::error::{Error, Result};
use crate::messages;
use crate::pktline::{Reader, encode};
use crate::sideband::{Progress, ProgressStyle};

const A: &str = "1111111111111111111111111111111111111111";
const B: &str = "2222222222222222222222222222222222222222";
const C: &str = "3333333333333333333333333333333333333333";

fn oid(hex: &str) -> ObjectId {
    ObjectId::from_hex_any(hex.as_bytes()).unwrap()
}

/// Builds canned server output.
#[derive(Default)]
struct Script(Vec<u8>);

impl Script {
    fn line(mut self, s: &str) -> Self {
        self.0.extend(encode(format!("{s}\n").as_bytes()).unwrap());
        self
    }
    fn raw(mut self, d: &[u8]) -> Self {
        self.0.extend(encode(d).unwrap());
        self
    }
    fn flush(mut self) -> Self {
        self.0.extend_from_slice(b"0000");
        self
    }
    fn delim(mut self) -> Self {
        self.0.extend_from_slice(b"0001");
        self
    }
    fn end(mut self) -> Self {
        self.0.extend_from_slice(b"0002");
        self
    }
    fn adv(self) -> Self {
        self.line("version 2")
            .line("agent=git/2.56.0-Linux")
            .line("ls-refs=unborn")
            .line("fetch=shallow wait-for-done filter")
            .line("server-option")
            .line("object-format=sha1")
            .flush()
    }
    fn pack(self, data: &[u8]) -> Self {
        let mut p = vec![1u8];
        p.extend_from_slice(data);
        self.line("packfile").raw(&p).flush()
    }
}

type TestClient = Client<Pipe<Cursor<Vec<u8>>, Vec<u8>>>;

fn connect(script: Script, stateless: bool) -> Result<TestClient> {
    Client::connect(
        Pipe::new(Cursor::new(script.0), Vec::new(), stateless),
        "test",
    )
}

fn client(script: Script) -> TestClient {
    let mut c = connect(script, false).unwrap();
    c.set_agent("rgit/test");
    c
}

/// The packets written, `0000`/`0001` for special packets.
fn sent(c: TestClient) -> Vec<String> {
    let (_, bytes) = c.into_inner().into_parts();
    let mut r = Reader::new(&bytes[..], "t");
    r.chomp_newline(false);
    let mut out = Vec::new();
    while let Some(p) = r.read_or_eof().unwrap() {
        out.push(p.describe());
    }
    out
}

fn fetch(
    c: &mut TestClient,
    args: &FetchArgs,
    n: &mut dyn Negotiator,
) -> (Result<FetchOutcome>, Vec<u8>, String) {
    let mut p = Progress::new(Vec::new(), ProgressStyle::Spaces);
    let mut pack = Vec::new();
    let r = c.fetch(args, n, &mut p, |d| {
        pack.extend_from_slice(d);
        Ok(())
    });
    (r, pack, String::from_utf8(p.into_inner()).unwrap())
}

/// Offers a fixed list; every id is reported as newly common once.
struct List(std::vec::IntoIter<ObjectId>, Vec<ObjectId>);

impl Negotiator for List {
    fn next_have(&mut self) -> Option<ObjectId> {
        self.0.next()
    }
    fn ack(&mut self, id: &ObjectId) -> bool {
        if self.1.contains(id) {
            false
        } else {
            self.1.push(*id);
            true
        }
    }
}

fn haves<S: AsRef<str>>(ids: &[S]) -> List {
    List(
        ids.iter()
            .map(|h| oid(h.as_ref()))
            .collect::<Vec<_>>()
            .into_iter(),
        Vec::new(),
    )
}

fn args(wants: &[&str]) -> FetchArgs {
    FetchArgs {
        wants: wants.iter().map(|h| oid(h)).collect(),
        thin_pack: true,
        no_progress: true,
        include_tag: true,
        ofs_delta: true,
        ..FetchArgs::default()
    }
}

fn fetch_error(s: Script, a: &FetchArgs, n: &mut dyn Negotiator) -> String {
    let mut c = client(s);
    fetch(&mut c, a, n).0.unwrap_err().to_string()
}

fn have_lines(request: &[String]) -> Vec<String> {
    request
        .iter()
        .filter_map(|l| l.strip_prefix("have ").map(|h| h.trim_end().to_owned()))
        .collect()
}

fn requests(lines: &[String]) -> Vec<Vec<String>> {
    lines
        .split(|l| l == "0000")
        .filter(|r| !r.is_empty())
        .map(<[String]>::to_vec)
        .collect()
}

#[test]
fn v0_and_v1_servers_are_refused() {
    let v0 = Script::default()
        .line(&format!("{A} HEAD\0multi_ack"))
        .flush();
    assert!(matches!(connect(v0, false), Err(Error::NotV2(0))));
    let v1 = Script::default().line("version 1").flush();
    assert!(matches!(connect(v1, false), Err(Error::NotV2(1))));
}

#[test]
fn object_formats() {
    let s = Script::default()
        .line("version 2")
        .line("object-format=md5")
        .flush();
    let e = connect(s, false).unwrap_err();
    assert_eq!(e.to_string(), "protocol error: unknown object format 'md5'");
    let s = Script::default()
        .line("version 2")
        .line("object-format=sha256")
        .flush();
    assert_eq!(connect(s, false).unwrap().object_format(), HashKind::Sha256);
    let s = Script::default().line("version 2").flush();
    assert_eq!(connect(s, false).unwrap().object_format(), HashKind::Sha1);
}

#[test]
fn stateless_skips_service_header_and_response_end() {
    let s = Script::default()
        .line("# service=git-upload-pack")
        .flush()
        .adv()
        .end()
        .line(&format!("{A} HEAD"))
        .flush();
    let mut c = connect(s, true).unwrap();
    assert!(c.is_stateless());
    let refs = c.ls_refs(&LsRefsArgs::default()).unwrap();
    assert_eq!(refs.len(), 1);
    c.close().unwrap();
    // No extra flush packet at the end of a stateless session.
    let lines = sent(c);
    assert_eq!(lines.iter().filter(|l| *l == "0000").count(), 1);
}

#[test]
fn ls_refs_request_and_parse() {
    let s = Script::default()
        .adv()
        .line(&format!("{A} HEAD symref-target:refs/heads/main"))
        .line(&format!("{A} refs/heads/main"))
        .line(&format!("{B} refs/tags/v1 peeled:{C}"))
        .line(&format!("{C} refs/tags/x future:attr"))
        .line("unborn refs/heads/new symref-target:refs/heads/other")
        .flush();
    let mut c = client(s);
    let mut a = LsRefsArgs::standard();
    a.ref_prefixes = vec!["refs/heads/".into(), "HEAD".into()];
    a.server_options = vec!["foo".into()];
    let refs = c.ls_refs(&a).unwrap();
    assert_eq!(refs.len(), 5);
    assert_eq!(refs[0].name, b"HEAD");
    assert_eq!(refs[0].oid, Some(oid(A)));
    assert_eq!(
        refs[0].symref_target.as_deref(),
        Some(&b"refs/heads/main"[..])
    );
    assert_eq!(refs[1].symref_target, None);
    assert_eq!(refs[2].peeled, Some(oid(C)));
    assert_eq!(refs[3].peeled, None);
    assert_eq!(refs[4].oid, None);
    c.close().unwrap();
    assert_eq!(
        sent(c),
        [
            "command=ls-refs\n",
            "agent=rgit/test\n",
            "object-format=sha1\n",
            "server-option=foo\n",
            "0001",
            "peel\n",
            "symrefs\n",
            "unborn\n",
            "ref-prefix refs/heads/\n",
            "ref-prefix HEAD\n",
            "0000",
            "0000",
        ]
    );
}

#[test]
fn minimal_server_gets_minimal_request() {
    // No agent, object-format, server-option or unborn advertised: none of
    // them is sent (as git does).
    let s = Script::default()
        .line("version 2")
        .line("ls-refs")
        .line("fetch")
        .flush()
        .flush();
    let mut c = client(s);
    c.ls_refs(&LsRefsArgs {
        server_options: vec!["x".into()],
        ..LsRefsArgs::standard()
    })
    .unwrap();
    assert_eq!(
        sent(c),
        ["command=ls-refs\n", "0001", "peel\n", "symrefs\n", "0000"]
    );
}

#[test]
fn ls_refs_errors() {
    let peel_bad = format!("{A} refs/x peeled:12");
    for (line, msg) in [
        (
            "zzz HEAD".to_owned(),
            "invalid ls-refs response: zzz HEAD".to_owned(),
        ),
        (A.to_owned(), format!("invalid ls-refs response: {A}")),
        (
            peel_bad.clone(),
            format!("invalid ls-refs response: {peel_bad}"),
        ),
    ] {
        let mut c = client(Script::default().adv().line(&line).flush());
        let e = c.ls_refs(&LsRefsArgs::standard()).unwrap_err();
        assert_eq!(e.to_string(), msg);
    }
    let mut c = client(Script::default().adv().line("ERR access denied"));
    let e = c.ls_refs(&LsRefsArgs::standard()).unwrap_err();
    assert_eq!(e.to_string(), "remote error: access denied");
}

#[test]
fn clone_sends_done_at_once() {
    let s = Script::default()
        .adv()
        .line("packfile")
        .raw(b"\x02Counting objects: 1\r")
        .raw(b"\x01PACK")
        .raw(b"\x02Counting objects: 2, done.\n")
        .raw(b"\x01rest")
        .flush();
    let mut c = client(s);
    let (r, pack, prog) = fetch(&mut c, &args(&[A, A]), &mut NoopNegotiator);
    let out = r.unwrap();
    assert!(out.packfile);
    assert_eq!(out.rounds, 1);
    assert_eq!(pack, b"PACKrest");
    assert_eq!(
        prog,
        "remote: Counting objects: 1        \rremote: Counting objects: 2, done.        \n"
    );
    assert_eq!(
        sent(c),
        [
            "command=fetch\n",
            "agent=rgit/test\n",
            "object-format=sha1\n",
            "0001",
            "thin-pack\n",
            "no-progress\n",
            "include-tag\n",
            "ofs-delta\n",
            &format!("want {A}\n"),
            &format!("want {A}\n"),
            "done\n",
            "0000",
        ]
    );
}

#[test]
fn negotiation_rounds() {
    // Round 1: 16 haves, NAK. Round 2: 32 haves, one ACKed. Round 3: the
    // ACKed commit first, then the remaining 12; ready and the pack.
    let ids: Vec<String> = (1..=60).map(|i| format!("{i:040x}")).collect();
    let acked = ids[20].clone();
    let s = Script::default()
        .adv()
        .line("acknowledgments")
        .line("NAK")
        .flush()
        .line("acknowledgments")
        .line(&format!("ACK {acked}"))
        .flush()
        .line("acknowledgments")
        .line(&format!("ACK {acked}"))
        .line("ready")
        .delim()
        .line("shallow-info")
        .line(&format!("shallow {B}"))
        .line(&format!("unshallow {C}"))
        .delim()
        .pack(b"PACKx");
    let mut c = client(s);
    let (r, pack, _) = fetch(&mut c, &args(&[A]), &mut haves(&ids));
    let out = r.unwrap();
    assert_eq!(out.rounds, 3);
    assert_eq!(out.common, vec![oid(&acked)]);
    assert_eq!(
        out.shallow_info,
        Some(ShallowInfo {
            shallow: vec![oid(B)],
            unshallow: vec![oid(C)],
        })
    );
    assert_eq!(pack, b"PACKx");
    let reqs = requests(&sent(c));
    assert_eq!(reqs.len(), 3);
    assert_eq!(have_lines(&reqs[0]), ids[..16]);
    assert_eq!(have_lines(&reqs[1]), ids[16..48]);
    let mut third = vec![acked.clone()];
    third.extend(ids[48..].iter().cloned());
    assert_eq!(have_lines(&reqs[2]), third);
    let want = format!("want {A}\n");
    assert!(reqs.iter().all(|r| r.contains(&want)));
    assert!(reqs.iter().all(|r| !r.contains(&"done\n".to_owned())));
}

#[test]
fn done_when_haves_run_out() {
    let s = Script::default()
        .adv()
        .line("acknowledgments")
        .line(&format!("ACK {B}"))
        .flush()
        .pack(b"PACK");
    let mut c = client(s);
    let (r, _, _) = fetch(&mut c, &args(&[A]), &mut haves(&[B]));
    assert_eq!(r.unwrap().rounds, 2);
    let reqs = requests(&sent(c));
    let last = &reqs[1];
    assert_eq!(
        last[last.len() - 3..],
        [
            format!("want {A}\n"),
            format!("have {B}\n"),
            "done\n".into()
        ]
    );
}

#[test]
fn gives_up_after_256_in_vain() {
    // A new ACK in round 1; then rounds of 32, 64 and 128 new haves without
    // one. After round 4 the count since the last new ACK is 224, so round
    // 5 (256 more) carries `done`.
    let ids: Vec<String> = (1..2000).map(|i| format!("{i:040x}")).collect();
    let mut s = Script::default();
    s = s.adv();
    for _ in 0..4 {
        s = s
            .line("acknowledgments")
            .line(&format!("ACK {}", ids[0]))
            .flush();
    }
    s = s.pack(b"PACK");
    let mut c = client(s);
    let (r, _, _) = fetch(&mut c, &args(&[A]), &mut haves(&ids));
    assert_eq!(r.unwrap().rounds, 5);
    let reqs = requests(&sent(c));
    let counts: Vec<usize> = reqs.iter().map(|r| have_lines(r).len()).collect();
    assert_eq!(counts, [16, 33, 65, 129, 257]);
    assert!(reqs[4].ends_with(&["done\n".to_owned()]));
}

#[test]
fn response_errors_as_git_reports_them() {
    let cases: Vec<(Script, bool, &str)> = vec![
        (
            Script::default()
                .adv()
                .line("acknowledgments")
                .line("NAK")
                .flush(),
            false,
            "expected 'packfile', received 'acknowledgments'",
        ),
        (
            Script::default().adv().line("bogus").flush(),
            false,
            "expected 'packfile', received 'bogus'",
        ),
        (
            Script::default().adv().line("bogus").flush(),
            true,
            "expected 'acknowledgments', received 'bogus'",
        ),
        (
            Script::default()
                .adv()
                .line("acknowledgments")
                .line("FOO")
                .flush(),
            true,
            "unexpected acknowledgment line: 'FOO'",
        ),
        (
            Script::default()
                .adv()
                .line("acknowledgments")
                .line("ready")
                .flush(),
            true,
            "expected packfile to be sent after 'ready'",
        ),
        (
            Script::default()
                .adv()
                .line("acknowledgments")
                .line("NAK")
                .delim()
                .line("packfile")
                .flush(),
            true,
            "expected no other sections to be sent after no 'ready'",
        ),
        (
            Script::default()
                .adv()
                .line("shallow-info")
                .line("foo")
                .delim()
                .line("packfile")
                .flush(),
            false,
            "expected shallow/unshallow, got foo",
        ),
        (
            Script::default()
                .adv()
                .line("packfile")
                .raw(b"\x03access denied\n"),
            false,
            "remote error: access denied",
        ),
        (
            Script::default().adv().line("packfile").raw(b"\x05x"),
            false,
            "protocol error: bad band #5",
        ),
        (
            Script::default().adv().line("packfile").raw(b"\x01PACK"),
            false,
            messages::SIDEBAND_DISCONNECT,
        ),
    ];
    for (s, negotiate, msg) in cases {
        let e = if negotiate {
            fetch_error(s, &args(&[A]), &mut haves(&[B]))
        } else {
            fetch_error(s, &args(&[A]), &mut NoopNegotiator)
        };
        assert_eq!(e, msg);
    }
}

#[test]
fn sideband_error_is_shown_as_remote_line() {
    let s = Script::default()
        .adv()
        .line("packfile")
        .raw(b"\x02half")
        .raw(b"\x03access denied\n");
    let mut c = client(s);
    let (r, _, prog) = fetch(&mut c, &args(&[A]), &mut NoopNegotiator);
    assert!(matches!(r, Err(Error::SidebandFatal(_))));
    assert_eq!(prog, "remote: half\nremote: access denied\n\n");
}

#[test]
fn wanted_refs_section() {
    let s = Script::default()
        .adv()
        .line("wanted-refs")
        .line(&format!("{A} refs/heads/main"))
        .delim()
        .pack(b"PACK");
    let mut c = client(s);
    let (r, _, _) = fetch(&mut c, &args(&[A]), &mut NoopNegotiator);
    assert_eq!(
        r.unwrap().wanted_refs,
        vec![(oid(A), b"refs/heads/main".to_vec())]
    );
}

#[test]
fn sections_out_of_order() {
    let s = Script::default()
        .adv()
        .line("wanted-refs")
        .delim()
        .line("shallow-info")
        .delim()
        .pack(b"PACK");
    let e = fetch_error(s, &args(&[A]), &mut NoopNegotiator);
    assert_eq!(e, "expected 'packfile', received 'shallow-info'");
}

#[test]
fn mismatched_algorithms() {
    let mut a = args(&[A]);
    a.local_format = Some(HashKind::Sha256);
    let e = fetch_error(Script::default().adv(), &a, &mut NoopNegotiator);
    assert_eq!(e, "mismatched algorithms: client sha256; server sha1");
}

#[test]
fn shallow_needs_server_support() {
    let s = Script::default().line("version 2").line("fetch").flush();
    let mut a = args(&[A]);
    a.deepen = Some(1);
    assert_eq!(
        fetch_error(s, &a, &mut NoopNegotiator),
        messages::SHALLOW_NOT_SUPPORTED
    );
}

#[test]
fn shallow_and_filter_arguments() {
    let s = Script::default().adv().pack(b"PACK");
    let mut c = client(s);
    let mut a = args(&[A]);
    a.shallow = vec![oid(B)];
    a.deepen = Some(2);
    a.deepen_relative = true;
    a.deepen_since = Some(1234);
    a.deepen_not = vec!["refs/heads/old".into()];
    a.filter = Some("blob:none".into());
    fetch(&mut c, &a, &mut NoopNegotiator).0.unwrap();
    let lines = sent(c);
    let start = lines.iter().position(|l| l == "ofs-delta\n").unwrap() + 1;
    assert_eq!(
        lines[start..],
        [
            format!("shallow {B}\n"),
            "deepen 2\n".into(),
            "deepen-relative\n".into(),
            "deepen-since 1234\n".into(),
            "deepen-not refs/heads/old\n".into(),
            "filter blob:none\n".into(),
            format!("want {A}\n"),
            "done\n".into(),
            "0000".into(),
        ]
    );
}

#[test]
fn filter_dropped_without_server_support() {
    let s = Script::default()
        .line("version 2")
        .line("fetch=shallow")
        .flush()
        .pack(b"PACK");
    let mut c = client(s);
    assert!(!c.supports_filter());
    assert!(c.supports_shallow());
    let mut a = args(&[A]);
    a.filter = Some("blob:none".into());
    fetch(&mut c, &a, &mut NoopNegotiator).0.unwrap();
    assert!(!sent(c).iter().any(|l| l.starts_with("filter")));
}

#[test]
fn request_lines_with_lf_are_refused() {
    let mut c = client(Script::default().adv());
    let a = LsRefsArgs {
        ref_prefixes: vec!["a\nb".into()],
        ..LsRefsArgs::default()
    };
    assert!(matches!(c.ls_refs(&a), Err(Error::BadRequestLine(_))));
}

#[test]
fn no_wants_sends_nothing() {
    let mut c = client(Script::default().adv());
    let out = fetch(&mut c, &args(&[]), &mut NoopNegotiator).0.unwrap();
    assert_eq!(out.rounds, 0);
    assert!(sent(c).is_empty());
}

#[test]
fn unadvertised_command_is_refused() {
    let s = Script::default().line("version 2").line("fetch").flush();
    let mut c = client(s);
    let e = c.ls_refs(&LsRefsArgs::standard()).unwrap_err();
    assert_eq!(
        e.to_string(),
        "protocol error: server does not support the 'ls-refs' command"
    );
}
