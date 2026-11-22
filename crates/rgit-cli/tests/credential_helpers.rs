// SPDX-License-Identifier: MIT
//! `git credential-store`, `git credential-cache` and
//! `git credential-cache--daemon`.
//!
//! Every test runs with its own `HOME`, `XDG_CONFIG_HOME` and
//! `XDG_CACHE_HOME` inside a temporary directory, so the user's real
//! `~/.git-credentials`, `~/.config/git/credentials` and `~/.cache/git` are
//! never touched. Cache daemons are always started on a socket inside the
//! temporary directory and stopped by [`Daemons`] (with `exit`, then by
//! killing whatever still serves that socket).

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use rgit_difftest::{Check, Fixture};

const ASKPASS: &str = r#"#!/bin/sh
echo "askpass: $*" >&2
case "$1" in Username*) echo askpass-username;; *) echo askpass-password;; esac
"#;

const RGIT: &str = env!("CARGO_BIN_EXE_rgit");

// ---- differential tests of credential-store ------------------------------

fn fixture() -> Fixture {
    Fixture::new()
        .rgit(RGIT)
        .write_executable("askpass", ASKPASS)
        .write("h/.keep", "")
}

/// Compare `git <args>` with `$W` replaced by the work directory, `HOME`
/// at `$W/h`; checks output and the files in the work directory (which
/// hold the credential files).
fn run(fx: &mut Fixture, args: &[&str], stdin: &str, env: &[(&str, &str)]) {
    let work = fx.work_dir();
    let w = work.display().to_string();
    let argv: Vec<String> = args.iter().map(|a| a.replace("$W", &w)).collect();
    let mut c = fx
        .compare(&argv)
        .stdin(stdin.as_bytes().to_vec())
        .env("GIT_ASKPASS", work.join("askpass"))
        .env("HOME", work.join("h"))
        .checks([
            Check::ExitCode,
            Check::Stdout,
            Check::Stderr,
            Check::Worktree,
        ]);
    for (k, v) in env {
        c = c.env(k, v.replace("$W", &w));
    }
    c.run();
}

const FILE: &str = "credential.helper=store --file=$W/creds";

#[test]
fn store_round_trip() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let fill = |fx: &mut Fixture, input: &str| {
        run(fx, &["-c", FILE, "credential", "fill"], input, &[]);
    };
    let approve = |fx: &mut Fixture, input: &str| {
        run(fx, &["-c", FILE, "credential", "approve"], input, &[]);
    };
    let reject = |fx: &mut Fixture, input: &str| {
        run(fx, &["-c", FILE, "credential", "reject"], input, &[]);
    };
    fill(&mut fx, "protocol=https\nhost=example.com\n");
    approve(
        &mut fx,
        "protocol=https\nhost=example.com:8080\nusername=u s@:/x\npassword=p:@/ %\u{e4}~-._\n",
    );
    approve(
        &mut fx,
        "protocol=https\nhost=example.com\nusername=user1\npassword=pass1\n",
    );
    approve(
        &mut fx,
        "protocol=https\nhost=example.com\nusername=user2\npassword=pass2\n",
    );
    approve(
        &mut fx,
        "protocol=https\nhost=example.com\nusername=user1\npassword=new1\n",
    );
    fill(&mut fx, "protocol=https\nhost=example.com\n");
    fill(
        &mut fx,
        "protocol=https\nhost=example.com\nusername=user2\n",
    );
    fill(&mut fx, "protocol=https\nhost=example.com:8080\n");
    fill(&mut fx, "protocol=http\nhost=example.com\n");
    fill(&mut fx, "protocol=https\nhost=EXAMPLE.com\n");
    // Paths, with and without credential.useHttpPath.
    run(
        &mut fx,
        &[
            "-c",
            FILE,
            "-c",
            "credential.useHttpPath=true",
            "credential",
            "approve",
        ],
        "protocol=https\nhost=path.tld\npath=a b/c%d!:@'\"<>\nusername=p\npassword=q\n",
        &[],
    );
    for path in ["a b/c%d!:@'\"<>", "other"] {
        run(
            &mut fx,
            &[
                "-c",
                FILE,
                "-c",
                "credential.useHttpPath=true",
                "credential",
                "fill",
            ],
            &format!("protocol=https\nhost=path.tld\npath={path}\n"),
            &[],
        );
    }
    fill(&mut fx, "url=https://path.tld/x.git\n");
    // Erasing compares the password when one is given.
    reject(
        &mut fx,
        "protocol=https\nhost=example.com\nusername=user2\npassword=wrong\n",
    );
    fill(
        &mut fx,
        "protocol=https\nhost=example.com\nusername=user2\n",
    );
    reject(
        &mut fx,
        "protocol=https\nhost=example.com\nusername=user2\n",
    );
    reject(&mut fx, "protocol=https\nhost=example.com\n");
    fill(&mut fx, "protocol=https\nhost=example.com\n");
    // Empty user name and password.
    approve(
        &mut fx,
        "protocol=https\nhost=sso.tld\nusername=\npassword=\n",
    );
    fill(&mut fx, "protocol=https\nhost=sso.tld\n");
}

#[test]
fn store_file_contents_and_errors() {
    let mut fx = fixture().write(
        "creds",
        "\n#comment\n\t\nhttps://a:b@x.com\r\nhttps://c:d@y.com/repo.git\r\n\
         ://user:pass@z.com\nhttps://user:pass@\nhttps://pass@w.com\ngarbage\n\
         https://e%3Af:g%40h@v.com\nfile://i:j@/some/path\n\
         https://k%41:m%41:n%41@q.com/c%25d:/x%41\n",
    );
    if fx.is_skipped() {
        return;
    }
    for input in [
        "protocol=https\nhost=x.com\n",
        "url=https://y.com/repo.git\n",
        "protocol=https\nhost=z.com\n",
        "protocol=https\nhost=w.com\n",
        "protocol=https\nhost=v.com\n",
        "protocol=file\nhost=\npath=some/path\n",
        "host=v.com\n",
        // Only the part after a component's first `:` is decoded.
        "protocol=https\nhost=q.com\n",
        "protocol=https\nhost=q.com\npath=c%25d:/xA\n",
        "protocol=https\nhost=q.com\npath=c%d:/xA\n",
        "url=https://u%41@h%41:1%41/p%41:%41\n",
    ] {
        run(&mut fx, &["-c", FILE, "credential", "fill"], input, &[]);
    }
    run(
        &mut fx,
        &[
            "-c",
            FILE,
            "-c",
            "credential.useHttpPath=true",
            "credential",
            "fill",
        ],
        "url=https://y.com/repo.git\n",
        &[],
    );
    // Rewrites keep lines that are not credentials.
    run(
        &mut fx,
        &["-c", FILE, "credential", "approve"],
        "protocol=https\nhost=v.com\nusername=e:f\npassword=new\n",
        &[],
    );
    run(
        &mut fx,
        &["-c", FILE, "credential", "reject"],
        "protocol=file\n",
        &[],
    );
    // The helper called directly.
    let file = "--file=$W/creds";
    for (args, input) in [
        (vec!["credential-store", "-h"], ""),
        (vec!["credential-store"], ""),
        (vec!["credential-store", file, "get", "extra"], ""),
        (vec!["credential-store", "--bogus", "get"], ""),
        (vec!["credential-store", file, "frob"], "protocol=https\n"),
        (
            vec!["credential-store", file, "get"],
            "protocol=https\nhost=v.com\r\n",
        ),
        (
            vec!["credential-store", "get", file],
            "protocol=https\nbad\n",
        ),
        (
            vec!["credential-store", "--file", "$W", "get"],
            "protocol=https\n",
        ),
        (
            vec!["credential-store", "--file=$W/missing/f", "store"],
            "protocol=https\nhost=h\nusername=u\npassword=p\n",
        ),
        (
            vec!["credential-store", file, "store"],
            "protocol=https\nusername=u\npassword=p\n",
        ),
        (
            vec!["credential-store", "--file=$W/none", "erase"],
            "host=h\n",
        ),
    ] {
        run(&mut fx, &args, input, &[]);
    }
    // A held lock.
    let mut fx = fx.write("creds.lock", "");
    run(
        &mut fx,
        &["credential-store", file, "store"],
        "protocol=https\nhost=h\nusername=u\npassword=p\n",
        &[],
    );
}

#[test]
fn store_default_files() {
    let mut fx = fixture();
    if fx.is_skipped() {
        return;
    }
    let approve = |fx: &mut Fixture, user: &str, env: &[(&str, &str)]| {
        run(
            fx,
            &["-c", "credential.helper=store", "credential", "approve"],
            &format!("protocol=https\nhost=example.com\nusername={user}\npassword=pw-{user}\n"),
            env,
        );
    };
    let fill = |fx: &mut Fixture, env: &[(&str, &str)]| {
        run(
            fx,
            &["-c", "credential.helper=store", "credential", "fill"],
            "protocol=https\nhost=example.com\n",
            env,
        );
    };
    // Nothing exists: ~/.git-credentials is created.
    approve(&mut fx, "home", &[]);
    // Only the XDG file exists: it is used.
    let mut fx = fx
        .remove("h/.git-credentials")
        .write("h/.config/git/credentials", "https://xdg:old@example.com\n");
    approve(&mut fx, "xdg", &[]);
    fill(&mut fx, &[]);
    // Both exist: lookups prefer ~/.git-credentials, stores go there,
    // erasing cleans both.
    let mut fx = fx.write("h/.git-credentials", "https://home:pw@example.com\n");
    fill(&mut fx, &[]);
    approve(&mut fx, "both", &[]);
    run(
        &mut fx,
        &["-c", "credential.helper=store", "credential", "reject"],
        "protocol=https\nhost=example.com\n",
        &[],
    );
    // $XDG_CONFIG_HOME; empty means unset.
    let mut fx = fx.write("x/git/credentials", "");
    approve(&mut fx, "custom", &[("XDG_CONFIG_HOME", "$W/x")]);
    fill(&mut fx, &[("XDG_CONFIG_HOME", "$W/x")]);
    fill(&mut fx, &[("XDG_CONFIG_HOME", "")]);
}

// ---- store files exchanged with git ----------------------------------------

/// A temporary home for one test, with isolated configuration and cache
/// locations.
struct Sandbox {
    dir: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        Sandbox {
            dir: tempfile::Builder::new()
                .prefix("rgit-cred-")
                .tempdir()
                .expect("tempdir"),
        }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn command(&self, program: &Path) -> Command {
        let mut cmd = Command::new(program);
        cmd.env_clear();
        if let Some(p) = std::env::var_os("PATH") {
            cmd.env("PATH", p);
        }
        let home = self.path("home");
        std::fs::create_dir_all(&home).expect("home");
        cmd.env("HOME", &home)
            .env("XDG_CONFIG_HOME", self.path("xdg-config"))
            .env("XDG_CACHE_HOME", self.path("xdg-cache"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("LC_ALL", "C")
            .current_dir(self.dir.path());
        cmd
    }

    /// Run `program args` with `stdin`; the daemon a client may start
    /// keeps running, so its output is not waited for beyond the client.
    fn run(&self, program: &Path, args: &[&str], stdin: &str) -> Output {
        let mut child = self
            .command(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        child.wait_with_output().expect("wait")
    }
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn store_files_are_exchanged_with_git() {
    let Some(git) = rgit_difftest::env::reference_git() else {
        return;
    };
    let rgit = PathBuf::from(RGIT);
    let sb = Sandbox::new();
    let file = sb.path("creds");
    let arg = format!("--file={}", file.display());
    let cred = "protocol=https\nhost=h.example:8443\npath=org/re po.git\n\
                username=al ice@corp\npassword=p@ss:w%rd/\u{e4}\n";
    let query = "protocol=https\nhost=h.example:8443\npath=org/re po.git\n";
    let expect = "username=al ice@corp\npassword=p@ss:w%rd/\u{e4}\n";
    for (writer, reader) in [(&git, &rgit), (&rgit, &git)] {
        let _ = std::fs::remove_file(&file);
        let o = sb.run(writer, &["credential-store", &arg, "store"], cred);
        assert!(o.status.success(), "{}", stderr(&o));
        let o = sb.run(reader, &["credential-store", &arg, "get"], query);
        assert_eq!(
            stdout(&o),
            expect,
            "{} wrote, {} read",
            writer.display(),
            reader.display()
        );
        let o = sb.run(reader, &["credential-store", &arg, "erase"], query);
        assert!(o.status.success());
        assert_eq!(std::fs::read(&file).unwrap(), b"");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(&file, "https://a:b@c\n").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        sb.run(&rgit, &["credential-store", &arg, "erase"], "host=none\n");
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

#[test]
fn store_never_uses_the_real_home() {
    // The isolated sandbox: the default files are created below it.
    let sb = Sandbox::new();
    let o = sb.run(
        Path::new(RGIT),
        &["credential-store", "store"],
        "protocol=https\nhost=h\nusername=u\npassword=p\n",
    );
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(
        std::fs::read_to_string(sb.path("home/.git-credentials")).unwrap(),
        "https://u:p@h\n"
    );
}

// ---- credential-cache --------------------------------------------------------

/// Stops every daemon started on the registered sockets.
#[cfg(unix)]
struct Daemons<'a> {
    sb: &'a Sandbox,
    sockets: Vec<PathBuf>,
}

#[cfg(unix)]
impl<'a> Daemons<'a> {
    fn new(sb: &'a Sandbox) -> Self {
        Daemons {
            sb,
            sockets: Vec::new(),
        }
    }

    fn watch(&mut self, socket: &Path) {
        self.sockets.push(socket.to_owned());
    }
}

#[cfg(unix)]
impl Drop for Daemons<'_> {
    fn drop(&mut self) {
        for socket in &self.sockets {
            let arg = format!("--socket={}", socket.display());
            let _ = self
                .sb
                .run(Path::new(RGIT), &["credential-cache", &arg, "exit"], "");
            wait_for(|| !socket.exists(), Duration::from_secs(3));
            kill_daemons_on(socket);
        }
    }
}

/// Kill any process whose command line names `socket` (a daemon that did
/// not react to `exit`).
#[cfg(unix)]
fn kill_daemons_on(socket: &Path) {
    let needle: OsString = socket.as_os_str().to_owned();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return;
    };
    for e in entries.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<i32>().ok()) else {
            continue;
        };
        let Ok(cmdline) = std::fs::read(e.path().join("cmdline")) else {
            continue;
        };
        let args: Vec<&[u8]> = cmdline.split(|&b| b == 0).collect();
        let is_daemon = args.iter().any(|a| *a == b"credential-cache--daemon");
        let on_socket = args.iter().any(|a| {
            use std::os::unix::ffi::OsStrExt;
            *a == needle.as_bytes()
        });
        if is_daemon && on_socket {
            let _ = Command::new("kill").arg("-9").arg(pid.to_string()).status();
        }
    }
}

fn wait_for(mut cond: impl FnMut() -> bool, max: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < max {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    cond()
}

#[cfg(unix)]
fn is_socket(p: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    std::fs::metadata(p).is_ok_and(|m| m.file_type().is_socket())
}

#[cfg(unix)]
#[test]
fn cache_store_get_erase_exit() {
    let sb = Sandbox::new();
    let mut daemons = Daemons::new(&sb);
    let socket = sb.path("sock/dir/socket");
    daemons.watch(&socket);
    let rgit = Path::new(RGIT);
    let arg = format!("--socket={}", socket.display());
    let cache =
        |action: &str, input: &str| sb.run(rgit, &["credential-cache", &arg, action], input);

    // Without a daemon, only `store` starts one.
    for action in ["get", "erase", "exit"] {
        let o = cache(action, "protocol=https\nhost=h\n");
        assert!(o.status.success() && o.stdout.is_empty(), "{action}");
        assert!(!socket.exists());
    }
    let o = cache("store", "protocol=https\nhost=h\nusername=u\npassword=p\n");
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(is_socket(&socket));
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(socket.parent().unwrap())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
    }
    cache(
        "store",
        "protocol=https\nhost=h\nusername=v\npassword=q\npassword_expiry_utc=9999999999\n\
         oauth_refresh_token=tok\n",
    );
    assert_eq!(
        stdout(&cache("get", "protocol=https\nhost=h\n")),
        "capability[]=authtype\nusername=u\npassword=p\n"
    );
    assert_eq!(
        stdout(&cache("get", "protocol=https\nhost=h\nusername=v\n")),
        "capability[]=authtype\nusername=v\npassword=q\npassword_expiry_utc=9999999999\n\
         oauth_refresh_token=tok\n"
    );
    assert_eq!(
        stdout(&cache("get", "protocol=https\nhost=h\npath=x\n")),
        ""
    );
    cache(
        "erase",
        "protocol=https\nhost=h\nusername=u\npassword=wrong\n",
    );
    assert_ne!(
        stdout(&cache("get", "protocol=https\nhost=h\nusername=u\n")),
        ""
    );
    cache("erase", "protocol=https\nhost=h\nusername=u\n");
    assert_eq!(
        stdout(&cache("get", "protocol=https\nhost=h\nusername=u\n")),
        ""
    );

    // Through `git credential` with the configured helper.
    let helper = format!("credential.helper=cache --socket={}", socket.display());
    let o = sb.run(
        rgit,
        &["-c", &helper, "credential", "fill"],
        "protocol=https\nhost=h\nusername=v\n",
    );
    assert_eq!(
        stdout(&o),
        "protocol=https\nhost=h\nusername=v\npassword=q\noauth_refresh_token=tok\n\
         password_expiry_utc=9999999999\n"
    );

    let o = cache("exit", "");
    assert!(o.status.success());
    assert!(wait_for(|| !socket.exists(), Duration::from_secs(5)));
    assert!(socket.parent().unwrap().is_dir());
}

#[cfg(unix)]
#[test]
fn cache_default_sockets_and_timeout() {
    let sb = Sandbox::new();
    let mut daemons = Daemons::new(&sb);
    let rgit = Path::new(RGIT);
    let xdg = sb.path("xdg-cache/git/credential/socket");
    let legacy_dir = sb.path("home/.git-credential-cache");
    daemons.watch(&xdg);
    daemons.watch(&legacy_dir.join("socket"));
    let cred = "protocol=https\nhost=t\nusername=u\npassword=p\n";

    // $XDG_CACHE_HOME (isolated by the sandbox).
    sb.run(rgit, &["credential-cache", "--timeout=1", "store"], cred);
    assert!(is_socket(&xdg));
    // The credential times out, and the daemon exits with its last one.
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(
        stdout(&sb.run(
            rgit,
            &["credential-cache", "get"],
            "protocol=https\nhost=t\n"
        )),
        ""
    );
    assert!(wait_for(|| !xdg.exists(), Duration::from_secs(5)));

    // ~/.git-credential-cache wins when it exists.
    std::fs::create_dir_all(&legacy_dir).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&legacy_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    sb.run(rgit, &["credential-cache", "store"], cred);
    assert!(is_socket(&legacy_dir.join("socket")));
    assert!(!xdg.exists());
    sb.run(rgit, &["credential-cache", "exit"], "");
    assert!(wait_for(
        || !legacy_dir.join("socket").exists(),
        Duration::from_secs(5)
    ));
}

#[cfg(unix)]
#[test]
fn cache_daemon_errors() {
    use std::os::unix::fs::PermissionsExt;
    let sb = Sandbox::new();
    let mut daemons = Daemons::new(&sb);
    let rgit = Path::new(RGIT);
    let open = sb.path("open");
    std::fs::create_dir_all(&open).unwrap();
    std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755)).unwrap();
    daemons.watch(&open.join("socket"));
    let arg = format!("--socket={}", open.join("socket").display());
    let o = sb.run(
        rgit,
        &["credential-cache", &arg, "store"],
        "protocol=https\nhost=h\nusername=u\npassword=p\n",
    );
    assert_eq!(o.status.code(), Some(128));
    assert_eq!(
        stderr(&o),
        format!(
            "fatal: The permissions on your socket directory are too loose; other\n\
             users may be able to read your cached credentials. Consider running:\n\
             \n\tchmod 0700 {}\nfatal: cache daemon did not start: \n",
            open.display()
        )
    );
    assert!(!open.join("socket").exists());

    let o = sb.run(rgit, &["credential-cache--daemon", "relative/socket"], "");
    assert_eq!(o.status.code(), Some(128));
    assert_eq!(
        stderr(&o),
        "fatal: socket directory must be an absolute path\n"
    );
    let o = sb.run(rgit, &["credential-cache--daemon"], "");
    assert_eq!(o.status.code(), Some(129));
    let o = sb.run(rgit, &["credential-cache"], "");
    assert_eq!(o.status.code(), Some(129));
    let o = sb.run(rgit, &["credential-cache", "--timeout=x", "get"], "");
    assert_eq!(o.status.code(), Some(129));
    let o = sb.run(rgit, &["credential-cache", "frob"], "");
    assert!(o.status.success() && o.stdout.is_empty());
}

/// The daemons of git and rgit understand each other's clients.
#[cfg(unix)]
#[test]
fn cache_interoperates_with_git() {
    let Some(git) = rgit_difftest::env::reference_git() else {
        return;
    };
    let rgit = PathBuf::from(RGIT);
    let sb = Sandbox::new();
    let mut daemons = Daemons::new(&sb);
    for (i, (server, client)) in [(&git, &rgit), (&rgit, &git)].into_iter().enumerate() {
        let socket = sb.path(&format!("s{i}/socket"));
        daemons.watch(&socket);
        let arg = format!("--socket={}", socket.display());
        let o = sb.run(
            server,
            &["credential-cache", &arg, "store"],
            "protocol=https\nhost=h\nusername=u\npassword=p\n",
        );
        assert!(o.status.success(), "{}", stderr(&o));
        let o = sb.run(
            client,
            &["credential-cache", &arg, "get"],
            "protocol=https\nhost=h\n",
        );
        assert_eq!(
            stdout(&o),
            "capability[]=authtype\nusername=u\npassword=p\n"
        );
        sb.run(client, &["credential-cache", &arg, "exit"], "");
        assert!(wait_for(|| !socket.exists(), Duration::from_secs(5)));
    }
}
