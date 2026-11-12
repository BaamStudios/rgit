// SPDX-License-Identifier: MIT
//! Differential tests against the `git` binary. Skipped when `git` is not
//! installed.

use std::path::{Path, PathBuf};
use std::process::Command;

use bstr::{BString, ByteSlice};
use rgit_config::{
    CommandLine, ConfigSet, Entry, Error, IntWidth, Level, LoadOptions, Loader, MapEnv, Origin,
    Scope, SourceSpec, StaticRepoFacts, sources,
};

fn have_git() -> bool {
    Command::new("git").arg("--version").output().is_ok()
}

/// Keeps git from discovering the rgit checkout around the test directory.
fn ceiling() -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("rgit-config")
}

fn tmpdir(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("rgit-config")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A `git` command with a clean, predictable environment.
fn git(dir: &Path, home: &Path) -> Command {
    let mut c = Command::new("git");
    c.env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env("GIT_CEILING_DIRECTORIES", ceiling())
        .current_dir(dir);
    c
}

struct Out {
    stdout: BString,
    stderr: BString,
    ok: bool,
}

fn run(mut c: Command) -> Out {
    let o = c.output().expect("run git");
    Out {
        stdout: o.stdout.into(),
        stderr: o.stderr.into(),
        ok: o.status.success(),
    }
}

/// Render entries like `git config list -z [--show-scope] [--show-origin]`.
fn render(entries: &[Entry], scope: bool, origin: bool) -> BString {
    let mut out = BString::default();
    for e in entries {
        if scope {
            out.extend_from_slice(e.source.scope.as_str().as_bytes());
            out.push(0);
        }
        if origin {
            out.extend_from_slice(&e.source.origin.show_origin(true));
            out.push(0);
        }
        out.extend_from_slice(&e.name);
        if let Some(v) = &e.value {
            out.push(b'\n');
            // git prints values as C strings.
            let v = match v.find_byte(0) {
                Some(i) => &v[..i],
                None => &v[..],
            };
            out.extend_from_slice(v);
        }
        out.push(0);
    }
    out
}

fn render_report(e: &Error) -> BString {
    let mut out = BString::default();
    for (level, msg) in e.report() {
        out.extend_from_slice(level.prefix().as_bytes());
        out.push(b' ');
        out.extend_from_slice(msg.as_bytes());
        out.push(b'\n');
    }
    out
}

fn load_file(path: &Path, includes: bool, env: &MapEnv) -> (ConfigSet, Option<Error>) {
    let mut set = ConfigSet::new();
    let err = Loader::new(LoadOptions {
        env,
        repo: None,
        includes,
    })
    .add(SourceSpec::File {
        path: path.to_path_buf(),
        scope: Scope::Command,
        required: true,
    })
    .load_into(&mut set)
    .err();
    (set, err)
}

const SYNTAX_CASES: &[&[u8]] = &[
    b"[section]\nkey garbage\n",
    b"[section\nkey=v\n",
    b"[s]\nk=\"abc\n",
    b"[s]\nk=a\\qb\n",
    b"[s]\nk=\"a\\tb\\nc\\bd\"\n",
    b"[s]\nk=a\\\nb\n",
    b"[s]\nk=\"a\\\n  b\"\n",
    b"[s]\nk=a ; c\n",
    b"[s]\nk=a\"#\"b\n",
    b"[S.Sub]\nK=v\n",
    b"[S \"Sub\"]\nK=v\n",
    b"[S \"a\\\"b\\\\c\\td\"]\nK=v\n",
    b"[S \"a\"b]\nK=v\n",
    b"[S \"a\" ]\nK=v\n",
    b"[S  \"a\"]\nK=v\n",
    b"[S\t\"a\"]\nK=v\n",
    b"[s] k = v\n",
    b"[s]k\n",
    b"[s]\n1k=v\n",
    b"[s]\nk_x=v\n",
    b"[s]\nk-x=v\n",
    b"k=v\n",
    b"[s]\r\nk=v\r\n",
    b"[s]\nk=a\rb\n",
    b"[s]\nk=\rb\n",
    b"\xef\xbb\xbf[s]\nk=v\n",
    b"\xef\xbb[s]\nk=v\n",
    b"\n\xef\xbb\xbf[s]\nk=1\n",
    b"[s]\nk=\n",
    b"[s.]\nk=v\n",
    b"[.s]\nk=v\n",
    b"[s.a.B]\nk=v\n",
    b"[s_x]\nk=v\n",
    b"[]\nk=v\n",
    b"[ \"x\"]\nk=1\n",
    b"[s \"a\nb\"]\nk=v\n",
    b"[s]\nk\t=\tv\t\n",
    b"[s]\nk = \"a\" \"b\"\n",
    b"[s]\nk = a\\",
    b"[s]\nk = a",
    b"[s] k=1 [t] j=2",
    b"[s] [t]\nk=1\n",
    b"[s] \r [t]\nk=1\n",
    b"  [s]\n\tk=1\n # c\n ; d\n",
    b"[s]\nk = a  \\\n  b\n",
    b"[s]\nk = \"a\\\"",
    b"[s]\nk\n=v\n",
    b"[s \"x\" y]\nk=1",
    b"[s \"\"]\nk=1",
    b"[s.x \"y\"]\nk=1",
    b"[s]\nk;c\n",
    b"[s]\nk =\n",
    b"[s]\nk==v\n",
    b"[-s]\nk=v\n",
    b"[s]\nk=a\x00b\nj=c\n",
    b"[s]\nk=a\nj\x00=c\n",
    b"[s]\nk = \"\" x\n",
    b"[s]\nk = a\t\"\" b\n",
    b"[s]\nk = a \"\" ;\n",
    b"[s]\nk=1 \\\n# c\n",
    b"[s]\nk = \x0c a\n",
    b"[s]\nk\x0b= a\n",
    b"[s \"x\\\\\"]\nk=1",
    b"[s \"x\\\"]\nk=1",
    b"[s \"x\"\n]k=1",
    b"[s \"x\r\"]\nk=1",
    b"[s",
    b"[s \"x",
    b"[s \"x\"",
    b"[s]\n\n[t",
    b"[s]\nk=1\n[s \"x\"\n",
    b"[s]\r\nk=1\r\n[s \"x\"\r\n]\r\n",
    b"[s]\nk=a\\\n\\q\n",
    b"[s]\nk=\"a\\\n\n",
    b"[s]\nk = \"a\r\"\n",
    b"[s]\nkey=\x80\xff\n",
    b"[s \"\xc3\xa4\"]\nk=1\n",
    b"[s]\n\xc3\xa4=1\n",
    b"[some]key = value\r",
    b"[s]\nk=1\n[s]\nk=2\n[S]\nk=3\n",
    b"[s.X]\nk=1\n[s \"X\"]\nk=2\n[s \"x\"]\nk=3\n",
    b"# c\n; c\n[s] # c\n\tk = v ; c\n",
    b"[s]\n\tsolid = rock\n\tsparse = big \t\t blue\n\tsparseAndTail = big \t\t blue \n\tsparseAndTailQuoted = \"big \t\t blue \"\n\tsparseAndBiggerTail = big \t\t blue \t \t\n\theadAndTail = \tbig blue \n\theadAndTailQuoted = \"\tbig blue \"\n\theadAndTailQuotedPlus = \"\tbig blue \" \n\tannotated = big blue\t# to be discarded\n\tannotatedQuoted = \"big blue\"\t# to be discarded\n",
];

#[test]
fn syntax_matches_git() {
    if !have_git() {
        return;
    }
    let dir = tmpdir("syntax");
    let env = MapEnv::new([("HOME", dir.to_str().unwrap())]);
    for (i, case) in SYNTAX_CASES.iter().enumerate() {
        let name = format!("case{i}");
        std::fs::write(dir.join(&name), case).unwrap();
        let mut c = git(&dir, &dir);
        c.args(["config", "-f", &name, "-l", "-z"]);
        let g = run(c);
        let (set, err) = with_cwd(&dir, || load_file(Path::new(&name), false, &env));
        let ours = render(set.entries(), false, false);
        assert_eq!(ours, g.stdout, "stdout of {:?}", case.as_bstr());
        match err {
            None => assert!(g.ok, "git failed on {:?}: {}", case.as_bstr(), g.stderr),
            Some(e) => {
                assert!(!g.ok, "git accepted {:?}", case.as_bstr());
                assert_eq!(
                    render_report(&e),
                    g.stderr,
                    "stderr of {:?}",
                    case.as_bstr()
                );
            }
        }
    }
}

#[test]
fn typed_values_match_git() {
    if !have_git() {
        return;
    }
    let dir = tmpdir("typed");
    let values: &[&str] = &[
        "k",
        "k=",
        "k=true",
        "k=TRUE",
        "k=Yes",
        "k=on",
        "k=1",
        "k=0",
        "k=2",
        "k=-1",
        "k=10k",
        "k=0x10",
        "k=010",
        "k= 5",
        "k=\"5 \"",
        "k=+5",
        "k=1k",
        "k=1M",
        "k=1g",
        "k=1t",
        "k=8g",
        "k=2147483648",
        "k=9223372036854775807",
        "k=9223372036854775808",
        "k=-9223372036854775808",
        "k=8589934592k",
        "k=abc",
        "k=1kb",
        "k=off",
        "k=nO",
        "k=-0",
        "k=00",
        "k=-",
        "k=0x",
        "k=\" \"",
        "k=\"\\t5\"",
        "k=09",
        "k=1e3",
        "k=+-1",
        "k=0X1f",
        "k=2147483647k",
        "k=99999999999999999999",
        "k=99999999999999999999x",
    ];
    for v in values {
        let content = format!("[s]\n{v}\n");
        std::fs::write(dir.join("f"), &content).unwrap();
        let set =
            rgit_config::parse_bytes(content.as_bytes(), Origin::File("f".into()), Scope::Command)
                .unwrap();
        let e = set.get("s.k").unwrap();
        for ty in ["bool", "int", "bool-or-int"] {
            let mut c = git(&dir, &dir);
            c.args(["config", "-f", "f", &format!("--type={ty}"), "s.k"]);
            let g = run(c);
            let ours: Result<String, String> = match ty {
                "bool" => e.bool().map(|b| b.to_string()).map_err(|e| e.to_string()),
                "int" => e
                    .int(IntWidth::I64)
                    .map(|n| n.to_string())
                    .map_err(|e| e.to_string()),
                _ => e
                    .bool_or_int()
                    .map(|b| match b {
                        rgit_config::BoolOrInt::Bool(b) => b.to_string(),
                        rgit_config::BoolOrInt::Int(n) => n.to_string(),
                    })
                    .map_err(|e| e.to_string()),
            };
            match ours {
                Ok(s) => {
                    assert!(g.ok, "{v} {ty}: git failed: {}", g.stderr);
                    assert_eq!(g.stdout, format!("{s}\n"), "{v} {ty}");
                }
                Err(msg) => {
                    assert!(!g.ok, "{v} {ty}: git succeeded with {}", g.stdout);
                    assert_eq!(g.stderr, format!("fatal: {msg}\n"), "{v} {ty}");
                }
            }
        }
    }
}

#[test]
fn command_line_matches_git() {
    if !have_git() {
        return;
    }
    let dir = tmpdir("cmdline");
    // (GIT_CONFIG_PARAMETERS, GIT_CONFIG_COUNT pairs, -c args)
    type Case = (
        Option<&'static str>,
        &'static [(&'static str, &'static str)],
        &'static [&'static str],
    );
    let cases: &[Case] = &[
        (
            None,
            &[],
            &["a.b=c", "S.Ab.K=v", "flag.on", "empty.v=", "a.x=y.k=v"],
        ),
        (
            Some("'q.r=1'"),
            &[
                ("GIT_CONFIG_COUNT", "1"),
                ("GIT_CONFIG_KEY_0", "E.v"),
                ("GIT_CONFIG_VALUE_0", "x"),
            ],
            &["a.b=c"],
        ),
        (
            Some(
                "'key.one'='foo'  'key.ambiguous=section.whatever'='value' 'key.with=equals.oldbool' 'key.n'=",
            ),
            &[],
            &[],
        ),
        (Some("'env.one=one'\\''' 'env.two=two'"), &[], &[]),
        (Some("'env.one=one'\\' 'env.two=two'"), &[], &[]),
        (Some("  "), &[], &[]),
        (Some(""), &[], &["x.one=1"]),
        (None, &[], &["s=v"]),
        (None, &[], &["s.1k=v"]),
        (None, &[], &[".k=v"]),
        (None, &[], &["s.k.=v"]),
        (None, &[], &["s_x.k=v"]),
        (None, &[], &["s.x\ny.k=v"]),
        (None, &[], &["=x"]),
        (None, &[], &["a=VAL"]),
        (None, &[], &["a.\"b c\".0d=VAL"]),
        (None, &[("GIT_CONFIG_COUNT", "10a")], &[]),
        (None, &[("GIT_CONFIG_COUNT", "-1")], &[]),
        (None, &[("GIT_CONFIG_COUNT", "9999999999999999")], &[]),
        (None, &[("GIT_CONFIG_COUNT", "1")], &[]),
        (
            None,
            &[("GIT_CONFIG_COUNT", "1"), ("GIT_CONFIG_KEY_0", "a.b")],
            &[],
        ),
        (
            None,
            &[
                ("GIT_CONFIG_COUNT", "1"),
                ("GIT_CONFIG_KEY_0", ""),
                ("GIT_CONFIG_VALUE_0", "v"),
            ],
            &[],
        ),
        (
            None,
            &[
                ("GIT_CONFIG_COUNT", "1"),
                ("GIT_CONFIG_KEY_0", "missing-section"),
                ("GIT_CONFIG_VALUE_0", "v"),
            ],
            &[],
        ),
        (None, &[("GIT_CONFIG_COUNT", "")], &[]),
        (None, &[], &["include.path=relative"]),
        (None, &[], &["include.path"]),
        (None, &[], &["includeIf.gitdir:./x.path=/nonexistent"]),
    ];
    for (params, envs, args) in cases {
        let mut c = git(&dir, &dir);
        let mut env = MapEnv::new([("HOME", dir.to_str().unwrap())]);
        if let Some(p) = params {
            c.env("GIT_CONFIG_PARAMETERS", p);
            env.set("GIT_CONFIG_PARAMETERS", *p);
        }
        for (k, v) in *envs {
            c.env(k, v);
            env.set(*k, *v);
        }
        let mut cl = CommandLine::default();
        for a in *args {
            c.arg("-c").arg(a);
            cl.push_c(a);
        }
        c.args(["config", "list", "--show-scope", "--show-origin", "-z"]);
        let g = run(c);
        let mut set = ConfigSet::new();
        let err = Loader::new(LoadOptions {
            env: &env,
            repo: None,
            includes: true,
        })
        .add(SourceSpec::CommandLine(cl))
        .load_into(&mut set)
        .err();
        let what = format!("{params:?} {envs:?} {args:?}");
        match err {
            None => {
                assert!(g.ok, "{what}: git failed: {}", g.stderr);
                assert_eq!(render(set.entries(), true, true), g.stdout, "{what}");
            }
            Some(e) => {
                assert!(!g.ok, "{what}: git succeeded");
                assert_eq!(render_report(&e), g.stderr, "{what}");
            }
        }
    }
}

fn write(path: &Path, content: &str) {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).unwrap();
    }
    std::fs::write(path, content).unwrap();
}

/// The full stack: system, XDG, global, local, worktree, command line, with
/// includes and conditional includes.
#[test]
fn full_stack_matches_git() {
    if !have_git() {
        return;
    }
    let root = tmpdir("stack");
    let home = root.join("home");
    let repo = root.join("repo");
    std::fs::create_dir_all(&home).unwrap();
    let mut init = git(&root, &home);
    init.args(["init", "-q", "-b", "topic/x", "repo"]);
    assert!(run(init).ok);
    let sys = root.join("sysconfig");
    write(&sys, "[sys]\n\tv = 1\n[include]\n\tpath = sys.inc\n");
    write(&root.join("sys.inc"), "[sys]\n\tinc = yes\n");
    write(&home.join("xdg/git/config"), "[xdg]\n\tv = 2\n");
    write(
        &home.join(".gitconfig"),
        "[global]\n\tv = 3\n[include]\n\tpath = ~/g.inc\n\tpath = missing.inc\n\
         [includeIf \"gitdir:repo/\"]\n\tpath = gitdir.inc\n\
         [includeIf \"gitdir/i:REPO/.GIT\"]\n\tpath = gitdiri.inc\n\
         [includeIf \"gitdir:other/\"]\n\tpath = never.inc\n\
         [includeIf \"onbranch:topic/\"]\n\tpath = branch.inc\n\
         [includeIf \"onbranch:main\"]\n\tpath = never.inc\n\
         [includeIf \"hasconfig:remote.*.url:https://example.com/**\"]\n\tpath = remote.inc\n\
         [includeIf \"hasconfig:remote.*.url:https://other.example/**\"]\n\tpath = never.inc\n\
         [includeIf \"gitdir:./repo/.git\"]\n\tpath = rel.inc\n",
    );
    write(&home.join("g.inc"), "[g]\n\tinc = 1\n");
    write(&home.join("gitdir.inc"), "[cond]\n\tgitdir = 1\n");
    write(&home.join("gitdiri.inc"), "[cond]\n\tgitdiri = 1\n");
    write(&home.join("branch.inc"), "[cond]\n\tbranch = 1\n");
    write(&home.join("remote.inc"), "[cond]\n\tremote = 1\n");
    write(&home.join("rel.inc"), "[cond]\n\trel = 1\n");
    write(&home.join("never.inc"), "[never]\n\tseen = 1\n");
    let local = repo.join(".git/config");
    let mut content = std::fs::read_to_string(&local).unwrap();
    content.push_str(
        "[extensions]\n\tworktreeConfig = true\n[remote \"origin\"]\n\turl = https://example.com/r.git\n\
         [include]\n\tpath = local.inc\n",
    );
    // worktreeConfig needs repositoryformatversion 1
    content = content.replace("repositoryformatversion = 0", "repositoryformatversion = 1");
    write(&local, &content);
    write(
        &repo.join(".git/local.inc"),
        "[local]\n\tinc = \"quoted value \"\n",
    );
    write(&repo.join(".git/config.worktree"), "[wt]\n\tv = 4\n");

    let xdg = home.join("xdg");
    let vars = [
        ("HOME", home.to_str().unwrap()),
        ("XDG_CONFIG_HOME", xdg.to_str().unwrap()),
        ("GIT_CONFIG_SYSTEM", sys.to_str().unwrap()),
        ("GIT_CONFIG_COUNT", "1"),
        ("GIT_CONFIG_KEY_0", "Env.Key"),
        ("GIT_CONFIG_VALUE_0", "envvalue"),
        ("GIT_CONFIG_PARAMETERS", "'params.one=1'"),
    ];
    let mut c = git(&repo, &home);
    c.env_remove("GIT_CONFIG_NOSYSTEM");
    for (k, v) in vars {
        c.env(k, v);
    }
    c.args([
        "-c",
        "cmd.Sub.v=5",
        "-c",
        "cmd.flag",
        "config",
        "list",
        "--show-origin",
        "--show-scope",
        "-z",
    ]);
    let g = run(c);
    assert!(g.ok, "{}", g.stderr);

    let env = MapEnv::new(vars);
    let facts = StaticRepoFacts {
        git_dir: Some(repo.join(".git")),
        worktree: Some(repo.clone()),
        branch: Some("topic/x".into()),
    };
    let mut cl = CommandLine::default();
    cl.push_c("cmd.Sub.v=5");
    cl.push_c("cmd.flag");
    // git prints the repository files relative to the work tree; load
    // through absolute paths and strip the prefix.
    let mut set = ConfigSet::new();
    if let Err(e) = sources::load_standard(
        LoadOptions {
            env: &env,
            repo: Some(&facts),
            includes: true,
        },
        Some(&sources::RepoConfig::new(repo.join(".git"))),
        &cl,
        sources::ScopeFilter::default(),
        &mut set,
    ) {
        panic!("{}", render_report(&e));
    }
    let ours = render(set.entries(), true, true);
    let prefix = format!("{}/", repo.display());
    let ours = BString::from(ours.replace(prefix.as_bytes(), b""));
    assert_eq!(ours, g.stdout);
    assert!(!ours.contains_str("never.seen"));
}

#[test]
fn include_errors_match_git() {
    if !have_git() {
        return;
    }
    let dir = tmpdir("includes");
    let env = MapEnv::new([("HOME", dir.to_str().unwrap())]);
    let cases: &[(&str, &str)] = &[
        ("cycle", "[include]\n\tpath = cycle\n"),
        ("novalue", "[include]\npath\n"),
        ("empty", "[include]\npath=\n[a]\nb=1\n"),
        ("nouser", "[include]\npath=~nosuchuserxx/x\n"),
        ("chain", "[include]\npath=chain2\n[a]\nb=1\n"),
        (
            "hasconfig",
            "[includeIf \"hasconfig:remote.*.url:https://e.com/**\"]\npath=bad.inc\n[remote \"o\"]\nurl=https://e.com/x\n",
        ),
        (
            "unknown",
            "[includeIf \"bogus:x\"]\npath=chain2\n[includeIf \"GITDIR:x\"]\npath=chain2\n[includeIf]\npath=chain2\n",
        ),
    ];
    // Nested chains of 10 and 11 includes around the depth limit.
    for n in 1..=11 {
        write(
            &dir.join(format!("deep{n}")),
            &format!("[d]\nv{n}=1\n[include]\npath=deep{}\n", n + 1),
        );
    }
    write(&dir.join("deep12"), "[d]\nlast=1\n");
    let mut cases = cases.to_vec();
    cases.push(("deep1", "[d]\nv1=1\n[include]\npath=deep2\n"));
    cases.push(("deep2", "[d]\nv2=1\n[include]\npath=deep3\n"));
    cases.push(("deep3", "[d]\nv3=1\n[include]\npath=deep4\n"));
    write(
        &dir.join("chain2"),
        "[a]\nc=2\n[include]\npath=sub/chain3\n",
    );
    write(
        &dir.join("sub/chain3"),
        "[a]\nd=3\n[include]\npath=../chain4\n",
    );
    write(&dir.join("chain4"), "[a]\ne=4\n");
    write(&dir.join("bad.inc"), "[remote \"x\"]\nurl=foo\n");
    for (name, content) in &cases {
        write(&dir.join(name), content);
        let mut c = git(&dir, &dir);
        c.args([
            "config",
            "-f",
            name,
            "--includes",
            "-l",
            "-z",
            "--show-origin",
        ]);
        let g = run(c);
        let mut set = ConfigSet::new();
        let err = with_cwd(&dir, || {
            Loader::new(LoadOptions {
                env: &env,
                repo: None,
                includes: true,
            })
            .add(SourceSpec::File {
                path: PathBuf::from(name),
                scope: Scope::Command,
                required: true,
            })
            .load_into(&mut set)
            .err()
        });
        let ours = render(set.entries(), false, true);
        match err {
            None => {
                assert!(g.ok, "{name}: git failed: {}", g.stderr);
                assert_eq!(ours, g.stdout, "{name}");
            }
            Some(e) => {
                assert!(!g.ok, "{name}: git succeeded");
                assert_eq!(render_report(&e), g.stderr, "{name}");
                // git streams entries before the error only in some cases;
                // the entries we kept must be a prefix of what git printed
                // or git printed nothing.
                assert!(
                    g.stdout.is_empty()
                        || ours.starts_with(&g.stdout)
                        || g.stdout.starts_with(&ours),
                    "{name}: {:?} vs {:?}",
                    ours,
                    g.stdout
                );
            }
        }
    }
}

/// Run `f` with the process working directory set to `dir`. Tests that
/// change the cwd hold this lock.
fn with_cwd<T>(dir: &Path, f: impl FnOnce() -> T) -> T {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let old = std::env::current_dir().unwrap();
    std::env::set_current_dir(dir).unwrap();
    let r = f();
    std::env::set_current_dir(old).unwrap();
    r
}

#[test]
fn levels_have_git_prefixes() {
    assert_eq!(Level::Fatal.prefix(), "fatal:");
}

#[test]
fn stdin_relative_include_matches_git() {
    if !have_git() {
        return;
    }
    use std::io::Write;
    use std::process::Stdio;
    let dir = tmpdir("stdin");
    let input = b"[a]\nb=1\n[include]path=one\n";
    let mut c = git(&dir, &dir);
    c.args(["config", "-f", "-", "--includes", "-l"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = c.spawn().unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let o = child.wait_with_output().unwrap();
    let env = MapEnv::default();
    let err = Loader::new(LoadOptions {
        env: &env,
        repo: None,
        includes: true,
    })
    .add(SourceSpec::Bytes {
        data: input.to_vec(),
        origin: Origin::Stdin,
        scope: Scope::Command,
    })
    .load()
    .unwrap_err();
    assert!(!o.status.success());
    assert_eq!(render_report(&err), BString::from(o.stderr));
}
