// SPDX-License-Identifier: MIT
//! Messages of `git upload-pack` printed by the command itself (the
//! service's messages are in `rgit_server::upload_pack::messages`).
//! Word for word as git 2.56 prints them.

pub const USAGE: &[&str] = &[
    "git-upload-pack [--[no-]strict] [--timeout=<n>] [--stateless-rpc]\n                       [--advertise-refs] <directory>",
];

pub const H_STATELESS_RPC: &str = "quit after a single request/response exchange";
pub const H_HTTP_BACKEND_INFO_REFS: &str = "serve up the info/refs for git-http-backend";
pub const H_STRICT: &str = "do not try <directory>/.git/ if <directory> is no Git directory";
pub const H_TIMEOUT: &str = "interrupt transfer after <n> seconds of inactivity";

/// `<directory>` is no repository.
pub fn not_a_repository(dir: &str) -> String {
    format!("'{dir}' does not appear to be a git repository")
}
