// SPDX-License-Identifier: MIT

//! The file system monitor cache (`FSMN`, gitformat-index(5)): the token
//! of the last query of the file system monitor and a bitmap of the
//! entries it did not vouch for.

use crate::{Index, ewah};

/// Parses the extension: the token (version 1 stores a timestamp in
/// nanoseconds, kept as its decimal form) and the positions of the dirty
/// entries.
pub(crate) fn parse(ext: &[u8]) -> Option<(Vec<u8>, Vec<usize>)> {
    let version = u32::from_be_bytes(ext.get(..4)?.try_into().ok()?);
    let (token, pos) = match version {
        1 => {
            let t = u64::from_be_bytes(ext.get(4..12)?.try_into().ok()?);
            (t.to_string().into_bytes(), 12)
        }
        2 => {
            let nul = ext.get(4..)?.iter().position(|&b| b == 0)?;
            (ext[4..4 + nul].to_vec(), 4 + nul + 1)
        }
        _ => return None,
    };
    let size = u32::from_be_bytes(ext.get(pos..pos + 4)?.try_into().ok()?) as usize;
    let bitmap = ext.get(pos + 4..pos + 4 + size)?;
    let (dirty, _) = ewah::decode(bitmap)?;
    Some((token, dirty))
}

/// Marks the entries the bitmap read with the index does not name as
/// valid. A bitmap naming more entries than the index has is ignored
/// (everything stays dirty).
pub(crate) fn apply_dirty(index: &mut Index) {
    let Some(dirty) = index.fsmonitor_dirty.take() else {
        return;
    };
    if dirty.last().is_some_and(|&d| d >= index.entries.len()) {
        return;
    }
    let mut next = dirty.iter().peekable();
    for (i, e) in index.entries.iter_mut().enumerate() {
        if next.peek() == Some(&&i) {
            next.next();
            e.fsm_valid = false;
        } else {
            e.fsm_valid = true;
        }
    }
}

/// The extension data (version 2) for `index` with `token`.
pub(crate) fn write(index: &Index, token: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(&2u32.to_be_bytes());
    out.extend_from_slice(token);
    out.push(0);
    let dirty: Vec<usize> = index
        .entries
        .iter()
        .enumerate()
        .filter(|(_, e)| !e.fsmonitor_valid())
        .map(|(i, _)| i)
        .collect();
    let mut bitmap = Vec::new();
    ewah::encode(&dirty, &mut bitmap);
    out.extend_from_slice(
        &u32::try_from(bitmap.len())
            .unwrap_or(u32::MAX)
            .to_be_bytes(),
    );
    out.extend_from_slice(&bitmap);
}

impl Index {
    /// The token of the last file system monitor query stored in the
    /// index (`FSMN`), if the index has the extension.
    pub fn fsmonitor_token(&self) -> Option<&[u8]> {
        self.fsmonitor.as_deref()
    }

    /// Sets or (with `None`) removes the `FSMN` extension.
    pub fn set_fsmonitor_token(&mut self, token: Option<Vec<u8>>) {
        self.fsmonitor = token;
    }

    /// Whether this process queried the file system monitor successfully
    /// and applied its answer, so that directories of the untracked cache
    /// it reported nothing in can be trusted without checking them.
    pub fn fsmonitor_has_run(&self) -> bool {
        self.fsmonitor_run
    }

    /// See [`Index::fsmonitor_has_run`].
    pub fn set_fsmonitor_has_run(&mut self, run: bool) {
        self.fsmonitor_run = run;
    }

    /// Marks every entry invalid for the file system monitor.
    pub fn invalidate_fsmonitor(&mut self) {
        for e in &mut self.entries {
            e.set_fsmonitor_valid(false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AddOptions, Mode, ReadOptions, WriteOptions};
    use rgit_hash::HashKind;

    #[test]
    fn round_trip() {
        let kind = HashKind::Sha1;
        let mut index = Index::new(kind);
        for p in ["a", "b", "c"] {
            index
                .add_path(p, Mode::FILE, kind.empty_blob(), AddOptions::default())
                .unwrap();
        }
        index.entry_mut(1).unwrap().set_fsmonitor_valid(true);
        index.set_fsmonitor_token(Some(b"tok".to_vec()));
        let bytes = index.to_bytes(&WriteOptions::default());
        let again = Index::from_bytes(&bytes, kind, &ReadOptions::default()).unwrap();
        assert_eq!(again.fsmonitor_token(), Some(&b"tok"[..]));
        let valid: Vec<bool> = again
            .entries()
            .iter()
            .map(|e| e.fsmonitor_valid())
            .collect();
        assert_eq!(valid, [false, true, false]);
    }

    #[test]
    fn version_one() {
        let mut ext = 1u32.to_be_bytes().to_vec();
        ext.extend_from_slice(&42u64.to_be_bytes());
        let mut bitmap = Vec::new();
        ewah::encode(&[0], &mut bitmap);
        ext.extend_from_slice(&(bitmap.len() as u32).to_be_bytes());
        ext.extend_from_slice(&bitmap);
        assert_eq!(parse(&ext), Some((b"42".to_vec(), vec![0])));
    }
}
