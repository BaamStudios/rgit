// SPDX-License-Identifier: MIT
use super::*;

fn read(input: &str) -> Credential {
    read_caller_credential(&mut input.as_bytes()).unwrap()
}

fn out(c: &Credential, a: Audience) -> String {
    let mut v = Vec::new();
    c.write_to(&mut v, a, true).unwrap();
    String::from_utf8(v).unwrap()
}

#[test]
fn helper_order() {
    let c = read(
        "capability[]=authtype\ncapability[]=state\nprotocol=https\nhost=example.com:8080\n\
         path=a/b.git\nusername=u\npassword=p\npassword_expiry_utc=99999999999\n\
         oauth_refresh_token=tok\nauthtype=Basic\ncredential=cc\nephemeral=1\nstate[]=s1\n\
         state[]=s2\nwwwauth[]=Basic realm=x\nwwwauth[]=Bearer\ncontinue=1\nfoo=bar\n",
    );
    assert_eq!(
        out(&c, Audience::Helper),
        "capability[]=authtype\ncapability[]=state\nauthtype=Basic\ncredential=cc\n\
         ephemeral=1\nprotocol=https\nhost=example.com:8080\npath=a/b.git\nusername=u\n\
         password=p\noauth_refresh_token=tok\npassword_expiry_utc=99999999999\n\
         wwwauth[]=Basic realm=x\nwwwauth[]=Bearer\ncontinue=1\nstate[]=s1\nstate[]=s2\n"
    );
    // The caller only sees capability-dependent fields a helper confirmed.
    assert_eq!(
        out(&c, Audience::Caller),
        "protocol=https\nhost=example.com:8080\npath=a/b.git\nusername=u\npassword=p\n\
         oauth_refresh_token=tok\npassword_expiry_utc=99999999999\n"
    );
}

#[test]
fn url_attribute() {
    let c = read("username=x\npath=zz\nurl=https://u:pw@example.com:443/x?y\n");
    assert_eq!(
        out(&c, Audience::Helper),
        "protocol=https\nhost=example.com:443\npath=x?y\nusername=u\npassword=pw\n"
    );
    let c = read("url=cert:///path/to/cert.pem\n");
    assert_eq!(
        out(&c, Audience::Helper),
        "protocol=cert\nhost=\npath=path/to/cert.pem\n"
    );
    let c = read("url=https://example.com%23%3f%2f/foo.git\n");
    assert_eq!(c.host.as_ref().unwrap(), "example.com#?/");
    let c = read("url=https://example.com#foo.git\n");
    assert_eq!(c.path.as_ref().unwrap(), "#foo.git");
    let c = read("url=https://example.com//a//\n");
    assert_eq!(c.path.as_ref().unwrap(), "a");
}

#[test]
fn bad_input() {
    let e = |s: &str| {
        read_caller_credential(&mut s.as_bytes())
            .unwrap_err()
            .to_string()
    };
    assert_eq!(e("noequals\n"), messages::UNABLE_TO_READ_CREDENTIAL);
    assert_eq!(
        e("url=https://ex%0aample.com/\n"),
        "credential url cannot be parsed: https://ex%0aample.com/"
    );
    assert_eq!(
        e("url=example.com\n"),
        "credential url cannot be parsed: example.com"
    );
}

#[test]
fn carriage_return_is_refused() {
    let c = read("protocol=https\nhost=example.com\nusername=a\rb\n");
    let mut v = Vec::new();
    let err = c.write_to(&mut v, Audience::Caller, true).unwrap_err();
    assert_eq!(
        err.to_string(),
        "credential value for username contains carriage return\n\
         If this is intended, set `credential.protectProtocol=false`"
    );
    assert_eq!(v, b"protocol=https\nhost=example.com\n");
    let mut v = Vec::new();
    c.write_to(&mut v, Audience::Caller, false).unwrap();
}

#[test]
fn describe_sanitizes() {
    let c = read("protocol=https\nhost=h #?@[]%\x01:9\npath=a/b c#?@%\nusername=#?/@:[]~_-.\x1b\n");
    assert_eq!(
        c.describe(true, true),
        "https://%23%3F%2F%40%3A%5B%5D~_-.%1B@h%20%23%3F%40[]%25%01:9/a/b%20c%23%3F%40%25"
    );
    assert_eq!(c.describe(false, false), "https://h #?@[]%\x01:9/a/b c#?@%");
    let c = read("protocol=file\nhost=\npath=tmp/x\nusername=\n");
    assert_eq!(c.describe(true, true), "file:///tmp/x");
}

#[test]
fn helper_commands() {
    assert_eq!(
        Credential::helper_command(b"verbatim foo bar", "get"),
        "git credential-verbatim foo bar get"
    );
    assert_eq!(
        Credential::helper_command(b"/usr/lib/helper --x", "store"),
        "/usr/lib/helper --x store"
    );
    assert_eq!(
        Credential::helper_command(b"!f() { :; }; f", "erase"),
        "f() { :; }; f erase"
    );
    assert_eq!(
        Credential::helper_command(b"~/dump", "get"),
        "git credential-~/dump get"
    );
}

#[test]
fn expiry_parsing() {
    assert_eq!(parse_expiry(b"9999999999"), 9_999_999_999);
    assert_eq!(parse_expiry(b" 5"), 5);
    assert_eq!(parse_expiry(b"5x"), 5);
    assert_eq!(parse_expiry(b"garbage"), 0);
    assert_eq!(parse_expiry(b""), 0);
}

fn config(pairs: &[(&str, &str)]) -> ConfigSet {
    let mut text = String::new();
    for (k, v) in pairs {
        let (section, rest) = k.split_once('.').unwrap();
        let (sub, key) = match rest.rsplit_once('.') {
            Some((s, k)) => (Some(s), k),
            None => (None, rest),
        };
        match sub {
            Some(s) => text.push_str(&format!("[{section} \"{s}\"]\n\t{key} = {v}\n")),
            None => text.push_str(&format!("[{section}]\n\t{key} = {v}\n")),
        }
    }
    rgit_config::parse_bytes(
        text.as_bytes(),
        Origin::File("test".into()),
        rgit_config::Scope::Local,
    )
    .unwrap()
}

#[test]
fn config_application() {
    let cfg = config(&[
        ("credential.helper", "a"),
        ("credential.https://example.com.helper", "b"),
        ("credential.https://other.com.helper", "x"),
        ("credential.https://example.com/path.git.username", "bob"),
        ("credential.https://example.com.username", "alice"),
    ]);
    let mut c = read("url=https://example.com/path.git\n");
    c.apply_config(&cfg).unwrap();
    let s = c.settings();
    assert_eq!(s.helpers, ["a", "b"]);
    assert_eq!(c.username.as_ref().unwrap(), "alice");
    assert_eq!(c.path, None);

    let cfg = config(&[
        ("credential.helper", "a"),
        ("credential.helper", ""),
        ("credential.helper", "c"),
        ("credential.https://*.org.useHttpPath", "true"),
    ]);
    let mut c = read("protocol=https\nhost=example.org\npath=foo.git\nusername=me\n");
    c.apply_config(&cfg).unwrap();
    assert_eq!(c.settings().helpers, ["c"]);
    assert_eq!(c.path.as_ref().unwrap(), "foo.git");
    assert_eq!(c.username.as_ref().unwrap(), "me");

    let cfg = config(&[("credential.useHttpPath", "maybe")]);
    let mut c = read("protocol=https\nhost=h\n");
    assert_eq!(
        c.apply_config(&cfg).unwrap_err().to_string(),
        "bad boolean config value 'maybe' for 'credential.usehttppath'"
    );
}

struct FakePrompter {
    asked: Vec<String>,
}

impl Prompter for FakePrompter {
    fn askpass(
        &mut self,
        _program: &[u8],
        prompt: &str,
        _base_env: &[(OsString, OsString)],
    ) -> Option<BString> {
        self.asked.push(prompt.to_string());
        Some(if prompt.starts_with("Username") {
            "askpass-username".into()
        } else {
            "askpass-password".into()
        })
    }

    fn terminal(&mut self, _prompt: &str, _echo: bool) -> io::Result<BString> {
        Err(io::Error::from_raw_os_error(6))
    }
}

#[test]
fn fill_prompts_via_askpass() {
    let env = rgit_config::MapEnv::new([("GIT_ASKPASS", "askpass")]);
    let cfg = ConfigSet::new();
    let mut p = FakePrompter { asked: Vec::new() };
    let mut ctx = CredentialContext {
        env: &env,
        config: &cfg,
        base_env: Vec::new(),
        prompter: &mut p,
    };
    let mut c = read("protocol=http\nhost=example.com\n");
    c.fill(&mut ctx).unwrap();
    assert_eq!(c.username.as_ref().unwrap(), "askpass-username");
    assert_eq!(
        p.asked,
        [
            "Username for 'http://example.com': ",
            "Password for 'http://askpass-username@example.com': "
        ]
    );
}

#[test]
fn fill_without_askpass() {
    let cfg = ConfigSet::new();
    let mut p = FakePrompter { asked: Vec::new() };
    let env = rgit_config::MapEnv::new([("GIT_TERMINAL_PROMPT", "0")]);
    let mut ctx = CredentialContext {
        env: &env,
        config: &cfg,
        base_env: Vec::new(),
        prompter: &mut p,
    };
    let mut c = read("protocol=https\nhost=example.com\n");
    assert_eq!(
        c.fill(&mut ctx).unwrap_err().to_string(),
        "could not read Username for 'https://example.com': terminal prompts disabled"
    );
    let env = rgit_config::MapEnv::new([("GIT_TERMINAL_PROMPT", "garbage")]);
    ctx.env = &env;
    let mut c = read("protocol=https\nhost=example.com\n");
    assert_eq!(
        c.fill(&mut ctx).unwrap_err().to_string(),
        "bad boolean environment value 'garbage' for 'GIT_TERMINAL_PROMPT'"
    );
    let env = rgit_config::MapEnv::new([("GIT_ASKPASS", "")]);
    ctx.env = &env;
    let mut c = read("protocol=https\nhost=example.com\n");
    assert_eq!(
        c.fill(&mut ctx).unwrap_err().to_string(),
        "could not read Username for 'https://example.com': No such device or address"
    );
    assert!(p.asked.is_empty());
}

#[test]
fn missing_fields() {
    let cfg = ConfigSet::new();
    let env = rgit_config::MapEnv::default();
    let mut p = FakePrompter { asked: Vec::new() };
    let mut ctx = CredentialContext {
        env: &env,
        config: &cfg,
        base_env: Vec::new(),
        prompter: &mut p,
    };
    let mut c = read("");
    assert_eq!(
        c.fill(&mut ctx).unwrap_err().to_string(),
        messages::MISSING_HOST
    );
    let mut c = read("host=h\n");
    assert_eq!(
        c.reject(&mut ctx).unwrap_err().to_string(),
        messages::MISSING_PROTOCOL
    );
    // No password: approve does nothing, even without host.
    let mut c = read("username=u\n");
    c.approve(&mut ctx).unwrap();
}
