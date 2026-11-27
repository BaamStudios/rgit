// SPDX-License-Identifier: MIT
//! `git send-email` (Documentation/git-send-email.adoc,
//! Documentation/config/sendemail.adoc): send patch files, directories of
//! them or a `format-patch` range as e-mails, through a sendmail-like
//! program or an SMTP server.
//!
//! The flow: read the options and the `sendemail.*` settings (with
//! identities), collect the files (running `format-patch` for revision
//! arguments), validate them (hook, line lengths, cover letter template,
//! undeclared 8bit), ask for what is missing, let the user compose an
//! introduction and annotate patches, then build and send one message per
//! file, threaded with `In-Reply-To:` / `References:`.
//!
//! Behaviour not written down in the documentation (log layout, the
//! prompts, which header fields of a patch are kept and in which order)
//! follows the `git` binary, checked with t9001.

#[path = "send_email/address.rs"]
mod address;
#[path = "send_email/aliases.rs"]
mod aliases;
#[path = "send_email/args.rs"]
mod args;
#[path = "send_email/deliver.rs"]
mod deliver;
#[path = "send_email/patch.rs"]
mod patch;

use std::collections::HashSet;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rgit_config::{ConfigSet, SystemEnv};

use crate::messages::send_email as msg;
use crate::prelude::*;

use aliases::Aliases;
use args::Args;
use deliver::{Delivery, Envelope};
use patch::Patch;

/// `sendemail.*` settings; with an identity, `sendemail.<identity>.*`
/// values take precedence.
struct Settings<'a> {
    cfg: &'a ConfigSet,
    identity: Option<String>,
}

impl Settings<'_> {
    fn keys(&self, key: &str) -> Vec<String> {
        let mut v = Vec::new();
        if let Some(id) = &self.identity {
            v.push(format!("sendemail.{id}.{key}"));
        }
        v.push(format!("sendemail.{key}"));
        v
    }

    fn entry(&self, key: &str) -> Option<&rgit_config::Entry> {
        self.keys(key).iter().find_map(|k| self.cfg.get(k))
    }

    fn string(&self, key: &str) -> Option<String> {
        self.entry(key)
            .map(|e| e.value.as_ref().map(|v| String::from_utf8_lossy(v).into_owned()).unwrap_or_default())
    }

    fn strings(&self, key: &str) -> Vec<String> {
        for k in self.keys(key) {
            let all: Vec<String> = self
                .cfg
                .get_all(&k)
                .filter_map(|e| e.value.as_ref())
                .map(|v| String::from_utf8_lossy(v).into_owned())
                .collect();
            if !all.is_empty() {
                return all;
            }
        }
        Vec::new()
    }

    fn bool(&self, key: &str) -> Option<bool> {
        self.entry(key).map(|e| e.bool().unwrap_or(false))
    }
}

/// Which addresses are not copied automatically (`--suppress-cc`).
#[derive(Debug, Default, Clone)]
struct Suppress {
    author: bool,
    self_: bool,
    cc: bool,
    bodycc: bool,
    sob: bool,
    misc_by: bool,
    cccmd: bool,
}

impl Suppress {
    fn add(&mut self, cat: &str) -> Result<(), Die> {
        match cat {
            "author" => self.author = true,
            "self" => self.self_ = true,
            "cc" => self.cc = true,
            "bodycc" => self.bodycc = true,
            "sob" => self.sob = true,
            "misc-by" => self.misc_by = true,
            "cccmd" => self.cccmd = true,
            "body" => {
                self.sob = true;
                self.bodycc = true;
                self.misc_by = true;
            }
            "all" => {
                *self = Suppress {
                    author: true,
                    self_: true,
                    cc: true,
                    bodycc: true,
                    sob: true,
                    misc_by: true,
                    cccmd: true,
                };
            }
            other => {
                eprintln!("{}", msg::bad_suppress(other));
                return Err(Die::Exit(1));
            }
        }
        Ok(())
    }
}

/// Everything decided before the first message goes out.
struct State<'a> {
    ctx: &'a Context,
    cwd: PathBuf,
    git_dir: Option<PathBuf>,
    aliases: Aliases,
    mailmap: Option<crate::history::mailmap::Mailmap>,
    sender: String,
    to: Vec<String>,
    cc: Vec<String>,
    bcc: Vec<String>,
    reply_to: Option<String>,
    initial_in_reply_to: Option<String>,
    subject: Option<String>,
    xmailer: bool,
    transfer_encoding: String,
    auto_8bit: Option<String>,
    compose_encoding: String,
    to_cmd: Option<String>,
    cc_cmd: Option<String>,
    header_cmd: Option<String>,
    suppress: Suppress,
    cc_cover: bool,
    to_cover: bool,
    thread: bool,
    chain_reply_to: bool,
    confirm: String,
    confirm_unconfigured: bool,
    quiet: bool,
    dry_run: bool,
    delivery: Delivery,
    envelope_sender: Option<String>,
    imap_folder: Option<String>,
    imap_only: bool,
}

fn rgit_command(ctx: &Context) -> Command {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("git"));
    let mut c = Command::new(exe);
    c.envs(ctx.child_env());
    c
}

/// A temporary directory removed when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new(prefix: &str) -> std::io::Result<TempDir> {
        let base = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        for i in 0..100u32 {
            let p = base.join(format!("{prefix}{}-{nanos:x}{i}", std::process::id()));
            if std::fs::create_dir(&p).is_ok() {
                return Ok(TempDir(p));
            }
        }
        Err(std::io::Error::other("cannot create a temporary directory"))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Read one line from standard input after showing `prompt` on standard
/// output; `None` at the end of input.
fn read_answer(prompt: &str) -> Option<String> {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(prompt.as_bytes());
    let _ = out.flush();
    drop(out);
    let mut line = String::new();
    match std::io::stdin().lock().read_line(&mut line) {
        Ok(0) | Err(_) => {
            println!();
            None
        }
        Ok(_) => Some(line.trim_end_matches(['\n', '\r']).to_string()),
    }
}

/// Ask until an answer matches `valid` (at most ten times); the end of
/// input gives `default`.
fn ask(prompt: &str, valid: impl Fn(&str) -> bool, default: Option<&str>) -> Option<String> {
    for _ in 0..10 {
        match read_answer(prompt) {
            None => return default.map(str::to_string),
            Some(a) if a.is_empty() && default.is_some() => return default.map(str::to_string),
            Some(a) if valid(&a) => return Some(a),
            Some(_) => {}
        }
    }
    None
}

pub fn run(ctx: &mut Context, argv: &[String]) -> CmdResult {
    let args = Args::parse(&argv[1..])?;
    if args.help {
        print!("{}", msg::USAGE);
        return Ok(0);
    }
    let cwd = std::env::current_dir().map_err(|e| Die::Fatal(e.to_string()))?;
    let setup = crate::repo::setup_gently(ctx)?;
    let _ = std::env::set_current_dir(&cwd);
    let cfg = match &setup {
        Some(s) => crate::repo::load_config(ctx, s)?,
        None => crate::repo::config_outside_repository(ctx),
    };
    let git_dir = setup.as_ref().map(|s| s.absolute_git_dir());
    let work_tree = setup
        .as_ref()
        .and_then(|s| s.work_tree.as_ref().map(|w| s.cwd.join(w)));

    let identity = if args.no_identity {
        None
    } else {
        args.identity.clone().or_else(|| {
            cfg.get("sendemail.identity")
                .and_then(|e| e.value.as_ref())
                .map(|v| String::from_utf8_lossy(v).into_owned())
        })
    };
    let settings = Settings {
        cfg: &cfg,
        identity,
    };

    if cfg.bool("sendemail.forbidSendmailVariables").ok().flatten() != Some(false)
        && cfg
            .entries()
            .iter()
            .any(|e| e.name.starts_with(b"sendmail."))
    {
        eprintln!("{}", msg::sendmail_variables());
        return Err(Die::Exit(255));
    }

    let alias_files = settings.strings("aliasesFile");
    let aliases = match Aliases::load(&alias_files, settings.string("aliasFileType").as_deref())
    {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return Err(Die::Exit(2));
        }
    };

    if args.dump_aliases {
        if args.own_options > 1 || !args.rest.is_empty() {
            eprintln!("{}", msg::DUMP_ALIASES_ALONE);
            return Err(Die::Exit(255));
        }
        let mut out = std::io::stdout().lock();
        for n in aliases.names() {
            let _ = writeln!(out, "{n}");
        }
        return Ok(0);
    }
    if args.translate_aliases {
        if args.own_options > 1 || !args.rest.is_empty() {
            eprintln!("{}", msg::TRANSLATE_ALIASES_ALONE);
            return Err(Die::Exit(255));
        }
        let mut out = std::io::stdout().lock();
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            for entry in aliases::split_input_line(&line) {
                for a in aliases.expand(&entry) {
                    let _ = writeln!(out, "{a}");
                }
            }
        }
        return Ok(0);
    }

    let mut state = resolve(ctx, &args, &settings, &cfg, cwd, git_dir, aliases)?;
    let validate = args
        .validate
        .or_else(|| settings.bool("validate"))
        .unwrap_or(true);

    // Collect the files.
    let mut tmp_patches: Option<TempDir> = None;
    let files = collect_files(ctx, &state, &args, setup.is_some(), &mut tmp_patches)?;
    if files.is_empty() && !args.compose {
        println!("\n{}\n", msg::NO_PATCH_FILES);
        print!("{}", msg::USAGE);
        return Err(Die::Exit(1));
    }

    // Validation, with the file names listed as they are checked.
    let total = files.len();
    for (i, f) in files.iter().enumerate() {
        if !state.quiet {
            println!("{}", f.display);
        }
        let data = std::fs::read(&f.path).map_err(|e| {
            Die::Fatal(format!("{}: {e}", f.display))
        })?;
        let p = Patch::parse(&data);
        if p.subject().is_none() {
            let _ = std::io::stdout().flush();
            eprintln!("{}", msg::no_subject(&f.display));
            return Err(Die::Exit(1));
        }
        if validate {
            if let Some(gd) = &state.git_dir {
                run_validate_hook(ctx, gd, work_tree.as_deref(), f, &p, i + 1, total)?;
            }
            let enc = state.transfer_encoding.as_str();
            if !matches!(enc, "auto" | "base64" | "quoted-printable")
                && let Some(n) = patch::first_long_line(&data, 998)
            {
                let _ = std::io::stdout().flush();
                eprintln!("{}", msg::too_long(&f.display, n));
                eprintln!("{}", msg::NO_PATCHES_SENT);
                return Err(Die::Exit(1));
            }
        }
    }
    let _ = std::io::stdout().flush();

    // The cover letter template must be edited first.
    if !args.force {
        for f in &files {
            let data = std::fs::read(&f.path).unwrap_or_default();
            let p = Patch::parse(&data);
            if p.subject().is_some_and(|s| s.contains(msg::COVER_TEMPLATE_SUBJECT)) {
                eprintln!("{}", msg::cover_template(&f.display));
                return Err(Die::Exit(1));
            }
        }
    }

    // Files with 8bit text that do not say how it is encoded.
    let mut broken: HashSet<usize> = HashSet::new();
    let mut need_charset = false;
    for (i, f) in files.iter().enumerate() {
        let data = std::fs::read(&f.path).unwrap_or_default();
        let p = Patch::parse(&data);
        if p.header("Content-Transfer-Encoding").is_none()
            && (patch::has_8bit(&p.body) || p.subject().is_some_and(|s| !s.is_ascii()))
        {
            broken.insert(i);
            if p.charset().is_none() {
                need_charset = true;
            }
        }
    }
    if !broken.is_empty() {
        println!("{}", msg::EIGHTBIT_FILES);
        for (i, f) in files.iter().enumerate() {
            if broken.contains(&i) {
                println!("    {}", f.display);
            }
        }
        if need_charset && state.auto_8bit.is_none() {
            state.auto_8bit = Some(ask_8bit_encoding());
        }
    }

    // Ask for what is missing.
    let mut prompting = false;
    if state.to.is_empty() && state.to_cmd.is_none() {
        let answer = read_answer(msg::ASK_TO).unwrap_or_default();
        state.to = expand_list(&state, &[answer]);
        prompting = true;
    }
    if state.thread && state.initial_in_reply_to.is_none() && prompting {
        let answer = read_answer(msg::ASK_IN_REPLY_TO).unwrap_or_default();
        state.initial_in_reply_to = normalize_message_id(&answer);
    }

    // Compose and annotate.
    let annotate = args
        .annotate
        .or_else(|| settings.bool("annotate"))
        .unwrap_or(false);
    let multi_edit = settings.bool("multiEdit").unwrap_or(true);
    let mut annotate_dir: Option<TempDir> = None;
    let mut files = files;
    let mut compose_msg: Option<Composed> = None;
    let compose_file = if args.compose {
        let dir = state
            .git_dir
            .clone()
            .unwrap_or_else(std::env::temp_dir);
        let path = dir.join(format!(
            ".gitsendemail.msg.{}{:x}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.subsec_nanos())
        ));
        write_compose_template(&state, &files, &path)?;
        Some(path)
    } else {
        None
    };
    let mut edit_list: Vec<PathBuf> = Vec::new();
    if let Some(p) = &compose_file {
        edit_list.push(p.clone());
    }
    if annotate {
        let dir = TempDir::new("rgit-send-email-annotate-").map_err(|e| Die::Fatal(e.to_string()))?;
        for (i, f) in files.iter_mut().enumerate() {
            let name = f.path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
            let copy = dir.0.join(format!("{:04}", i + 1)).join(name);
            let _ = std::fs::create_dir_all(copy.parent().unwrap_or(&dir.0));
            std::fs::copy(&f.path, &copy).map_err(|e| Die::Fatal(e.to_string()))?;
            f.path = copy.clone();
            edit_list.push(copy);
        }
        annotate_dir = Some(dir);
    }
    if !edit_list.is_empty() {
        let result = if multi_edit {
            run_editor(&cfg, &edit_list)
        } else {
            edit_list.iter().try_for_each(|p| run_editor(&cfg, std::slice::from_ref(p)))
        };
        if let Err(e) = result {
            if let Some(p) = &compose_file {
                let _ = std::fs::remove_file(p);
            }
            return Err(e);
        }
    }
    if let Some(p) = &compose_file {
        let text = std::fs::read(p).unwrap_or_default();
        let _ = std::fs::remove_file(p);
        compose_msg = apply_compose(&mut state, &text);
    }

    // Send.
    let result = send_all(&mut state, &files, &broken, compose_msg);
    drop(annotate_dir);
    drop(tmp_patches);
    result
}

/// A file to send: where it is and how it is shown.
#[derive(Debug, Clone)]
struct PatchFile {
    path: PathBuf,
    display: String,
}

fn resolve<'a>(
    ctx: &'a Context,
    args: &Args,
    s: &Settings<'_>,
    cfg: &ConfigSet,
    cwd: PathBuf,
    git_dir: Option<PathBuf>,
    aliases: Aliases,
) -> Result<State<'a>, Die> {
    let transfer_encoding = args
        .transfer_encoding
        .clone()
        .or_else(|| s.string("transferEncoding"))
        .unwrap_or_else(|| "auto".into())
        .to_ascii_lowercase();
    if !matches!(
        transfer_encoding.as_str(),
        "7bit" | "8bit" | "quoted-printable" | "base64" | "auto"
    ) {
        eprintln!("{}", msg::bad_transfer_encoding(&transfer_encoding));
        return Err(Die::Exit(1));
    }

    let mut suppress = Suppress::default();
    let mut suppress_given = false;
    let mut cats: Vec<String> = s.strings("suppressCc");
    cats.extend(args.suppress_cc.iter().cloned());
    for c in &cats {
        suppress.add(c)?;
        suppress_given = true;
    }
    let suppress_from = args.suppress_from.or_else(|| s.bool("suppressFrom"));
    if suppress_from == Some(true) {
        suppress.self_ = true;
    }
    let sob_cc = args
        .signed_off_by_cc
        .or_else(|| s.bool("signedOffByCc"))
        .or_else(|| s.bool("signedOffCc"));
    if sob_cc == Some(false) {
        suppress.sob = true;
        suppress.bodycc = true;
        suppress.misc_by = true;
    }
    if suppress_from.is_some() || sob_cc.is_some() {
        suppress_given = true;
    }

    let confirm_setting = args.confirm.clone().or_else(|| s.string("confirm"));
    let confirm_unconfigured = confirm_setting.is_none();
    let confirm = confirm_setting.unwrap_or_else(|| {
        if suppress_given {
            "compose".into()
        } else {
            "auto".into()
        }
    });
    if !matches!(confirm.as_str(), "always" | "never" | "cc" | "compose" | "auto") {
        eprintln!("{}", msg::bad_confirm(&confirm));
        return Err(Die::Exit(1));
    }

    let mailmap_on = args.mailmap.or_else(|| s.bool("mailmap")).unwrap_or(false);
    let mailmap = if mailmap_on {
        let mut m = crate::history::mailmap::Mailmap::load_for(ctx)?;
        let _ = std::env::set_current_dir(&cwd);
        if let Some(blob) = s.string("mailmap.blob") {
            if let crate::history::mailmap::BlobLookup::Blob(data) =
                crate::revisions::read_blob(ctx, &blob, "")
            {
                m.add(&data);
            }
            let _ = std::env::set_current_dir(&cwd);
        }
        if let Some(file) = s.string("mailmap.file")
            && let Ok(data) = std::fs::read(aliases::expand_home(&file))
        {
            m.add(&data);
        }
        Some(m)
    } else {
        None
    };

    let smtp_server = args.smtp_server.clone().or_else(|| s.string("smtpServer"));
    let sendmail_cmd = args.sendmail_cmd.clone().or_else(|| {
        if args.smtp_server.is_some() {
            None
        } else {
            s.string("sendmailCmd")
        }
    });
    let encryption = args
        .smtp_encryption
        .clone()
        .or_else(|| s.string("smtpEncryption"))
        .or_else(|| (s.bool("smtpSSL") == Some(true)).then(|| "ssl".to_string()))
        .unwrap_or_default();
    let batch_size: usize = args
        .batch_size
        .clone()
        .or_else(|| s.string("smtpBatchSize"))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    let relogin_delay: u64 = args
        .relogin_delay
        .clone()
        .or_else(|| s.string("smtpReloginDelay"))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    if args.relogin_delay.is_some() && batch_size == 0 {
        eprintln!("{}", msg::RELOGIN_WITHOUT_BATCH);
        return Err(Die::Exit(1));
    }
    let delivery = deliver::choose(deliver::Choice {
        sendmail_cmd,
        smtp_server,
        port: args
            .smtp_server_port
            .clone()
            .or_else(|| s.string("smtpServerPort")),
        server_options: if args.smtp_server_options.is_empty() {
            s.strings("smtpServerOption")
        } else {
            args.smtp_server_options.clone()
        },
        encryption,
        domain: args.smtp_domain.clone().or_else(|| s.string("smtpDomain")),
        user: args.smtp_user.clone().or_else(|| s.string("smtpUser")),
        pass: args.smtp_pass.clone().or_else(|| s.string("smtpPass")),
        auth: args.smtp_auth.clone().or_else(|| s.string("smtpAuth")),
        cert_path: args
            .smtp_ssl_cert_path
            .clone()
            .or_else(|| s.string("smtpSSLCertPath")),
        client_cert: args
            .smtp_ssl_client_cert
            .clone()
            .or_else(|| s.string("smtpSSLClientCert")),
        client_key: args
            .smtp_ssl_client_key
            .clone()
            .or_else(|| s.string("smtpSSLClientKey")),
        debug: args
            .smtp_debug
            .clone()
            .or_else(|| s.string("smtpDebug"))
            .is_some_and(|v| v.trim() == "1"),
        batch_size,
        relogin_delay,
        outlook_id_fix: args.outlook_id_fix.or_else(|| s.bool("outlookidfix")),
    })?;

    let imap_folder = args
        .imap_sent_folder
        .clone()
        .or_else(|| s.string("imapSentFolder"));
    let imap_only = args
        .use_imap_only
        .or_else(|| s.bool("useImapOnly"))
        .unwrap_or(false);
    if imap_only && imap_folder.is_none() {
        eprintln!("{}", msg::NEED_IMAP_FOLDER);
        return Err(Die::Exit(1));
    }

    let mut state = State {
        ctx,
        cwd,
        git_dir,
        aliases,
        mailmap,
        sender: String::new(),
        to: Vec::new(),
        cc: Vec::new(),
        bcc: Vec::new(),
        reply_to: None,
        initial_in_reply_to: None,
        subject: args.subject.clone(),
        xmailer: args.xmailer.or_else(|| s.bool("xmailer")).unwrap_or(true),
        transfer_encoding,
        auto_8bit: args
            .eightbit_encoding
            .clone()
            .or_else(|| s.string("assume8bitEncoding")),
        compose_encoding: args
            .compose_encoding
            .clone()
            .or_else(|| s.string("composeEncoding"))
            .unwrap_or_else(|| "UTF-8".into()),
        to_cmd: args.to_cmd.clone().or_else(|| s.string("toCmd")),
        cc_cmd: args.cc_cmd.clone().or_else(|| s.string("ccCmd")),
        header_cmd: if args.no_header_cmd {
            None
        } else {
            args.header_cmd.clone().or_else(|| s.string("headerCmd"))
        },
        suppress,
        cc_cover: args.cc_cover.or_else(|| s.bool("ccCover")).unwrap_or(false),
        to_cover: args.to_cover.or_else(|| s.bool("toCover")).unwrap_or(false),
        thread: args.thread.or_else(|| s.bool("thread")).unwrap_or(true),
        chain_reply_to: args
            .chain_reply_to
            .or_else(|| s.bool("chainReplyTo"))
            .unwrap_or(false),
        confirm,
        confirm_unconfigured,
        quiet: args.quiet,
        dry_run: args.dry_run,
        delivery,
        envelope_sender: None,
        imap_folder,
        imap_only,
    };

    // Sender.
    let from = args.from.clone().or_else(|| s.string("from"));
    let from = match from {
        Some(f) => address::trim(&f).to_string(),
        None => default_sender(ctx)?,
    };
    state.sender = expand_one(&state, &from);

    let mut to = args.to.clone();
    if to.is_empty() && !args.no_to {
        to = s.strings("to");
    }
    let mut cc = args.cc.clone();
    if cc.is_empty() && !args.no_cc {
        cc = s.strings("cc");
    }
    let mut bcc = args.bcc.clone();
    if bcc.is_empty() && !args.no_bcc {
        bcc = s.strings("bcc");
    }
    state.to = expand_list(&state, &to);
    state.cc = expand_list(&state, &cc);
    state.bcc = expand_list(&state, &bcc);
    state.reply_to = args
        .reply_to
        .as_deref()
        .map(|r| expand_one(&state, r))
        .filter(|r| !r.is_empty());
    state.initial_in_reply_to = args
        .in_reply_to
        .as_deref()
        .and_then(normalize_message_id);
    state.envelope_sender = args
        .envelope_sender
        .clone()
        .or_else(|| s.string("envelopeSender"))
        .map(|e| {
            if e == "auto" {
                address::email(&state.sender)
            } else {
                address::email(&e)
            }
        });
    let _ = cfg;
    Ok(state)
}

/// `GIT_AUTHOR_IDENT` (else `GIT_COMMITTER_IDENT`) without the date.
fn default_sender(ctx: &Context) -> Result<String, Die> {
    let mut last_status = 128;
    for var in ["GIT_AUTHOR_IDENT", "GIT_COMMITTER_IDENT"] {
        let out = rgit_command(ctx)
            .arg("var")
            .arg(var)
            .stdin(Stdio::null())
            .stderr(Stdio::inherit())
            .output()
            .map_err(|e| Die::Fatal(e.to_string()))?;
        if out.status.success() {
            let ident = String::from_utf8_lossy(&out.stdout).trim_end().to_string();
            return Ok(match ident.rfind('>') {
                Some(gt) => ident[..=gt].to_string(),
                None => ident,
            });
        }
        last_status = out.status.code().unwrap_or(128);
    }
    Err(Die::Exit(last_status))
}

/// `<id>` with angle brackets added; `None` for an empty answer.
fn normalize_message_id(s: &str) -> Option<String> {
    let t = address::trim(s);
    if t.is_empty() {
        return None;
    }
    let mut id = String::new();
    if !t.starts_with('<') {
        id.push('<');
    }
    id.push_str(t);
    if !t.ends_with('>') {
        id.push('>');
    }
    Some(id)
}

/// Apply the mailmap to one address.
fn map_address(state: &State<'_>, addr: &str) -> String {
    let Some(mm) = &state.mailmap else {
        return addr.to_string();
    };
    let (name, email) = address::parts(addr);
    let (n, e) = mm.map(name.as_bytes(), email.as_bytes());
    let n = String::from_utf8_lossy(n).into_owned();
    let e = String::from_utf8_lossy(e).into_owned();
    if e == email && n == name {
        return addr.to_string();
    }
    if n.is_empty() {
        e
    } else {
        address::sanitize(&format!("{n} <{e}>"))
    }
}

/// One address through aliases, sanitizing and the mailmap.
fn expand_one(state: &State<'_>, addr: &str) -> String {
    expand_list(state, &[addr.to_string()])
        .into_iter()
        .next()
        .unwrap_or_default()
}

/// Split, expand aliases, sanitize and map a list of address arguments.
fn expand_list(state: &State<'_>, list: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for item in list {
        for a in address::split_list(item) {
            for e in state.aliases.expand(&a) {
                for x in address::split_list(&e) {
                    out.push(map_address(state, &address::sanitize(&x)));
                }
            }
        }
    }
    out
}

fn collect_files(
    ctx: &Context,
    state: &State<'_>,
    args: &Args,
    in_repo: bool,
    tmp: &mut Option<TempDir>,
) -> Result<Vec<PatchFile>, Die> {
    let mut files = Vec::new();
    let mut revs: Vec<String> = Vec::new();
    let mut after_dashdash = false;
    for arg in &args.rest {
        if after_dashdash {
            revs.push(arg.clone());
            continue;
        }
        if arg == "--" {
            after_dashdash = true;
            revs.push(arg.clone());
            continue;
        }
        if arg.starts_with('-') {
            revs.push(arg.clone());
            continue;
        }
        let path = state.cwd.join(arg);
        if path.exists() && args.format_patch != Some(true) {
            if args.format_patch.is_none() && in_repo && is_revision(ctx, arg) {
                eprintln!("{}", msg::ambiguous_file(arg));
                return Err(Die::Exit(255));
            }
            if path.is_dir() {
                let mut entries: Vec<String> = std::fs::read_dir(&path)
                    .map_err(|e| Die::Fatal(format!("{arg}: {e}")))?
                    .flatten()
                    .filter(|e| e.path().is_file())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| !n.starts_with('.'))
                    .collect();
                entries.sort();
                for n in entries {
                    let display = format!("{}/{n}", arg.trim_end_matches('/'));
                    files.push(PatchFile {
                        path: path.join(&n),
                        display,
                    });
                }
            } else {
                files.push(PatchFile {
                    path,
                    display: arg.clone(),
                });
            }
        } else {
            revs.push(arg.clone());
        }
    }
    if !revs.is_empty() {
        let dir = TempDir::new("rgit-send-email-").map_err(|e| Die::Fatal(e.to_string()))?;
        let out = rgit_command(ctx)
            .arg("format-patch")
            .arg("-o")
            .arg(&dir.0)
            .args(&revs)
            .current_dir(&state.cwd)
            .stdin(Stdio::null())
            .stderr(Stdio::inherit())
            .output()
            .map_err(|e| Die::Fatal(e.to_string()))?;
        if !out.status.success() {
            let code = out.status.code().unwrap_or(128);
            let cmdline = format!("format-patch -o {} {}", dir.0.display(), revs.join(" "));
            eprintln!("{}", msg::format_patch_failed(&cmdline, code));
            return Err(Die::Exit(code));
        }
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let l = line.trim();
            if l.is_empty() {
                continue;
            }
            let p = PathBuf::from(l);
            let path = if rgit_fs::path::is_absolute(&p) { p } else { state.cwd.join(p) };
            files.push(PatchFile {
                display: path.display().to_string(),
                path,
            });
        }
        *tmp = Some(dir);
    }
    Ok(files)
}

fn is_revision(ctx: &Context, arg: &str) -> bool {
    rgit_command(ctx)
        .args(["rev-parse", "--verify", "--quiet", arg])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn run_validate_hook(
    ctx: &Context,
    git_dir: &Path,
    work_tree: Option<&Path>,
    f: &PatchFile,
    p: &Patch,
    counter: usize,
    total: usize,
) -> Result<(), Die> {
    let dir = TempDir::new("rgit-send-email-hook-").map_err(|e| Die::Fatal(e.to_string()))?;
    let header = dir.0.join("header");
    let mut text = String::new();
    for (n, v) in &p.headers {
        text.push_str(&format!("{n}: {v}\n"));
    }
    std::fs::write(&header, text).map_err(|e| Die::Fatal(e.to_string()))?;
    let abs = rgit_fs::path::canonicalize(&f.path)
        .or_else(|_| std::path::absolute(&f.path))
        .unwrap_or_else(|_| f.path.clone());
    let mut cmd = rgit_command(ctx);
    if let Some(w) = work_tree {
        cmd.current_dir(w);
    }
    let status = cmd
        .args(["hook", "run", "--ignore-missing", "sendemail-validate", "--"])
        .arg(&abs)
        .arg(&header)
        .env("GIT_DIR", git_dir)
        .env("GIT_SENDEMAIL_FILE_COUNTER", counter.to_string())
        .env("GIT_SENDEMAIL_FILE_TOTAL", total.to_string())
        .status()
        .map_err(|e| Die::Fatal(e.to_string()))?;
    if !status.success() {
        let _ = std::io::stdout().flush();
        eprintln!("{}", msg::hook_rejected(&f.display));
        eprintln!("{}", msg::hook_died(status.code().unwrap_or(1)));
        eprintln!("{}", msg::NO_PATCHES_SENT);
        return Err(Die::Exit(1));
    }
    Ok(())
}

fn ask_8bit_encoding() -> String {
    loop {
        let Some(answer) = read_answer(msg::ASK_8BIT) else {
            return "UTF-8".into();
        };
        let answer = answer.trim().to_string();
        if answer.is_empty() {
            return "UTF-8".into();
        }
        if answer.chars().count() >= 4 {
            return answer;
        }
        match read_answer(&msg::ask_charset_anyway(&answer)) {
            Some(y) if y.trim().eq_ignore_ascii_case("y") || y.trim().eq_ignore_ascii_case("yes") => {
                return answer;
            }
            None => return "UTF-8".into(),
            _ => {}
        }
    }
}

fn run_editor(cfg: &ConfigSet, files: &[PathBuf]) -> Result<(), Die> {
    let env = SystemEnv::default();
    let editor = rgit_ident::editor::editor(cfg, &env)
        .ok()
        .flatten()
        .unwrap_or_else(|| "vi".into());
    if editor == ":" {
        return Ok(());
    }
    let args: Vec<&std::ffi::OsStr> = files.iter().map(|p| p.as_os_str()).collect();
    let status = rgit_ident::editor::shell_command(&editor, &args).status();
    match status {
        Ok(s) if s.success() => Ok(()),
        _ => {
            eprintln!("{}", msg::EDITOR_FAILED);
            Err(Die::Exit(1))
        }
    }
}

fn write_compose_template(
    state: &State<'_>,
    files: &[PatchFile],
    path: &Path,
) -> Result<(), Die> {
    let mut t = String::new();
    t.push_str(&format!("From {} # This line is ignored.\n", state.sender));
    t.push_str("GIT: Lines starting with \"GIT:\" are dropped.\n");
    t.push_str("GIT: Write an introduction to the series below the headers,\n");
    t.push_str("GIT: for example a summary or a diffstat of the patches.\n");
    t.push_str("GIT: \n");
    t.push_str("GIT: Leave the body empty to send no introduction.\n");
    t.push_str(&format!("From: {}\n", state.sender));
    t.push_str(&format!("To: {}\n", state.to.join(", ")));
    t.push_str(&format!("Cc: {}\n", state.cc.join(", ")));
    t.push_str(&format!("Bcc: {}\n", state.bcc.join(", ")));
    t.push_str(&format!(
        "Reply-To: {}\n",
        state.reply_to.as_deref().unwrap_or("")
    ));
    t.push_str(&format!(
        "Subject: {}\n",
        state.subject.as_deref().unwrap_or("")
    ));
    t.push_str(&format!(
        "In-Reply-To: {}\n",
        state.initial_in_reply_to.as_deref().unwrap_or("")
    ));
    t.push('\n');
    for f in files {
        let data = std::fs::read(&f.path).unwrap_or_default();
        if let Some(s) = Patch::parse(&data).subject() {
            t.push_str(&format!("GIT: {s}\n"));
        }
    }
    std::fs::write(path, t).map_err(|e| Die::Fatal(format!("{}: {e}", path.display())))
}

/// The introduction written with `--compose`.
struct Composed {
    subject: String,
    /// MIME header fields the user wrote (`Content-Type:` ...).
    mime: Vec<(String, String)>,
    body: Vec<u8>,
}

/// Read the edited compose file: its header fields replace the settings;
/// a body with text becomes the first message.
fn apply_compose(state: &mut State<'_>, text: &[u8]) -> Option<Composed> {
    let text = String::from_utf8_lossy(text);
    let mut lines = text.split_inclusive('\n').peekable();
    if lines.peek().is_some_and(|l| l.starts_with("From ")) {
        lines.next();
    }
    let mut in_header = true;
    let mut body = String::new();
    let mut mime = Vec::new();
    let mut subject = state.subject.clone().unwrap_or_default();
    for line in lines {
        if line.starts_with("GIT:") {
            continue;
        }
        if in_header {
            let l = line.trim_end_matches(['\n', '\r']);
            if l.trim().is_empty() {
                in_header = false;
                continue;
            }
            let Some((k, v)) = l.split_once(':') else {
                continue;
            };
            let v = v.trim().to_string();
            match k.trim().to_ascii_lowercase().as_str() {
                "from" => {
                    if !v.is_empty() {
                        state.sender = expand_one(state, &v);
                    }
                }
                "to" => state.to = expand_list(state, &[v]),
                "cc" => state.cc = expand_list(state, &[v]),
                "bcc" => state.bcc = expand_list(state, &[v]),
                "subject" => subject = v,
                "reply-to" => {
                    state.reply_to = (!v.is_empty()).then(|| expand_one(state, &v));
                }
                "in-reply-to" => state.initial_in_reply_to = normalize_message_id(&v),
                "mime-version" => {}
                "content-type" | "content-transfer-encoding" => {
                    mime.push((k.trim().to_string(), v));
                }
                _ => {}
            }
            continue;
        }
        body.push_str(line);
    }
    state.subject = Some(subject.clone());
    if body.trim().is_empty() {
        return None;
    }
    Some(Composed {
        subject,
        mime,
        body: body.into_bytes(),
    })
}

/// One message ready to go.
struct Outgoing {
    header: String,
    body: Vec<u8>,
    subject: String,
    message_id: String,
    to: Vec<String>,
    cc: Vec<String>,
    recipients: Vec<String>,
    cc_added: bool,
    is_compose: bool,
}

fn unique_by_email(list: Vec<String>, exclude: &[String]) -> Vec<String> {
    let mut seen: HashSet<String> = exclude
        .iter()
        .map(|a| address::email(a).to_ascii_lowercase())
        .collect();
    let mut out = Vec::new();
    for a in list {
        if a.is_empty() {
            continue;
        }
        if seen.insert(address::email(&a).to_ascii_lowercase()) {
            out.push(a);
        }
    }
    out
}

/// Run a `--to-cmd` / `--cc-cmd` / `--header-cmd` command with the file
/// as argument; its output lines.
fn run_cmd(prefix: &str, cmd: &str, file: &Path, cwd: &Path) -> Result<Vec<String>, Die> {
    let out = rgit_ident::editor::shell_command(cmd, &[file.as_os_str()])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output();
    match out {
        Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(str::to_string)
            .collect()),
        _ => {
            eprintln!("{}", msg::cmd_failed(prefix, cmd));
            Err(Die::Exit(1))
        }
    }
}

/// Header lines from `--header-cmd`: continuation lines are unfolded, an
/// empty line in the middle is an error.
fn header_cmd_lines(cmd: &str, file: &Path, cwd: &Path) -> Result<Vec<String>, Die> {
    let lines = run_cmd("header-cmd", cmd, file, cwd)?;
    let mut out: Vec<String> = Vec::new();
    let mut ended = false;
    for l in lines {
        if l.trim().is_empty() {
            ended = true;
            continue;
        }
        if ended {
            eprintln!("{}", msg::malformed_cmd_output("header-cmd", cmd));
            return Err(Die::Exit(1));
        }
        if l.starts_with([' ', '\t']) {
            if let Some(last) = out.last_mut() {
                last.push(' ');
                last.push_str(l.trim_start());
                continue;
            }
        }
        out.push(l);
    }
    Ok(out)
}

/// Encode a subject with non-ASCII characters as an RFC 2047 word.
fn encode_subject(subject: &str, charset: &str) -> String {
    if subject.is_ascii() || subject.contains("=?") {
        subject.to_string()
    } else {
        address::encode_word(subject.as_bytes(), charset)
    }
}

/// Encode `body` for transfer; returns the body and the encoding name.
fn transfer_encode(body: Vec<u8>, wanted: &str) -> Result<(Vec<u8>, String), Die> {
    let target = if wanted == "auto" {
        let long = patch::first_long_line(&body, 998).is_some();
        if long || body.contains(&b'\r') {
            "quoted-printable"
        } else {
            "8bit"
        }
    } else {
        wanted
    };
    let out = match target {
        "7bit" => {
            if patch::has_8bit(&body) {
                eprintln!("{}", msg::CANNOT_7BIT);
                return Err(Die::Exit(1));
            }
            body
        }
        "quoted-printable" => rgit_mailnet::encoding::quoted_printable(&body),
        "base64" => rgit_mailnet::encoding::base64_body(&body),
        _ => body,
    };
    Ok((out, target.to_string()))
}

fn decode_body(body: &[u8], cte: Option<&str>) -> Vec<u8> {
    match cte.map(|c| c.trim().to_ascii_lowercase()).as_deref() {
        Some("quoted-printable") => rgit_mailnet::encoding::quoted_printable_decode(body),
        Some("base64") => rgit_mailnet::encoding::base64_decode(body).unwrap_or_else(|| body.to_vec()),
        _ => body.to_vec(),
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn rfc2822_date(t: i64) -> String {
    let dctx = rgit_date::DateContext::from_env();
    let tz = dctx.local_tz(t);
    rgit_date::show_date(
        t.max(0) as u64,
        tz,
        &rgit_date::DateMode::new(rgit_date::DateStyle::Rfc2822),
        &dctx,
    )
    .unwrap_or_default()
}

/// `<YYYYmmddHHMMSS.pid-n-sender>`
fn make_message_id(start: i64, n: usize, sender: &str) -> String {
    let dctx = rgit_date::DateContext::utc(start);
    let stamp = rgit_date::show_date(
        start.max(0) as u64,
        0,
        &rgit_date::DateMode::new(rgit_date::DateStyle::Strftime("%Y%m%d%H%M%S".into())),
        &dctx,
    )
    .unwrap_or_else(|_| start.to_string());
    let email = address::email(sender);
    let email = if email.contains('@') {
        email
    } else {
        format!("{email}@localhost")
    };
    format!("<{stamp}.{}-{n}-{email}>", std::process::id())
}

fn send_all(
    state: &mut State<'_>,
    files: &[PatchFile],
    broken: &HashSet<usize>,
    composed: Option<Composed>,
) -> CmdResult {
    let start = now_secs();
    let total = files.len() + usize::from(composed.is_some());
    let mut date = start - total as i64;
    let mut first_id: Option<String> = None;
    let mut chain: Vec<String> = Vec::new();
    let initial_refs: Vec<String> = state.initial_in_reply_to.iter().cloned().collect();
    let mut cover_to: Vec<String> = Vec::new();
    let mut cover_cc: Vec<String> = Vec::new();
    let mut note_shown = false;
    let mut session = deliver::Session::default();
    let mut index = 0usize;

    let mut items: Vec<Option<usize>> = Vec::new();
    if composed.is_some() {
        items.push(None);
    }
    items.extend((0..files.len()).map(Some));

    for item in items {
        index += 1;
        date += 1;
        let (irt, refs) = if state.thread {
            if index == 1 {
                (state.initial_in_reply_to.clone(), initial_refs.clone())
            } else if state.chain_reply_to {
                let mut r = initial_refs.clone();
                r.extend(chain.iter().cloned());
                (chain.last().cloned(), r)
            } else {
                let mut r = initial_refs.clone();
                r.extend(first_id.iter().cloned());
                (first_id.clone(), r)
            }
        } else {
            (state.initial_in_reply_to.clone(), initial_refs.clone())
        };
        let mut out = match item {
            None => {
                let c = composed.as_ref().expect("composed message");
                build_compose(state, c, index, start, date, irt, refs)?
            }
            Some(i) => {
                let f = &files[i];
                let data = std::fs::read(&f.path)
                    .map_err(|e| Die::Fatal(format!("{}: {e}", f.display)))?;
                let p = Patch::parse(&data);
                let first_file = i == 0;
                let b = build_patch(
                    state,
                    &p,
                    f,
                    broken.contains(&i),
                    index,
                    start,
                    date,
                    irt,
                    refs,
                    (&cover_to, &cover_cc),
                )?;
                if first_file {
                    if state.to_cover {
                        cover_to = p.header_all("To").flat_map(address::split_list).collect();
                        cover_to = expand_list(state, &cover_to);
                    }
                    if state.cc_cover {
                        cover_cc = p.header_all("Cc").flat_map(address::split_list).collect();
                        cover_cc = expand_list(state, &cover_cc);
                    }
                }
                b
            }
        };

        // Confirmation.
        let needs = match state.confirm.as_str() {
            "always" => true,
            "never" => false,
            "cc" => out.cc_added,
            "compose" => out.is_compose,
            _ => out.cc_added || out.is_compose,
        };
        if needs {
            let mut summary = String::from("\n");
            for line in out.header.lines() {
                let lower = line.to_ascii_lowercase();
                if lower.starts_with("date:")
                    || lower.starts_with("message-id:")
                    || lower.starts_with("x-mailer:")
                {
                    continue;
                }
                summary.push_str(line);
                summary.push('\n');
            }
            println!("{summary}");
            if state.confirm_unconfigured && out.cc_added && !note_shown {
                println!("{}", msg::CC_EXPANDED_NOTE);
                note_shown = true;
            }
            let default = state.confirm_unconfigured.then_some("y");
            let answer = ask(
                msg::ASK_SEND,
                |a| {
                    let a = a.trim().to_ascii_lowercase();
                    ["y", "n", "e", "q", "a"]
                        .iter()
                        .any(|p| a.starts_with(p))
                },
                default,
            );
            let Some(answer) = answer else {
                eprintln!("{}", msg::SEND_REPLY_REQUIRED);
                return Err(Die::Exit(255));
            };
            match answer.trim().to_ascii_lowercase().chars().next() {
                Some('n') => continue,
                Some('q') => break,
                Some('a') => state.confirm = "never".into(),
                Some('e') => edit_outgoing(state, &mut out)?,
                _ => {}
            }
        }

        // Delivery.
        let envelope = Envelope {
            from: state
                .envelope_sender
                .clone()
                .unwrap_or_else(|| address::email(&state.sender)),
            explicit_envelope: state.envelope_sender.clone(),
            recipients: out.recipients.clone(),
        };
        let mut message = out.header.clone().into_bytes();
        message.push(b'\n');
        message.extend_from_slice(&out.body);
        let new_id = if state.imap_only {
            None
        } else {
            deliver::send(
                &mut session,
                &state.delivery,
                state.ctx,
                &envelope,
                &out.header,
                &message,
                &out.subject,
                state.dry_run,
                state.quiet,
            )?
        };
        if let Some(folder) = &state.imap_folder
            && !state.dry_run
        {
            deliver::store_imap(state.ctx, folder, &message, &out.message_id);
        }
        let id = new_id.unwrap_or_else(|| out.message_id.clone());
        if first_id.is_none() {
            first_id = Some(id.clone());
        }
        chain.push(id);
        let _ = (&out.to, &out.cc);
    }
    session.finish();
    Ok(0)
}

fn edit_outgoing(state: &State<'_>, out: &mut Outgoing) -> Result<(), Die> {
    let dir = TempDir::new("rgit-send-email-edit-").map_err(|e| Die::Fatal(e.to_string()))?;
    let path = dir.0.join("message");
    let mut text = out.header.clone().into_bytes();
    text.push(b'\n');
    text.extend_from_slice(&out.body);
    std::fs::write(&path, &text).map_err(|e| Die::Fatal(e.to_string()))?;
    let env = SystemEnv::default();
    let _ = state;
    let cfg = ConfigSet::new();
    let editor = rgit_ident::editor::editor(&cfg, &env).ok().flatten();
    if let Some(ed) = editor {
        let status = rgit_ident::editor::shell_command(&ed, &[path.as_os_str()]).status();
        if !status.is_ok_and(|s| s.success()) {
            eprintln!("{}", msg::EDITOR_FAILED);
            return Err(Die::Exit(1));
        }
    }
    let edited = std::fs::read(&path).unwrap_or(text);
    let split = edited
        .windows(2)
        .position(|w| w == b"\n\n")
        .map_or(edited.len(), |i| i + 1);
    out.header = String::from_utf8_lossy(&edited[..split]).into_owned();
    out.body = edited.get(split + 1..).unwrap_or_default().to_vec();
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn assemble(
    state: &State<'_>,
    to: &[String],
    cc: &[String],
    subject: &str,
    date: i64,
    message_id: &str,
    irt: Option<&str>,
    refs: &[String],
    reply_to: Option<&str>,
    extra: &[(String, String)],
    cte: &str,
) -> String {
    let mut h = String::new();
    h.push_str(&format!("From: {}\n", state.sender));
    h.push_str(&format!("To: {}\n", to.join(",\n\t")));
    if !cc.is_empty() {
        h.push_str(&format!("Cc: {}\n", cc.join(",\n\t")));
    }
    h.push_str(&format!("Subject: {subject}\n"));
    h.push_str(&format!("Date: {}\n", rfc2822_date(date)));
    h.push_str(&format!("Message-ID: {message_id}\n"));
    if state.xmailer {
        h.push_str(&format!(
            "X-Mailer: git-send-email {}\n",
            rgit_version::version()
        ));
    }
    if let Some(i) = irt {
        h.push_str(&format!("In-Reply-To: {i}\n"));
        if !refs.is_empty() {
            h.push_str(&format!("References: {}\n", refs.join("\n ")));
        }
    }
    if let Some(r) = reply_to {
        h.push_str(&format!("Reply-To: {r}\n"));
    }
    h.push_str("MIME-Version: 1.0\n");
    for (n, v) in extra {
        h.push_str(&format!("{n}: {v}\n"));
    }
    h.push_str(&format!("Content-Transfer-Encoding: {cte}\n"));
    h
}

fn recipients(to: &[String], cc: &[String], bcc: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for a in to.iter().chain(cc).chain(bcc) {
        let e = address::email(a);
        if !e.is_empty() && seen.insert(e.to_ascii_lowercase()) {
            out.push(e);
        }
    }
    out
}

fn build_compose(
    state: &State<'_>,
    c: &Composed,
    index: usize,
    start: i64,
    date: i64,
    irt: Option<String>,
    refs: Vec<String>,
) -> Result<Outgoing, Die> {
    let to = unique_by_email(state.to.clone(), &[]);
    let cc = unique_by_email(state.cc.clone(), &to);
    let subject = encode_subject(&c.subject, &state.compose_encoding);
    let mut extra: Vec<(String, String)> = Vec::new();
    let mut source_cte = None;
    for (k, v) in &c.mime {
        if k.eq_ignore_ascii_case("content-transfer-encoding") {
            source_cte = Some(v.clone());
        } else {
            extra.push((k.clone(), v.clone()));
        }
    }
    let has_ct = extra
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("content-type"));
    if !has_ct && (patch::has_8bit(&c.body) || !c.subject.is_ascii()) {
        extra.push((
            "Content-Type".into(),
            format!("text/plain; charset={}", state.compose_encoding),
        ));
    }
    let body = decode_body(&c.body, source_cte.as_deref());
    let (body, cte) = transfer_encode(body, &state.transfer_encoding)?;
    let message_id = make_message_id(start, index, &state.sender);
    let header = assemble(
        state,
        &to,
        &cc,
        &subject,
        date,
        &message_id,
        irt.as_deref(),
        &refs,
        state.reply_to.as_deref(),
        &extra,
        &cte,
    );
    let recipients = recipients(&to, &cc, &state.bcc);
    Ok(Outgoing {
        header,
        body,
        subject,
        message_id,
        to,
        cc,
        recipients,
        cc_added: false,
        is_compose: true,
    })
}

/// Header fields of a patch that send-email writes itself.
const OWN_FIELDS: &[&str] = &[
    "from",
    "to",
    "cc",
    "bcc",
    "subject",
    "date",
    "message-id",
    "in-reply-to",
    "references",
    "reply-to",
    "mime-version",
    "content-transfer-encoding",
    "x-mailer",
];

#[allow(clippy::too_many_arguments)]
fn build_patch(
    state: &State<'_>,
    p: &Patch,
    f: &PatchFile,
    broken: bool,
    index: usize,
    start: i64,
    date: i64,
    irt: Option<String>,
    refs: Vec<String>,
    cover: (&[String], &[String]),
) -> Result<Outgoing, Die> {
    let quiet = state.quiet;
    let mut to: Vec<String> = state.to.clone();
    to.extend(cover.0.iter().cloned());
    let mut cc: Vec<String> = state.cc.clone();
    cc.extend(cover.1.iter().cloned());
    let mut cc_added = false;
    let sender_key = address::canonical(&state.sender);
    let is_self = |a: &str| state.suppress.self_ && address::canonical(a) == sender_key;
    let source = if p.lots { "non-mbox" } else { "mbox" };
    let mut author: Option<String> = None;

    for (name, value) in &p.headers {
        let line = format!("{name}: {value}");
        match name.to_ascii_lowercase().as_str() {
            "from" if !p.lots => {
                author = Some(value.clone());
                if state.suppress.author {
                    continue;
                }
                let a = map_address(state, &address::sanitize(value));
                if is_self(&a) {
                    continue;
                }
                if !quiet {
                    println!("{}", msg::adding(source, "cc", &a, &line));
                }
                cc.push(a);
                cc_added = true;
            }
            "to" => {
                for a in expand_list(state, std::slice::from_ref(value)) {
                    if !quiet {
                        println!("{}", msg::adding(source, "to", &a, &line));
                    }
                    to.push(a);
                }
            }
            "cc" => {
                if state.suppress.cc {
                    continue;
                }
                for a in expand_list(state, std::slice::from_ref(value)) {
                    if is_self(&a) {
                        continue;
                    }
                    if !quiet {
                        println!("{}", msg::adding(source, "cc", &a, &line));
                    }
                    cc.push(a);
                    cc_added = true;
                }
            }
            _ => {}
        }
    }

    // Trailers in the body.
    for raw in p.body.split(|&b| b == b'\n') {
        let line = String::from_utf8_lossy(patch::strip_eol(raw)).into_owned();
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let lower = key.to_ascii_lowercase();
        let is_cc = lower == "cc";
        let is_by = !is_cc
            && lower.ends_with("-by")
            && lower.chars().all(|c| c.is_ascii_alphabetic() || c == '-');
        if !is_cc && !is_by {
            continue;
        }
        if !value.starts_with(' ') && !value.is_empty() {
            continue;
        }
        let suppressed = if is_cc {
            state.suppress.bodycc
        } else if lower == "signed-off-by" {
            state.suppress.sob
        } else {
            state.suppress.misc_by
        };
        if suppressed {
            continue;
        }
        let candidate = address::first_of_trailer(value);
        if !candidate.contains('@') {
            if !quiet {
                println!("{}", msg::ignoring("body", key, &line));
            }
            continue;
        }
        let a = map_address(state, &address::sanitize(&candidate));
        if is_self(&a) {
            continue;
        }
        if !quiet {
            println!("{}", msg::adding("body", "cc", &a, &line));
        }
        cc.push(a);
        cc_added = true;
    }

    if let Some(cmd) = &state.to_cmd {
        for l in run_cmd("to-cmd", cmd, &f.path, &state.cwd)? {
            let l = address::strip_comment(&l);
            if l.is_empty() {
                continue;
            }
            for a in expand_list(state, &[l]) {
                if !quiet {
                    println!("{}", msg::adding_from_cmd("to-cmd", "to", &a, cmd));
                }
                to.push(a);
            }
        }
    }
    if let Some(cmd) = &state.cc_cmd
        && !state.suppress.cccmd
    {
        for l in run_cmd("cc-cmd", cmd, &f.path, &state.cwd)? {
            let l = address::strip_comment(&l);
            if l.is_empty() {
                continue;
            }
            for a in expand_list(state, &[l]) {
                if is_self(&a) {
                    continue;
                }
                if !quiet {
                    println!("{}", msg::adding_from_cmd("cc-cmd", "cc", &a, cmd));
                }
                cc.push(a);
                cc_added = true;
            }
        }
    }

    let to = unique_by_email(to, &[]);
    let cc = unique_by_email(cc, &to);

    // Header fields.
    let charset = p
        .charset()
        .or_else(|| broken.then(|| state.auto_8bit.clone()).flatten())
        .unwrap_or_else(|| "UTF-8".into());
    let subject = p.subject().unwrap_or("").to_string();
    let subject = if broken || p.charset().is_some() {
        encode_subject(&subject, &charset)
    } else {
        subject
    };
    let message_id = p
        .header("Message-ID")
        .map(str::to_string)
        .unwrap_or_else(|| make_message_id(start, index, &state.sender));
    let (irt, refs) = match irt {
        Some(i) => (Some(i), refs),
        None => (
            p.header("In-Reply-To").map(str::to_string),
            p.header("References")
                .map(|r| vec![r.to_string()])
                .unwrap_or_default(),
        ),
    };
    let reply_to = p
        .header("Reply-To")
        .map(str::to_string)
        .or_else(|| state.reply_to.clone());
    let mut extra: Vec<(String, String)> = p
        .headers
        .iter()
        .filter(|(n, _)| !OWN_FIELDS.contains(&n.to_ascii_lowercase().as_str()))
        .cloned()
        .collect();
    if p.lots {
        extra.retain(|(n, _)| !n.eq_ignore_ascii_case("cc"));
    }
    if broken
        && p.charset().is_none()
        && let Some(enc) = &state.auto_8bit
    {
        extra.push(("Content-Type".into(), format!("text/plain; charset={enc}")));
    }
    if let Some(cmd) = &state.header_cmd {
        for l in header_cmd_lines(cmd, &f.path, &state.cwd)? {
            match l.split_once(':') {
                Some((n, v)) => extra.push((n.to_string(), v.trim_start().to_string())),
                None => extra.push((l, String::new())),
            }
        }
    }

    // Body.
    let mut body = decode_body(&p.body, p.header("Content-Transfer-Encoding"));
    if let Some(a) = &author
        && address::canonical(a) != sender_key
    {
        let mut b = format!("From: {}\n\n", address::display(a)).into_bytes();
        b.extend_from_slice(&body);
        body = b;
    }
    let (body, cte) = transfer_encode(body, &state.transfer_encoding)?;

    let header = assemble(
        state,
        &to,
        &cc,
        &subject,
        date,
        &message_id,
        irt.as_deref(),
        &refs,
        reply_to.as_deref(),
        &extra,
        &cte,
    );
    let recipients = recipients(&to, &cc, &state.bcc);
    Ok(Outgoing {
        header,
        body,
        subject,
        message_id,
        to,
        cc,
        recipients,
        cc_added,
        is_compose: false,
    })
}
