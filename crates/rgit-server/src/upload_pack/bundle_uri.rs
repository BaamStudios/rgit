// SPDX-License-Identifier: MIT
//! The `bundle-uri` command (`uploadpack.advertiseBundleURIs`): the
//! `bundle.*` keys of the served repository's configuration, one
//! `<key>=<value>` line each, in configuration order
//! (`Documentation/gitprotocol-v2.adoc`, `technical/bundle-uri.adoc`).

use std::io::{Read, Write};

use rgit_config::ConfigSet;

use super::serve::{END_WITHOUT_FLUSH, Session};
use super::{Error, Result, messages as msg};

pub(crate) fn run<R: Read, W: Write>(s: &mut Session<'_, R, W>, args: &[String]) -> Result<()> {
    // The command takes no arguments.
    if let Some(arg) = args.first() {
        if arg == END_WITHOUT_FLUSH {
            return Err(Error::fatal(format!(
                "bundle-uri: {}",
                msg::EXPECTED_FLUSH_AFTER_ARGUMENTS
            )));
        }
        return Err(Error::fatal(msg::bundle_uri_unexpected_argument(arg)));
    }
    for line in lines(s.repo.config) {
        s.out.data(&line)?;
    }
    s.out.flush()
}

/// The lines of the bundle list. A key without a value, or with an empty
/// one, is no part of a bundle list: it is left out with a warning.
fn lines(config: &ConfigSet) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for entry in config.entries() {
        if !entry.name.starts_with(b"bundle.") {
            continue;
        }
        match entry.value.as_ref().filter(|v| !v.is_empty()) {
            Some(value) => {
                let mut line = Vec::with_capacity(entry.name.len() + value.len() + 1);
                line.extend_from_slice(&entry.name);
                line.push(b'=');
                line.extend_from_slice(value);
                out.push(line);
            }
            None => eprintln!(
                "{}",
                msg::config_has_no_value(&String::from_utf8_lossy(&entry.name))
            ),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_keys_in_order_without_empty_values() {
        let text = b"[uploadpack]\n\tadvertiseBundleURIs = true\n\
            [bundle]\n\tversion = 1\n\tmode = all\n\tflag\n\
            [bundle \"One\"]\n\turi = file:///a.bdl\n\tcreationToken = 5\n\turi =\n\
            [other]\n\tx = 1\n";
        let config = rgit_config::parse_bytes(
            text,
            rgit_config::Origin::CommandLine,
            rgit_config::Scope::Local,
        )
        .unwrap();
        let got: Vec<String> = lines(&config)
            .into_iter()
            .map(|l| String::from_utf8(l).unwrap())
            .collect();
        assert_eq!(
            got,
            [
                "bundle.version=1",
                "bundle.mode=all",
                "bundle.One.uri=file:///a.bdl",
                "bundle.One.creationtoken=5",
            ]
        );
    }
}
