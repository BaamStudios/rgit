// SPDX-License-Identifier: MIT
//! Differential tests against the reference `git` binary: refspec
//! validation (through `ls-remote --get-url`), the native transport URL
//! split (`fetch-pack --diag-url`), the clone directory name, `insteadOf`
//! rewriting and `@{upstream}` / `@{push}`. Skipped when no reference git
//! is found (see `rgit-difftest`).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use bstr::ByteSlice;
use rgit_config::{Origin, Scope, parse_bytes};
use rgit_remote::branch::{BranchConfig, PushDefault, push_ref, upstream_ref};
use rgit_remote::refspec::{Direction, Refspec};
use rgit_remote::remote::Remotes;
use rgit_remote::url::{Protocol, humanish_dir, parse_connect_url};

struct Git {
    bin: PathBuf,
    tmp: tempfile::TempDir,
}

impl Git {
    fn new() -> Option<Git> {
        let bin = rgit_difftest::env::reference_git()?;
        Some(Git {
            bin,
            tmp: tempfile::tempdir().unwrap(),
        })
    }

    fn cmd(&self, dir: &Path) -> Command {
        let mut c = Command::new(&self.bin);
        c.env_clear();
        if let Some(p) = std::env::var_os("PATH") {
            c.env("PATH", p);
        }
        c.env("HOME", self.tmp.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CEILING_DIRECTORIES", self.tmp.path())
            .env("LC_ALL", "C")
            .env("GIT_AUTHOR_NAME", "A U Thor")
            .env("GIT_AUTHOR_EMAIL", "author@example.com")
            .env("GIT_COMMITTER_NAME", "C O Mitter")
            .env("GIT_COMMITTER_EMAIL", "committer@example.com")
            .current_dir(dir);
        c
    }

    fn run(&self, dir: &Path, args: &[&[u8]]) -> Output {
        let mut c = self.cmd(dir);
        for a in args {
            c.arg(a.to_os_str().unwrap());
        }
        c.output().unwrap()
    }

    fn work(&self) -> PathBuf {
        let d = self.tmp.path().join("work");
        std::fs::create_dir_all(&d).unwrap();
        d
    }
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// Refspecs from t5511 plus generated combinations of prefixes, sources
/// and destinations.
fn refspec_cases() -> Vec<String> {
    let mut out: Vec<String> = [
        "",
        ":",
        "::",
        "+:",
        "refs/heads/*:refs/remotes/frotz/*",
        "refs/heads/*:refs/remotes/frotz",
        "refs/heads:refs/remotes/frotz/*",
        "refs/heads/main::refs/remotes/frotz/xyzzy",
        "refs/heads/maste :refs/remotes/frotz/xyzzy",
        "HEAD~4:refs/remotes/frotz/new",
        "refs/heads/ nitfol:",
        ":refs/remotes/frotz/HEAD to me",
        "refs/heads/*/for-linus:refs/remotes/mine/*-blah",
        "refs/heads*/for-linus:refs/remotes/mine/*",
        "refs/heads/*/*/for-linus:refs/remotes/mine/*",
        "refs/heads/*g*/for-linus:refs/remotes/mine/*",
        "refs/heads/\u{c4}",
        "refs/heads/\ttab",
        "a\\:b",
        "x:refs/heads/a@{",
        "a/./b:c",
        "foo.lock/x:c",
        "^refs/heads/x:y",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let prefixes = ["", "+", "^", "+^", "^+", "++"];
    let srcs = [
        "",
        "refs/heads/main",
        "main",
        "@",
        "refs/heads/*",
        "a*b",
        "**",
        "main~1",
        "a b",
        "x.lock",
        "0123456789012345678901234567890123456789",
        "0123456789abcdef",
        "a..b",
        "a:b",
        "@{1}",
        ".a",
        "a/",
    ];
    let dsts = [
        None,
        Some(""),
        Some("refs/remotes/o/*"),
        Some("refs/x"),
        Some("@"),
        Some("x y"),
        Some("*"),
        Some("a/*/b"),
        Some("a.lock"),
        Some("HEAD"),
    ];
    for p in prefixes {
        for s in srcs {
            for d in dsts {
                out.push(match d {
                    None => format!("{p}{s}"),
                    Some(d) => format!("{p}{s}:{d}"),
                });
            }
        }
    }
    out
}

#[test]
fn refspec_validation_matches_git() {
    let Some(git) = Git::new() else { return };
    let work = git.work();
    let mut failures = Vec::new();
    for spec in refspec_cases() {
        for (dir, key) in [(Direction::Fetch, "fetch"), (Direction::Push, "push")] {
            let kv = format!("remote.frotz.{key}={spec}");
            let o = git.run(
                &work,
                &[
                    b"-c",
                    b"remote.frotz.url=.",
                    b"-c",
                    kv.as_bytes(),
                    b"ls-remote",
                    b"--get-url",
                    b"frotz",
                ],
            );
            let ours = Refspec::parse(&spec, dir);
            let theirs_ok = o.status.success();
            let same = match &ours {
                Ok(_) => theirs_ok,
                Err(e) => !theirs_ok && stderr(&o) == format!("fatal: {e}\n"),
            };
            if !same {
                failures.push(format!(
                    "{key} {spec:?}: rgit {:?}, git {:?}",
                    ours.map(|_| ()),
                    stderr(&o)
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// URLs for `--diag-url`: the t5500 shapes plus generated combinations.
fn url_cases() -> Vec<String> {
    let mut out = Vec::new();
    for r in ["repo", "re:po", "re/po"] {
        for p in ["ssh+git", "git+ssh", "git", "ssh"] {
            for h in ["host", "user@host", "user@[::1]", "user@::1"] {
                for c in ["", ":"] {
                    out.push(format!("{p}://{h}{c}/{r}"));
                    out.push(format!("{p}://{h}{c}/~{r}"));
                }
            }
            for h in ["host", "User@host", "User@[::1]"] {
                out.push(format!("{p}://{h}:22/{r}"));
            }
        }
        out.push(format!("file://host/{r}"));
        out.push(format!("file://host/~{r}"));
        for h in ["nohost", "nohost:12", "[::1]", "[::1]:23", "[", "[:aa"] {
            out.push(format!("./{h}:{r}"));
            out.push(format!("./file:{h}/~{r}"));
        }
        for h in ["host", "[::1]"] {
            out.push(format!("{h}:{r}"));
            out.push(format!("{h}:/~{r}"));
        }
    }
    let schemes = ["ssh://", "git://", "file://", "", "http://", "SSH://"];
    let users = ["", "u@", "u:p@", "a@b@", "%41@"];
    let hosts = ["h", "[::1]", "::1", "[h:22]", "[u@h]", "h%3a1", "[::1"];
    let ports = ["", ":", ":22", ":+7", ":65536", ":x", ": 1"];
    let paths = ["/r", "/~u/r", "/~", "r", "/re:po", "/%41%zz", ""];
    let mut seed: u64 = 0x1234_5678;
    let mut pick = |n: usize| {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((seed >> 33) as usize) % n
    };
    for _ in 0..300 {
        let s = schemes[pick(schemes.len())];
        let u = users[pick(users.len())];
        let h = hosts[pick(hosts.len())];
        let p = ports[pick(ports.len())];
        let path = paths[pick(paths.len())];
        let url = if s.is_empty() {
            format!("{u}{h}{p}:{}", path.trim_start_matches('/'))
        } else {
            format!("{s}{u}{h}{p}{path}")
        };
        out.push(url);
    }
    out
}

fn diag(url: &str) -> (String, String) {
    match parse_connect_url(url.as_bytes()) {
        Ok(c) => {
            let mut s = format!("Diag: url={url}\nDiag: protocol={}\n", c.protocol.name());
            if c.protocol == Protocol::Ssh {
                s += &format!(
                    "Diag: userandhost={}\nDiag: port={}\n",
                    c.host,
                    c.port.map_or("NONE".to_string(), |p| p.to_string())
                );
            } else {
                s += &format!("Diag: hostandport={}\n", c.host);
            }
            s += &format!("Diag: path={}\n", c.path);
            (s, String::new())
        }
        Err(e) => (String::new(), format!("fatal: {e}\n")),
    }
}

#[test]
fn connect_url_split_matches_git() {
    let Some(git) = Git::new() else { return };
    let work = init_repo(&git, "diag", "");
    let cases = url_cases();
    assert!(cases.len() >= 200);
    let mut failures = Vec::new();
    for url in cases {
        let o = git.run(&work, &[b"fetch-pack", b"--diag-url", url.as_bytes()]);
        let theirs = (String::from_utf8_lossy(&o.stdout).into_owned(), stderr(&o));
        let ours = diag(&url);
        if ours != theirs {
            failures.push(format!("{url:?}:\n rgit {ours:?}\n git  {theirs:?}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn clone_directory_name_matches_git() {
    let Some(git) = Git::new() else { return };
    let work = git.work();
    let urls = [
        "host:foo",
        "host:foo/.git",
        "host:foo.git///",
        "host:foo///.git/",
        "ssh://host/foo/.git///",
        "ssh://host/",
        "ssh://host:1234/",
        "ssh://user:passw@rd@host:1234/",
        "user:passw@rd@host:/",
        "ssh://host/foo@bar.git",
        "ssh://user:password@host/test:1234.git",
        "host:",
        "ssh://host/.git",
        "ssh://host/foo.git.git",
        "ssh://host/a\t\tb",
        "ssh://host/ a",
        "ssh://host/a .git",
        "ssh://[::1]/",
        "ssh://h@[::1]:22/",
        "ssh://host:12a/",
        "ssh://host/x/.git/.git",
        "zz::/",
        "zz::host/foo/bar.git/",
        "host:foo@bar",
        "ssh://host/x.GIT",
        "ssh://host/~user",
        "ssh://host/foo/bar.bundle",
        "zz::https://example.com/a/b.git",
        "https://example.com/a/b.git",
    ];
    let mut failures = Vec::new();
    for url in urls {
        for bare in [false, true] {
            let mut args: Vec<&[u8]> = vec![b"clone"];
            if bare {
                args.push(b"--bare");
            }
            args.push(url.as_bytes());
            let mut c = git.cmd(&work);
            c.env("GIT_ALLOW_PROTOCOL", "none");
            for a in &args {
                c.arg(a.to_os_str().unwrap());
            }
            let o = c.output().unwrap();
            let err = stderr(&o);
            let theirs = if let Some(rest) = err.strip_prefix("Cloning into bare repository '") {
                rest.split("'...").next().unwrap().to_string()
            } else if let Some(rest) = err.strip_prefix("Cloning into '") {
                rest.split("'...").next().unwrap().to_string()
            } else {
                err.strip_prefix("fatal: ")
                    .unwrap_or(&err)
                    .trim_end()
                    .to_string()
            };
            let ours = humanish_dir(url.as_bytes(), bare, false)
                .map_or_else(|e| e.to_string(), |d| d.to_string());
            if ours != theirs {
                failures.push(format!(
                    "{url:?} bare={bare}: rgit {ours:?}, git {theirs:?}"
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

const REWRITE_CONFIG: &str = "[url \"B\"]\n\tinsteadOf = x\n[url \"A\"]\n\tinsteadOf = x\n\
[url \"C\"]\n\tinsteadOf = xy\n[url \"P\"]\n\tpushInsteadOf = x\n[url \"Q\"]\n\tpushInsteadOf = xyz\n\
[url \"Y\"]\n\tinsteadOf = y\n[url \"Yfoo\"]\n\tinsteadOf = Yf\n\
[remote \"r1\"]\n\turl = xfoo\n[remote \"r2\"]\n\turl = xyzfoo\n\tpushurl = xyzbar\n\
[remote \"r3\"]\n\turl = xyzfoo\n\turl = xyq\n\turl = yq\n[remote \"r4\"]\n\turl = a\n\turl =\n\turl = Yfoo\n\
[remote \"r5\"]\n\tfetch = refs/heads/*:refs/remotes/r5/*\n[remote \"t\"]\n\turl = xx\n\tpushurl = p\n\tpushurl =\n";

fn init_repo(git: &Git, name: &str, config_extra: &str) -> PathBuf {
    let dir = git.tmp.path().join(name);
    let o = git.run(
        git.tmp.path(),
        &[b"init", b"-q", b"-b", b"main", name.as_bytes()],
    );
    assert!(o.status.success(), "{}", stderr(&o));
    let cfg = dir.join(".git/config");
    let mut text = std::fs::read_to_string(&cfg).unwrap();
    text.push_str(config_extra);
    std::fs::write(&cfg, text).unwrap();
    dir
}

fn load_config(repo: &Path) -> rgit_config::ConfigSet {
    let data = std::fs::read(repo.join(".git/config")).unwrap();
    parse_bytes(&data, Origin::File(".git/config".into()), Scope::Local).unwrap()
}

#[test]
fn url_rewrites_match_git() {
    let Some(git) = Git::new() else { return };
    let repo = init_repo(&git, "rw", REWRITE_CONFIG);
    let remotes = Remotes::from_config(&load_config(&repo)).unwrap();
    let mut failures = Vec::new();
    for name in [
        "r1", "r2", "r3", "r4", "r5", "t", "xyzzy", "Yfoo", "other", "a b",
    ] {
        let r = remotes.get(name.as_bytes(), None).unwrap();
        let fetch = git.run(&repo, &[b"remote", b"get-url", b"--all", name.as_bytes()]);
        let push = git.run(
            &repo,
            &[b"remote", b"get-url", b"--push", b"--all", name.as_bytes()],
        );
        let lsr = git.run(&repo, &[b"ls-remote", b"--get-url", name.as_bytes()]);
        let join = |v: &[bstr::BString]| v.iter().map(|u| format!("{u}\n")).collect::<String>();
        if r.is_configured() {
            if String::from_utf8_lossy(&fetch.stdout) != join(&r.url) {
                failures.push(format!("{name} fetch: {:?} vs {:?}", r.url, fetch));
            }
            if String::from_utf8_lossy(&push.stdout) != join(&r.push_url) {
                failures.push(format!("{name} push: {:?} vs {:?}", r.push_url, push));
            }
        }
        if String::from_utf8_lossy(&lsr.stdout) != format!("{}\n", r.url[0]) {
            failures.push(format!("{name} ls-remote: {:?} vs {:?}", r.url, lsr));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn tracking_refs_match_git() {
    let Some(git) = Git::new() else { return };
    let base = "[remote \"origin\"]\n\turl = /x\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n\
[remote \"pub\"]\n\turl = /y\n";
    let scenarios: &[&str] = &[
        "",
        "[branch \"main\"]\n\tremote = origin\n",
        "[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/other\n",
        "[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n",
        "[branch \"main\"]\n\tremote = .\n\tmerge = refs/heads/main\n",
        "[branch \"main\"]\n\tremote = nosuch\n\tmerge = refs/heads/main\n",
        "[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/nottracked\n[remote \"origin\"]\n\tfetch = refs/heads/x:refs/remotes/origin/x\n",
        "[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/other\n[push]\n\tdefault = nothing\n",
        "[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/other\n[push]\n\tdefault = upstream\n",
        "[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/other\n[push]\n\tdefault = current\n",
        "[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/other\n[push]\n\tdefault = matching\n",
        "[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n[remote \"origin\"]\n\tpush = refs/heads/other:refs/heads/x\n",
        "[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n[remote \"origin\"]\n\tpush = refs/heads/main\n",
        "[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n[remote \"origin\"]\n\tpush = refs/heads/main:refs/heads/x\n",
        "[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n\tpushRemote = pub\n",
        "[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n\tpushRemote = pub\n[remote \"pub\"]\n\tfetch = +refs/heads/*:refs/remotes/pub/*\n",
        "[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n[remote]\n\tpushDefault = pub\n[remote \"pub\"]\n\tfetch = +refs/heads/*:refs/remotes/pub/*\n\tpush = refs/heads/*:refs/heads/q/*\n",
        "[branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n[remote \"pub\"]\n\tfetch = +refs/heads/*:refs/remotes/pub/*\n[push]\n\tdefault = current\n",
    ];
    let mut failures = Vec::new();
    for (i, extra) in scenarios.iter().enumerate() {
        let repo = init_repo(&git, &format!("t{i}"), &format!("{base}{extra}"));
        let o = git.run(&repo, &[b"commit", b"-q", b"--allow-empty", b"-m", b"x"]);
        assert!(o.status.success(), "{}", stderr(&o));
        let cfg = load_config(&repo);
        let remotes = Remotes::from_config(&cfg).unwrap();
        let branch = BranchConfig::from_config(&cfg, b"main").unwrap();
        let ours_up = upstream_ref(&remotes, None, Some(&branch));
        let ours_push = PushDefault::from_config(&cfg)
            .map_err(|e| e.to_string())
            .and_then(|pd| push_ref(&remotes, None, Some(&branch), pd).map_err(|e| e.to_string()));
        // Create the refs we predict so that git can resolve them.
        for r in [ours_up.as_ref().ok(), ours_push.as_ref().ok()]
            .into_iter()
            .flatten()
        {
            if r.starts_with(b"refs/remotes/") {
                git.run(&repo, &[b"update-ref", r.as_slice(), b"HEAD"]);
            }
        }
        for (spec, ours) in [
            ("@{upstream}", ours_up.map_err(|e| e.to_string())),
            ("@{push}", ours_push),
        ] {
            let o = git.run(
                &repo,
                &[b"rev-parse", b"--symbolic-full-name", spec.as_bytes()],
            );
            let theirs = if o.status.success() {
                Ok(String::from_utf8_lossy(&o.stdout).trim_end().to_string())
            } else {
                Err(stderr(&o)
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .trim_start_matches("fatal: ")
                    .to_string())
            };
            let ours = ours.map(|r| r.to_string());
            if ours != theirs {
                failures.push(format!(
                    "scenario {i} {spec}: rgit {ours:?}, git {theirs:?}"
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn protocol_policy_matches_git() {
    use rgit_config::MapEnv;
    use rgit_remote::protocol::check_allowed;
    let Some(git) = Git::new() else { return };
    let work = git.work();
    let urls: &[(&str, &str)] = &[
        ("file", "/nonexistent-rgit-remote-test"),
        ("ssh", "h:x"),
        ("ext", "ext::false"),
        ("zz", "zz::x"),
    ];
    let configs: &[&[&str]] = &[
        &[],
        &["protocol.allow=never"],
        &["protocol.allow=never", "protocol.file.allow=always"],
        &["protocol.allow=always"],
        &["protocol.ssh.allow=user"],
        &["protocol.zz.allow=NEVER"],
        &["protocol.allow=bogus"],
        &["protocol.FILE.allow=never"],
    ];
    let envs: &[&[(&str, &str)]] = &[
        &[],
        &[("GIT_PROTOCOL_FROM_USER", "0")],
        &[("GIT_PROTOCOL_FROM_USER", "bogus")],
        &[("GIT_ALLOW_PROTOCOL", "file:zz")],
        &[("GIT_ALLOW_PROTOCOL", "")],
    ];
    let mut failures = Vec::new();
    for (proto, url) in urls {
        for cfg_items in configs {
            for env in envs {
                let mut text = String::new();
                for item in *cfg_items {
                    let (k, v) = item.split_once('=').unwrap();
                    let (sec, rest) = k.split_once('.').unwrap();
                    match rest.rsplit_once('.') {
                        Some((sub, key)) => text += &format!("[{sec} \"{sub}\"]\n{key} = {v}\n"),
                        None => text += &format!("[{sec}]\n{rest} = {v}\n"),
                    }
                }
                let cfg =
                    parse_bytes(text.as_bytes(), Origin::CommandLine, Scope::Command).unwrap();
                let menv = MapEnv::new(env.iter().map(|(k, v)| (k.to_string(), v.to_string())));
                let ours = check_allowed(proto.as_bytes(), &cfg, &menv)
                    .err()
                    .map(|e| format!("fatal: {e}"));
                let mut c = git.cmd(&work);
                c.env("GIT_SSH", "false");
                for (k, v) in *env {
                    c.env(k, v);
                }
                for item in *cfg_items {
                    c.arg("-c").arg(item);
                }
                let o = c.arg("ls-remote").arg(url).output().unwrap();
                let first = stderr(&o).lines().next().unwrap_or_default().to_string();
                let policy_error = first.contains("not allowed")
                    || first.contains("unknown value for config")
                    || first.contains("bad boolean environment value");
                let theirs = policy_error.then_some(first);
                if ours != theirs {
                    failures.push(format!(
                        "{url} {cfg_items:?} {env:?}: rgit {ours:?}, git {theirs:?}"
                    ));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
