// SPDX-License-Identifier: MIT
//! `git upload-archive` (Documentation/git-upload-archive.adoc): the
//! server side of `git archive --remote`.
//!
//! As observed with the reference `git`, the service answers `ACK` at
//! once and runs the archiver (`upload-archive--writer`) as a child that
//! reads the client's arguments itself; the archive and the archiver's
//! messages are relayed on sideband channels 1 and 2. When the archiver
//! fails, channel 3 says so and the service dies.

use crate::argv::CommandArgs as _;
use std::process::{Command, Stdio};

use rgit_archive::messages as amsg;
use rgit_archive::remote;
use rgit_protocol::pktline::Writer;

use crate::messages::archive as msg;
use crate::prelude::*;

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    if let Some(r) = super::archive::usage_line_help(argv, msg::UPLOAD_ARCHIVE_USAGE) {
        return r;
    }

    let exe = std::env::current_exe().map_err(|e| Die::Fatal(crate::global::strerror(&e)))?;
    let mut child = Command::new(exe)
        .raw_arg("upload-archive--writer")
        .raw_args(&argv[1..])
        .envs(ctx.child_env())
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Die::Fatal(crate::global::strerror(&e)))?;
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");

    let mut w = Writer::new(std::io::stdout(), "upload-archive");
    let fail = |e: rgit_protocol::Error| Die::Fatal(e.to_string());
    remote::write_ack(&mut w).map_err(fail)?;
    remote::multiplex(&mut w, stdout, stderr).map_err(fail)?;
    let ok = child.wait().map(|s| s.success()).unwrap_or(false);
    remote::finish(&mut w, ok).map_err(fail)?;
    if ok {
        Ok(0)
    } else {
        Err(Die::Fatal(amsg::sent_error(amsg::ARCHIVER_DIED)))
    }
}
