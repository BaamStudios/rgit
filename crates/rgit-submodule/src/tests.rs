// SPDX-License-Identifier: MIT
//! Unit tests; expectations observed from the reference `test-tool
//! submodule` helpers.

use bstr::ByteSlice;

use crate::*;

#[test]
fn names() {
    for ok in [
        "valid",
        "valid/with/paths",
        ".",
        "./foo",
        "foo/./bar",
        "..foo",
        "foo..",
        "/foo",
        "foo//bar",
        "a\\b",
        "-x",
        " ..",
        "...",
        ".. /x",
        "x/..:",
    ] {
        assert!(check_name(ok.as_bytes()), "{ok}");
    }
    for bad in [
        "",
        "..",
        "../foo",
        "/../foo",
        "..\\foo",
        "\\..\\foo",
        "foo/..",
        "foo/../",
        "foo\\..",
        "foo\\..\\",
        "foo/../bar",
        "modules/../x",
    ] {
        assert!(!check_name(bad.as_bytes()), "{bad}");
    }
}

#[test]
fn urls() {
    for ok in [
        "./bar/baz/foo.git",
        "https://example.com/foo.git",
        "http://example.com:80/deeper/foo.git",
        "../foo//bar",
        "./foo/..//x",
        "foo:bar",
        "../x:foo",
        "https://ex.com/%0d",
        "ssh://-oProxy/x",
        "file:///tmp/x",
        "/abs/path",
        "user@host:path",
        "",
        "https://ex.com/a%5cb",
        "https://ex.com:/x",
        "a::b",
        "./::b",
        "https:/x",
        "./a%0d",
        "../a%25",
        "x%0ay",
        "https::https://example.com/x",
        "ftp::ftp://ex/x",
        "ssh::x",
        "HTTPS::x",
        "Https://ex.com/%0a",
        "ssh://ex.com/%0a",
        "data://acjbkd%0akajfdickajkd",
        "git:///x",
        "git://ex.com/%",
        "git://:99999/x",
        "http://u:p@h/x",
        "http://[::1]/x",
        "http://h:65535/x",
        "http://h:00080/x",
        "http://[x/y",
        "http://@h/x",
        "https://ex.com/x%00y",
        "..",
        "../",
        "../..",
        ".//x",
        "./:foo",
        "../\\x",
        "..\\\\/x",
        "../.:x",
        "../%2fx",
        ".%0a",
        "./%",
        "https::ssh://x",
    ] {
        assert!(check_url(ok.as_bytes()), "{ok}");
    }
    for bad in [
        "-a./foo",
        "../../..//test/foo.git",
        "../../../../../:localhost:8080/foo.git",
        "..\\../.\\../:example.com/foo.git",
        "./%0ahost=example.com/foo.git",
        "https://one.example.com/evil?%0ahost=two.example.com",
        "https:///example.com/foo.git",
        "http://example.com:test/foo.git",
        "https::example.com/foo.git",
        "http:::example.com/foo.git",
        "http::://one.example.com/foo.git",
        "../../../:://one.example.com/foo.git",
        "../:foo",
        "../foo%0a",
        "https://ex.com/%0a",
        "-x",
        "ftp::ex",
        "ftps::x",
        "http::",
        "git://example.com:1234/repo%0a.git",
        "ftp://ex.com/%0a",
        "http://",
        "http://user@/x",
        "http://h:99999999/x",
        "http://h:0/x",
        "http://h:-1/x",
        "http://h:65536/x",
        "http://h/%",
        "http://h/%zz",
        "http://h/x%0",
        "http://h@@x/x",
        "./..//x",
        ".././/x",
        "..\\:x",
        "./%0A",
        "https::ssh://x/%0a",
        "https::https::https://h/x",
    ] {
        assert!(!check_url(bad.as_bytes()), "{bad}");
    }
}

fn rel(up: Option<&str>, remote: &str, url: &str) -> String {
    match resolve_relative_url(up.map(str::as_bytes), remote.as_bytes(), url.as_bytes()) {
        Ok(s) => s.to_str_lossy().into_owned(),
        Err(e) => format!("error: {e}"),
    }
}

#[test]
fn relative_urls() {
    let cases: &[(Option<&str>, &str, &str, &str)] = &[
        (Some("../"), "../foo", "../submodule", "../../submodule"),
        (
            Some("../"),
            "../foo/bar",
            "../submodule",
            "../../foo/submodule",
        ),
        (Some("../"), "./foo", "../submodule", "../submodule"),
        (Some("../"), "./foo/bar", "../submodule", "../foo/submodule"),
        (
            Some("../../../"),
            "../foo/bar",
            "../sub/a/b/c",
            "../../../../foo/sub/a/b/c",
        ),
        (Some("../"), "/w/addtest", "../repo", "/w/repo"),
        (Some("../"), "foo/bar", "../submodule", "../foo/submodule"),
        (Some("../"), "foo", "../submodule", "../submodule"),
        (None, "../foo/bar", "../sub/a/b/c/", "../foo/sub/a/b/c"),
        (None, "../foo/bar/", "../sub/a/b/c", "../foo/sub/a/b/c"),
        (None, "../foo", "../submodule", "../submodule"),
        (None, "./foo/bar", "../submodule", "foo/submodule"),
        (None, "./foo", "../submodule", "submodule"),
        (
            None,
            "//somewhere else/repo",
            "../subrepo",
            "//somewhere else/subrepo",
        ),
        (None, "//somewhere else/repo", "../../subrepo", "//subrepo"),
        (
            None,
            "//somewhere else/repo",
            "../../../subrepo",
            "/subrepo",
        ),
        (
            None,
            "//somewhere else/repo",
            "../../../../subrepo",
            "subrepo",
        ),
        (None, "/w/.", "../.", "/w/."),
        (None, "/w", "./.", "/w/."),
        (None, "/w", "./å äö", "/w/å äö"),
        (
            None,
            "/w/home2/../remote",
            "../bundle1",
            "/w/home2/../bundle1",
        ),
        (
            None,
            "file:///tmp/repo",
            "../subrepo",
            "file:///tmp/subrepo",
        ),
        (None, "foo/bar", "../submodule", "foo/submodule"),
        (None, "foo", "../submodule", "submodule"),
        (
            None,
            "helper:://hostname/repo",
            "../../subrepo",
            "helper:://subrepo",
        ),
        (
            None,
            "helper:://hostname/repo",
            "../../../subrepo",
            "helper::/subrepo",
        ),
        (
            None,
            "helper:://hostname/repo",
            "../../../../subrepo",
            "helper::subrepo",
        ),
        (
            None,
            "helper:://hostname/repo",
            "../../../../../subrepo",
            "helper:subrepo",
        ),
        (
            None,
            "helper:://hostname/repo",
            "../../../../../../subrepo",
            ".:subrepo",
        ),
        (
            None,
            "ssh://hostname/repo",
            "../../../subrepo",
            "ssh:/subrepo",
        ),
        (
            None,
            "ssh://hostname/repo",
            "../../../../subrepo",
            "ssh:subrepo",
        ),
        (
            None,
            "ssh://hostname/repo",
            "../../../../../subrepo",
            ".:subrepo",
        ),
        (
            None,
            "ssh://hostname:22/repo",
            "../subrepo",
            "ssh://hostname:22/subrepo",
        ),
        (
            None,
            "user@host:path/to/repo",
            "../subrepo",
            "user@host:path/to/subrepo",
        ),
        (None, "user@host:repo", "../subrepo", "user@host:subrepo"),
        (None, "user@host:repo", "../../subrepo", ".:subrepo"),
        (
            None,
            "../foo",
            "../../x",
            "error: cannot strip one component off url '..'",
        ),
        (None, "..", "../x", "x"),
        (None, "a/b:c", "../../x", "x"),
        (
            None,
            "a/b:c",
            "../../../x",
            "error: cannot strip one component off url '.'",
        ),
        (None, "foo", "x", "foo/x"),
        (None, "foo", "./../x", "x"),
        (None, "foo/bar/", "../x//", "foo/x/"),
        (None, "foo", "..\\x", "foo/..\\x"),
        (Some("../"), "foo:bar", "../x", "foo:x"),
        (Some("../"), "/abs", "../x", "/x"),
        (Some("../"), ".", "../x", "../x"),
        (Some("../"), "./", "../x", "../x"),
        (None, ".", "./x", "./x"),
        (None, "", "./x", "error: invalid empty remote_url"),
        (None, "/", "../x", "x"),
        (None, "foo", "../", ""),
        (None, "foo/bar", "..", "foo/bar/.."),
        (Some("x/"), "foo", "./y", "x/foo/y"),
        (Some("x"), "foo", "./y", "xfoo/y"),
        (None, "C:/repo", "../x", "C:/x"),
    ];
    for (up, remote, url, want) in cases {
        assert_eq!(rel(*up, remote, url), *want, "{up:?} {remote} {url}");
    }
}

fn parse(text: &str, mode: Mode) -> Result<(Gitmodules, Vec<Diagnostic>), ParseError> {
    Gitmodules::parse(
        text.as_bytes(),
        &Origin::File("/r/.gitmodules".into()),
        mode,
    )
}

#[test]
fn gitmodules_lookup() {
    let (gm, diags) = parse(
        "[submodule \"a\"]\n\tpath = p1\n\tpath = p2\n\turl = u1\n\
         [submodule \"b\"]\n\tpath = p2\n\
         [submodule \"../evil\"]\n\tpath = evil\n\
         [submodule]\n\tpath = noname\n\
         [submodule \"d\"]\n\tpath = d\n\tignore = bogus\n\tupdate = merge\n\
         [submodule \"e\"]\n\tpath = -e\n\turl = -u\n",
        Mode::Current,
    )
    .unwrap();
    assert_eq!(
        diags,
        vec![
            Diagnostic::Warning("ignoring suspicious submodule name: ../evil".into()),
            Diagnostic::Warning(
                "Invalid parameter 'bogus' for config option 'submodule.d.ignore'".into()
            ),
            Diagnostic::Warning(
                "ignoring 'submodule.e.path' which may be interpreted as a command-line option: -e"
                    .into()
            ),
            Diagnostic::Warning(
                "ignoring 'submodule.e.url' which may be interpreted as a command-line option: -u"
                    .into()
            ),
        ]
    );
    assert!(gm.by_path(b"p1").is_none());
    assert_eq!(gm.by_path(b"p2").unwrap().name, "b");
    assert_eq!(
        gm.by_name(b"a").unwrap().path.as_ref().map(|p| p.as_bstr()),
        Some(b"p2".as_bstr())
    );
    assert_eq!(
        gm.by_name(b"a").unwrap().url.as_ref().map(|p| p.as_bstr()),
        Some(b"u1".as_bstr())
    );
    assert!(gm.by_path(b"evil").is_none() && gm.by_path(b"noname").is_none());
    let d = gm.by_name(b"d").unwrap();
    assert_eq!((d.ignore, d.update.clone()), (None, Some(Update::Merge)));
    assert!(gm.by_name(b"e").is_none());
}

#[test]
fn gitmodules_errors() {
    let one = |line: &str, mode| parse(&format!("[submodule \"s\"]\n\tpath = s\n\t{line}\n"), mode);
    let fatal = |line: &str| one(line, Mode::Current).unwrap_err().fatal;
    assert_eq!(
        fatal("update = bogus"),
        "invalid value for 'submodule.s.update'"
    );
    assert_eq!(
        fatal("update = !rm"),
        "invalid value for 'submodule.s.update'"
    );
    assert_eq!(
        fatal("fetchRecurseSubmodules = bogus"),
        "bad submodule.s.fetchrecursesubmodules argument: bogus"
    );
    assert_eq!(
        fatal("shallow = bogus"),
        "bad boolean config value 'bogus' for 'submodule.s.shallow'"
    );
    let e = one("url", Mode::Current).unwrap_err();
    assert_eq!(e.fatal, "bad config line 3 in file /r/.gitmodules");
    assert_eq!(
        e.diagnostics,
        vec![Diagnostic::Error(
            "missing value for 'submodule.s.url'".into()
        )]
    );
    assert_eq!(
        fatal("value = \""),
        "bad config line 3 in file /r/.gitmodules"
    );
    let (gm, _) = one("fetchRecurseSubmodules = on-demand", Mode::Current).unwrap();
    assert_eq!(
        gm.by_name(b"s").unwrap().fetch_recurse,
        Some(FetchRecurse::OnDemand)
    );
    let (gm, _) = one("fetchRecurseSubmodules = bogus", Mode::History).unwrap();
    assert_eq!(gm.by_name(b"s").unwrap().fetch_recurse, None);

    let blob = Origin::Blob("1234:.gitmodules".into());
    let (gm, diags) = Gitmodules::parse(
        b"[submodule \"s\"]\n\tpath = s\n\tpath = t\n\tvalue = \"\n[submodule \"t\"]\n\tpath = t\n",
        &blob,
        Mode::History,
    )
    .unwrap();
    assert_eq!(
        diags,
        vec![
            Diagnostic::Warning(
                "1234:.gitmodules, multiple configurations found for 'submodule.s.path'. Skipping second one!"
                    .into()
            ),
            Diagnostic::Error("bad config line 4 in submodule-blob 1234:.gitmodules".into()),
        ]
    );
    assert_eq!(gm.by_path(b"s").unwrap().name, "s");
    assert!(gm.by_name(b"t").is_none());
}

#[test]
fn active() {
    let cfg = |text: &str| {
        rgit_config::parse_bytes(
            text.as_bytes(),
            rgit_config::Origin::CommandLine,
            rgit_config::Scope::Local,
        )
        .unwrap()
    };
    let c = cfg(
        "[submodule \"foo\"]\n active = false\n url = x\n[submodule \"bar\"]\n active = true\n\
         [submodule \"baz\"]\n url = y\n",
    );
    assert!(!is_active(&c, b"foo", b"foo"));
    assert!(is_active(&c, b"bar", b"bar"));
    assert!(is_active(&c, b"baz", b"baz"));
    assert!(!is_active(&c, b"none", b"none"));
    let c = cfg(
        "[submodule \"foo\"]\n active = true\n url = x\n[submodule \"bar\"]\n url = y\n\
         [submodule \"baz\"]\n url = z\n[submodule]\n active = b*\n active = :(exclude) baz\n",
    );
    // The value keeps the space after `)`, so the exclude names " baz" and
    // does not exclude "baz" (gitsubmodules(7) suggests otherwise; git
    // behaves like this).
    for (n, want) in [
        ("foo", true),
        ("bar", true),
        ("baz", true),
        ("bob", true),
        ("x", false),
    ] {
        assert_eq!(is_active(&c, n.as_bytes(), n.as_bytes()), want, "{n}");
    }
}

#[test]
fn settings_override_gitmodules() {
    let (gm, _) = parse(
        "[submodule \"s\"]\n\tpath = s\n\tignore = dirty\n\tupdate = merge\n",
        Mode::Current,
    )
    .unwrap();
    let c = rgit_config::parse_bytes(
        b"[submodule \"s\"]\n ignore = all\n update = !true\n",
        rgit_config::Origin::CommandLine,
        rgit_config::Scope::Local,
    )
    .unwrap();
    let s = gm.by_name(b"s");
    assert_eq!(settings::ignore(&c, b"s", s), Some(Ignore::All));
    assert_eq!(
        settings::update(&c, b"s", s),
        Some(Update::Command("true".into()))
    );
    let empty = rgit_config::ConfigSet::new();
    assert_eq!(settings::ignore(&empty, b"s", s), Some(Ignore::Dirty));
}
