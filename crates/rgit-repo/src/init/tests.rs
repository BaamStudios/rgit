// SPDX-License-Identifier: MIT
//! Unit tests of `init`; the comparison with git's results is in the
//! `rgit-cli` differential tests.

use std::path::PathBuf;

use rgit_config::{CommandLine, ConfigSet, MapEnv};

use super::*;

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let p = std::env::temp_dir().join(format!(
            "rgit-repo-init-{}-{name}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn options(git_dir: PathBuf) -> InitOptions {
    InitOptions {
        git_dir,
        separate_git_dir: None,
        bare: false,
        work_tree: None,
        object_dir: None,
        template: None,
        shared: None,
        object_format: None,
        ref_format: None,
        initial_branch: None,
    }
}

fn run(opts: &InitOptions, env: &MapEnv) -> (Result<Outcome, InitError>, Vec<Notice>) {
    let mut notices = Vec::new();
    let r = init(
        opts,
        env,
        &CommandLine::default(),
        ConfigSet::new(),
        &mut |n| notices.push(n),
    );
    (r, notices)
}

#[test]
fn fresh_and_reinit() {
    let tmp = TempDir::new("fresh");
    let env = MapEnv::new([("HOME", tmp.0.join("home"))]);
    let git_dir = tmp.0.join("repo/.git");
    let (r, notices) = run(&options(git_dir.clone()), &env);
    let out = r.unwrap();
    assert!(!out.reinitialized);
    assert!(!out.shared);
    assert_eq!(notices, [Notice::DefaultBranchName]);
    assert_eq!(
        fs::read_to_string(git_dir.join("HEAD")).unwrap(),
        "ref: refs/heads/master\n"
    );
    let config = fs::read_to_string(git_dir.join("config")).unwrap();
    assert!(config.starts_with("[core]\n\trepositoryformatversion = 0\n\tfilemode = "));
    assert!(config.ends_with("\tbare = false\n\tlogallrefupdates = true\n"));
    for d in [
        "refs/heads",
        "refs/tags",
        "objects/info",
        "objects/pack",
        "hooks",
    ] {
        assert!(git_dir.join(d).is_dir(), "{d}");
    }
    assert!(git_dir.join("info/exclude").is_file());
    assert!(git_dir.join("description").is_file());

    let mut opts = options(git_dir.clone());
    opts.initial_branch = Some("other".into());
    let (r, notices) = run(&opts, &env);
    assert!(r.unwrap().reinitialized);
    assert_eq!(
        notices,
        [Notice::Warning(
            "re-init: ignored --initial-branch=other".into()
        )]
    );
    assert_eq!(
        fs::read_to_string(git_dir.join("config")).unwrap(),
        config,
        "reinit keeps the configuration"
    );
}

#[test]
fn bare_shared_and_branch() {
    let tmp = TempDir::new("bare");
    let env = MapEnv::new([
        ("HOME", tmp.0.join("home").into_os_string()),
        ("GIT_TEST_DEFAULT_INITIAL_BRANCH_NAME", "trunk".into()),
    ]);
    let git_dir = tmp.0.join("b.git");
    let mut opts = options(git_dir.clone());
    opts.bare = true;
    opts.shared = Some(SharedRepository::Mode(0o640));
    opts.template = Some(Template::Empty);
    let (r, notices) = run(&opts, &env);
    assert!(r.unwrap().shared);
    assert!(notices.is_empty());
    assert_eq!(
        fs::read_to_string(git_dir.join("HEAD")).unwrap(),
        "ref: refs/heads/trunk\n"
    );
    let config = fs::read_to_string(git_dir.join("config")).unwrap();
    assert!(config.ends_with(
        "\tbare = true\n\tsharedrepository = 0640\n[receive]\n\tdenyNonFastforwards = true\n"
    ));
    assert!(!git_dir.join("description").exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &str| fs::metadata(git_dir.join(p)).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode("config"), 0o640);
        assert_eq!(mode("refs"), 0o2750);
    }
}

#[test]
fn refuses_unsupported_formats() {
    let tmp = TempDir::new("formats");
    let env = MapEnv::new([("HOME", tmp.0.join("home"))]);
    let mut opts = options(tmp.0.join("a/.git"));
    opts.object_format = Some(HashKind::Sha256);
    let err = run(&opts, &env).0.unwrap_err();
    assert!(err.to_string().contains("extensions.objectFormat=sha256"));
    assert!(!tmp.0.join("a/.git").exists());

    let mut opts = options(tmp.0.join("b/.git"));
    opts.initial_branch = Some("with space".into());
    let err = run(&opts, &env).0.unwrap_err();
    assert_eq!(err.to_string(), "invalid initial branch name: 'with space'");
}
