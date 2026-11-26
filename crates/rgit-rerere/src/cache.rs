// SPDX-License-Identifier: MIT
//! The `rr-cache` directory and the `MERGE_RR` file
//! (Documentation/git-rerere.adoc, Documentation/technical/rerere.adoc;
//! layout checked against the `git` binary).
//!
//! `rr-cache/<id>/` holds the variants of conflict `<id>`: different files
//! (or resolutions) with the same normalised conflict. Variant 0 uses the
//! names `preimage`, `postimage` and `thisimage`, variant `n` appends
//! `.<n>`. A variant exists while its preimage or postimage does.
//!
//! `MERGE_RR` lists the conflicts being recorded: `<id>[.<n>]`, a tab, the
//! path and a NUL for each.

use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// The files of a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Image {
    /// The normalised conflict.
    Pre,
    /// The resolution.
    Post,
    /// The normalised conflict a recorded resolution was last tried on.
    This,
}

impl Image {
    fn name(self) -> &'static str {
        match self {
            Image::Pre => "preimage",
            Image::Post => "postimage",
            Image::This => "thisimage",
        }
    }
}

/// The `rr-cache` directory.
#[derive(Debug, Clone)]
pub struct Cache {
    root: PathBuf,
}

impl Cache {
    pub fn new(root: impl Into<PathBuf>) -> Cache {
        Cache { root: root.into() }
    }

    /// The directory itself.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The directory of conflict `id`.
    pub fn dir(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }

    /// The file `image` of variant `variant` of `id`.
    pub fn file(&self, id: &str, variant: u32, image: Image) -> PathBuf {
        let name = if variant == 0 {
            image.name().to_owned()
        } else {
            format!("{}.{variant}", image.name())
        };
        self.dir(id).join(name)
    }

    pub fn has(&self, id: &str, variant: u32, image: Image) -> bool {
        self.file(id, variant, image).exists()
    }

    pub fn read(&self, id: &str, variant: u32, image: Image) -> io::Result<Vec<u8>> {
        std::fs::read(self.file(id, variant, image))
    }

    /// Writes an image, creating the directory of `id`.
    pub fn write(&self, id: &str, variant: u32, image: Image, data: &[u8]) -> io::Result<()> {
        std::fs::create_dir_all(self.dir(id))?;
        std::fs::write(self.file(id, variant, image), data)
    }

    pub fn remove(&self, id: &str, variant: u32, image: Image) {
        let _ = std::fs::remove_file(self.file(id, variant, image));
    }

    /// Removes all files of a variant and the directory once it is empty.
    pub fn remove_variant(&self, id: &str, variant: u32) {
        for image in [Image::Pre, Image::Post, Image::This] {
            self.remove(id, variant, image);
        }
        let _ = std::fs::remove_dir(self.dir(id));
    }

    /// Sets the modification time of an image to now (a resolution that
    /// was used again is kept longer by `gc`).
    pub fn touch(&self, id: &str, variant: u32, image: Image) {
        if let Ok(f) = std::fs::File::options()
            .write(true)
            .open(self.file(id, variant, image))
        {
            let _ = f.set_modified(SystemTime::now());
        }
    }

    /// The variants of `id` in ascending order.
    pub fn variants(&self, id: &str) -> Vec<u32> {
        let mut out = Vec::new();
        let Ok(rd) = std::fs::read_dir(self.dir(id)) else {
            return out;
        };
        for e in rd.flatten() {
            let name = e.file_name();
            if let Some(v) = name.to_str().and_then(variant_of) {
                out.push(v);
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The lowest variant number of `id` not in use.
    pub fn free_variant(&self, id: &str) -> u32 {
        let used = self.variants(id);
        (0..).find(|v| used.binary_search(v).is_err()).unwrap_or(0)
    }
}

/// The variant a pre- or postimage file name belongs to.
pub(crate) fn variant_of(name: &str) -> Option<u32> {
    let rest = name
        .strip_prefix("preimage")
        .or_else(|| name.strip_prefix("postimage"))?;
    if rest.is_empty() {
        return Some(0);
    }
    let n = rest.strip_prefix('.')?;
    if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) || n.starts_with('0') {
        return None;
    }
    n.parse().ok()
}

/// One line of `MERGE_RR`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RrEntry {
    pub path: Vec<u8>,
    pub id: String,
    pub variant: u32,
}

/// `MERGE_RR` is not in the expected format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CorruptMergeRr;

/// Parses `MERGE_RR`.
///
/// # Errors
/// A record without a tab or with an invalid id.
pub fn parse_merge_rr(data: &[u8]) -> Result<Vec<RrEntry>, CorruptMergeRr> {
    let mut out: Vec<RrEntry> = Vec::new();
    for rec in data.split(|&b| b == 0) {
        if rec.is_empty() {
            continue;
        }
        let tab = rec.iter().position(|&b| b == b'\t').ok_or(CorruptMergeRr)?;
        let name = std::str::from_utf8(&rec[..tab]).map_err(|_| CorruptMergeRr)?;
        let (id, variant) = match name.split_once('.') {
            Some((id, v)) => (id, v.parse::<u32>().map_err(|_| CorruptMergeRr)?),
            None => (name, 0),
        };
        if id.is_empty() || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(CorruptMergeRr);
        }
        let path = rec[tab + 1..].to_vec();
        out.retain(|e| e.path != path);
        out.push(RrEntry {
            path,
            id: id.to_owned(),
            variant,
        });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// Serialises `MERGE_RR`.
pub fn format_merge_rr(entries: &[RrEntry]) -> Vec<u8> {
    let mut out = Vec::new();
    for e in entries {
        out.extend_from_slice(e.id.as_bytes());
        if e.variant > 0 {
            out.extend_from_slice(format!(".{}", e.variant).as_bytes());
        }
        out.push(b'\t');
        out.extend_from_slice(&e.path);
        out.push(0);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variant_names() {
        assert_eq!(variant_of("preimage"), Some(0));
        assert_eq!(variant_of("postimage.12"), Some(12));
        assert_eq!(variant_of("thisimage.1"), None);
        assert_eq!(variant_of("preimage.x"), None);
        assert_eq!(variant_of("preimage."), None);
        assert_eq!(variant_of("preimagex"), None);
    }

    #[test]
    fn merge_rr_round_trip() {
        let data = b"7d65\tf1\x007d65.1\tf2\x00";
        let e = parse_merge_rr(data).unwrap();
        assert_eq!(e.len(), 2);
        assert_eq!(e[1].variant, 1);
        assert_eq!(format_merge_rr(&e), data);
        assert!(parse_merge_rr(b"nothex\tf\0").is_err());
        assert!(parse_merge_rr(b"abc").is_err());
    }

    #[test]
    fn variants_and_free_slot() {
        let t = tempfile::tempdir().unwrap();
        let c = Cache::new(t.path());
        assert_eq!(c.free_variant("ab"), 0);
        c.write("ab", 0, Image::Post, b"x").unwrap();
        c.write("ab", 2, Image::Pre, b"x").unwrap();
        c.write("ab", 1, Image::This, b"x").unwrap();
        assert_eq!(c.variants("ab"), vec![0, 2]);
        assert_eq!(c.free_variant("ab"), 1);
        c.remove_variant("ab", 0);
        c.remove_variant("ab", 2);
        assert!(c.dir("ab").exists());
        c.remove_variant("ab", 1);
        assert!(!c.dir("ab").exists());
    }
}
