// SPDX-License-Identifier: MIT
//! Trailer configuration: `trailer.separators`, `trailer.where`,
//! `trailer.ifexists`, `trailer.ifmissing` and the per-alias
//! `trailer.<alias>.key|where|ifexists|ifmissing|cmd|command`
//! (Documentation/config/trailer.adoc).

use bstr::{BString, ByteSlice};
use rgit_config::{ConfigSet, Entry};

use crate::messages;

/// Where a new trailer goes (`trailer.where`, `--where`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Where {
    End,
    After,
    Before,
    Start,
}

impl Where {
    /// Parse a placement name (case does not matter).
    pub fn parse(s: &[u8]) -> Option<Where> {
        Some(match s.to_ascii_lowercase().as_slice() {
            b"end" => Where::End,
            b"after" => Where::After,
            b"before" => Where::Before,
            b"start" => Where::Start,
            _ => return None,
        })
    }

    /// Whether the new trailer is placed relative to the last trailer with
    /// the same key (`after`, `end`) rather than the first.
    pub(crate) fn counts_from_end(self) -> bool {
        matches!(self, Where::After | Where::End)
    }
}

/// What happens when a trailer with the same key exists
/// (`trailer.ifexists`, `--if-exists`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IfExists {
    AddIfDifferentNeighbor,
    AddIfDifferent,
    Add,
    Replace,
    DoNothing,
}

impl IfExists {
    /// Parse an action name (case does not matter).
    pub fn parse(s: &[u8]) -> Option<IfExists> {
        Some(match s.to_ascii_lowercase().as_slice() {
            b"addifdifferentneighbor" => IfExists::AddIfDifferentNeighbor,
            b"addifdifferent" => IfExists::AddIfDifferent,
            b"add" => IfExists::Add,
            b"replace" => IfExists::Replace,
            b"donothing" => IfExists::DoNothing,
            _ => return None,
        })
    }
}

/// What happens when no trailer with the same key exists
/// (`trailer.ifmissing`, `--if-missing`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IfMissing {
    Add,
    DoNothing,
}

impl IfMissing {
    /// Parse an action name (case does not matter).
    pub fn parse(s: &[u8]) -> Option<IfMissing> {
        Some(match s.to_ascii_lowercase().as_slice() {
            b"add" => IfMissing::Add,
            b"donothing" => IfMissing::DoNothing,
            _ => return None,
        })
    }
}

/// One `trailer.<alias>.*` group.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Item {
    /// The alias (`<alias>` in `trailer.<alias>.key`), as written.
    pub name: BString,
    /// `trailer.<alias>.key`.
    pub key: Option<BString>,
    pub where_: Option<Where>,
    pub if_exists: Option<IfExists>,
    pub if_missing: Option<IfMissing>,
    /// `trailer.<alias>.command` (deprecated; `$ARG` is replaced).
    pub command: Option<BString>,
    /// `trailer.<alias>.cmd` (the value is passed as `$1`).
    pub cmd: Option<BString>,
}

impl Item {
    fn new(name: &[u8]) -> Self {
        Item {
            name: name.into(),
            ..Default::default()
        }
    }

    /// The key: `trailer.<alias>.key`, or the alias itself.
    pub fn key_or_name(&self) -> &[u8] {
        self.key.as_deref().map_or(&self.name[..], |k| &k[..])
    }

    /// Whether `token` (from the command line or the input) refers to this
    /// item: it is a prefix of the key or of the alias, ignoring case.
    pub fn matches(&self, token: &[u8]) -> bool {
        prefix_ci(token, self.key_or_name()) || prefix_ci(token, &self.name)
    }

    /// Whether the item has a command (`cmd` wins over `command`).
    pub fn has_command(&self) -> bool {
        self.cmd.is_some() || self.command.is_some()
    }
}

/// `a` is a prefix of `b`, ignoring ASCII case.
pub(crate) fn prefix_ci(a: &[u8], b: &[u8]) -> bool {
    a.len() <= b.len() && b[..a.len()].eq_ignore_ascii_case(a)
}

/// All trailer settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// `trailer.separators` (default `:`). The first one is used when a
    /// trailer is written.
    pub separators: BString,
    pub where_: Where,
    pub if_exists: IfExists,
    pub if_missing: IfMissing,
    /// The `trailer.<alias>.*` groups in order of first appearance.
    pub items: Vec<Item>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            separators: ":".into(),
            where_: Where::End,
            if_exists: IfExists::AddIfDifferentNeighbor,
            if_missing: IfMissing::Add,
            items: Vec::new(),
        }
    }
}

impl Config {
    /// Read the `trailer.*` variables. Unknown values give warnings (in
    /// reading order); a variable that needs a value but has none is
    /// returned as the error (the caller reports it like git:
    /// `missing value for '<name>'`).
    pub fn from_config(set: &ConfigSet) -> Result<(Config, Vec<String>), Entry> {
        let mut conf = Config::default();
        let mut warnings = Vec::new();
        for e in set.entries() {
            let Some(rest) = e.name.strip_prefix(b"trailer.") else {
                continue;
            };
            let value = e.value.as_deref().map(|v| &v[..]);
            match rest.rfind_byte(b'.') {
                None => conf.global(rest, value, e, &mut warnings)?,
                Some(dot) => conf.item(&rest[..dot], &rest[dot + 1..], value, e, &mut warnings)?,
            }
        }
        Ok((conf, warnings))
    }

    fn global(
        &mut self,
        var: &[u8],
        value: Option<&[u8]>,
        e: &Entry,
        warnings: &mut Vec<String>,
    ) -> Result<(), Entry> {
        match var {
            b"separators" => self.separators = value.ok_or_else(|| e.clone())?.into(),
            b"where" => match value {
                // A value-less `trailer.where` behaves like `start`.
                None => self.where_ = Where::Start,
                Some(v) => match Where::parse(v) {
                    Some(w) => self.where_ = w,
                    None => warnings.push(unknown(v, e)),
                },
            },
            b"ifexists" => {
                if let Some(v) = value {
                    match IfExists::parse(v) {
                        Some(a) => self.if_exists = a,
                        None => warnings.push(unknown(v, e)),
                    }
                }
            }
            b"ifmissing" => {
                if let Some(v) = value {
                    match IfMissing::parse(v) {
                        Some(a) => self.if_missing = a,
                        None => warnings.push(unknown(v, e)),
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn item(
        &mut self,
        name: &[u8],
        var: &[u8],
        value: Option<&[u8]>,
        e: &Entry,
        warnings: &mut Vec<String>,
    ) -> Result<(), Entry> {
        if !matches!(
            var,
            b"key" | b"command" | b"cmd" | b"where" | b"ifexists" | b"ifmissing"
        ) {
            return Ok(());
        }
        let idx = match self.items.iter().position(|i| i.name == name) {
            Some(i) => i,
            None => {
                self.items.push(Item::new(name));
                self.items.len() - 1
            }
        };
        let item = &mut self.items[idx];
        let need = || value.map(BString::from).ok_or_else(|| e.clone());
        match var {
            b"key" => item.key = Some(need()?),
            b"command" => item.command = Some(need()?),
            b"cmd" => item.cmd = Some(need()?),
            b"where" => match value {
                None => item.where_ = Some(Where::Start),
                Some(v) => match Where::parse(v) {
                    Some(w) => item.where_ = Some(w),
                    None => warnings.push(unknown(v, e)),
                },
            },
            b"ifexists" => {
                if let Some(v) = value {
                    match IfExists::parse(v) {
                        Some(a) => item.if_exists = Some(a),
                        None => warnings.push(unknown(v, e)),
                    }
                }
            }
            b"ifmissing" => {
                if let Some(v) = value {
                    match IfMissing::parse(v) {
                        Some(a) => item.if_missing = Some(a),
                        None => warnings.push(unknown(v, e)),
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// The separator written between key and value.
    pub fn default_separator(&self) -> u8 {
        self.separators.first().copied().unwrap_or(0)
    }

    /// The first item `token` refers to.
    pub fn find_item(&self, token: &[u8]) -> Option<usize> {
        self.items.iter().position(|i| i.matches(token))
    }
}

fn unknown(value: &[u8], e: &Entry) -> String {
    messages::unknown_value(&value.to_str_lossy(), &e.name.to_str_lossy())
}
