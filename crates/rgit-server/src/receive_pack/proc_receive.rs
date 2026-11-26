// SPDX-License-Identifier: MIT
//! The `proc-receive` hook (githooks(5), `receive.procReceiveRefs`):
//! commands on matching references are handed to the hook over a pkt-line
//! protocol instead of being executed here.
//!
//! ```text
//! S: version=1\0<features>      (features: atomic, push-options)
//! S: flush
//! H: version=1[\0<features>]
//! H: flush
//! S: <old> <new> <ref>          (one per command)
//! S: flush
//! S: <push option>...           (only if the hook named push-options)
//! S: flush
//! H: ok <ref> | ng <ref> [<reason>] | option <key> [<value>] ...
//! H: flush
//! ```

use std::io::{Read, Write};

use rgit_hash::{HashKind, ObjectId};
use rgit_protocol::pktline::{Packet, Reader, Writer};

use super::{Backend, Command, Push, RefReport, Sink, messages as m};

/// A running `proc-receive` hook.
pub trait ProcReceive {
    /// The hook's standard input.
    fn input(&mut self) -> &mut dyn Write;
    /// The hook's standard output.
    fn output(&mut self) -> &mut dyn Read;
    /// The hook's standard error, if it is to be relayed to the client.
    fn take_stderr(&mut self) -> Option<Box<dyn Read + Send>>;
    /// Closes the pipes and waits; the exit code.
    fn finish(&mut self) -> i32;
}

/// Why talking to the hook failed.
enum Failure {
    /// Before or during version negotiation.
    Negotiate,
    /// The version is not supported.
    Version(String),
    /// Later (writing commands, reading the report).
    Io,
}

/// Sends our version line, reads the hook's; returns whether the hook
/// wants push options.
fn negotiate(hook: &mut dyn ProcReceive, features: &str) -> Result<bool, Failure> {
    {
        let mut w = Writer::new(hook.input(), "receive-pack");
        let line = if features.is_empty() {
            "version=1\n".to_string()
        } else {
            format!("version=1\0{features}\n")
        };
        w.write_data(line.as_bytes())
            .and_then(|()| w.flush_pkt())
            .and_then(|()| w.flush_io())
            .map_err(|_| Failure::Negotiate)?;
    }
    let mut r = Reader::new(hook.output(), "receive-pack");
    let mut version = None;
    let mut push_options = false;
    loop {
        match r.read() {
            Ok(Packet::Flush) => break,
            Ok(p) => {
                let Some(d) = p.data() else { continue };
                let (head, feats) = match d.iter().position(|&b| b == 0) {
                    Some(i) => (&d[..i], &d[i + 1..]),
                    None => (d, &b""[..]),
                };
                if let Some(v) = head.strip_prefix(b"version=") {
                    version = Some(String::from_utf8_lossy(v).into_owned());
                }
                push_options |= feats.split(|&b| b == b' ').any(|f| f == b"push-options");
            }
            Err(_) => return Err(Failure::Negotiate),
        }
    }
    // No version line: version 0 (no features).
    let v = version.unwrap_or_else(|| "0".to_string());
    // The number is read like `atoi`: leading digits.
    let digits: String = v.chars().take_while(char::is_ascii_digit).collect();
    let n: u64 = digits.parse().unwrap_or(0);
    if n <= 1 {
        Ok(push_options)
    } else {
        Err(Failure::Version(n.to_string()))
    }
}

/// Runs the hook for the commands marked `run_proc_receive`.
pub(super) fn run<B: Backend, W: Write + Send>(push: &mut Push<'_, B, W>) {
    let env = push.hook_env();
    let atomic = push.req.atomic;
    let fail_all = |commands: &mut [Command], reason: &str| {
        for c in commands {
            if c.error.is_none() && (c.run_proc_receive || atomic) {
                c.error = Some(reason.to_string());
            }
        }
    };
    let Some(mut hook) = push.backend.start_proc_receive(&env, &push.out) else {
        push.out.error(m::PROC_RECEIVE_NOT_FOUND);
        fail_all(&mut push.commands, m::REASON_PROC_RECEIVE_MISSING);
        return;
    };
    let mut features = Vec::new();
    if push.req.atomic {
        features.push("atomic");
    }
    if push.req.push_options {
        features.push("push-options");
    }
    let features = features.join(" ");
    let hash = push.hash;
    let stderr = hook.take_stderr();
    let options: Vec<String> = push.push_options.clone().unwrap_or_default();
    let Push {
        ref out,
        ref mut commands,
        ..
    } = *push;
    let result = std::thread::scope(|scope| {
        let relay = stderr.map(|mut err| {
            scope.spawn(move || {
                let mut buf = [0u8; 4096];
                loop {
                    match err.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => out.write(&buf[..n]),
                    }
                }
            })
        });
        let mut errors = Vec::new();
        let outcome = talk(
            hook.as_mut(),
            &features,
            commands,
            &options,
            &mut errors,
            hash,
        );
        let code = hook.finish();
        if let Some(t) = relay {
            let _ = t.join();
        }
        (outcome, code, errors)
    });
    // Our complaints about the report come after everything the hook
    // printed (the hook echoes each line before it sends it), whatever
    // the scheduling of the relay thread.
    let (outcome, code, errors) = result;
    for e in &errors {
        out.error(e);
    }
    match (outcome, code) {
        (Err(Failure::Version(v)), _) => {
            out.error(&m::proc_receive_bad_version(&v));
            fail_all(commands, m::REASON_PROC_RECEIVE_MISSING);
        }
        (Err(Failure::Negotiate), _) => {
            out.error(m::PROC_RECEIVE_NO_VERSION);
            fail_all(commands, m::REASON_PROC_RECEIVE_MISSING);
        }
        (Err(Failure::Io), _) => fail_all(commands, m::REASON_PROC_RECEIVE_MISSING),
        (Ok(()), code) if code != 0 => fail_all(commands, m::REASON_PROC_RECEIVE_MISSING),
        (Ok(()), _) => {
            for c in commands.iter_mut() {
                if c.run_proc_receive && c.error.is_none() && c.reports.is_empty() {
                    c.error = Some(m::REASON_PROC_RECEIVE_NO_REPORT.to_string());
                }
            }
        }
    }
}

/// The whole exchange with the hook.
fn talk(
    hook: &mut dyn ProcReceive,
    features: &str,
    commands: &mut [Command],
    options: &[String],
    errors: &mut Vec<String>,
    hash: HashKind,
) -> Result<(), Failure> {
    let push_options = negotiate(hook, features)?;
    {
        let mut w = Writer::new(hook.input(), "receive-pack");
        for c in commands
            .iter()
            .filter(|c| c.run_proc_receive && c.error.is_none())
        {
            let line = format!("{} {} {}", c.old.to_hex(), c.new.to_hex(), c.name);
            w.write_data(line.as_bytes()).map_err(|_| Failure::Io)?;
        }
        w.flush_pkt().map_err(|_| Failure::Io)?;
        if push_options {
            for o in options {
                w.write_data(o.as_bytes()).map_err(|_| Failure::Io)?;
            }
            w.flush_pkt().map_err(|_| Failure::Io)?;
        }
        w.flush_io().map_err(|_| Failure::Io)?;
    }
    if read_report(errors, commands, hash, hook) {
        Ok(())
    } else {
        Err(Failure::Io)
    }
}

/// Reads the hook's status lines; `false` if the stream broke. Problems
/// with the report are collected in `errors`.
fn read_report(
    errors: &mut Vec<String>,
    commands: &mut [Command],
    hash: HashKind,
    hook: &mut dyn ProcReceive,
) -> bool {
    let mut r = Reader::new(hook.output(), "receive-pack");
    // The command the last `ok` line was about.
    let mut current: Option<usize> = None;
    // Options after a missing `ok` line are reported once.
    let mut orphan_reported = false;
    loop {
        let line = match r.read() {
            Ok(Packet::Flush) => return true,
            Ok(p) => match p.data() {
                Some(d) => String::from_utf8_lossy(d).into_owned(),
                None => continue,
            },
            Err(_) => return false,
        };
        if let Some(rest) = line.strip_prefix("option ") {
            let Some(i) = current else {
                if !orphan_reported {
                    errors.push(m::PROC_RECEIVE_OPTION_WITHOUT_OK.to_string());
                    orphan_reported = true;
                }
                continue;
            };
            let (key, value) = match rest.split_once(' ') {
                Some((k, v)) => (k, Some(v)),
                None => (rest, None),
            };
            let cmd = &mut commands[i];
            if key == "fall-through" {
                cmd.run_proc_receive = false;
                cmd.reports.clear();
                current = None;
                continue;
            }
            let Some(report) = cmd.reports.last_mut() else {
                continue;
            };
            match (key, value) {
                ("refname", Some(v)) => report.refname = Some(v.to_string()),
                ("old-oid", Some(v)) => report.old = ObjectId::from_hex(hash, v.as_bytes()).ok(),
                ("new-oid", Some(v)) => report.new = ObjectId::from_hex(hash, v.as_bytes()).ok(),
                ("forced-update", _) => report.forced_update = true,
                _ => {}
            }
            continue;
        }
        orphan_reported = false;
        let Some((status, rest)) = line.split_once(' ') else {
            errors.push(m::proc_receive_incomplete(&line));
            current = None;
            continue;
        };
        let (refname, msg) = match rest.split_once(' ') {
            Some((n, msg)) => (n, Some(msg)),
            None => (rest, None),
        };
        if status != "ok" && status != "ng" {
            errors.push(m::proc_receive_bad_status(status, refname));
            current = None;
            continue;
        }
        let Some(i) = commands
            .iter()
            .position(|c| c.name == refname && c.run_proc_receive)
        else {
            if commands.iter().any(|c| c.name == refname) {
                errors.push(m::proc_receive_unexpected_ref(refname));
            } else {
                errors.push(m::proc_receive_unknown_ref(refname));
            }
            current = None;
            continue;
        };
        if status == "ng" {
            commands[i].error = Some(msg.unwrap_or("failed").to_string());
            current = None;
        } else {
            commands[i].reports.push(RefReport::default());
            current = Some(i);
        }
    }
}
