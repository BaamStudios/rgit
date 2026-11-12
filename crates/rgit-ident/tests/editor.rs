// SPDX-License-Identifier: MIT
//! Running the editor (expected messages observed with the `git` binary,
//! t7005).

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use rgit_config::{Level, MapEnv};
use rgit_ident::editor::{LaunchOptions, launch_with_term};

fn script(dir: &Path, name: &str, body: &str) {
    let p = dir.join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn lines(r: Result<(), rgit_ident::editor::EditorError>) -> Vec<(Level, String)> {
    r.unwrap_err().report().to_vec()
}

#[test]
fn runs_editor_on_file() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    script(
        d,
        "ed.sh",
        "for a; do f=$a; done; echo \"edited $#\" > \"$f\"",
    );
    script(d, "e space.sh", "echo space > \"$1\"");
    let file = d.join("MSG");
    std::fs::write(&file, "x").unwrap();
    let env = MapEnv::new([("TERM", "dumb")]);
    let opts = LaunchOptions::default();

    let ed = d.join("ed.sh");
    launch_with_term(Some(ed.to_str().unwrap()), &file, &opts, &env).unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "edited 1\n");

    // Shell syntax: arguments in the editor string and escaped spaces.
    let cmd = format!("{} --flag", ed.display());
    launch_with_term(Some(&cmd), &file, &opts, &env).unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "edited 2\n");
    let opts_cwd = LaunchOptions {
        cwd: Some(d.to_path_buf()),
        ..Default::default()
    };
    launch_with_term(Some("./e\\ space.sh"), &file, &opts_cwd, &env).unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "space\n");

    // `:` does nothing.
    launch_with_term(Some(":"), &file, &opts, &env).unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "space\n");
}

#[test]
fn editor_failures() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("MSG");
    std::fs::write(&file, "x").unwrap();
    let env = MapEnv::new([("TERM", "dumb")]);
    let opts = LaunchOptions::default();

    assert_eq!(
        lines(launch_with_term(None, &file, &opts, &env)),
        vec![(Level::Error, "Terminal is dumb, but EDITOR unset".into())]
    );
    assert_eq!(
        lines(launch_with_term(Some("false"), &file, &opts, &env)),
        vec![(
            Level::Error,
            "there was a problem with the editor 'false'".into()
        )]
    );
    assert_eq!(
        lines(launch_with_term(Some("exit 3;"), &file, &opts, &env)),
        vec![(
            Level::Error,
            "there was a problem with the editor 'exit 3;'".into()
        )]
    );
    assert_eq!(
        lines(launch_with_term(Some("kill -TERM $$;"), &file, &opts, &env)),
        vec![
            (Level::Error, "kill -TERM $$; died of signal 15".into()),
            (
                Level::Error,
                "there was a problem with the editor 'kill -TERM $$;'".into()
            )
        ]
    );
    assert_eq!(
        lines(launch_with_term(Some(""), &file, &opts, &env)),
        vec![
            (
                Level::Error,
                "cannot run : No such file or directory".into()
            ),
            (Level::Error, "unable to start editor ''".into())
        ]
    );
    assert_eq!(
        lines(launch_with_term(
            Some("/nonexistent/ed"),
            &file,
            &opts,
            &env
        )),
        vec![
            (
                Level::Fatal,
                "cannot exec '/nonexistent/ed': No such file or directory".into()
            ),
            (
                Level::Error,
                "unable to start editor '/nonexistent/ed'".into()
            )
        ]
    );
}
