// SPDX-License-Identifier: MIT
//! `git version` (Documentation/git-version.adoc).

use crate::context::version_number;
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

    println!("git version {}", version_number());
    if !ctx.invoked_as_git {
        println!("{NOT_OFFICIAL_GIT}");
    }
    if build_options.get() {
        println!("cpu: {}", std::env::consts::ARCH);
        match option_env!("RGIT_BUILD_COMMIT") {
            Some(c) if !c.is_empty() => println!("built from commit: {c}"),
            _ => println!("{}", crate::messages::NO_BUILD_COMMIT),
        }
        println!("sizeof-long: {}", size_of::<std::ffi::c_long>());
        println!("sizeof-size_t: {}", size_of::<usize>());
        println!("shell-path: /bin/sh");
        println!("rgit: {}", env!("CARGO_PKG_VERSION"));
        println!("default-ref-format: files");
        println!("default-hash: sha1");
    }
    Ok(0)
}
