// SPDX-License-Identifier: MIT
//! Unit tests: configuration order and execution semantics.

use super::*;
use rgit_config::{Origin, parse_bytes};

fn config(text: &str) -> ConfigSet {
    parse_bytes(
        text.as_bytes(),
        Origin::File(PathBuf::from("config")),
        Scope::Local,
    )
    .expect("config parses")
}

fn hooks(text: &str, dir: Option<&Path>, run_dir: &Path) -> Hooks {
    Hooks::new(
        &config(text),
        dir.map(|d| HookDir {
            shown: d.to_path_buf(),
            path: d.to_path_buf(),
        }),
        run_dir.to_path_buf(),
        Vec::new(),
        Advisor::silent(),
    )
}

fn names(h: &Hooks, event: &str) -> Vec<String> {
    h.find(event)
        .expect("no fatal error")
        .into_iter()
        .map(|h| match h.source {
            Source::Configured { name, .. } => name,
            Source::HookDir { .. } => "<hookdir>".into(),
        })
        .collect()
}

#[test]
fn order_follows_event_lines() {
    let dir = tempfile::tempdir().unwrap();
    let h = hooks(
        "[hook \"b\"]\n\tcommand = b\n\tevent = e\n[hook \"a\"]\n\tcommand = a\n\tevent = e\n[hook \"b\"]\n\tevent = e\n",
        None,
        dir.path(),
    );
    assert_eq!(names(&h, "e"), ["a", "b"]);
}

#[test]
fn empty_event_resets() {
    let dir = tempfile::tempdir().unwrap();
    let h = hooks(
        "[hook \"a\"]\n\tcommand = a\n\tevent = e\n\tevent = f\n\tevent =\n\tevent = g\n",
        None,
        dir.path(),
    );
    assert!(names(&h, "e").is_empty());
    assert!(names(&h, "f").is_empty());
    assert_eq!(names(&h, "g"), ["a"]);
}

#[test]
fn disabled_and_event_disabled() {
    let dir = tempfile::tempdir().unwrap();
    let h = hooks(
        "[hook \"a\"]\n\tcommand = a\n\tevent = e\n\tenabled = false\n[hook \"b\"]\n\tcommand = b\n\tevent = e\n",
        None,
        dir.path(),
    );
    assert_eq!(names(&h, "e"), ["b"]);
    let h = hooks(
        "[hook \"b\"]\n\tcommand = b\n\tevent = e\n[hook \"e\"]\n\tenabled = false\n",
        None,
        dir.path(),
    );
    assert!(names(&h, "e").is_empty());
    let listed = h.list("e").unwrap();
    assert_eq!(listed[0].state, State::EventDisabled);
}

#[test]
fn fatal_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let h = hooks("[hook \"x\"]\n\tevent = e\n", None, dir.path());
    assert_eq!(h.find("other"), Err(msg::missing_command("x")));
    let h = hooks(
        "[hook \"pre-commit\"]\n\tevent = post-commit\n\tcommand = c\n",
        None,
        dir.path(),
    );
    assert_eq!(h.find("e"), Err(msg::collides_with_event("pre-commit")));
}

#[cfg(unix)]
#[test]
fn exit_codes_are_ored_and_stdin_is_fed() {
    let dir = tempfile::tempdir().unwrap();
    let h = hooks(
        "[hook \"a\"]\n\tcommand = exit 1\n\tevent = e\n[hook \"b\"]\n\tcommand = exit 2\n\tevent = e\n",
        None,
        dir.path(),
    );
    let out = h.run("e", &RunOptions::default()).unwrap();
    assert_eq!(
        out,
        Outcome {
            found: true,
            ran: 2,
            code: 3
        }
    );

    let h = hooks(
        "[hook \"a\"]\n\tcommand = \"cat >out\"\n\tevent = e\n[hook \"b\"]\n\tcommand = true\n\tevent = e\n",
        None,
        dir.path(),
    );
    let data = vec![b'x'; 1 << 20];
    let out = h
        .run("e", &RunOptions::default().stdin(Input::Data(&data)))
        .unwrap();
    assert!(out.ok());
    assert_eq!(
        std::fs::read(dir.path().join("out")).unwrap().len(),
        1 << 20
    );
}

#[cfg(unix)]
#[test]
fn hookdir_needs_executable_bit() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let hooks_dir = dir.path().join("hooks");
    std::fs::create_dir(&hooks_dir).unwrap();
    let file = hooks_dir.join("e");
    std::fs::write(&file, "#!/bin/sh\necho \"$1\" >arg\n").unwrap();
    let h = hooks("", Some(&hooks_dir), dir.path());
    assert!(!h.exists("e"));
    assert!(names(&h, "e").is_empty());
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(h.exists("e"));
    let out = h.run("e", &RunOptions::with_args(["one"])).unwrap();
    assert!(out.ok());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("arg")).unwrap(),
        "one\n"
    );
}
