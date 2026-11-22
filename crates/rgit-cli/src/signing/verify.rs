// SPDX-License-Identifier: MIT
//! Verifying signatures (#51), observed with the `git` binary and stub
//! programs:
//!
//! - OpenPGP: `gpg --keyid-format=long --status-fd=1 --verify <sigfile> -`
//!   with the payload on standard input; X.509 the same without
//!   `--keyid-format`. The status lines (standard output) give the result,
//!   standard error is the text shown to the user. The verification
//!   succeeds when the program exits with 0 and reported `GOODSIG` (or
//!   `EXPKEYSIG`).
//! - SSH: `ssh-keygen -Y find-principals -f <allowedSignersFile> -s
//!   <sigfile>` names the principals of the key; each is tried with `-Y
//!   verify -n git -f <file> -I <principal> -s <sigfile>` (plus `-r
//!   <revocationFile>`) until one succeeds. Without a principal, `-Y
//!   check-novalidate -n git -s <sigfile>` checks the signature alone (an
//!   untrusted result that never verifies). Every call gets
//!   `-Overify-time=<local YYYYMMDDHHMMSS>` of the payload's committer or
//!   tagger date.
//!
//! The results are the `%G?` letters: `G` good, `U` good with unknown
//! validity, `B` bad, `X` expired signature, `Y` expired key, `R` revoked
//! key, `E` cannot be checked, `N` no signature.

use rgit_hash::HashKind;

use super::{Format, Settings, Trust, lossy, run, temp_file};
use crate::messages::signing as msg;

/// The outcome of a signature check.
#[derive(Debug, Clone)]
pub(crate) struct Check {
    /// The `%G?` letter.
    pub(crate) result: u8,
    pub(crate) trust: Trust,
    pub(crate) key: Option<String>,
    pub(crate) signer: Option<String>,
    pub(crate) fingerprint: Option<String>,
    pub(crate) primary_key_fingerprint: Option<String>,
    /// The human-readable output of the program.
    pub(crate) output: Vec<u8>,
    /// The raw status output (`--raw`); the output for SSH.
    pub(crate) status: Vec<u8>,
    /// The program itself accepted the signature.
    verified: bool,
}

impl Default for Check {
    fn default() -> Self {
        Check {
            result: b'N',
            trust: Trust::Undefined,
            key: None,
            signer: None,
            fingerprint: None,
            primary_key_fingerprint: None,
            output: Vec::new(),
            status: Vec::new(),
            verified: false,
        }
    }
}

impl Check {
    /// The verdict of `verify-commit`, `verify-tag` and `--show-signature`
    /// (the latter for the colour): verified, a good result and at least
    /// trust `min`.
    pub(crate) fn good(&self, min: Trust) -> bool {
        self.verified && matches!(self.result, b'G' | b'U' | b'Y') && self.trust >= min
    }
}

/// The signed payload and the signature of the raw commit `data`, for
/// the signature header of the repository's hash (`gpgsig` /
/// `gpgsig-sha256`). The payload is the commit without both signature
/// headers; the message is kept byte for byte (even with NULs).
pub(crate) fn commit_signature(hash: HashKind, data: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let wanted: &[u8] = match hash {
        HashKind::Sha256 => b"gpgsig-sha256",
        _ => b"gpgsig",
    };
    let mut payload = Vec::with_capacity(data.len());
    let mut sig = Vec::new();
    let mut found = false;
    let mut in_header = true;
    // Inside a signature header: whether it is the wanted one.
    let mut current: Option<bool> = None;
    for line in data.split_inclusive(|&b| b == b'\n') {
        if !in_header {
            payload.extend_from_slice(line);
            continue;
        }
        if line == b"\n" {
            in_header = false;
            payload.extend_from_slice(line);
            continue;
        }
        if let (Some(rest), Some(w)) = (line.strip_prefix(b" "), current) {
            if w {
                sig.extend_from_slice(rest);
            }
            continue;
        }
        let name = line.split(|&b| b == b' ').next().unwrap_or(line);
        if name == b"gpgsig" || name == b"gpgsig-sha256" {
            let w = name == wanted;
            current = Some(w);
            if w && !found {
                found = true;
                sig.extend_from_slice(&line[name.len() + 1..]);
            }
            continue;
        }
        current = None;
        payload.extend_from_slice(line);
    }
    if !found {
        return None;
    }
    if !sig.ends_with(b"\n") {
        sig.push(b'\n');
    }
    Some((payload, sig))
}

/// The signed payload and the signature of the raw tag object `data`.
pub(crate) fn tag_signature(data: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    // The signature starts at the last line that begins with a marker;
    // header lines never do, so the whole object can be searched.
    let (payload, sig) = rgit_object::split_signature(data);
    let sig = sig?;
    // Signatures over the other hash's form (`gpgsig-sha256` headers) are
    // not part of the payload.
    let mut out = Vec::with_capacity(payload.len());
    let mut in_header = true;
    let mut skipping = false;
    for line in payload.split_inclusive(|&b| b == b'\n') {
        if in_header {
            if line == b"\n" {
                in_header = false;
            } else if skipping && line.starts_with(b" ") {
                continue;
            } else {
                skipping = line.starts_with(b"gpgsig ") || line.starts_with(b"gpgsig-sha256 ");
                if skipping {
                    continue;
                }
            }
        }
        out.extend_from_slice(line);
    }
    Some((out, sig.to_vec()))
}

/// The committer (or tagger) date of a payload, for SSH's verify time.
fn payload_time(payload: &[u8]) -> Option<i64> {
    for line in payload.split(|&b| b == b'\n') {
        if line.is_empty() {
            break;
        }
        let rest = line
            .strip_prefix(b"committer ")
            .or_else(|| line.strip_prefix(b"tagger "));
        if let Some(rest) = rest {
            let close = rest.iter().rposition(|&b| b == b'>')?;
            let mut fields = rest[close + 1..]
                .split(|&b| b == b' ')
                .filter(|f| !f.is_empty());
            return std::str::from_utf8(fields.next()?).ok()?.parse().ok();
        }
    }
    None
}

/// `YYYYMMDDHHMMSS` in local time.
fn local_stamp(t: i64) -> String {
    let ctx = rgit_date::DateContext::from_env();
    stamp(t + rgit_date::tz_to_minutes(ctx.local_tz(t)) * 60)
}

/// `YYYYMMDDHHMMSS` of `t` (seconds, already shifted into the zone).
fn stamp(t: i64) -> String {
    let (days, rem) = (t.div_euclid(86400), t.rem_euclid(86400));
    // Days since the epoch to the civil date (proleptic Gregorian).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}{:02}{:02}{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Checks `signature` over `payload`.
pub(crate) fn check(settings: &Settings, payload: &[u8], signature: &[u8]) -> Check {
    let format = Format::of_signature(signature).unwrap_or(Format::OpenPgp);
    let program = settings.program(format);
    if format == Format::Ssh {
        return check_ssh(settings, &program, payload, signature);
    }
    let Ok(sig_file) = temp_file(".git_vtag_tmp", signature) else {
        eprintln!("error: {}", msg::CANNOT_CREATE_TEMP_SIG);
        return Check::default();
    };
    let path = sig_file.path().display().to_string();
    let mut args = Vec::new();
    if format == Format::OpenPgp {
        args.push("--keyid-format=long");
    }
    args.extend(["--status-fd=1", "--verify", path.as_str(), "-"]);
    let (ok, status, output) = run(&program, &args, Some(payload));
    drop(sig_file);
    let mut c = parse_gpg_status(&status);
    c.verified = ok
        && (contains(&status, b"\n[GNUPG:] GOODSIG ")
            || contains(&status, b"\n[GNUPG:] EXPKEYSIG "));
    c.output = output;
    c.status = status;
    c
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// Interprets gpg's / gpgsm's status lines.
fn parse_gpg_status(status: &[u8]) -> Check {
    let mut c = Check::default();
    let mut seen_exclusive = false;
    for line in status.split(|&b| b == b'\n') {
        let Some(rest) = line.strip_prefix(b"[GNUPG:] ") else {
            continue;
        };
        let rest = lossy(rest);
        let (keyword, args) = rest.split_once(' ').unwrap_or((rest.as_str(), ""));
        let letter = match keyword {
            "GOODSIG" => Some(b'G'),
            "BADSIG" => Some(b'B'),
            "EXPSIG" => Some(b'X'),
            "EXPKEYSIG" => Some(b'Y'),
            "REVKEYSIG" => Some(b'R'),
            "ERRSIG" => Some(b'E'),
            _ => None,
        };
        if let Some(letter) = letter {
            if seen_exclusive {
                // More than one signature: cannot be checked.
                return Check {
                    result: b'E',
                    ..Check::default()
                };
            }
            seen_exclusive = true;
            c.result = letter;
            let (key, who) = args.split_once(' ').unwrap_or((args, ""));
            c.key = Some(key.to_owned());
            if letter != b'E' {
                c.signer = Some(who.to_owned());
            }
            continue;
        }
        match keyword {
            "VALIDSIG" => {
                let fields: Vec<&str> = args.split(' ').collect();
                c.fingerprint = fields.first().map(|s| (*s).to_owned());
                if let Some(p) = fields.get(9) {
                    c.primary_key_fingerprint = Some((*p).to_owned());
                }
            }
            "TRUST_UNDEFINED" => c.trust = Trust::Undefined,
            "TRUST_NEVER" => c.trust = Trust::Never,
            "TRUST_MARGINAL" => c.trust = Trust::Marginal,
            "TRUST_FULLY" => c.trust = Trust::Fully,
            "TRUST_ULTIMATE" => c.trust = Trust::Ultimate,
            _ => {}
        }
    }
    if c.result == b'G' && c.trust < Trust::Marginal {
        c.result = b'U';
    }
    c
}

fn check_ssh(settings: &Settings, program: &str, payload: &[u8], signature: &[u8]) -> Check {
    let allowed = settings.allowed_signers.as_ref().filter(|p| p.exists());
    let Some(allowed) = allowed else {
        eprintln!("error: {}", msg::ALLOWED_SIGNERS_NEEDED);
        return Check::default();
    };
    let allowed = allowed.display().to_string();
    // A missing revocation file is reported for every verification.
    let revocation = || {
        settings.revocation_file.as_ref().and_then(|p| {
            if p.exists() {
                Some(p.display().to_string())
            } else {
                eprintln!(
                    "warning: {}",
                    msg::revocation_file_missing(&p.display().to_string())
                );
                None
            }
        })
    };
    let Ok(sig_file) = temp_file(".git_vtag_tmp", signature) else {
        eprintln!("error: {}", msg::CANNOT_CREATE_TEMP_SIG);
        return Check::default();
    };
    let sig_path = sig_file.path().display().to_string();
    let time = payload_time(payload).map(|t| format!("-Overify-time={}", local_stamp(t)));
    let mut args = vec!["-Y", "find-principals", "-f", &allowed, "-s", &sig_path];
    args.extend(time.as_deref());
    let (found, principals, principals_err) = run(program, &args, None);
    let principals = lossy(&principals);
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut verified_as = None;
    let lines: Vec<&str> = principals.lines().filter(|l| !l.is_empty()).collect();
    if found && !lines.is_empty() {
        for principal in lines {
            let mut args = vec![
                "-Y", "verify", "-n", "git", "-f", &allowed, "-I", principal, "-s", &sig_path,
            ];
            args.extend(time.as_deref());
            let revocation = revocation();
            if let Some(r) = &revocation {
                args.extend(["-r", r.as_str()]);
            }
            let (ok, o, e) = run(program, &args, Some(payload));
            out = o;
            err = e;
            if ok {
                verified_as = Some(principal.to_owned());
                break;
            }
        }
    } else {
        let mut args = vec!["-Y", "check-novalidate", "-n", "git", "-s", &sig_path];
        args.extend(time.as_deref());
        let (_, o, e) = run(program, &args, Some(payload));
        out = o;
        err = e;
    }
    drop(sig_file);
    let mut output = super::strip_cr(&out);
    output.extend_from_slice(&super::strip_cr(&principals_err));
    output.extend_from_slice(&super::strip_cr(&err));
    let mut c = Check {
        result: b'B',
        trust: Trust::Never,
        output: output.clone(),
        status: output,
        ..Check::default()
    };
    let text = lossy(&out);
    let first = text.lines().next().unwrap_or("");
    let fingerprint = first
        .rfind(" key ")
        .map(|i| first[i + 5..].trim().to_owned());
    if let Some(principal) = verified_as {
        if first.starts_with(msg::SSH_GOOD_PREFIX) {
            c.result = b'G';
            c.trust = Trust::Fully;
            c.signer = Some(principal);
            c.key.clone_from(&fingerprint);
            c.fingerprint = fingerprint;
            c.verified = true;
        }
    } else if first.starts_with(msg::SSH_GOOD_UNTRUSTED_PREFIX) {
        c.result = b'U';
        c.trust = Trust::Undefined;
        c.key.clone_from(&fingerprint);
        c.fingerprint = fingerprint;
    }
    c
}

/// `merge --verify-signatures` for one merged commit (raw object `raw`,
/// named `abbrev`): fatal unless it has a good signature with at least
/// `marginal` trust (`gpg.minTrustLevel`); the good signature is reported
/// unless `quiet`.
pub(crate) fn verify_merge_signature(
    settings: &Settings,
    hash: HashKind,
    raw: &[u8],
    abbrev: &str,
    quiet: bool,
) -> Result<(), crate::Die> {
    let c = match commit_signature(hash, raw) {
        Some((payload, sig)) => check(settings, &payload, &sig),
        None => Check::default(),
    };
    let signer = c.signer.clone().unwrap_or_default();
    let die = |m: String| Err(crate::Die::Fatal(m));
    match c.result {
        b'N' => return die(msg::no_gpg_signature(abbrev)),
        b'G' | b'U' => {}
        _ => return die(msg::bad_gpg_signature(abbrev, &signer)),
    }
    if c.trust < settings.min_trust_or(Trust::Marginal) {
        return die(msg::untrusted_gpg_signature(abbrev, &signer));
    }
    if !quiet {
        println!("{}", msg::good_gpg_signature(abbrev, &signer));
    }
    Ok(())
}

/// The lines of `output` for `--show-signature`, each in `color` (when
/// colouring) and ending with a newline.
pub(crate) fn show_lines(output: &[u8], color: Option<&str>, out: &mut Vec<u8>) {
    for line in output.split_inclusive(|&b| b == b'\n') {
        let text = line.strip_suffix(b"\n").unwrap_or(line);
        if let Some(c) = color {
            out.extend_from_slice(c.as_bytes());
            out.extend_from_slice(text);
            out.extend_from_slice(crate::color::RESET.as_bytes());
        } else {
            out.extend_from_slice(text);
        }
        out.push(b'\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_results() {
        let s = b"[GNUPG:] NEWSIG\n[GNUPG:] GOODSIG K1 Name <e>\n\
                  [GNUPG:] VALIDSIG FPR 2026 1 0 4 0 17 2 00 PRIMARY\n\
                  [GNUPG:] TRUST_UNDEFINED 0 pgp\n";
        let c = parse_gpg_status(s);
        assert_eq!(c.result, b'U');
        assert_eq!(c.key.as_deref(), Some("K1"));
        assert_eq!(c.signer.as_deref(), Some("Name <e>"));
        assert_eq!(c.fingerprint.as_deref(), Some("FPR"));
        assert_eq!(c.primary_key_fingerprint.as_deref(), Some("PRIMARY"));
        let c = parse_gpg_status(b"[GNUPG:] GOODSIG K1 N\n[GNUPG:] TRUST_FULLY 0\n");
        assert_eq!((c.result, c.trust), (b'G', Trust::Fully));
        let c = parse_gpg_status(b"[GNUPG:] GOODSIG K1 N\n[GNUPG:] GOODSIG K2 M\n");
        assert_eq!((c.result, c.key), (b'E', None));
        let c = parse_gpg_status(b"[GNUPG:] ERRSIG K1 17 2 00 1 9 F\n");
        assert_eq!(
            (c.result, c.key.as_deref(), c.signer),
            (b'E', Some("K1"), None)
        );
        assert_eq!(parse_gpg_status(b"").result, b'N');
    }

    #[test]
    fn times() {
        let p = b"tree x\ncommitter C <c@e> 1112912653 -0700\n\nmsg\n";
        assert_eq!(payload_time(p), Some(1112912653));
        assert_eq!(payload_time(b"object x\n\ncommitter a <b> 5 +0000\n"), None);
        assert_eq!(stamp(1112912653), "20050407222413");
        assert_eq!(stamp(951782400), "20000229000000");
        assert_eq!(stamp(0), "19700101000000");
    }
}
