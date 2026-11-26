// SPDX-License-Identifier: MIT
//! Messages and usage of `git url-parse` (CLEANROOM.md: user-visible
//! strings are reproduced verbatim and collected here).

pub const USAGE: &[&str] = &["git url-parse [-c <component>] [--] <url>..."];
pub const HELP_COMPONENT: &str = "which URL component to extract";

pub fn invalid_component(c: &str) -> String {
    format!("invalid git URL component '{c}'")
}

pub fn invalid_url(url: &str, why: &str) -> String {
    format!("invalid git URL '{url}': {why}")
}

pub fn not_a_url_absolute(url: &str) -> String {
    format!("'{url}' is not a URL; if you meant a local repository, use 'file://{url}'")
}

pub fn not_a_url(url: &str) -> String {
    format!(
        "'{url}' is not a URL; if you meant a local repository, use a 'file://' URL with an absolute path"
    )
}

pub const BAD_SCHEME: &str = "invalid URL scheme name or missing '://' suffix";
pub const BAD_ESCAPE: &str = "invalid %XX escape sequence";
pub const BAD_HOST: &str = "invalid characters in host name";
pub const BAD_PORT: &str = "invalid port number";
pub const NO_HOST: &str = "missing host and scheme is not 'file:'";
pub const BAD_DOTDOT: &str = "invalid '..' path segment";
