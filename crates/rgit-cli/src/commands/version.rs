// SPDX-License-Identifier: MIT
//! `git version` (Documentation/git-version.adoc).

use crate::messages::NOT_OFFICIAL_GIT;
use crate::prelude::*;

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let build_options = Cell::new(false);
    Parser::new(
        &["git version [--build-options]"],
        vec![Opt::flag(
            None,
            "build-options",
            &build_options,
            "also print build options",
        )],
    )
    .parse(argv)?;

    let mut out = format!("{}\n", rgit_version::version_line());
    if !ctx.invoked_as_git {
        out += &format!("{NOT_OFFICIAL_GIT}\n");
    }
    if build_options.get() {
        out += &build_options_text();
    }
    print!("{out}");
    Ok(0)
}

/// The lines `--build-options` adds after the version (also part of
/// `bugreport` and `diagnose` output).
pub fn build_options_text() -> String {
    let mut s = format!("cpu: {}\n", std::env::consts::ARCH);
    match option_env!("RGIT_BUILD_COMMIT") {
        Some(c) if !c.is_empty() => s += &format!("built from commit: {c}\n"),
        _ => s += &format!("{}\n", crate::messages::NO_BUILD_COMMIT),
    }
    s += &format!("sizeof-long: {}\n", size_of::<std::ffi::c_long>());
    s += &format!("sizeof-size_t: {}\n", size_of::<usize>());
    s += "shell-path: /bin/sh\n";
    if cfg!(target_os = "linux") {
        s += "feature: fsmonitor--daemon\n";
    }
    s += &format!("rgit: {}\n", rgit_version::RGIT_VERSION);
    s += "zlib: zlib-rs\n";
    s += "default-ref-format: files\n";
    s += "default-hash: sha1\n";
    s
}
