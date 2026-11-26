// SPDX-License-Identifier: MIT
//! `serve` driven in memory with a fake repository. The expected bytes
//! were observed from git 2.56.0's `receive-pack` with the same input.

use std::collections::{BTreeMap, HashSet};
use std::io::Read;

use super::*;

const A: &str = "ad636fabf2b373682351deb10b5d6119eb230501";
const B: &str = "5927bcbf0750f346173992206df24fe267501e6a";
const Z: &str = "0000000000000000000000000000000000000000";

/// A hook run: name, arguments, standard input, environment.
type HookRun = (String, Vec<String>, String, Vec<(String, String)>);

fn oid(hex: &str) -> ObjectId {
    ObjectId::from_hex(HashKind::Sha1, hex.as_bytes()).unwrap()
}

#[derive(Default)]
struct Fake {
    refs: BTreeMap<String, ObjectId>,
    objects: HashSet<ObjectId>,
    /// Objects the pack brings.
    pack: Vec<ObjectId>,
    bare: bool,
    head: Option<String>,
    hooks: BTreeMap<String, i32>,
    hook_calls: Vec<HookRun>,
    ingested: Option<IngestOptions>,
    quarantined: bool,
    /// A `proc-receive` hook: what it writes to standard output and,
    /// late, to standard error.
    proc_receive: Option<(Vec<u8>, Vec<u8>)>,
}

/// A `proc-receive` hook with canned output.
struct FakeProc {
    input: Vec<u8>,
    output: std::io::Cursor<Vec<u8>>,
    stderr: Option<Vec<u8>>,
}

/// Standard error that arrives only after a while, as when the relay
/// thread is scheduled late.
struct LateStderr(std::io::Cursor<Vec<u8>>, bool);

impl Read for LateStderr {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if !self.1 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            self.1 = true;
        }
        self.0.read(buf)
    }
}

impl ProcReceive for FakeProc {
    fn input(&mut self) -> &mut dyn std::io::Write {
        &mut self.input
    }
    fn output(&mut self) -> &mut dyn Read {
        &mut self.output
    }
    fn take_stderr(&mut self) -> Option<Box<dyn Read + Send>> {
        self.stderr
            .take()
            .map(|e| Box::new(LateStderr(std::io::Cursor::new(e), false)) as Box<dyn Read + Send>)
    }
    fn finish(&mut self) -> i32 {
        0
    }
}

impl Backend for Fake {
    fn hash_kind(&self) -> HashKind {
        HashKind::Sha1
    }
    fn refs(&mut self) -> Vec<(String, ObjectId)> {
        self.refs.iter().map(|(k, v)| (k.clone(), *v)).collect()
    }
    fn is_bare(&self) -> bool {
        self.bare
    }
    fn head_branch(&mut self) -> Option<String> {
        self.head.clone()
    }
    fn checked_out(&mut self, name: &str) -> Option<String> {
        (!self.bare && self.head.as_deref() == Some(name)).then(String::new)
    }
    fn has_object(&mut self, oid: &ObjectId) -> bool {
        self.objects.contains(oid)
    }
    fn is_ancestor(&mut self, ancestor: &ObjectId, descendant: &ObjectId) -> bool {
        // A is the parent of B.
        ancestor == descendant || (ancestor.to_hex() == A && descendant.to_hex() == B)
    }
    fn ingest(
        &mut self,
        input: &mut dyn Read,
        opts: &IngestOptions,
        _sink: &dyn Sink,
    ) -> Result<(), String> {
        let mut rest = Vec::new();
        input.read_to_end(&mut rest).unwrap();
        self.ingested = Some(opts.clone());
        self.quarantined = true;
        self.objects.extend(self.pack.iter().copied());
        Ok(())
    }
    fn is_connected(&mut self, tips: &[ObjectId], _sink: &dyn Sink) -> bool {
        tips.iter().all(|t| self.objects.contains(t))
    }
    fn run_hook(&mut self, name: &str, call: &HookCall<'_>, sink: &dyn Sink) -> Option<i32> {
        let code = *self.hooks.get(name)?;
        let stdin = call
            .stdin
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .unwrap_or_default();
        self.hook_calls
            .push((name.to_string(), call.args.clone(), stdin, call.env.clone()));
        sink.write(format!("{name} ran\n").as_bytes());
        Some(code)
    }
    fn start_proc_receive(
        &mut self,
        _env: &[(String, String)],
        sink: &dyn Sink,
    ) -> Option<Box<dyn ProcReceive>> {
        let (out, err) = self.proc_receive.clone()?;
        Some(Box::new(FakeProc {
            input: Vec::new(),
            output: std::io::Cursor::new(out),
            stderr: sink.is_sideband().then_some(err),
        }))
    }
    fn migrate_quarantine(&mut self) -> Result<(), String> {
        self.quarantined = false;
        Ok(())
    }
    fn discard_quarantine(&mut self) {
        if self.quarantined {
            for o in &self.pack {
                self.objects.remove(o);
            }
        }
        self.quarantined = false;
    }
    fn update_refs(
        &mut self,
        changes: &[RefChange],
        _atomic: bool,
    ) -> Result<Vec<Result<(), RefFailure>>, String> {
        let mut out = Vec::new();
        for c in changes {
            let cur = self.refs.get(&c.name).copied();
            match c.old {
                Some(o) if !o.is_null() && cur != Some(o) => {
                    out.push(Err(RefFailure {
                        reason: m::REASON_REF_OLD_VALUE.to_string(),
                        message: format!("cannot lock ref '{}'", c.name),
                    }));
                    continue;
                }
                _ => {}
            }
            if c.new.is_null() {
                self.refs.remove(&c.name);
            } else {
                self.refs.insert(c.name.clone(), c.new);
            }
            out.push(Ok(()));
        }
        Ok(out)
    }
    fn update_worktree(
        &mut self,
        _worktree: &str,
        _new: &ObjectId,
        _env: &[(String, String)],
        _sink: &dyn Sink,
    ) -> Result<(), String> {
        Err(m::UNSTAGED_CHANGES.to_string())
    }
}

fn pkt(s: &str) -> Vec<u8> {
    pktline::encode(s.as_bytes()).unwrap()
}

/// A request: commands (the first with `caps`), flush, options, pack.
fn request(caps: &str, commands: &[&str], options: Option<&[&str]>, pack: bool) -> Vec<u8> {
    let mut out = Vec::new();
    for (i, c) in commands.iter().enumerate() {
        if i == 0 {
            out.extend(pkt(&format!("{c}\0{caps}")));
        } else {
            out.extend(pkt(c));
        }
    }
    out.extend_from_slice(b"0000");
    if let Some(opts) = options {
        for o in opts {
            out.extend(pkt(o));
        }
        out.extend_from_slice(b"0000");
    }
    if pack {
        out.extend_from_slice(b"PACK\0\0\0\x02\0\0\0\x01");
    }
    out
}

fn settings() -> Settings {
    Settings::default()
}

fn opts() -> Options {
    Options {
        agent: "git/2.56.0-Linux".to_string(),
        stateless_rpc: true,
        ..Default::default()
    }
}

fn run(fake: &mut Fake, s: &Settings, o: &Options, input: &[u8]) -> Result<Vec<u8>, Fatal> {
    let mut out = Vec::new();
    serve(fake, s, o, input, &mut out)?;
    Ok(out)
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn repo() -> Fake {
    let mut f = Fake {
        bare: true,
        head: Some("refs/heads/main".to_string()),
        ..Default::default()
    };
    f.refs.insert("refs/heads/main".to_string(), oid(A));
    f.refs.insert("refs/tags/v1".to_string(), oid(A));
    f.objects.insert(oid(A));
    f.pack.push(oid(B));
    f
}

#[test]
fn advertisement() {
    let mut empty = Fake::default();
    let o = Options {
        stateless_rpc: false,
        advertise_refs: true,
        ..opts()
    };
    let out = run(&mut empty, &settings(), &o, b"").unwrap();
    assert_eq!(
        text(&out),
        format!(
            "00b7{Z} capabilities^{{}}\0{} atomic ofs-delta object-format=sha1 \
             agent=git/2.56.0-Linux\n0000",
            m::BASE_CAPABILITIES
        )
    );
    let mut r = repo();
    let s = Settings {
        advertise_atomic: false,
        advertise_push_options: true,
        hide_refs: vec!["refs/tags".to_string()],
        ..settings()
    };
    let o = Options {
        push_cert_nonce: Some("1-abc".to_string()),
        ..o
    };
    let out = run(&mut r, &s, &o, b"").unwrap();
    let expect = format!(
        "{A} refs/heads/main\0{} ofs-delta push-cert=1-abc push-options \
         object-format=sha1 agent=git/2.56.0-Linux\n",
        m::BASE_CAPABILITIES
    );
    assert_eq!(text(&out), format!("{:04x}{expect}0000", expect.len() + 4));
}

#[test]
fn namespace_and_alternates_become_have_lines() {
    let mut r = repo();
    r.refs
        .insert("refs/namespaces/ns/refs/heads/x".to_string(), oid(B));
    let o = Options {
        advertise_refs: true,
        namespace: Some("refs/namespaces/ns/".to_string()),
        ..opts()
    };
    let out = text(&run(&mut r, &settings(), &o, b"").unwrap());
    // main and v1 share an id: one .have line.
    assert!(out.contains(&format!("{A} .have\0")));
    assert_eq!(out.matches(".have").count(), 1);
    assert!(out.contains(&format!("{B} refs/heads/x\n")));
}

#[test]
fn update_with_report_and_sideband() {
    let mut r = repo();
    let input = request(
        "report-status side-band-64k",
        &[&format!("{A} {B} refs/heads/main")],
        None,
        true,
    );
    let out = run(&mut r, &settings(), &opts(), &input).unwrap();
    // Observed from git: the report inside one sideband packet, then a
    // flush.
    assert_eq!(
        text(&out),
        "002e\x01000eunpack ok\n0017ok refs/heads/main\n00000000"
    );
    assert_eq!(r.refs["refs/heads/main"], oid(B));
    let ing = r.ingested.unwrap();
    assert!(!ing.keep_pack && ing.objects == 1 && ing.version == 2);
}

#[test]
fn policies() {
    let mut r = repo();
    r.bare = false;
    r.objects.insert(oid(B));
    let input = request(
        "report-status",
        &[
            &format!("{A} {B} refs/heads/main"),
            &format!("{Z} {B} HEAD"),
            &format!("{Z} {B} refs/x"),
            &format!("{A} {Z} refs/tags/v1"),
            &format!("{Z} {Z} refs/heads/gone"),
        ],
        None,
        true,
    );
    let s = Settings {
        deny_deletes: true,
        ..settings()
    };
    let out = text(&run(&mut r, &s, &opts(), &input).unwrap());
    assert_eq!(
        out,
        "000eunpack ok\n\
         0037ng refs/heads/main branch is currently checked out\n\
         001ang HEAD funny refname\n\
         001cng refs/x funny refname\n\
         0014ok refs/tags/v1\n\
         0017ok refs/heads/gone\n\
         0000"
    );
    assert!(!r.refs.contains_key("refs/tags/v1"));
}

#[test]
fn atomic_failure_rejects_all() {
    let mut r = repo();
    r.bare = false;
    let input = request(
        "report-status atomic",
        &[
            &format!("{Z} {B} refs/heads/new"),
            &format!("{A} {B} refs/heads/main"),
        ],
        None,
        true,
    );
    let out = text(&run(&mut r, &settings(), &opts(), &input).unwrap());
    assert!(out.contains("ng refs/heads/new atomic push failure\n"));
    assert!(out.contains("ng refs/heads/main branch is currently checked out\n"));
    assert!(!r.refs.contains_key("refs/heads/new"));
}

#[test]
fn duplicates_fail_the_batch() {
    let mut r = repo();
    let input = request(
        "report-status",
        &[
            &format!("{Z} {B} refs/heads/y"),
            &format!("{Z} {B} refs/heads/y"),
        ],
        None,
        true,
    );
    let out = text(&run(&mut r, &settings(), &opts(), &input).unwrap());
    assert_eq!(out.matches("failed to update refs").count(), 2);
}

#[test]
fn hooks_and_push_options() {
    let mut r = repo();
    r.hooks.insert("pre-receive".to_string(), 0);
    r.hooks.insert("update".to_string(), 0);
    r.hooks.insert("post-receive".to_string(), 0);
    r.hooks.insert("post-update".to_string(), 0);
    let s = Settings {
        advertise_push_options: true,
        ..settings()
    };
    let input = request(
        "report-status push-options",
        &[&format!("{A} {B} refs/heads/main")],
        Some(&["a=1", "b"]),
        true,
    );
    run(&mut r, &s, &opts(), &input).unwrap();
    let names: Vec<&str> = r.hook_calls.iter().map(|c| c.0.as_str()).collect();
    assert_eq!(
        names,
        ["pre-receive", "update", "post-receive", "post-update"]
    );
    let pre = &r.hook_calls[0];
    assert_eq!(pre.2, format!("{A} {B} refs/heads/main\n"));
    assert!(
        pre.3
            .contains(&("GIT_PUSH_OPTION_COUNT".to_string(), "2".to_string()))
    );
    assert!(
        pre.3
            .contains(&("GIT_PUSH_OPTION_1".to_string(), "b".to_string()))
    );
    assert_eq!(r.hook_calls[1].1, ["refs/heads/main", A, B]);
    assert_eq!(r.hook_calls[3].1, ["refs/heads/main"]);
}

#[test]
fn pre_receive_decline_discards_the_pack() {
    let mut r = repo();
    r.hooks.insert("pre-receive".to_string(), 1);
    let input = request(
        "report-status",
        &[&format!("{A} {B} refs/heads/main")],
        None,
        true,
    );
    let out = text(&run(&mut r, &settings(), &opts(), &input).unwrap());
    assert!(out.contains("ng refs/heads/main pre-receive hook declined"));
    assert!(!r.objects.contains(&oid(B)));
}

#[test]
fn bad_input() {
    let mut r = repo();
    let input = request("report-status", &["garbage"], None, false);
    assert_eq!(
        run(&mut r, &settings(), &opts(), &input),
        Err(Fatal(m::expected_old_new_ref("garbage")))
    );
    let input = request(
        "report-status object-format=sha256",
        &[&format!("{A} {B} refs/heads/main")],
        None,
        true,
    );
    assert_eq!(
        run(&mut r, &settings(), &opts(), &input),
        Err(Fatal(m::unsupported_object_format("sha256")))
    );
    // A short pack header.
    let mut input = request(
        "report-status",
        &[&format!("{A} {B} refs/heads/main")],
        None,
        false,
    );
    input.extend_from_slice(b"PAC");
    let out = text(&run(&mut r, &settings(), &opts(), &input).unwrap());
    assert!(out.contains(&format!("unpack {}\n", m::EOF_BEFORE_HEADER)));
    assert!(out.contains("ng refs/heads/main unpacker error\n"));
}

/// Complaints about the `proc-receive` report come after everything the
/// hook wrote to standard error, however late the relay delivers it
/// (t5411 failed now and then under load, #741).
#[test]
fn proc_receive_errors_follow_the_hook_output() {
    let cases: [(&[&str], &str); 3] = [
        (
            &["xx refs/for/main/topic"],
            "error: proc-receive reported bad status \"xx\" on ref \"refs/for/main/topic\"",
        ),
        (
            &["ok refs/heads/main"],
            "error: proc-receive reported status on unexpected ref: refs/heads/main",
        ),
        (
            &["option refname refs/pull/1/head", "option old-oid 0"],
            "error: proc-receive reported \"option\" without a matching \"ok/ng\" directive",
        ),
    ];
    for (report, error) in cases {
        let mut r = repo();
        let mut out = pkt("version=1\n");
        out.extend_from_slice(b"0000");
        let mut err = Vec::new();
        for l in report {
            out.extend(pkt(&format!("{l}\n")));
            err.extend_from_slice(format!("proc-receive> {l}\n").as_bytes());
        }
        out.extend_from_slice(b"0000");
        r.proc_receive = Some((out, err));
        let mut s = settings();
        s.proc_receive_refs.push(settings::ProcReceiveRef {
            prefix: "refs/for".to_string(),
            negated: false,
            create: true,
            modify: true,
            delete: true,
        });
        let input = request(
            "report-status side-band-64k",
            &[
                &format!("{A} {B} refs/heads/main"),
                &format!("{Z} {B} refs/for/main/topic"),
            ],
            None,
            true,
        );
        let got = text(&run(&mut r, &s, &opts(), &input).unwrap());
        let last_hook_line = got
            .rfind("proc-receive> ")
            .unwrap_or_else(|| panic!("{got:?}"));
        let error_at = got.find(error).unwrap_or_else(|| panic!("{got:?}"));
        assert!(last_hook_line < error_at, "{got:?}");
        assert_eq!(got.matches("error: proc-receive").count(), 1, "{got:?}");
    }
}
