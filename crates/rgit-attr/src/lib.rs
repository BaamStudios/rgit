// SPDX-License-Identifier: MIT

//! Git attributes (gitattributes(5)) for rgit.
//!
//! [`Attributes`] answers which attributes a path has. It reads, in this
//! order (which is also the order in which attribute names are first seen,
//! the order of `git check-attr --all`):
//!
//! 1. the system file (`$(prefix)/etc/gitattributes`),
//! 2. the global file (`core.attributesFile`, by default
//!    `$XDG_CONFIG_HOME/git/attributes`),
//! 3. the top-level `.gitattributes`,
//! 4. `$GIT_DIR/info/attributes`,
//!
//! and the `.gitattributes` of deeper directories when a path below them
//! is looked up. Per-directory files come from a [`Source`]: the work tree
//! (falling back to the index for files missing there, or the other way
//! round during checkout), only the index (`--cached`), a tree
//! (`--source`, `--attr-source`, `GIT_ATTR_SOURCE`, `attr.tree`) or none
//! (bare repositories).
//!
//! Precedence, highest first: `info/attributes`, the `.gitattributes`
//! files from the directory of the path up to the top, the global file,
//! the system file. Within a file later lines win, within a line later
//! attributes win, and every attribute is decided by the first assignment
//! found in this order.
//!
//! Macros (`[attr]name attrs...`) may be defined in the four top-level
//! files only; the definition read last wins, and the built-in `binary`
//! is `-diff -merge -text`. When an assignment sets a macro, the macro's
//! attributes are applied right there, with the same precedence. Unset,
//! valued or reset macros expand to nothing.
//!
//! ```
//! use rgit_attr::{Attributes, Options, Source, State};
//!
//! let mut attrs = Attributes::new(&Options::default(), Source::None);
//! attrs.add_lines(b"*.png binary\n*.c diff=cpp\n", "attributes");
//! let text = attrs.id(b"text");
//! let diff = attrs.id(b"diff");
//! let out = attrs.lookup(b"img/a.png", false);
//! assert_eq!(out.get(text), &State::Unset);
//! assert_eq!(attrs.lookup(b"a.c", false).get(diff), &State::Value(b"cpp".to_vec()));
//! ```
//!
//! Behaviour follows Documentation/gitattributes.adoc,
//! Documentation/git-check-attr.adoc and the `git` binary (see the
//! differential tests in `tests/` and of `check-attr` in `rgit-cli`).
//! Consumers of particular attributes (line endings #111, filters #112,
//! diff and merge drivers) build on [`Attributes::lookup`].

pub mod messages;
mod os;
mod parse;
mod source;

use std::collections::HashMap;
use std::path::PathBuf;

use rgit_pathspec::{AttrRequirement, AttrSpec};

pub use parse::{Pattern, State, is_reserved_name, is_valid_name};
pub use source::{ObjectReader, Order, Source};

use parse::Line;

/// Attributes files larger than this are ignored with a warning.
pub const MAX_FILE_SIZE: u64 = 100 * 1024 * 1024;

/// Lines of this length or longer are ignored with a warning.
pub const MAX_LINE_LEN: usize = 2048;

/// The attribute git computes from the file mode instead of reading it
/// from files (`git check-attr builtin_objectmode`).
pub const BUILTIN_OBJECTMODE: &[u8] = b"builtin_objectmode";

/// The files outside the work tree, each as (path to read, name shown in
/// messages).
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// The system file, unless `GIT_ATTR_NOSYSTEM` is true.
    pub system: Option<(PathBuf, String)>,
    /// The global file.
    pub global: Option<(PathBuf, String)>,
    /// `$GIT_COMMON_DIR/info/attributes`.
    pub info: Option<(PathBuf, String)>,
    /// Compare patterns case-insensitively (`core.ignoreCase`).
    pub ignore_case: bool,
}

/// An interned attribute name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AttrId(u32);

impl AttrId {
    fn index(self) -> usize {
        self.0 as usize
    }
}

type Assignments = Vec<(AttrId, State)>;

/// One rule of a file: pattern and assignments.
#[derive(Debug, Clone)]
struct Rule {
    pattern: Pattern,
    assignments: Assignments,
}

/// The rules of one file and the directory they are relative to.
#[derive(Debug, Clone)]
struct File {
    /// Empty or ending in `/`.
    base: Vec<u8>,
    rules: Vec<Rule>,
}

/// The attributes of the paths of one repository.
#[derive(Debug)]
pub struct Attributes<'a> {
    names: Vec<Vec<u8>>,
    ids: HashMap<Vec<u8>, AttrId>,
    macros: HashMap<AttrId, Assignments>,
    /// System and global file, lowest precedence first.
    outer: Vec<File>,
    info: Option<File>,
    /// Per-directory files by directory (empty or ending in `/`).
    per_dir: HashMap<Vec<u8>, Option<File>>,
    source: Source<'a>,
    icase: bool,
    warnings: Vec<String>,
}

impl<'a> Attributes<'a> {
    /// Reads the system, global, top-level and `info/attributes` files.
    /// Messages go to [`Attributes::take_warnings`].
    #[must_use]
    pub fn new(opts: &Options, source: Source<'a>) -> Attributes<'a> {
        let mut a = Attributes {
            names: Vec::new(),
            ids: HashMap::new(),
            macros: HashMap::new(),
            outer: Vec::new(),
            info: None,
            per_dir: HashMap::new(),
            source,
            icase: opts.ignore_case,
            warnings: Vec::new(),
        };
        let binary = a.id(b"binary");
        let expansion = [b"diff".as_slice(), b"merge", b"text"]
            .into_iter()
            .map(|n| (a.id(n), State::Unset))
            .collect();
        a.macros.insert(binary, expansion);
        for (path, shown) in [&opts.system, &opts.global].into_iter().flatten() {
            if let Some(data) = source::read_file(path, shown, &mut a.warnings) {
                let f = a.parse(&data, shown, Vec::new(), true);
                a.outer.push(f);
            }
        }
        a.load_dir(b"");
        if let Some((path, shown)) = &opts.info {
            if let Some(data) = source::read_file(path, shown, &mut a.warnings) {
                a.info = Some(a.parse(&data, shown, Vec::new(), true));
            }
        }
        a
    }

    /// Adds the lines of an attributes file with the precedence of
    /// `info/attributes` (above everything read so far). Macros may be
    /// defined. Meant for tests and callers with their own files.
    pub fn add_lines(&mut self, data: &[u8], shown: &str) {
        let f = self.parse(data, shown, Vec::new(), true);
        if let Some(old) = self.info.take() {
            self.outer.push(old);
        }
        self.info = Some(f);
    }

    /// The id of the attribute `name`, registering it if it is new.
    pub fn id(&mut self, name: &[u8]) -> AttrId {
        if let Some(&id) = self.ids.get(name) {
            return id;
        }
        let id = AttrId(u32::try_from(self.names.len()).expect("fewer than 2^32 attributes"));
        self.names.push(name.to_vec());
        self.ids.insert(name.to_vec(), id);
        id
    }

    /// The name of `id`.
    #[must_use]
    pub fn name(&self, id: AttrId) -> &[u8] {
        &self.names[id.index()]
    }

    /// Messages collected while reading files (whole lines as git prints
    /// them to standard error), emptied by this call.
    pub fn take_warnings(&mut self) -> Vec<String> {
        std::mem::take(&mut self.warnings)
    }

    fn parse(&mut self, data: &[u8], shown: &str, base: Vec<u8>, top: bool) -> File {
        let mut rules = Vec::new();
        for (n, line) in parse::lines(data) {
            if line.len() >= MAX_LINE_LEN {
                self.warnings.push(messages::line_too_long(n));
                continue;
            }
            match parse::parse_line(line, n, shown, top, &mut self.warnings) {
                Some(Line::Rule(pattern, attrs)) => {
                    let assignments = self.intern(attrs);
                    rules.push(Rule {
                        pattern,
                        assignments,
                    });
                }
                Some(Line::Macro(name, attrs)) => {
                    let id = self.id(&name);
                    let assignments = self.intern(attrs);
                    self.macros.insert(id, assignments);
                }
                None => {}
            }
        }
        File { base, rules }
    }

    fn intern(&mut self, attrs: Vec<(Vec<u8>, State)>) -> Assignments {
        attrs.into_iter().map(|(n, s)| (self.id(&n), s)).collect()
    }

    /// Reads the `.gitattributes` of `dir` (empty or ending in `/`) once.
    fn load_dir(&mut self, dir: &[u8]) {
        if self.per_dir.contains_key(dir) {
            return;
        }
        let mut rel = dir.to_vec();
        rel.extend_from_slice(b".gitattributes");
        let file = self.source.read(&rel, &mut self.warnings).map(|data| {
            let shown = String::from_utf8_lossy(&rel).into_owned();
            self.parse(&data, &shown, dir.to_vec(), dir.is_empty())
        });
        self.per_dir.insert(dir.to_vec(), file);
    }

    /// The attributes of `path` (relative to the top of the work tree,
    /// `/`-separated, without trailing `/`). `is_dir`: the path names a
    /// directory (patterns ending in `/` match only then).
    pub fn lookup(&mut self, path: &[u8], is_dir: bool) -> Outcome {
        let mut dirs = vec![0];
        for (i, &b) in path.iter().enumerate() {
            if b == b'/' {
                self.load_dir(&path[..=i]);
                dirs.push(i + 1);
            }
        }
        let mut out = Outcome {
            states: vec![None; self.names.len()],
        };
        let mut files: Vec<&File> = Vec::new();
        files.extend(self.info.as_ref());
        for &end in dirs.iter().rev() {
            if let Some(Some(f)) = self.per_dir.get(&path[..end]) {
                files.push(f);
            }
        }
        files.extend(self.outer.iter().rev());
        for f in files {
            let Some(rel) = path.strip_prefix(f.base.as_slice()) else {
                continue;
            };
            for rule in f.rules.iter().rev() {
                if rule.pattern.matches(rel, is_dir, self.icase) {
                    apply(&mut out.states, &self.macros, &rule.assignments);
                }
            }
        }
        out
    }

    /// Whether `path` meets all requirements of a pathspec's `attr:` magic.
    pub fn satisfies(&mut self, path: &[u8], is_dir: bool, spec: &AttrSpec) -> bool {
        let ids: Vec<AttrId> = spec
            .requirements
            .iter()
            .map(|r| self.id(r.name()))
            .collect();
        let out = self.lookup(path, is_dir);
        spec.requirements
            .iter()
            .zip(ids)
            .all(|(r, id)| requirement_met(r, out.get(id)))
    }
}

/// Whether `state` meets one requirement of a pathspec's `attr:` magic.
#[must_use]
pub fn requirement_met(req: &AttrRequirement, state: &State) -> bool {
    match req {
        AttrRequirement::Set(_) => state.is_set(),
        AttrRequirement::Unset(_) => state.is_unset(),
        AttrRequirement::Unspecified(_) => state.is_unspecified(),
        AttrRequirement::Value(_, v) => state.value() == Some(v.as_slice()),
    }
}

/// Applies `assignments` (last first) to the undecided attributes,
/// expanding macros that get set. Iterative, so deep macro chains need no
/// deep stack.
fn apply(
    states: &mut [Option<State>],
    macros: &HashMap<AttrId, Assignments>,
    assignments: &[(AttrId, State)],
) {
    let mut stack = vec![assignments.iter().rev()];
    while let Some(top) = stack.last_mut() {
        let Some((id, state)) = top.next() else {
            stack.pop();
            continue;
        };
        let slot = &mut states[id.index()];
        if slot.is_some() {
            continue;
        }
        *slot = Some(state.clone());
        if state.is_set() {
            if let Some(expansion) = macros.get(id) {
                stack.push(expansion.iter().rev());
            }
        }
    }
}

/// The attributes of one path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    states: Vec<Option<State>>,
}

static UNSPECIFIED: State = State::Unspecified;

impl Outcome {
    /// The state of `id`.
    #[must_use]
    pub fn get(&self, id: AttrId) -> &State {
        self.states
            .get(id.index())
            .and_then(Option::as_ref)
            .unwrap_or(&UNSPECIFIED)
    }

    /// The attributes that are not unspecified, in the order their names
    /// were first seen (`git check-attr --all`).
    pub fn specified(&self) -> impl Iterator<Item = (AttrId, &State)> {
        self.states.iter().enumerate().filter_map(|(i, s)| match s {
            Some(s) if !s.is_unspecified() => Some((AttrId(u32::try_from(i).expect("id fits")), s)),
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(attrs: &mut Attributes<'_>, path: &str) -> String {
        let out = attrs.lookup(path.as_bytes(), false);
        let mut s = String::new();
        for (id, st) in out.specified() {
            s.push_str(&format!(
                "{}={} ",
                String::from_utf8_lossy(attrs.name(id)),
                String::from_utf8_lossy(st.display())
            ));
        }
        s.trim_end().to_string()
    }

    fn with(lines: &str) -> Attributes<'static> {
        let mut a = Attributes::new(&Options::default(), Source::None);
        a.add_lines(lines.as_bytes(), "f");
        a
    }

    #[test]
    fn later_wins() {
        let mut a = with("x a b=1\nx -a\n");
        assert_eq!(all(&mut a, "x"), "a=unset b=1");
        let mut a = with("x a -a\n");
        assert_eq!(all(&mut a, "x"), "a=unset");
    }

    #[test]
    fn macros() {
        let mut a = with("file binary text\n");
        assert_eq!(
            all(&mut a, "file"),
            "binary=set diff=unset merge=unset text=set"
        );
        let mut a = with("file text binary\n");
        assert_eq!(
            all(&mut a, "file"),
            "binary=set diff=unset merge=unset text=unset"
        );
        let mut a = with("file binary\nfile -binary\n");
        assert_eq!(all(&mut a, "file"), "binary=unset");
        let mut a = with("file m1\n[attr]m1 m2\n[attr]m2 x -y\n");
        assert_eq!(all(&mut a, "file"), "m1=set m2=set x=set y=unset");
        let mut a = with("[attr]m1 x\nfile m1\n[attr]m1 y\n");
        assert_eq!(all(&mut a, "file"), "m1=set y=set");
        let mut a = with("[attr]c1 c2\n[attr]c2 c1\nfile c1\n");
        assert_eq!(all(&mut a, "file"), "c1=set c2=set");
        let mut a = with("[attr]m1 m1x !text\nfile text\nfile m1\n");
        assert_eq!(all(&mut a, "file"), "m1=set m1x=set");
    }

    #[test]
    fn deep_macro_chain() {
        let mut s = String::new();
        for i in 0..20000 {
            s.push_str(&format!("[attr]a{i} a{}\n", i + 1));
        }
        s.push_str("[attr]a20000 -text\nfile a0\n");
        let mut a = with(&s);
        let text = a.id(b"text");
        assert_eq!(a.lookup(b"file", false).get(text), &State::Unset);
    }

    #[test]
    fn requirements() {
        let mut a = with("*.c x -y z=1\n");
        let spec = AttrSpec::parse(b"x -y z=1 !w").unwrap();
        assert!(a.satisfies(b"a.c", false, &spec));
        assert!(!a.satisfies(b"a.h", false, &spec));
    }
}
