// SPDX-License-Identifier: MIT
//! Command line of `send-email`. Options it does not know, and arguments
//! that are not files, are handed to `format-patch` unchanged (in order).

use crate::Die;
use crate::messages::send_email as msg;

/// The parsed command line. `None` means "not given" (configuration or
/// default applies).
#[derive(Debug, Default)]
pub struct Args {
    pub from: Option<String>,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub no_to: bool,
    pub no_cc: bool,
    pub no_bcc: bool,
    pub subject: Option<String>,
    pub reply_to: Option<String>,
    pub in_reply_to: Option<String>,
    pub outlook_id_fix: Option<bool>,
    pub xmailer: Option<bool>,
    pub annotate: Option<bool>,
    pub compose: bool,
    pub compose_encoding: Option<String>,
    pub eightbit_encoding: Option<String>,
    pub transfer_encoding: Option<String>,
    pub mailmap: Option<bool>,

    pub envelope_sender: Option<String>,
    pub sendmail_cmd: Option<String>,
    pub smtp_server: Option<String>,
    pub smtp_server_options: Vec<String>,
    pub smtp_server_port: Option<String>,
    pub smtp_user: Option<String>,
    pub smtp_pass: Option<String>,
    pub smtp_encryption: Option<String>,
    pub smtp_ssl_cert_path: Option<String>,
    pub smtp_ssl_client_cert: Option<String>,
    pub smtp_ssl_client_key: Option<String>,
    pub smtp_domain: Option<String>,
    pub smtp_auth: Option<String>,
    pub smtp_debug: Option<String>,
    pub imap_sent_folder: Option<String>,
    pub use_imap_only: Option<bool>,
    pub batch_size: Option<String>,
    pub relogin_delay: Option<String>,

    pub identity: Option<String>,
    pub no_identity: bool,
    pub to_cmd: Option<String>,
    pub cc_cmd: Option<String>,
    pub header_cmd: Option<String>,
    pub no_header_cmd: bool,
    pub suppress_cc: Vec<String>,
    pub cc_cover: Option<bool>,
    pub to_cover: Option<bool>,
    pub signed_off_by_cc: Option<bool>,
    pub suppress_from: Option<bool>,
    pub chain_reply_to: Option<bool>,
    pub thread: Option<bool>,

    pub confirm: Option<String>,
    pub quiet: bool,
    pub dry_run: bool,
    pub validate: Option<bool>,
    pub format_patch: Option<bool>,
    pub force: bool,

    pub dump_aliases: bool,
    pub translate_aliases: bool,
    pub help: bool,

    /// How many options of our own were given (for `--dump-aliases`).
    pub own_options: usize,
    /// Files, directories and format-patch arguments, in order.
    pub rest: Vec<String>,
}

/// What an option takes.
enum Kind {
    /// A string value.
    Value,
    /// An optional string value (`--smtp-pass`).
    OptionalValue,
    /// A switch that has a `--no-` form.
    Bool,
    /// A switch without a negation.
    Flag,
}

const OPTIONS: &[(&str, Kind)] = &[
    ("from", Kind::Value),
    ("to", Kind::Value),
    ("cc", Kind::Value),
    ("bcc", Kind::Value),
    ("subject", Kind::Value),
    ("reply-to", Kind::Value),
    ("in-reply-to", Kind::Value),
    ("outlook-id-fix", Kind::Bool),
    ("xmailer", Kind::Bool),
    ("annotate", Kind::Bool),
    ("compose", Kind::Flag),
    ("compose-encoding", Kind::Value),
    ("8bit-encoding", Kind::Value),
    ("transfer-encoding", Kind::Value),
    ("mailmap", Kind::Bool),
    ("envelope-sender", Kind::Value),
    ("sendmail-cmd", Kind::Value),
    ("smtp-server", Kind::Value),
    ("smtp-server-option", Kind::Value),
    ("smtp-server-port", Kind::Value),
    ("smtp-user", Kind::Value),
    ("smtp-pass", Kind::OptionalValue),
    ("smtp-encryption", Kind::Value),
    ("smtp-ssl", Kind::Flag),
    ("smtp-ssl-cert-path", Kind::Value),
    ("smtp-ssl-client-cert", Kind::Value),
    ("smtp-ssl-client-key", Kind::Value),
    ("smtp-domain", Kind::Value),
    ("smtp-auth", Kind::Value),
    ("smtp-debug", Kind::Value),
    ("imap-sent-folder", Kind::Value),
    ("use-imap-only", Kind::Bool),
    ("batch-size", Kind::Value),
    ("relogin-delay", Kind::Value),
    ("identity", Kind::Value),
    ("to-cmd", Kind::Value),
    ("cc-cmd", Kind::Value),
    ("header-cmd", Kind::Value),
    ("suppress-cc", Kind::Value),
    ("cc-cover", Kind::Bool),
    ("to-cover", Kind::Bool),
    ("signed-off-by-cc", Kind::Bool),
    ("signed-off-cc", Kind::Bool),
    ("suppress-from", Kind::Bool),
    ("chain-reply-to", Kind::Bool),
    ("thread", Kind::Bool),
    ("confirm", Kind::Value),
    ("quiet", Kind::Flag),
    ("dry-run", Kind::Flag),
    ("validate", Kind::Bool),
    ("format-patch", Kind::Bool),
    ("force", Kind::Flag),
    ("dump-aliases", Kind::Flag),
    ("translate-aliases", Kind::Flag),
];

/// Negations that clear a list or setting instead of a switch.
const CLEARING: &[&str] = &["to", "cc", "bcc", "identity", "header-cmd", "smtp-auth"];

impl Args {
    pub fn parse(argv: &[String]) -> Result<Args, Die> {
        let mut a = Args::default();
        let mut i = 0;
        while i < argv.len() {
            let arg = &argv[i];
            i += 1;
            if arg == "--" {
                a.rest.push(arg.clone());
                a.rest.extend(argv[i..].iter().cloned());
                break;
            }
            if arg == "-h" || arg == "--help-all" {
                a.help = true;
                continue;
            }
            let Some(body) = arg.strip_prefix("--") else {
                a.rest.push(arg.clone());
                continue;
            };
            let (name, inline) = match body.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (body, None),
            };
            if let Some(neg) = name.strip_prefix("no-")
                && inline.is_none()
                && let Some(done) = a.negate(neg)
            {
                if done {
                    a.own_options += 1;
                    continue;
                }
            }
            let Some((_, kind)) = OPTIONS.iter().find(|(n, _)| *n == name) else {
                a.rest.push(arg.clone());
                continue;
            };
            a.own_options += 1;
            let value = match kind {
                Kind::Value => match inline {
                    Some(v) => Some(v),
                    None => {
                        if i < argv.len() {
                            i += 1;
                            Some(argv[i - 1].clone())
                        } else {
                            return Err(Die::Error(msg::needs_argument(name), 1));
                        }
                    }
                },
                Kind::OptionalValue => match inline {
                    Some(v) => Some(v),
                    None if i < argv.len() && !argv[i].starts_with('-') => {
                        i += 1;
                        Some(argv[i - 1].clone())
                    }
                    None => Some(String::new()),
                },
                Kind::Bool | Kind::Flag => None,
            };
            a.set(name, value);
        }
        Ok(a)
    }

    /// Apply `--no-<name>`; `None` when there is no such negation.
    fn negate(&mut self, name: &str) -> Option<bool> {
        if CLEARING.contains(&name) {
            match name {
                "to" => self.no_to = true,
                "cc" => self.no_cc = true,
                "bcc" => self.no_bcc = true,
                "identity" => {
                    self.no_identity = true;
                    self.identity = None;
                }
                "header-cmd" => self.no_header_cmd = true,
                "smtp-auth" => self.smtp_auth = Some("none".into()),
                _ => {}
            }
            return Some(true);
        }
        let known = OPTIONS
            .iter()
            .any(|(n, k)| *n == name && matches!(k, Kind::Bool));
        if !known {
            return None;
        }
        self.set_bool(name, false);
        Some(true)
    }

    fn set_bool(&mut self, name: &str, v: bool) {
        let slot = match name {
            "outlook-id-fix" => &mut self.outlook_id_fix,
            "xmailer" => &mut self.xmailer,
            "annotate" => &mut self.annotate,
            "mailmap" => &mut self.mailmap,
            "use-imap-only" => &mut self.use_imap_only,
            "cc-cover" => &mut self.cc_cover,
            "to-cover" => &mut self.to_cover,
            "signed-off-by-cc" | "signed-off-cc" => &mut self.signed_off_by_cc,
            "suppress-from" => &mut self.suppress_from,
            "chain-reply-to" => &mut self.chain_reply_to,
            "thread" => &mut self.thread,
            "validate" => &mut self.validate,
            "format-patch" => &mut self.format_patch,
            _ => return,
        };
        *slot = Some(v);
    }

    fn set(&mut self, name: &str, value: Option<String>) {
        let v = value.unwrap_or_default();
        match name {
            "from" => self.from = Some(v),
            "to" => self.to.push(v),
            "cc" => self.cc.push(v),
            "bcc" => self.bcc.push(v),
            "subject" => self.subject = Some(v),
            "reply-to" => self.reply_to = Some(v),
            "in-reply-to" => self.in_reply_to = Some(v),
            "compose" => self.compose = true,
            "compose-encoding" => self.compose_encoding = Some(v),
            "8bit-encoding" => self.eightbit_encoding = Some(v),
            "transfer-encoding" => self.transfer_encoding = Some(v),
            "envelope-sender" => self.envelope_sender = Some(v),
            "sendmail-cmd" => self.sendmail_cmd = Some(v),
            "smtp-server" => self.smtp_server = Some(v),
            "smtp-server-option" => self.smtp_server_options.push(v),
            "smtp-server-port" => self.smtp_server_port = Some(v),
            "smtp-user" => self.smtp_user = Some(v),
            "smtp-pass" => self.smtp_pass = Some(v),
            "smtp-encryption" => self.smtp_encryption = Some(v),
            "smtp-ssl" => self.smtp_encryption = Some("ssl".into()),
            "smtp-ssl-cert-path" => self.smtp_ssl_cert_path = Some(v),
            "smtp-ssl-client-cert" => self.smtp_ssl_client_cert = Some(v),
            "smtp-ssl-client-key" => self.smtp_ssl_client_key = Some(v),
            "smtp-domain" => self.smtp_domain = Some(v),
            "smtp-auth" => self.smtp_auth = Some(v),
            "smtp-debug" => self.smtp_debug = Some(v),
            "imap-sent-folder" => self.imap_sent_folder = Some(v),
            "batch-size" => self.batch_size = Some(v),
            "relogin-delay" => self.relogin_delay = Some(v),
            "identity" => {
                self.identity = Some(v);
                self.no_identity = false;
            }
            "to-cmd" => self.to_cmd = Some(v),
            "cc-cmd" => self.cc_cmd = Some(v),
            "header-cmd" => {
                self.header_cmd = Some(v);
                self.no_header_cmd = false;
            }
            "suppress-cc" => self.suppress_cc.push(v),
            "confirm" => self.confirm = Some(v),
            "quiet" => self.quiet = true,
            "dry-run" => self.dry_run = true,
            "force" => self.force = true,
            "dump-aliases" => self.dump_aliases = true,
            "translate-aliases" => self.translate_aliases = true,
            other => self.set_bool(other, true),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Args {
        Args::parse(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap()
    }

    #[test]
    fn values_switches_and_passthrough() {
        let a = parse(&[
            "--to=a@x",
            "--to",
            "b@x",
            "--no-thread",
            "-v",
            "3",
            "--cover-letter",
            "file.patch",
            "--dry-run",
        ]);
        assert_eq!(a.to, vec!["a@x", "b@x"]);
        assert_eq!(a.thread, Some(false));
        assert!(a.dry_run);
        assert_eq!(a.rest, vec!["-v", "3", "--cover-letter", "file.patch"]);
    }

    #[test]
    fn negations_clear() {
        let a = parse(&["--no-to", "--to=x", "--no-identity", "--no-smtp-auth"]);
        assert!(a.no_to);
        assert_eq!(a.to, vec!["x"]);
        assert!(a.no_identity);
        assert_eq!(a.smtp_auth.as_deref(), Some("none"));
    }

    #[test]
    fn optional_password() {
        assert_eq!(parse(&["--smtp-pass"]).smtp_pass.as_deref(), Some(""));
        assert_eq!(
            parse(&["--smtp-pass", "-1"]).smtp_pass.as_deref(),
            Some("")
        );
        assert_eq!(parse(&["--smtp-pass=s"]).smtp_pass.as_deref(), Some("s"));
    }
}
