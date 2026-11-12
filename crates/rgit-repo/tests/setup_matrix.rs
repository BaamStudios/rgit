// SPDX-License-Identifier: MIT
//! Differential test of repository setup: for every combination of
//! `GIT_DIR`, `GIT_WORK_TREE`, `core.worktree`, `.git` file and `core.bare`
//! and several starting directories, the five `GIT_TRACE_SETUP` values (or
//! the error) of the reference git must equal rgit-repo's result.

mod common;

use std::ffi::OsString;

use common::{World, relative};

#[test]
fn t1510_matrix() {
    let Some(w) = World::new() else {
        eprintln!("skipped: no reference git");
        return;
    };
    let mut mismatches = Vec::new();
    let mut count = 0;
    let worktree_cfgs: [Option<&str>; 5] = [
        None,
        Some("ABS_TOP"),
        Some(".."),
        Some("../wt"),
        Some("non-existent"),
    ];
    let mut n = 0;
    for gitfile in [false, true] {
        for bare in [None, Some("true"), Some("false")] {
            for wtcfg in worktree_cfgs {
                n += 1;
                let name = format!("r{n}");
                let top = w.root.join(&name);
                w.git(&w.root, &["init", "-q", &name]);
                for d in ["sub/sub", "wt/sub", ".git/wt/sub"] {
                    std::fs::create_dir_all(top.join(d)).unwrap();
                }
                let cfg = top.join(".git/config");
                let cfg = cfg.to_str().unwrap();
                if let Some(b) = bare {
                    w.git(&w.root, &["config", "--file", cfg, "core.bare", b]);
                }
                if let Some(v) = wtcfg {
                    let v = if v == "ABS_TOP" {
                        top.to_str().unwrap().to_string()
                    } else {
                        v.to_string()
                    };
                    w.git(&w.root, &["config", "--file", cfg, "core.worktree", &v]);
                }
                let gitdir = if gitfile {
                    let real = w.root.join(format!("{name}.git"));
                    std::fs::rename(top.join(".git"), &real).unwrap();
                    std::fs::write(top.join(".git"), format!("gitdir: ../{name}.git\n")).unwrap();
                    real
                } else {
                    top.join(".git")
                };
                let mut cwds = vec![
                    top.clone(),
                    top.join("sub/sub"),
                    top.join("wt/sub"),
                    w.root.clone(),
                ];
                if !gitfile {
                    cwds.push(gitdir.clone());
                    cwds.push(gitdir.join("wt/sub"));
                }
                for cwd in &cwds {
                    let dotgit = top.join(".git");
                    let gitdirs: Vec<Option<OsString>> = vec![
                        None,
                        Some(dotgit.clone().into()),
                        Some(relative(&dotgit, cwd).into()),
                    ];
                    let worktrees: Vec<Option<OsString>> = vec![
                        None,
                        Some(top.clone().into()),
                        Some(relative(&top, cwd).into()),
                        Some(top.join("wt").into()),
                        Some(relative(&top.join("wt"), cwd).into()),
                        Some(w.root.clone().into()),
                    ];
                    for gd in &gitdirs {
                        for wt in &worktrees {
                            let mut extra: Vec<(&str, OsString)> = Vec::new();
                            if let Some(g) = gd {
                                extra.push(("GIT_DIR", g.clone()));
                            }
                            if let Some(t) = wt {
                                extra.push(("GIT_WORK_TREE", t.clone()));
                            }
                            let expect = w.reference(cwd, &extra);
                            let got = w.ours(cwd, &extra);
                            count += 1;
                            if expect != got {
                                mismatches.push(format!(
                                    "case {name} gitfile={gitfile} bare={bare:?} core.worktree={wtcfg:?}\n  cwd={}\n  env={extra:?}\n--- git\n{expect}--- rgit\n{got}",
                                    cwd.display()
                                ));
                            }
                        }
                    }
                }
            }
        }
    }
    eprintln!("{count} cases compared");
    assert!(
        mismatches.is_empty(),
        "{} of {count} cases differ:\n{}",
        mismatches.len(),
        mismatches
            .iter()
            .take(15)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}
