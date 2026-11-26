// SPDX-License-Identifier: MIT
//! `git request-pull` (Documentation/git-request-pull.adoc): summarize
//! the changes between `<start>` and `<end>` and where to fetch them.
//!
//! The pieces come from rgit's own commands (`rev-parse`, `merge-base`,
//! `ls-remote`, `show`, `shortlog`, `diff`); the layout of the request and
//! the checks of the remote ref follow the `git` binary (t5150): the ref
//! named by `<end>` (or `<local>:<remote>`) must exist at `<url>` and
//! point to the same object as locally, else a warning is printed and the
//! exit status is 1.

use std::io::Write;
use std::process::{Command, Stdio};

use crate::messages::request_pull as msg;
use crate::prelude::*;

fn rgit(ctx: &Context) -> Command {
    let exe = std::env::current_exe().unwrap_or_else(|_| "git".into());
    let mut c = Command::new(exe);
    c.envs(ctx.child_env());
    c.stdin(Stdio::null());
    c
}

/// Run an rgit command; its standard output when it succeeds.
fn output(ctx: &Context, args: &[&str]) -> Option<Vec<u8>> {
    let out = rgit(ctx).args(args).stderr(Stdio::null()).output().ok()?;
    out.status.success().then_some(out.stdout)
}

fn output_str(ctx: &Context, args: &[&str]) -> Option<String> {
    output(ctx, args).map(|o| String::from_utf8_lossy(&o).trim_end().to_string())
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let mut patch = false;
    let mut positional: Vec<&str> = Vec::new();
    let mut dashdash = false;
    for a in &argv[1..] {
        if dashdash {
            positional.push(a);
            continue;
        }
        match a.as_str() {
            "-h" | "--help-all" => {
                // Like git, asking for help is no error here.
                print!("{}", msg::USAGE);
                return Ok(0);
            }
            "-p" => patch = true,
            "--" => dashdash = true,
            s if s.starts_with('-') && s.len() > 1 => {
                eprint!("{}", msg::USAGE);
                return Err(Die::Exit(129));
            }
            s => positional.push(s),
        }
    }
    if positional.len() < 2 || positional.len() > 3 {
        eprint!("{}", msg::USAGE);
        return Err(Die::Exit(1));
    }
    let start = positional[0];
    let url = positional[1];
    let end = positional.get(2).copied().unwrap_or("HEAD");
    let (local, remote) = match end.split_once(':') {
        Some((l, r)) => (if l.is_empty() { "HEAD" } else { l }, r),
        None => (end, end),
    };
    let cwd = std::env::current_dir().ok();
    let _ = crate::repo::setup(ctx, crate::repo::Mode::Required)?;
    if let Some(d) = cwd {
        let _ = std::env::set_current_dir(d);
    }

    let Some(base_tip) = output_str(ctx, &["rev-parse", "--verify", "--quiet", &format!("{start}^0")])
    else {
        eprintln!("{}", msg::not_a_revision(start));
        return Err(Die::Exit(1));
    };
    let Some(head) = output_str(ctx, &["rev-parse", "--verify", "--quiet", &format!("{local}^0")])
    else {
        eprintln!("{}", msg::not_a_revision(local));
        return Err(Die::Exit(1));
    };
    let local_object = output_str(ctx, &["rev-parse", "--verify", "--quiet", local])
        .unwrap_or_else(|| head.clone());
    let Some(merge_base) = output_str(ctx, &["merge-base", &base_tip, &head]) else {
        eprintln!("{}", msg::no_common_commits(start, local));
        return Err(Die::Exit(1));
    };

    // The branch whose description goes into the request.
    let branch = output_str(ctx, &["rev-parse", "--symbolic-full-name", local])
        .and_then(|r| r.strip_prefix("refs/heads/").map(str::to_string));

    // The URL to show: a remote's configured URL for a remote name.
    let shown_url = output_str(ctx, &["config", "--get", &format!("remote.{url}.url")])
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| url.to_string());

    // Find the ref at the remote.
    let listing = output(ctx, &["ls-remote", url]).unwrap_or_default();
    let refs: Vec<(String, String)> = String::from_utf8_lossy(&listing)
        .lines()
        .filter_map(|l| {
            let (oid, name) = l.split_once('\t')?;
            (!name.ends_with("^{}")).then(|| (oid.to_string(), name.to_string()))
        })
        .collect();
    let mut status = 0;
    let found = find_ref(&refs, remote);
    let shown_ref = match &found {
        Some((_, name)) if name == "HEAD" => String::new(),
        Some((_, name)) => short_ref(name),
        None if remote == "HEAD" => String::new(),
        None => short_ref(remote),
    };
    match &found {
        Some((oid, _)) if *oid == local_object || (remote == "HEAD" && *oid == head) => {}
        Some((_, name)) => {
            eprintln!("{}", msg::different_object(name, url));
            eprintln!("{}", msg::pushed_there(remote));
            status = 1;
        }
        None => {
            eprintln!("{}", msg::no_match(&head, url));
            eprintln!("{}", msg::pushed_there(remote));
            status = 1;
        }
    }

    let describe = |oid: &str| {
        output_str(ctx, &["show", "-s", "--format=%s (%ci)", oid])
            .unwrap_or_default()
    };
    let mut out = String::new();
    out.push_str(&format!(
        "{}\n\n  {}\n\n{}\n\n  {shown_url} {shown_ref}\n\n{}\n\n  {}\n\n{}\n",
        msg::since_commit(&merge_base),
        describe(&merge_base),
        msg::AVAILABLE_AT,
        msg::fetch_up_to(&head),
        describe(&head),
        msg::RULE,
    ));
    let is_tag = found
        .as_ref()
        .is_some_and(|(_, n)| n.starts_with("refs/tags/"));
    if is_tag {
        if let Some(text) = output(ctx, &["cat-file", "tag", &local_object]) {
            let text = String::from_utf8_lossy(&text);
            let message = text.split_once("\n\n").map_or("", |x| x.1);
            let message = match message.find("-----BEGIN PGP SIGNATURE-----") {
                Some(i) => &message[..i],
                None => message,
            };
            out.push_str(message);
            if !message.ends_with('\n') {
                out.push('\n');
            }
            out.push('\n');
            out.push_str(msg::RULE);
            out.push('\n');
        }
    } else if let Some(b) = &branch
        && let Some(desc) = output(ctx, &["config", "--get", &format!("branch.{b}.description")])
    {
        let desc = String::from_utf8_lossy(&desc);
        out.push_str(&msg::from_description(b));
        out.push_str("\n\n");
        out.push_str(&desc);
        if !desc.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(msg::RULE);
        out.push('\n');
    }
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(out.as_bytes());
    let range = format!("{merge_base}..{head}");
    let shortlog = output(ctx, &["shortlog", &format!("^{merge_base}"), &head]).unwrap_or_default();
    let _ = stdout.write_all(&shortlog);
    let mut diff_args = vec!["diff", "-M", "--stat", "--summary"];
    if patch {
        diff_args.push("-p");
    }
    diff_args.push(&range);
    let diff = output(ctx, &diff_args).unwrap_or_default();
    let _ = stdout.write_all(&diff);
    let _ = stdout.flush();
    Ok(status)
}

/// The ref at the remote that `name` means: exactly that name, under
/// `refs/`, a branch, a tag, or any ref ending in `/<name>`.
fn find_ref(refs: &[(String, String)], name: &str) -> Option<(String, String)> {
    let candidates = [
        name.to_string(),
        format!("refs/{name}"),
        format!("refs/heads/{name}"),
        format!("refs/tags/{name}"),
    ];
    for c in &candidates {
        if let Some(r) = refs.iter().find(|(_, n)| n == c) {
            return Some(r.clone());
        }
    }
    if name == "HEAD" {
        return None;
    }
    let suffix = format!("/{name}");
    refs.iter()
        .find(|(_, n)| n.starts_with("refs/") && n.ends_with(&suffix))
        .cloned()
}

/// `refs/heads/x` is shown as `x`, other refs without `refs/`.
fn short_ref(name: &str) -> String {
    name.strip_prefix("refs/heads/")
        .or_else(|| name.strip_prefix("refs/"))
        .unwrap_or(name)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refs() -> Vec<(String, String)> {
        vec![
            ("1".into(), "HEAD".into()),
            ("2".into(), "refs/heads/for-upstream".into()),
            ("3".into(), "refs/tags/full".into()),
            ("4".into(), "refs/tags/v2-0".into()),
        ]
    }

    #[test]
    fn finds_refs_by_short_names() {
        assert_eq!(find_ref(&refs(), "full").unwrap().1, "refs/tags/full");
        assert_eq!(find_ref(&refs(), "tags/full").unwrap().1, "refs/tags/full");
        assert_eq!(find_ref(&refs(), "for-upstream").unwrap().0, "2");
        assert_eq!(find_ref(&refs(), "HEAD").unwrap().0, "1");
        assert!(find_ref(&refs(), "tags/v2.0").is_none());
    }

    #[test]
    fn short_names() {
        assert_eq!(short_ref("refs/heads/main"), "main");
        assert_eq!(short_ref("refs/tags/full"), "tags/full");
    }
}
