// SPDX-License-Identifier: MIT
//! Helpers for the protocol v2 client tests against the reference git:
//! repositories with fixed commit times, a commit graph for the negotiator,
//! upload-pack connections (stateful, stateless, git daemon) and a driver
//! that does what `git fetch`/`git clone` do on the wire.

#![allow(dead_code)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::rc::Rc;

use rgit_hash::ObjectId;
use rgit_protocol::v2::{
    Client, CommitInfo, ConsecutiveNegotiator, Duplex, FetchArgs, FetchOutcome, LsRefsArgs,
    RemoteRef,
};
use rgit_protocol::{Progress, ProgressStyle};

use crate::common;

pub const USER_AGENT: &str = "rgit-protocol-test";

pub struct Env {
    pub git: PathBuf,
    pub home: PathBuf,
}

impl Env {
    pub fn cmd(&self) -> Command {
        common::git(&self.git, &self.home)
    }

    /// `git -C <repo> <args>`, required to succeed; stdout.
    pub fn run(&self, repo: &Path, args: &[&str]) -> String {
        let mut c = self.cmd();
        c.arg("-C").arg(repo).args(args);
        String::from_utf8(common::run(c).stdout).unwrap()
    }

    pub fn init(&self, name: &str) -> PathBuf {
        let repo = self.home.join(name);
        let mut c = self.cmd();
        c.args(["init", "-q", "-b", "main"]).arg(&repo);
        common::run(c);
        repo
    }

    pub fn clone(&self, from: &Path, name: &str, extra: &[&str]) -> PathBuf {
        let to = self.home.join(name);
        let mut c = self.cmd();
        c.args(["clone", "-q"]).args(extra).arg(from).arg(&to);
        common::run(c);
        to
    }

    /// Feed a fast-import stream into `repo`.
    pub fn fast_import(&self, repo: &Path, stream: &str) {
        let mut c = self.cmd();
        c.arg("-C")
            .arg(repo)
            .args(["fast-import", "--quiet"])
            .stdin(Stdio::piped());
        let mut child = c.spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stream.as_bytes())
            .unwrap();
        assert!(child.wait().unwrap().success());
    }

    /// Add `n` empty commits named `<prefix><i>` to `branch`, committed at
    /// `t0 + i`. `from` starts the chain (`None`: continue the branch, or a
    /// root commit for a new branch).
    pub fn commits(
        &self,
        repo: &Path,
        branch: &str,
        from: Option<&str>,
        prefix: &str,
        n: usize,
        t0: i64,
    ) {
        let mut c = self.cmd();
        c.arg("-C")
            .arg(repo)
            .args(["rev-parse", "--verify", "-q"])
            .arg(format!("refs/heads/{branch}"));
        let existing = c.output().unwrap();
        let tip = String::from_utf8(existing.stdout).unwrap();
        let tip = tip.trim();
        let from = from.or((!tip.is_empty()).then_some(tip));
        let mut s = String::new();
        for i in 1..=n {
            let msg = format!("{prefix}{i}");
            s.push_str(&format!(
                "commit refs/heads/{branch}\ncommitter C <c@example.com> {} +0000\ndata {}\n{msg}\n",
                t0 + i as i64,
                msg.len()
            ));
            if i == 1 {
                if let Some(f) = from {
                    s.push_str(&format!("from {f}\n"));
                }
            }
            s.push('\n');
        }
        self.fast_import(repo, &s);
    }

    /// The commits of `repo` (all refs) for the negotiator.
    pub fn graph(&self, repo: &Path) -> HashMap<ObjectId, CommitInfo> {
        let out = self.run(repo, &["log", "--all", "--format=%H %ct %P"]);
        out.lines()
            .map(|l| {
                let mut w = l.split(' ').filter(|s| !s.is_empty());
                let id = oid(w.next().unwrap());
                let time = w.next().unwrap().parse().unwrap();
                let parents = w.map(oid).collect();
                (id, CommitInfo { time, parents })
            })
            .collect()
    }

    /// The local ref tips (`for-each-ref` order).
    pub fn tips(&self, repo: &Path) -> Vec<ObjectId> {
        self.run(repo, &["for-each-ref", "--format=%(objectname)"])
            .lines()
            .map(oid)
            .collect()
    }

    /// `git upload-pack <repo>` speaking v2 on its stdin/stdout.
    pub fn upload_pack(&self, repo: &Path) -> Child {
        let mut c = self.cmd();
        c.env("GIT_PROTOCOL", "version=2")
            .env("GIT_USER_AGENT", USER_AGENT)
            .arg("upload-pack")
            .arg(repo)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        c.spawn().unwrap()
    }

    /// Check a received pack with `git index-pack --fix-thin` in `repo`.
    pub fn index_pack(&self, repo: &Path, pack: &[u8]) {
        assert!(pack.starts_with(b"PACK"), "no pack received");
        let mut c = self.cmd();
        c.arg("-C")
            .arg(repo)
            .args(["index-pack", "--stdin", "--fix-thin"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = c.spawn().unwrap();
        child.stdin.take().unwrap().write_all(pack).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "index-pack: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

pub fn oid(hex: &str) -> ObjectId {
    ObjectId::from_hex_any(hex.trim().as_bytes()).unwrap()
}

/// Stateless upload-pack: what is written up to a flush becomes one
/// `git upload-pack --stateless-rpc` run, whose output (followed by a
/// response-end packet, as git's HTTP helper adds) is read next. The
/// advertisement comes from `--http-backend-info-refs`.
pub struct Stateless {
    inner: Rc<RefCell<StatelessInner>>,
}

struct StatelessInner {
    git: PathBuf,
    home: PathBuf,
    repo: PathBuf,
    request: Vec<u8>,
    response: io::Cursor<Vec<u8>>,
    pub requests: usize,
}

impl Stateless {
    /// Reader and writer halves.
    pub fn new(env: &Env, repo: &Path, service_header: bool) -> (Stateless, Stateless) {
        let mut c = env.cmd();
        c.env("GIT_PROTOCOL", "version=2")
            .env("GIT_USER_AGENT", USER_AGENT)
            .arg("upload-pack")
            .arg("--http-backend-info-refs")
            .arg(repo);
        let mut adv = Vec::new();
        if service_header {
            adv.extend_from_slice(b"001e# service=git-upload-pack\n0000");
        }
        adv.extend(common::run(c).stdout);
        let inner = Rc::new(RefCell::new(StatelessInner {
            git: env.git.clone(),
            home: env.home.clone(),
            repo: repo.to_owned(),
            request: Vec::new(),
            response: io::Cursor::new(adv),
            requests: 0,
        }));
        (
            Stateless {
                inner: inner.clone(),
            },
            Stateless { inner },
        )
    }

    pub fn requests(&self) -> usize {
        self.inner.borrow().requests
    }
}

impl Read for Stateless {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.borrow_mut().response.read(buf)
    }
}

impl Write for Stateless {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.borrow_mut().request.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut inner = self.inner.borrow_mut();
        if inner.request.is_empty() {
            return Ok(());
        }
        let request = std::mem::take(&mut inner.request);
        let mut c = common::git(&inner.git, &inner.home);
        c.env("GIT_PROTOCOL", "version=2")
            .env("GIT_USER_AGENT", USER_AGENT)
            .arg("upload-pack")
            .arg("--stateless-rpc")
            .arg(&inner.repo)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        let mut child = c.spawn()?;
        let mut stdin = child.stdin.take().unwrap();
        let writer = std::thread::spawn(move || stdin.write_all(&request));
        let out = child.wait_with_output()?;
        writer.join().unwrap()?;
        assert!(out.status.success());
        let mut response = out.stdout;
        response.extend_from_slice(b"0002");
        inner.response = io::Cursor::new(response);
        inner.requests += 1;
        Ok(())
    }
}

/// What the driver should do.
pub struct Plan {
    /// `ls-refs` arguments.
    pub ls_refs: LsRefsArgs,
    /// Fetch arguments without wants.
    pub fetch: FetchArgs,
    /// Clone: want every listed ref, no negotiation.
    pub clone: bool,
}

pub struct Result {
    pub refs: Vec<RemoteRef>,
    pub outcome: FetchOutcome,
    pub pack: Vec<u8>,
}

/// ls-refs and fetch the way git does: wants are the listed refs under
/// `refs/heads/` whose objects are missing locally (clone: all listed refs);
/// advertised tips present locally are known common; the walk starts at
/// all local refs.
pub fn drive<C: Duplex>(
    env: &Env,
    client: &mut Client<C>,
    local: Option<&Path>,
    plan: &Plan,
) -> Result {
    client.set_agent(USER_AGENT);
    let refs = client.ls_refs(&plan.ls_refs).unwrap();
    let graph = local.map(|r| env.graph(r)).unwrap_or_default();
    let mut args = plan.fetch.clone();
    for r in &refs {
        let Some(id) = r.oid else { continue };
        if plan.clone || (r.name.starts_with(b"refs/heads/") && !graph.contains_key(&id)) {
            args.wants.push(id);
        }
    }
    let tips = local.map(|r| env.tips(r)).unwrap_or_default();
    let lookup = graph.clone();
    let mut negotiator = ConsecutiveNegotiator::new(move |id: &ObjectId| lookup.get(id).cloned());
    if !plan.clone {
        for r in &refs {
            if let Some(id) = r.oid {
                if graph.contains_key(&id) {
                    negotiator.known_common(id);
                }
            }
        }
        for t in tips {
            negotiator.add_tip(t);
        }
    }
    let mut progress = Progress::new(Vec::new(), ProgressStyle::Spaces);
    let mut pack = Vec::new();
    let outcome = if args.wants.is_empty() {
        client.close().unwrap();
        FetchOutcome::default()
    } else {
        client
            .fetch(&args, &mut negotiator, &mut progress, |d| {
                pack.extend_from_slice(d);
                Ok(())
            })
            .unwrap()
    };
    Result {
        refs,
        outcome,
        pack,
    }
}

/// The standard fetch arguments of `git fetch` (no progress on a pipe).
pub fn fetch_args() -> FetchArgs {
    FetchArgs {
        thin_pack: true,
        no_progress: true,
        include_tag: true,
        ofs_delta: true,
        ..FetchArgs::default()
    }
}
