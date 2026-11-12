// SPDX-License-Identifier: MIT

//! Pathspec magic flags (`:(top)`, `:(glob)`, `:!`, ...).

use std::fmt;
use std::ops::{BitAnd, BitOr, BitOrAssign, Not};

/// A set of pathspec magic words.
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Magic(u8);

impl Magic {
    /// No magic.
    pub const NONE: Self = Self(0);
    /// `top` / `/`: relative to the top of the work tree, not the cwd.
    pub const TOP: Self = Self(1);
    /// `literal`: no wildcards.
    pub const LITERAL: Self = Self(2);
    /// `glob`: wildmatch with `WM_PATHNAME` semantics.
    pub const GLOB: Self = Self(4);
    /// `icase`: case-insensitive.
    pub const ICASE: Self = Self(8);
    /// `exclude` / `!` / `^`: subtract matching paths.
    pub const EXCLUDE: Self = Self(16);
    /// `attr:...`: attribute requirements.
    pub const ATTR: Self = Self(32);
    /// Every magic word.
    pub const ALL: Self = Self(63);

    /// Whether all of `other` is set.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Whether any of `other` is set.
    #[must_use]
    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    /// Whether no magic is set.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The individual magic words in git's display order.
    pub fn iter(self) -> impl Iterator<Item = MagicWord> {
        MagicWord::ALL
            .into_iter()
            .filter(move |w| self.contains(w.flag()))
    }
}

impl BitOr for Magic {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for Magic {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl BitAnd for Magic {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}

impl Not for Magic {
    type Output = Self;
    fn not(self) -> Self {
        Self(!self.0 & Self::ALL.0)
    }
}

impl fmt::Debug for Magic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set()
            .entries(self.iter().map(MagicWord::name))
            .finish()
    }
}

/// One magic word, with its long name and optional short mnemonic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MagicWord {
    /// `top`, mnemonic `/`.
    Top,
    /// `literal`.
    Literal,
    /// `glob`.
    Glob,
    /// `icase`.
    Icase,
    /// `exclude`, mnemonic `!` (or `^`).
    Exclude,
    /// `attr`.
    Attr,
}

impl MagicWord {
    /// All words in git's order (used when listing unsupported magic).
    pub const ALL: [Self; 6] = [
        Self::Top,
        Self::Literal,
        Self::Glob,
        Self::Icase,
        Self::Exclude,
        Self::Attr,
    ];

    /// The long name as written in `:(...)`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Top => "top",
            Self::Literal => "literal",
            Self::Glob => "glob",
            Self::Icase => "icase",
            Self::Exclude => "exclude",
            Self::Attr => "attr",
        }
    }

    /// The short form character, if any.
    #[must_use]
    pub const fn mnemonic(self) -> Option<char> {
        match self {
            Self::Top => Some('/'),
            Self::Exclude => Some('!'),
            _ => None,
        }
    }

    /// The flag for this word.
    #[must_use]
    pub const fn flag(self) -> Magic {
        match self {
            Self::Top => Magic::TOP,
            Self::Literal => Magic::LITERAL,
            Self::Glob => Magic::GLOB,
            Self::Icase => Magic::ICASE,
            Self::Exclude => Magic::EXCLUDE,
            Self::Attr => Magic::ATTR,
        }
    }

    pub(crate) fn from_name(name: &[u8]) -> Option<Self> {
        Self::ALL.into_iter().find(|w| w.name().as_bytes() == name)
    }
}
