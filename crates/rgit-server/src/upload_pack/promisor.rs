// SPDX-License-Identifier: MIT
//! The `promisor-remote` capability (gitprotocol-v2.adoc,
//! config/promisor.adoc): with `promisor.advertise` the server names the
//! promisor remotes it uses, and a client that accepts some of them lets
//! the server leave out the objects it would have to fetch from them.
//!
//! The advertised remotes are those marked with `remote.<name>.promisor`
//! or `remote.<name>.partialCloneFilter`, in configuration order, then the
//! one named by `extensions.partialClone`; a remote without URL is left
//! out. Each comes as `name=<name>,url=<url>`, followed by the fields
//! `promisor.sendFields` names (comma separated, case-insensitive) that
//! are set to a non-empty value: `partialCloneFilter` before `token`.
//! Values are encoded: `%`, `,`, `;`, the space and control characters
//! as `%xx` (observed with git 2.56.1).

use rgit_config::ConfigSet;

/// The value of the `promisor-remote` capability, when one is advertised.
pub(crate) fn advertisement(config: &ConfigSet) -> Option<String> {
    if config.bool("promisor.advertise").ok().flatten() != Some(true) {
        return None;
    }
    let fields = send_fields(config);
    let mut out: Vec<String> = Vec::new();
    for name in promisor_remotes(config) {
        let Some(url) = value(config, &format!("remote.{name}.url")) else {
            continue;
        };
        let mut entry = format!("name={},url={}", encode(&name), encode(&url));
        for (field, key) in [
            ("partialCloneFilter", "partialclonefilter"),
            ("token", "token"),
        ] {
            if !fields.iter().any(|f| f == key) {
                continue;
            }
            if let Some(v) = value(config, &format!("remote.{name}.{key}")) {
                entry.push_str(&format!(",{field}={}", encode(&v)));
            }
        }
        out.push(entry);
    }
    (!out.is_empty()).then(|| out.join(";"))
}

/// The names of the promisor remotes in the order they are advertised.
pub(crate) fn promisor_remotes(config: &ConfigSet) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for e in config.entries() {
        let name = String::from_utf8_lossy(&e.name);
        let Some(rest) = name.strip_prefix("remote.") else {
            continue;
        };
        let Some((remote, key)) = rest.rsplit_once('.') else {
            continue;
        };
        let marked = match key {
            "promisor" => config.bool(e.name.as_slice()).ok().flatten() == Some(true),
            "partialclonefilter" => e.value.is_some(),
            _ => false,
        };
        if marked && !out.iter().any(|r| r == remote) {
            out.push(remote.to_owned());
        }
    }
    if let Some(ext) = value(config, "extensions.partialclone") {
        out.retain(|r| *r != ext);
        out.push(ext);
    }
    out
}

/// The last value of `key`, if set and not empty.
fn value(config: &ConfigSet, key: &str) -> Option<String> {
    config
        .get(key)
        .and_then(|e| e.value.as_ref())
        .map(|v| String::from_utf8_lossy(v).into_owned())
        .filter(|v| !v.is_empty())
}

/// `promisor.sendFields`, lowercased.
fn send_fields(config: &ConfigSet) -> Vec<String> {
    config
        .get_all("promisor.sendfields")
        .filter_map(|e| e.value.as_ref())
        .flat_map(|v| {
            String::from_utf8_lossy(v)
                .split(',')
                .map(|f| f.trim().to_ascii_lowercase())
                .filter(|f| !f.is_empty())
                .collect::<Vec<_>>()
        })
        .collect()
}

/// A name or field value as sent.
pub(crate) fn encode(s: &str) -> String {
    let mut out: Vec<u8> = Vec::with_capacity(s.len());
    for b in s.bytes() {
        if b <= b' ' || b == 0x7f || matches!(b, b'%' | b',' | b';') {
            out.extend_from_slice(format!("%{b:02x}").as_bytes());
        } else {
            out.push(b);
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(text: &str) -> ConfigSet {
        rgit_config::parse_bytes(
            text.as_bytes(),
            rgit_config::Origin::Stdin,
            rgit_config::Scope::Local,
        )
        .unwrap()
    }

    #[test]
    fn encodes_separators_and_blanks() {
        assert_eq!(encode("a b,c;d%e=f/g:h"), "a%20b%2cc%3bd%25e=f/g:h");
    }

    #[test]
    fn advertises_marked_remotes_with_url() {
        let c = config(
            "[promisor]\n\tadvertise = true\n\tsendFields = token, partialCloneFilter\n\
             [remote \"lop\"]\n\tpromisor = true\n\turl = file:///x\n\ttoken = t\n\
             [remote \"nourl\"]\n\tpromisor = true\n\
             [remote \"other\"]\n\tpartialCloneFilter = blob:none\n\turl = https://o\n",
        );
        assert_eq!(
            advertisement(&c).as_deref(),
            Some(
                "name=lop,url=file:///x,token=t;name=other,url=https://o,partialCloneFilter=blob:none"
            )
        );
    }

    #[test]
    fn nothing_without_advertise() {
        let c = config("[remote \"lop\"]\n\tpromisor = true\n\turl = file:///x\n");
        assert_eq!(advertisement(&c), None);
    }
}
