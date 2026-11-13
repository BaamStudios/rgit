// SPDX-License-Identifier: MIT
//! Differential test of the ssh command line: the reference git and
//! rgit's [`SshTransport`] both run the same recording fake `ssh`, and the
//! recorded argument vectors (and `GIT_PROTOCOL`) must be byte-identical.
//!
//! The cases follow the ssh and URL parts of `t5601-clone.sh` plus further
//! observations. The test is skipped when no reference git is found (see
//! `rgit-difftest`).

#![cfg(unix)]

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use rgit_config::{MapEnv, Origin, Scope};
use rgit_protocol::ProtocolVersion;
use rgit_transport::url::{Remote, parse_remote};
use rgit_transport::{ConnectOptions, IpVersion, Service, SshCommand, SshTransport, Transport};

/// Records its arguments (NUL-terminated) and `GIT_PROTOCOL`, then fails.
const RECORDER: &str = r#"#!/bin/sh
{ for a in "$@"; do printf '%s\0' "$a"; done; printf 'GIT_PROTOCOL=%s\n' "${GIT_PROTOCOL-unset}"; } >>"$OUT"
exit 1
"#;

/// Like the recorder, but succeeds for OpenSSH's `-G` probe.
const SSHLIKE: &str = r#"#!/bin/sh
{ for a in "$@"; do printf '%s\0' "$a"; done; printf 'GIT_PROTOCOL=%s\n' "${GIT_PROTOCOL-unset}"; } >>"$OUT"
test "$1" = "-G" && exit 0
exit 1
"#;

struct Case {
    url: &'static str,
    /// Environment; `$T` is replaced by the temporary directory.
    env: Vec<(&'static str, &'static str)>,
    /// `-c` settings; `$T` is replaced as above.
    config: Vec<(&'static str, &'static str)>,
    version: u8,
    ip: IpVersion,
    upload_pack: Option<&'static str>,
}

fn case(url: &'static str) -> Case {
    Case {
        url,
        env: vec![("GIT_SSH", "$T/ssh")],
        config: Vec::new(),
        version: 0,
        ip: IpVersion::Any,
        upload_pack: None,
    }
}

impl Case {
    fn ssh(mut self, prog: &'static str) -> Self {
        self.env = vec![("GIT_SSH", prog)];
        self
    }
    fn env(mut self, k: &'static str, v: &'static str) -> Self {
        self.env.push((k, v));
        self
    }
    fn config(mut self, k: &'static str, v: &'static str) -> Self {
        self.config.push((k, v));
        self
    }
    fn v(mut self, version: u8) -> Self {
        self.version = version;
        self
    }
    fn ip(mut self, ip: IpVersion) -> Self {
        self.ip = ip;
        self
    }
    fn upload_pack(mut self, p: &'static str) -> Self {
        self.upload_pack = Some(p);
        self
    }
}

fn subst(s: &str, t: &Path) -> String {
    s.replace("$T", &t.display().to_string())
}

fn setup() -> Option<(PathBuf, tempfile::TempDir)> {
    let git = rgit_difftest::env::reference_git()?;
    let dir = tempfile::tempdir().unwrap();
    let t = dir.path();
    for name in [
        "ssh",
        "plink",
        "plink.exe",
        "putty",
        "tortoiseplink",
        "simple",
        "uplink",
    ] {
        let p = t.join(name);
        fs::write(&p, RECORDER).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let p = t.join("sshlike");
    fs::write(&p, SSHLIKE).unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    let status = Command::new(&git)
        .args(["init", "-q"])
        .arg(t.join("repo"))
        .env("HOME", t)
        .env("XDG_CONFIG_HOME", t.join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .status()
        .unwrap();
    assert!(status.success());
    Some((git, dir))
}

/// Run the reference git; returns (recording, stderr).
fn run_git(git: &Path, t: &Path, c: &Case) -> (Vec<u8>, String) {
    let out = t.join("out-git");
    let _ = fs::remove_file(&out);
    let mut cmd = Command::new(git);
    cmd.arg("-c").arg(format!("protocol.version={}", c.version));
    for (k, v) in &c.config {
        cmd.arg("-c").arg(format!("{k}={}", subst(v, t)));
    }
    cmd.arg("-C").arg(t.join("repo")).arg("fetch");
    match c.ip {
        IpVersion::V4 => {
            cmd.arg("-4");
        }
        IpVersion::V6 => {
            cmd.arg("-6");
        }
        IpVersion::Any => {}
    }
    if let Some(p) = c.upload_pack {
        cmd.arg(format!("--upload-pack={p}"));
    }
    cmd.arg(c.url);
    cmd.env("HOME", t)
        .env("XDG_CONFIG_HOME", t.join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("OUT", &out)
        .env_remove("GIT_SSH")
        .env_remove("GIT_SSH_COMMAND")
        .env_remove("GIT_SSH_VARIANT")
        .env_remove("GIT_PROTOCOL");
    for (k, v) in &c.env {
        cmd.env(k, subst(v, t));
    }
    let o = cmd.output().unwrap();
    (
        fs::read(&out).unwrap_or_default(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

/// Run rgit's transport; returns (recording, error message).
fn run_rgit(t: &Path, c: &Case) -> (Vec<u8>, Option<String>) {
    let out = t.join("out-rgit");
    let _ = fs::remove_file(&out);
    let env = MapEnv::new(c.env.iter().map(|(k, v)| (*k, subst(v, t))));
    let mut text = String::new();
    for (k, v) in &c.config {
        let (section, key) = k.split_once('.').unwrap();
        text.push_str(&format!("[{section}]\n\t{key} = \"{}\"\n", subst(v, t)));
    }
    let config =
        rgit_config::parse_bytes(text.as_bytes(), Origin::File("t".into()), Scope::Command)
            .unwrap();
    let target = match parse_remote(c.url, &|_| false) {
        Ok(Remote::Ssh(target)) => target,
        Ok(other) => panic!("{}: not ssh: {other:?}", c.url),
        Err(e) => return (Vec::new(), Some(e.to_string())),
    };
    let mut transport = SshTransport {
        target,
        ssh: SshCommand::from_config(&env, &config),
        ip: c.ip,
        base_env: vec![(OsString::from("OUT"), out.clone().into_os_string())],
    };
    let mut opts = ConnectOptions::new(Service::UploadPack);
    opts.protocol_version = match c.version {
        0 => ProtocolVersion::V0,
        1 => ProtocolVersion::V1,
        _ => ProtocolVersion::V2,
    };
    opts.program = c.upload_pack.map(String::from);
    let err = match transport.connect(&opts) {
        Ok(conn) => {
            let _ = conn.close();
            None
        }
        Err(e) => Some(e.to_string()),
    };
    (fs::read(&out).unwrap_or_default(), err)
}

fn show(rec: &[u8]) -> String {
    String::from_utf8_lossy(rec).replace('\0', " | ")
}

fn check(cases: Vec<Case>) {
    let Some((git, dir)) = setup() else {
        eprintln!("ssh_argv: skipping, no reference git");
        return;
    };
    let t = dir.path();
    let mut recorded = 0;
    let total = cases.len();
    for c in cases {
        let (want, stderr) = run_git(&git, t, &c);
        if !want.is_empty() {
            recorded += 1;
        }
        let (got, err) = run_rgit(t, &c);
        let label = format!(
            "url {} env {:?} config {:?} v{} {:?}",
            c.url, c.env, c.config, c.version, c.ip
        );
        assert_eq!(show(&got), show(&want), "{label}");
        if let Some(e) = err {
            assert!(
                stderr.contains(&format!("fatal: {e}")),
                "{label}: rgit error {e:?}, git stderr {stderr:?}"
            );
        }
    }
    // Most cases reach ssh; guards against a harness that records nothing.
    assert!(recorded * 4 >= total * 3, "{recorded} of {total} recorded");
}

#[test]
fn variants_like_t5601() {
    use IpVersion::*;
    check(vec![
        case("myhost:src"),
        case("myhost:src").v(2),
        case("[myhost:123]:src"),
        case("[myhost:123]:src").v(2),
        case("[myhost:123]:src").ip(V4),
        case("[myhost:123]:src").ip(V6).v(2),
        case("[myhost:123]:src")
            .ssh("$T/putty")
            .config("ssh.variant", "putty")
            .ip(V4),
        case("[myhost:123]:src")
            .ssh("$T/plink")
            .config("ssh.variant", "auto")
            .ip(V4),
        case("myhost:src").ssh("$T/simple").ip(V4),
        case("[myhost:123]:src").ssh("$T/simple"),
        case("[myhost:123]:src").ssh("$T/uplink"),
        case("myhost:src").ssh("$T/uplink"),
        case("myhost:src").ssh("$T/uplink").v(2),
        case("[myhost:123]:src").ssh("$T/sshlike"),
        case("[myhost:123]:src").ssh("$T/sshlike").v(2),
        case("[myhost:123]:src").ssh("$T/plink"),
        case("[myhost:123]:src").ssh("$T/plink").v(2),
        case("[myhost:123]:src").ssh("$T/plink.exe"),
        case("[myhost:123]:src").ssh("$T/tortoiseplink"),
        case("[myhost:123]:src").ssh("$T/tortoiseplink").ip(V4),
        case("[myhost:123]:src").env("GIT_SSH_COMMAND", "\"$T/plink.exe\" -v"),
        case("[myhost:123]:src").env("GIT_SSH_COMMAND", "'$T/plink.exe' -v"),
        case("[myhost:123]:src").env("GIT_SSH_COMMAND", "'$T/plink.exe -v"),
        case("[myhost:123]:src")
            .env("GIT_SSH_COMMAND", "$T/ssh -v")
            .v(2),
        case("[myhost:123]:src")
            .ssh("$T/plink")
            .env("GIT_SSH_VARIANT", "ssh"),
        case("[myhost:123]:src")
            .ssh("$T/plink")
            .config("ssh.variant", "ssh")
            .v(2),
        case("[myhost:123]:src")
            .ssh("$T/plink")
            .env("GIT_SSH_VARIANT", "plink"),
        case("[myhost:123]:src")
            .ssh("$T/plink")
            .env("GIT_SSH_VARIANT", "tortoiseplink"),
        case("[myhost:123]:src")
            .env("GIT_SSH_VARIANT", "auto")
            .config("ssh.variant", "plink")
            .v(2),
        case("[myhost:123]:src").config("core.sshCommand", "$T/plink -x"),
        case("myhost:src").config("ssh.variant", "simple").v(2),
        case("myhost:src").env("GIT_SSH_VARIANT", "foo").v(2),
    ]);
}

#[test]
fn urls_like_t5601() {
    let mut cases = Vec::new();
    for url in [
        "c:temp",
        "host:rep",
        "host:rep/home/project",
        "host:123",
        "[::1]:rep",
        "[::1]:rep/home/project",
        "[::1]:123",
        "host:/~repo",
        "[::1]:/~repo",
        "ssh://host.xz/home/user/repo",
        "ssh://host.xz:/home/user/repo",
        "ssh://host.xz/~repo",
        "ssh://host.xz:/~repo",
        "ssh://host.xz:22/home/user/repo",
        "ssh://host.xz:22/~repo",
        "ssh://::1/home/user/repo",
        "ssh://[::1]/home/user/repo",
        "ssh://[::1]:/home/user/repo",
        "ssh://user@::1/home/user/repo",
        "ssh://user@[::1]/home/user/repo",
        "ssh://user@[::1]:/home/user/repo",
        "ssh://[user@::1]/home/user/repo",
        "ssh://[user@::1]:/home/user/repo",
        "ssh://[::1]:22/home/user/repo",
        "ssh://user@[::1]:22/~repo",
        "ssh://[user@::1]:22/~repo",
        "ssh://host:abc/r",
        "ssh://host:-22/r",
        "ssh://host:99999/r",
        "ssh://host:0/r",
        "ssh://user:pw@host/r",
        "ssh://us%40er@ho%41st/r%20epo",
        "git+ssh://host/repo",
        "ssh+git://host/repo",
        "ssh://host/-repo",
        "host:it's",
        "host:a!b",
        "host:repo with space",
        "[user@host]:repo",
        "[host:22:33]:x",
        "ssh://-oProxyCommand=x/repo",
        "[-x]:repo",
        "host:-repo",
        "ssh://host",
    ] {
        cases.push(case(url));
        cases.push(case(url).v(2));
    }
    cases.push(case("host:x").upload_pack("foo bar"));
    cases.push(case("host:x").upload_pack("/opt/git-upload-pack").v(2));
    check(cases);
}
