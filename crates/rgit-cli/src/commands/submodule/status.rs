// SPDX-License-Identifier: MIT
//! `git submodule status`.

use std::path::Path;

use crate::messages::submodule_cmd as msg;
use crate::prelude::*;

use super::common::{self, Super};

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let quiet = Cell::new(false);
    let cached = Cell::new(false);
    let recursive = Cell::new(false);
    let super_prefix = RefCell::new(None::<String>);
    let mut parser = Parser::new(
        msg::USAGE_STATUS,
        vec![
            Opt::flag('q', "quiet", &quiet, msg::HELP_STATUS_QUIET),
            Opt::flag(None, "cached", &cached, msg::HELP_STATUS_CACHED),
            Opt::flag(None, "recursive", &recursive, msg::HELP_RECURSIVE),
            Opt::string(None, "super-prefix", &super_prefix, "path", "").hidden(),
        ],
    );
    let args = parser.parse(argv)?.args;
    drop(parser);
    let mut sup = Super::open(ctx, super_prefix.borrow().as_deref())?;
    let list = sup.module_list(&args)?;
    let gm = sup.gitmodules();
    let mut out = String::new();
    let flush = |out: &mut String| {
        if !quiet.get() {
            print!("{out}");
        }
        out.clear();
    };
    for m in &list {
        let name = match sup.name_of(&gm, &m.path) {
            Ok(n) => n,
            Err(e) => {
                flush(&mut out);
                return Err(e);
            }
        };
        let display = sup.display_path(&m.path);
        let dir = sup.work_tree.join(&m.path);
        if m.unmerged {
            out.push_str(&format!("U{} {display}\n", m.oid.to_hex()));
            continue;
        }
        if !sup.is_active(&name, &m.path) || !common::is_populated(&dir) {
            out.push_str(&format!("-{} {display}\n", m.oid.to_hex()));
            continue;
        }
        let head = common::sub_head(&dir, sup.hash());
        if head == Some(m.oid) {
            let d = describe(&sup, &dir, &m.oid.to_hex());
            out.push_str(&format!(" {} {display}{d}\n", m.oid.to_hex()));
        } else {
            let shown = if cached.get() {
                m.oid
            } else {
                head.unwrap_or(m.oid)
            };
            let d = describe(&sup, &dir, &shown.to_hex());
            out.push_str(&format!("+{} {display}{d}\n", shown.to_hex()));
        }
        if recursive.get() {
            flush(&mut out);
            let mut a = vec![
                "submodule".to_owned(),
                "status".to_owned(),
                "--recursive".to_owned(),
                format!("--super-prefix={display}/"),
            ];
            if cached.get() {
                a.push("--cached".into());
            }
            if quiet.get() {
                a.push("--quiet".into());
            }
            let refs: Vec<&str> = a.iter().map(String::as_str).collect();
            let mut cmd = sup.sub_git(Path::new(&m.path), &refs)?;
            if common::run_status(&mut cmd)? != 0 {
                return Err(Die::Fatal(msg::failed_recurse(&display)));
            }
        }
    }
    flush(&mut out);
    Ok(0)
}

/// ` (<describe>)` for `rev` in the submodule: an annotated tag, any tag,
/// a containing tag, else any ref or the abbreviated name.
fn describe(sup: &Super, dir: &Path, rev: &str) -> String {
    let tries: [&[&str]; 4] = [
        &["describe"],
        &["describe", "--tags"],
        &["describe", "--contains"],
        &["describe", "--all", "--always"],
    ];
    for t in tries {
        let mut a: Vec<&str> = t.to_vec();
        a.push(rev);
        let Ok(mut cmd) = sup.sub_git(dir, &a) else {
            continue;
        };
        if let Some(o) = common::capture(&mut cmd) {
            let o = o.trim();
            if !o.is_empty() {
                return format!(" ({o})");
            }
        }
    }
    String::new()
}
