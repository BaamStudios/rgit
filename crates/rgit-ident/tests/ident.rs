// SPDX-License-Identifier: MIT
//! Identity resolution rules (expected results observed with the `git`
//! binary; see also the differential tests of `rgit var`).

use rgit_config::{MapEnv, Origin, Scope, parse_bytes};
use rgit_ident::{IdentConfig, Resolver, Role, Strictness, UserInfo};

fn user(full_name: &str, bogus: bool) -> UserInfo {
    UserInfo {
        login: Some("pat".into()),
        full_name: full_name.into(),
        domain: if bogus { "box.(none)" } else { "box.lan" }.into(),
        domain_bogus: bogus,
    }
}

fn config(text: &str) -> IdentConfig {
    let set = parse_bytes(text.as_bytes(), Origin::CommandLine, Scope::Command).unwrap();
    IdentConfig::from_config(&set).unwrap()
}

fn resolve(
    cfg: &str,
    env: &[(&str, &str)],
    user_info: UserInfo,
    role: Role,
    strictness: Strictness,
) -> Result<String, String> {
    let cfg = config(cfg);
    let env = MapEnv::new(env.iter().copied());
    Resolver::new(&cfg, &env)
        .with_user(user_info)
        .with_now(1000, 100)
        .ident(role, strictness)
        .map(|i| i.to_string())
        .map_err(|e| {
            let advice = if e.advice.is_some() { "advice: " } else { "" };
            format!("{advice}{}", e.message)
        })
}

use Role::{Author, Committer};
use Strictness::{Lenient, Strict};

#[test]
fn precedence() {
    let cfg = "[user]\nname = U\nemail = u@x\n[author]\nname = A\n[committer]\nemail = c@x\n";
    assert_eq!(
        resolve(cfg, &[], user("", true), Author, Strict).unwrap(),
        "A <u@x> 1000 +0100"
    );
    assert_eq!(
        resolve(cfg, &[], user("", true), Committer, Strict).unwrap(),
        "U <c@x> 1000 +0100"
    );
    let env = [("GIT_AUTHOR_NAME", "E"), ("GIT_AUTHOR_EMAIL", "")];
    assert_eq!(
        resolve(cfg, &env, user("", true), Author, Strict).unwrap(),
        "E <> 1000 +0100"
    );
    // Empty `author.*` falls back to `user.*`.
    let cfg = "[user]\nname = U\nemail = u@x\n[author]\nname =\nemail =\n";
    assert_eq!(
        resolve(cfg, &[], user("", true), Author, Strict).unwrap(),
        "U <u@x> 1000 +0100"
    );
}

#[test]
fn guessing() {
    assert_eq!(
        resolve("", &[], user("Pat Doe", false), Author, Strict).unwrap(),
        "Pat Doe <pat@box.lan> 1000 +0100"
    );
    assert_eq!(
        resolve("", &[], user("Pat Doe", true), Author, Strict).unwrap_err(),
        "advice: unable to auto-detect email address (got 'pat@box.(none)')"
    );
    assert_eq!(
        resolve("", &[], user("", true), Author, Lenient).unwrap(),
        "pat <pat@box.(none)> 1000 +0100"
    );
    assert_eq!(
        resolve("", &[("EMAIL", " <e@f> ")], user("", true), Author, Strict).unwrap_err(),
        "advice: empty ident name (for <<e@f>>) not allowed"
    );
    // Any `*.email` turns guessing off: the address is empty.
    assert_eq!(
        resolve(
            "[author]\nemail = a@b\n",
            &[("GIT_COMMITTER_NAME", "n")],
            user("", true),
            Committer,
            Strict
        )
        .unwrap(),
        "n <> 1000 +0100"
    );
}

#[test]
fn use_config_only() {
    let only = "[user]\nuseConfigOnly = true\n";
    assert_eq!(
        resolve(
            only,
            &[("EMAIL", "e@f")],
            user("Pat", false),
            Author,
            Strict
        )
        .unwrap_err(),
        "advice: no email was given and auto-detection is disabled"
    );
    assert_eq!(
        resolve(
            only,
            &[("GIT_AUTHOR_EMAIL", "e")],
            user("Pat", false),
            Author,
            Strict
        )
        .unwrap_err(),
        "advice: no name was given and auto-detection is disabled"
    );
    assert_eq!(
        resolve(only, &[], user("Pat", false), Author, Lenient).unwrap(),
        "Pat <pat@box.lan> 1000 +0100"
    );
}

#[test]
fn empty_and_crud_names() {
    let env = [("GIT_AUTHOR_EMAIL", "a@b")];
    assert_eq!(
        resolve("[user]\nname =\n", &env, user("Pat", false), Author, Strict).unwrap_err(),
        "advice: empty ident name (for <a@b>) not allowed"
    );
    let env = [("GIT_AUTHOR_EMAIL", "a@b"), ("GIT_AUTHOR_NAME", "")];
    assert_eq!(
        resolve("", &env, user("Pat", false), Author, Strict).unwrap_err(),
        "empty ident name (for <a@b>) not allowed"
    );
    assert_eq!(
        resolve("", &env, user("Pat", false), Author, Lenient).unwrap(),
        "pat <a@b> 1000 +0100"
    );
    let env = [("GIT_AUTHOR_EMAIL", "a@b"), ("GIT_AUTHOR_NAME", " ,;<>")];
    assert_eq!(
        resolve("", &env, user("Pat", false), Author, Strict).unwrap_err(),
        "name consists only of disallowed characters:  ,;<>"
    );
    assert_eq!(
        resolve("", &env, user("Pat", false), Author, Lenient).unwrap(),
        " <a@b> 1000 +0100"
    );
}

#[test]
fn dates() {
    let env = [
        ("GIT_AUTHOR_NAME", "n"),
        ("GIT_AUTHOR_EMAIL", "e"),
        ("GIT_AUTHOR_DATE", "@1234567890 +0200"),
    ];
    assert_eq!(
        resolve("", &env, user("", false), Author, Strict).unwrap(),
        "n <e> 1234567890 +0200"
    );
    let env = [
        ("GIT_AUTHOR_NAME", "n"),
        ("GIT_AUTHOR_EMAIL", "e"),
        ("GIT_AUTHOR_DATE", "garbage"),
    ];
    assert_eq!(
        resolve("", &env, user("", false), Author, Strict).unwrap_err(),
        "invalid date format: garbage"
    );
}
