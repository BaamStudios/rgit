// SPDX-License-Identifier: MIT
//! Messages of `git send-email`.

/// `-h` output (our own wording; the options follow
/// Documentation/git-send-email.adoc).
pub const USAGE: &str = "\
usage: git send-email [<options>] <file|directory>...
   or: git send-email [<options>] <format-patch options>
   or: git send-email --dump-aliases
   or: git send-email --translate-aliases

Composing:
    --from <address>           sender of the messages
    --[no-]to <address>        recipients (repeatable)
    --[no-]cc <address>        carbon copies (repeatable)
    --[no-]bcc <address>       blind carbon copies (repeatable)
    --subject <subject>        subject of the first message (with --compose)
    --reply-to <address>       address for replies
    --in-reply-to <id>         Message-ID the first message replies to
    --[no-]outlook-id-fix      learn the Message-ID an Outlook server assigned
    --[no-]xmailer             add an X-Mailer: header
    --[no-]annotate            edit each patch before sending
    --compose                  write an introductory message in the editor
    --compose-encoding <enc>   charset of the introductory message
    --8bit-encoding <enc>      charset to declare for undeclared 8bit messages
    --transfer-encoding <enc>  7bit, 8bit, quoted-printable, base64 or auto
    --[no-]mailmap             map addresses through the mailmap

Sending:
    --envelope-sender <addr>   envelope sender (\"auto\": the From address)
    --sendmail-cmd <command>   sendmail-like command to send with
    --smtp-server <host|path>  SMTP server (host[:port]) or sendmail-like program
    --smtp-server-port <port>  SMTP server port
    --smtp-server-option <opt> option for a sendmail-like program (repeatable)
    --smtp-user <user>         user name for SMTP AUTH
    --smtp-pass[=<password>]   password for SMTP AUTH
    --smtp-encryption <mode>   ssl (implicit TLS) or tls (STARTTLS)
    --smtp-ssl                 same as --smtp-encryption ssl
    --smtp-ssl-cert-path <p>   CA certificates (file or directory, empty: no check)
    --smtp-ssl-client-cert <p> client certificate
    --smtp-ssl-client-key <p>  client certificate key
    --smtp-domain <fqdn>       name to send with EHLO
    --smtp-auth <mechanisms>   allowed SASL mechanisms, or \"none\"
    --no-smtp-auth             same as --smtp-auth=none
    --smtp-debug <0|1>         show the SMTP dialogue
    --imap-sent-folder <dir>   also store a copy with imap-send in this folder
    --[no-]use-imap-only       only store the copy, do not send
    --batch-size <n>           messages per SMTP connection
    --relogin-delay <secs>     pause before reconnecting (with --batch-size)

Automating:
    --identity <name>          use the sendemail.<name>.* settings
    --no-identity              ignore sendemail.identity
    --to-cmd <command>         command printing To: addresses for a patch
    --cc-cmd <command>         command printing Cc: addresses for a patch
    --header-cmd <command>     command printing extra headers for a message
    --no-header-cmd            do not run a header command
    --suppress-cc <category>   author, self, cc, bodycc, sob, misc-by, cccmd, body, all
    --[no-]cc-cover            copy the cover letter's Cc: to every message
    --[no-]to-cover            copy the cover letter's To: to every message
    --[no-]signed-off-by-cc    copy Signed-off-by: addresses (default on)
    --[no-]suppress-from       do not copy the sender
    --[no-]chain-reply-to      reply to the previous message instead of the first
    --[no-]thread              add In-Reply-To: and References: (default on)

Administering:
    --confirm <mode>           always, never, cc, compose or auto
    --quiet                    one line per message
    --dry-run                  do everything except sending
    --[no-]validate            run sanity checks (default on)
    --[no-]format-patch        read ambiguous arguments as format-patch arguments
    --force                    send even if safety checks fail

Information:
    --dump-aliases             list the alias names and exit
    --translate-aliases        expand aliases read from standard input and exit
";

pub const NO_PATCH_FILES: &str = "No patch files specified!";

pub const DUMP_ALIASES_ALONE: &str = "--dump-aliases incompatible with other options";
pub const TRANSLATE_ALIASES_ALONE: &str = "--translate-aliases incompatible with other options";

pub fn sendmail_variables() -> String {
    "fatal: found configuration options for 'sendmail'\n\
     git-send-email is configured with the sendemail.* options - note the 'e'.\n\
     Set sendemail.forbidSendmailVariables to false to disable this check."
        .to_string()
}

pub fn needs_argument(opt: &str) -> String {
    format!("Option {opt} requires an argument")
}

pub fn opening(path: &str, err: &str) -> String {
    format!("opening {path}: {err}")
}

pub fn alias_quotes(line: &str) -> String {
    format!("warning: sendmail alias with quotes is not supported: {line}")
}

pub fn alias_redirection(line: &str) -> String {
    format!("warning: `/file` or `|pipe` redirection not supported: {line}")
}

pub fn alias_include(line: &str) -> String {
    format!("warning: `:include:` not supported: {line}")
}

pub fn alias_unknown(line: &str) -> String {
    format!("warning: sendmail line is not recognized: {line}")
}

pub fn ambiguous_file(name: &str) -> String {
    format!(
        "File '{name}' exists but it could also be the range of commits\n\
         to produce patches for.  Please disambiguate by...\n\
         \n    * Saying \"./{name}\" if you mean a file; or\n    \
         * Giving --format-patch option if you mean a range."
    )
}

pub fn format_patch_failed(cmdline: &str, code: i32) -> String {
    format!("{cmdline}: command returned error: {code}")
}

pub fn no_subject(file: &str) -> String {
    format!("No 'Subject:' line in '{file}'")
}

pub fn too_long(file: &str, line: usize) -> String {
    format!("fatal: {file}:{line} is longer than 998 characters")
}

pub const NO_PATCHES_SENT: &str = "warning: no patches were sent";

pub fn hook_rejected(file: &str) -> String {
    format!("fatal: {file}: rejected by sendemail-validate hook")
}

pub fn hook_died(code: i32) -> String {
    format!(
        "fatal: command 'git hook run --ignore-missing sendemail-validate -- <patch> <header>' died with exit code {code}"
    )
}

pub fn cover_template(file: &str) -> String {
    format!(
        "Refusing to send because the patch\n\t{file}\n\
         has the template subject '*** SUBJECT HERE ***'. Pass --force if you really want to send."
    )
}

pub const COVER_TEMPLATE_SUBJECT: &str = "*** SUBJECT HERE ***";

pub const EIGHTBIT_FILES: &str =
    "The following files are 8bit, but do not declare a Content-Transfer-Encoding.";

pub const ASK_8BIT: &str = "Declare which 8bit encoding to use [default: UTF-8]? ";

pub fn ask_charset_anyway(cs: &str) -> String {
    format!("'{cs}' does not appear to be a valid charset name. Use it anyway [y/N]? ")
}

pub const ASK_TO: &str = "To whom should the emails be sent (if anyone)? ";
pub const ASK_IN_REPLY_TO: &str =
    "Message-ID to be used as In-Reply-To for the first email (if any)? ";
pub const ASK_SEND: &str = "Send this email? ([y]es|[n]o|[e]dit|[q]uit|[a]ll): ";
pub const SEND_REPLY_REQUIRED: &str = "Send this email reply required";

/// Shown before the first confirmation when `sendemail.confirm` is not
/// configured and the Cc list grew from the patch.
pub const CC_EXPANDED_NOTE: &str = "    The Cc list above has been expanded by additional
    addresses found in the patch commit message. By default
    send-email prompts before sending whenever this occurs.
    This behavior is controlled by the sendemail.confirm
    configuration setting.

    For additional information, run 'git send-email --help'.
    To retain the current behavior, but squelch this message,
    run 'git config --global sendemail.confirm auto'.
";

pub const EDITOR_FAILED: &str = "the editor exited uncleanly, aborting everything";

pub const CANNOT_7BIT: &str = "cannot send message as 7bit";

pub fn bad_transfer_encoding(enc: &str) -> String {
    format!("fatal: Unknown --transfer-encoding value: {enc}")
}

pub fn bad_confirm(mode: &str) -> String {
    format!("fatal: Unknown --confirm setting: '{mode}'")
}

pub fn bad_suppress(cat: &str) -> String {
    format!("fatal: Unknown --suppress-cc field: '{cat}'")
}

pub fn malformed_cmd_output(prefix: &str, cmd: &str) -> String {
    format!("({prefix}) Malformed output from '{cmd}'")
}

pub fn cmd_failed(prefix: &str, cmd: &str) -> String {
    format!("({prefix}) failed to close pipe to '{cmd}'")
}

pub fn adding(source: &str, kind: &str, addr: &str, line: &str) -> String {
    format!("({source}) Adding {kind}: {addr} from line '{line}'")
}

pub fn adding_from_cmd(source: &str, kind: &str, addr: &str, cmd: &str) -> String {
    format!("({source}) Adding {kind}: {addr} from: '{cmd}'")
}

pub fn ignoring(source: &str, what: &str, line: &str) -> String {
    format!("({source}) Ignoring {what} from line '{line}'")
}

pub fn smtp_init_failed(server: &str, enc: &str, hello: &str, port: u16) -> String {
    format!(
        "Unable to initialize SMTP properly. Check config and use --smtp-debug. \
         VALUES: server={server} encryption={enc} hello={hello} port={port}"
    )
}

pub fn smtp_failed(subject: &str, reply: &str) -> String {
    format!("Failed to send {subject}\n{reply}")
}

pub fn smtp_auth_failed(reply: &str) -> String {
    format!("fatal: SMTP authentication failed: {reply}")
}

pub fn sendmail_failed(cmd: &str, status: &str) -> String {
    format!("fatal: '{cmd}' failed: {status}")
}

pub fn bad_port(port: &str) -> String {
    format!("fatal: invalid SMTP server port: {port}")
}

pub fn imap_failed(folder: &str) -> String {
    format!("warning: could not store the message in IMAP folder '{folder}'")
}

pub const NEED_IMAP_FOLDER: &str =
    "fatal: --use-imap-only requires --imap-sent-folder or sendemail.imapSentFolder";

pub const RELOGIN_WITHOUT_BATCH: &str =
    "fatal: --relogin-delay can only be used together with --batch-size";

/// The separator line before a message in an mbox (for `imap-send`).
pub fn mbox_from_line(id: &str) -> String {
    format!("From {id} Mon Sep 17 00:00:00 2001\n")
}
