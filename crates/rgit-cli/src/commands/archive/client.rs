// SPDX-License-Identifier: MIT
//! `git archive --remote`: ask `git-upload-archive` for the archive.

use std::io::{Read, Write};

use bstr::ByteSlice;
use rgit_archive::messages as amsg;
use rgit_archive::remote::{self, Ack};
use rgit_config::SystemEnv;
use rgit_protocol::pktline::{Reader, Writer};
use rgit_protocol::sideband::{Progress, SidebandReader};
use rgit_remote::Remotes;
use rgit_transport::{ConnectOptions, IpVersion, Service, Settings};

use super::engine::{Dest, write_error};
use super::{format_from_filename, formats};
use crate::prelude::*;

fn fatal(e: &dyn std::fmt::Display) -> Die {
    Die::Fatal(e.to_string())
}

pub fn run(
    ctx: &mut Context,
    remote: &str,
    exec: Option<String>,
    output: Option<String>,
    rest: Vec<String>,
) -> CmdResult {
    let env = SystemEnv::default();
    let setup = crate::repo::setup_gently(ctx)?;
    let config = match &setup {
        Some(setup) => crate::repo::config(ctx, setup),
        None => crate::repo::config_outside_repository(ctx),
    };

    // The server cannot see the output file: name the format it implies.
    let mut args = Vec::new();
    let has_format = rest
        .iter()
        .take_while(|a| *a != "--")
        .any(|a| a == "--format" || a.starts_with("--format="));
    let all = formats(&config);
    if !has_format {
        if let Some(f) = output.as_deref().and_then(|o| format_from_filename(&all, o)) {
            args.push(format!("--format={}", f.name));
        }
    }
    args.extend(rest);

    let remotes = Remotes::from_config(&config)
        .map_err(crate::commands::ls_remote::remote_config_die)?;
    let git_dir = setup.as_ref().map(|s| s.absolute_git_dir());
    let found = remotes
        .get(remote.as_bytes(), git_dir.as_deref())
        .map_err(|e| fatal(&e))?;
    let url = found
        .fetch_url()
        .map(|u| u.to_str_lossy().into_owned())
        .unwrap_or_default();
    rgit_remote::check_allowed(rgit_fetch::protocol_name(&url).as_bytes(), &config, &env)
        .map_err(|e| fatal(&e))?;
    let settings = Settings {
        env: &env,
        config: &config,
        ip: IpVersion::Any,
        base_env: ctx.child_env(),
    };
    let Some(mut transport) = rgit_http::transport_for(&url, &settings).map_err(|e| fatal(&e))?
    else {
        return Err(Die::Fatal(format!(
            "unable to find remote helper for '{}'",
            url.split("://").next().unwrap_or(&url)
        )));
    };
    let mut options = ConnectOptions::new(Service::UploadArchive);
    options.program = exec;
    options.trace_name = Some("archive".into());
    let mut conn = transport.connect(&options).map_err(|e| fatal(&e))?;
    {
        let (_, w) = conn.split();
        let mut pw = Writer::new(w, "git");
        remote::write_request(&mut pw, &args).map_err(|e| fatal(&e))?;
    }
    conn.end_request().map_err(|e| fatal(&e))?;

    let mut reader = Reader::new(conn, "git");
    reader.chomp_newline(true);
    match remote::read_ack(&mut reader).map_err(|e| fatal(&e))? {
        Ack::Ack => {}
        Ack::Nack(reason) => return Err(Die::Fatal(amsg::nack(&reason))),
        Ack::MissingFlush => return Err(Die::Fatal(amsg::EXPECTED_FLUSH.into())),
        Ack::Other(_) => return Err(Die::Fatal(amsg::PROTOCOL_ERROR.into())),
    }

    let dest = Dest::open(output.as_deref())?;
    let mut out = dest.into_writer();
    let mut sideband = SidebandReader::new(reader, Progress::stderr());
    let mut buf = vec![0u8; 65536];
    loop {
        match sideband.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => out.write_all(&buf[..n]).map_err(write_error)?,
            Err(e) => {
                let _ = out.flush();
                return match sideband.take_error() {
                    Some(rgit_protocol::Error::SidebandFatal(_)) => Err(Die::Exit(1)),
                    Some(err) => Err(fatal(&err)),
                    None => Err(fatal(&e)),
                };
            }
        }
    }
    out.flush().map_err(write_error)?;
    let (reader, _) = sideband.into_parts();
    let _ = reader.into_inner().close();
    Ok(0)
}
