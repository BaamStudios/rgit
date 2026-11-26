// SPDX-License-Identifier: MIT
//! The `receive.*` configuration (Documentation/config/receive.adoc) and
//! the `transfer.*` settings it falls back to.

use rgit_config::value::{parse_bool, parse_int};
use rgit_config::{ConfigSet, IntWidth};

/// `receive.denyCurrentBranch` / `receive.denyDeleteCurrent`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Deny {
    /// Not configured (behaves like `Refuse`, with an explanation).
    Unconfigured,
    Ignore,
    Warn,
    Refuse,
    /// `updateInstead` (only for `denyCurrentBranch`).
    UpdateInstead,
}

impl Deny {
    fn parse(value: Option<&str>) -> Deny {
        match value {
            None => Deny::Refuse,
            Some(v) if v.eq_ignore_ascii_case("ignore") => Deny::Ignore,
            Some(v) if v.eq_ignore_ascii_case("warn") => Deny::Warn,
            Some(v) if v.eq_ignore_ascii_case("refuse") => Deny::Refuse,
            Some(v) if v.eq_ignore_ascii_case("updateinstead") => Deny::UpdateInstead,
            Some(v) => match parse_bool(Some(v.as_bytes())) {
                Some(true) => Deny::Refuse,
                Some(false) => Deny::Ignore,
                None => Deny::Ignore,
            },
        }
    }
}

/// One `receive.procReceiveRefs` entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcReceiveRef {
    /// The prefix the reference name starts with.
    pub prefix: String,
    /// Matches commands that do not match the prefix.
    pub negated: bool,
    /// Actions it applies to (`a`, `m`, `d`); all when empty.
    pub create: bool,
    pub modify: bool,
    pub delete: bool,
}

/// The settings `receive-pack` reads.
#[derive(Clone, Debug)]
pub struct Settings {
    pub deny_current_branch: Deny,
    pub deny_delete_current: Deny,
    pub deny_deletes: bool,
    pub deny_non_fast_forwards: bool,
    pub fsck_objects: bool,
    /// `receive.fsck.<msg-id>` as
    /// `<msg-id>=<severity>`, `skiplist=<path>` for the skip list.
    pub fsck_msgs: Vec<(String, String)>,
    pub unpack_limit: u64,
    pub max_input_size: u64,
    pub advertise_atomic: bool,
    pub advertise_push_options: bool,
    pub advertise_sid: bool,
    pub auto_gc: bool,
    pub update_server_info: bool,
    pub shallow_update: bool,
    pub keep_alive: i64,
    /// `transfer.hideRefs` and `receive.hideRefs`, in configuration order.
    pub hide_refs: Vec<String>,
    pub proc_receive_refs: Vec<ProcReceiveRef>,
    pub cert_nonce_seed: Option<String>,
    pub cert_nonce_slop: i64,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            deny_current_branch: Deny::Unconfigured,
            deny_delete_current: Deny::Unconfigured,
            deny_deletes: false,
            deny_non_fast_forwards: false,
            fsck_objects: false,
            fsck_msgs: Vec::new(),
            unpack_limit: 100,
            max_input_size: 0,
            advertise_atomic: true,
            advertise_push_options: false,
            advertise_sid: false,
            auto_gc: true,
            update_server_info: false,
            shallow_update: false,
            keep_alive: 5,
            hide_refs: Vec::new(),
            proc_receive_refs: Vec::new(),
            cert_nonce_seed: None,
            cert_nonce_slop: 0,
        }
    }
}

fn value_str(e: &rgit_config::Entry) -> Option<String> {
    e.value.as_ref().map(|v| v.to_string())
}

fn bool_of(e: &rgit_config::Entry) -> bool {
    parse_bool(e.value.as_ref().map(|v| v.as_slice())).unwrap_or(false)
}

fn int_of(e: &rgit_config::Entry) -> Option<i64> {
    e.value
        .as_ref()
        .and_then(|v| parse_int(v, IntWidth::I64).ok())
}

impl Settings {
    /// Reads the settings from the configuration (last value wins).
    pub fn from_config(config: &ConfigSet) -> Settings {
        let mut s = Settings::default();
        let mut receive_fsck: Option<bool> = None;
        let mut transfer_fsck: Option<bool> = None;
        let mut receive_unpack: Option<u64> = None;
        let mut transfer_unpack: Option<u64> = None;
        for e in config.entries() {
            let name = e.name.to_string();
            match name.as_str() {
                "receive.denycurrentbranch" => {
                    s.deny_current_branch = match &e.value {
                        None => Deny::Refuse,
                        Some(v) => Deny::parse(Some(&v.to_string())),
                    }
                }
                "receive.denydeletecurrent" => {
                    s.deny_delete_current = match &e.value {
                        None => Deny::Refuse,
                        Some(v) => Deny::parse(Some(&v.to_string())),
                    }
                }
                "receive.denydeletes" => s.deny_deletes = bool_of(e),
                "receive.denynonfastforwards" => s.deny_non_fast_forwards = bool_of(e),
                "receive.fsckobjects" => receive_fsck = Some(bool_of(e)),
                "transfer.fsckobjects" => transfer_fsck = Some(bool_of(e)),
                "receive.unpacklimit" => receive_unpack = int_of(e).map(|v| v.max(0) as u64),
                "transfer.unpacklimit" => transfer_unpack = int_of(e).map(|v| v.max(0) as u64),
                "receive.maxinputsize" => {
                    s.max_input_size = int_of(e).map(|v| v.max(0) as u64).unwrap_or(0)
                }
                "receive.advertiseatomic" => s.advertise_atomic = bool_of(e),
                "receive.advertisepushoptions" => s.advertise_push_options = bool_of(e),
                "transfer.advertisesid" => s.advertise_sid = bool_of(e),
                "receive.autogc" => s.auto_gc = bool_of(e),
                "receive.updateserverinfo" => s.update_server_info = bool_of(e),
                "receive.shallowupdate" => s.shallow_update = bool_of(e),
                "receive.keepalive" => s.keep_alive = int_of(e).unwrap_or(5),
                "transfer.hiderefs" | "receive.hiderefs" => {
                    if let Some(v) = value_str(e) {
                        s.hide_refs.push(v);
                    }
                }
                "receive.procreceiverefs" => {
                    if let Some(v) = value_str(e) {
                        if let Some(r) = parse_proc_receive_ref(&v) {
                            s.proc_receive_refs.push(r);
                        }
                    }
                }
                "receive.certnonceseed" => s.cert_nonce_seed = value_str(e),
                "receive.certnonceslop" => s.cert_nonce_slop = int_of(e).unwrap_or(0),
                _ => {
                    // Only `receive.fsck.*`: there is no fallback to
                    // `fsck.*`.
                    if let Some(id) = name.strip_prefix("receive.fsck.") {
                        if let Some(v) = value_str(e) {
                            s.fsck_msgs.push((id.to_string(), v));
                        }
                    }
                }
            }
        }
        s.fsck_objects = receive_fsck.or(transfer_fsck).unwrap_or(false);
        s.unpack_limit = receive_unpack.or(transfer_unpack).unwrap_or(100);
        s
    }
}

/// `[<modifiers>:]<prefix>`: `a` create, `m` modify, `d` delete, `!`
/// negate.
fn parse_proc_receive_ref(v: &str) -> Option<ProcReceiveRef> {
    let (mods, prefix) = match v.split_once(':') {
        Some((m, p)) => (m, p),
        None => ("", v),
    };
    let mut r = ProcReceiveRef {
        prefix: prefix.trim_end_matches('/').to_string(),
        negated: false,
        create: false,
        modify: false,
        delete: false,
    };
    for c in mods.chars() {
        match c {
            'a' => r.create = true,
            'm' => r.modify = true,
            'd' => r.delete = true,
            '!' => r.negated = true,
            _ => {}
        }
    }
    if !(r.create || r.modify || r.delete) {
        r.create = true;
        r.modify = true;
        r.delete = true;
    }
    if r.prefix.is_empty() {
        return None;
    }
    Some(r)
}

impl ProcReceiveRef {
    /// Whether a command on `name` (old/new null as given) is handled by
    /// `proc-receive` according to this entry.
    pub fn matches(&self, name: &str, old_null: bool, new_null: bool) -> bool {
        let action = if old_null {
            self.create
        } else if new_null {
            self.delete
        } else {
            self.modify
        };
        if !action {
            return false;
        }
        let hit = name
            .strip_prefix(&self.prefix)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'));
        hit != self.negated
    }
}

/// Whether `transfer.hideRefs` / `receive.hideRefs` hide `name` (the name
/// with the namespace removed is `stripped`): the last matching pattern
/// decides; `!` negates, `^` matches the full name.
pub fn is_hidden(patterns: &[String], name: &str, stripped: &str) -> bool {
    for p in patterns.iter().rev() {
        let (neg, p) = match p.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (false, p.as_str()),
        };
        let (subject, p) = match p.strip_prefix('^') {
            Some(rest) => (name, rest),
            None => (stripped, p),
        };
        let p = p.trim_end_matches('/');
        let hit = subject
            .strip_prefix(p)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'));
        if hit {
            return !neg;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_last_match_wins() {
        let p = vec!["refs/hidden".to_string(), "!refs/hidden/ok".to_string()];
        assert!(is_hidden(&p, "refs/hidden/x", "refs/hidden/x"));
        assert!(!is_hidden(&p, "refs/hidden/ok", "refs/hidden/ok"));
        assert!(!is_hidden(&p, "refs/hiddenx", "refs/hiddenx"));
        let full = vec!["^refs/namespaces/a/refs/heads".to_string()];
        assert!(is_hidden(
            &full,
            "refs/namespaces/a/refs/heads/m",
            "refs/heads/m"
        ));
    }

    #[test]
    fn proc_receive_refs() {
        let r = parse_proc_receive_ref("refs/for").unwrap();
        assert!(r.matches("refs/for/main/topic", true, false));
        assert!(!r.matches("refs/heads/main", true, false));
        let a = parse_proc_receive_ref("a:refs/heads").unwrap();
        assert!(a.matches("refs/heads/x", true, false));
        assert!(!a.matches("refs/heads/x", false, false));
        let n = parse_proc_receive_ref("!:refs/heads").unwrap();
        assert!(n.matches("refs/tags/x", false, false));
        assert!(!n.matches("refs/heads/x", false, false));
    }
}
