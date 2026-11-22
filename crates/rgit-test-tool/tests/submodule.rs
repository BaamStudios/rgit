// SPDX-License-Identifier: MIT

//! `submodule check-name`, `check-url` and `resolve-relative-url` against
//! the reference `test-tool` (cases of t7450 and t0060 plus edge cases).

mod common;

const NAMES: &str = "valid\nvalid/with/paths\n\n../foo\n/../foo\n..\\foo\n\\..\\foo\nfoo/..\n\
foo/../\nfoo\\..\nfoo\\..\\\nfoo/../bar\n.\n./foo\n..foo\n.. /x\nx/.. \n..  \na  \n";

const URLS: &str = "./bar/baz/foo.git\nhttps://example.com/foo.git\n\
http://example.com:80/deeper/foo.git\n-a./foo\n../../..//test/foo.git\n\
../../../../../:localhost:8080/foo.git\n..\\../.\\../:example.com/foo.git\n\
./%0ahost=example.com/foo.git\nhttps://one.example.com/evil?%0ahost=two.example.com\n\
https:///example.com/foo.git\nhttp://example.com:test/foo.git\nhttps::example.com/foo.git\n\
http:::example.com/foo.git\nhttp::://one.example.com/foo.git\ngit://example.com:1234/repo%0a.git\n\
data://acjbkd%0akajfdickajkd\nhttp://h:0/x\nhttp://h:65535/x\nhttp://h/%zz\nhttp://x y/z\n\
http://x/a b\nhttps::https://h/x\n./a%0d\nfoo:bar\n";

#[test]
fn check_name_and_url() {
    let Some(mut fx) = common::fixture() else {
        return;
    };
    fx.compare(["submodule", "check-name"])
        .stdin(NAMES)
        .output_only()
        .run();
    fx.compare(["submodule", "check-url"])
        .stdin(URLS)
        .output_only()
        .run();
}

#[test]
fn resolve_relative_url() {
    let Some(mut fx) = common::fixture() else {
        return;
    };
    for args in [
        ["../", "../foo/bar", "../submodule"],
        ["(null)", "//somewhere else/repo", "../../../subrepo"],
        [
            "(null)",
            "helper:://hostname/repo",
            "../../../../../../subrepo",
        ],
        ["(null)", "user@host:repo", "../../subrepo"],
        ["(null)", "../foo", "../../x"],
        ["x/", "foo", "./y"],
    ] {
        let mut argv = vec!["submodule", "resolve-relative-url"];
        argv.extend(args);
        common::same(&mut fx, &argv);
    }
}

#[test]
fn usage() {
    let Some(mut fx) = common::fixture() else {
        return;
    };
    common::same(&mut fx, &["submodule"]);
    common::same(&mut fx, &["submodule", "bogus"]);
    common::same(&mut fx, &["submodule", "check-name", "extra"]);
    common::same(&mut fx, &["submodule", "resolve-relative-url", "a"]);
}
