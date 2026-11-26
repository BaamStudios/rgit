// SPDX-License-Identifier: MIT
//! `test-tool bundle-uri parse-key-values <file>` and
//! `test-tool bundle-uri parse-config <file>`: parse a bundle list (lines
//! of `key=value` as a protocol v2 `bundle-uri` answer sends them, or a
//! file in git's config format) with `rgit_bundle::list` and print it in
//! config format. Relative URIs are resolved against `<uri>`.
//!
//! Observed from the reference `test-tool` (t5750-bundle-uri-parse.sh):
//! errors go to stderr and give exit code 1 after the list is printed; a
//! relative URI above `<uri>` is fatal (exit 128).

use std::io::Write;
use std::path::Path;

use rgit_bundle::BundleList;
use rgit_bundle::list::{self, ListError};

const USAGE: &str = "usage: test-tool bundle-uri parse-key-values <input>\n   or: test-tool bundle-uri parse-config <input>";

/// The base the tool resolves relative URIs against.
const BASE: &str = "<uri>";

fn print(list: &BundleList) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(list.to_config_text().as_bytes());
}

pub fn bundle_uri(args: &[String]) -> i32 {
    let (Some(action), Some(file)) = (args.first(), args.get(1)) else {
        eprintln!("{USAGE}");
        return 129;
    };
    let Ok(data) = std::fs::read(file) else {
        eprintln!("fatal: could not open '{file}'");
        return 128;
    };
    match action.as_str() {
        "parse-key-values" => {
            let mut l = BundleList::new(BASE);
            let mut failed = false;
            let text = String::from_utf8_lossy(&data);
            let mut lines: Vec<&str> = text.split('\n').collect();
            if text.ends_with('\n') {
                lines.pop();
            }
            for line in lines {
                if let Err(errs) = l.parse_line(line, &mut |m| eprintln!("warning: {m}")) {
                    for e in errs {
                        if e.starts_with("fatal: ") {
                            eprintln!("{e}");
                            return 128;
                        }
                        eprintln!("error: {e}");
                    }
                    failed = true;
                }
            }
            print(&l);
            i32::from(failed)
        }
        "parse-config" => {
            match list::parse_config(BASE, &data, Path::new(file), &mut |m| {
                eprintln!("warning: {m}")
            }) {
                Ok(l) => {
                    print(&l);
                    0
                }
                Err(ListError::Fatal) => 128,
                Err(ListError::Invalid) => {
                    print(&BundleList::new(BASE));
                    1
                }
            }
        }
        _ => {
            eprintln!("{USAGE}");
            129
        }
    }
}
