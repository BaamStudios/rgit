// SPDX-License-Identifier: MIT

//! Messages about reachability bitmaps (verbatim from git).

pub const FAILED_TO_WRITE: &str = "failed to write bitmap index";
pub const PROGRESS_BUILDING: &str = "Building bitmaps";

pub fn no_full_closure(hex: &str) -> String {
    format!(
        "Failed to write bitmap index. Packfile doesn't have full closure (object {hex} is missing)"
    )
}

pub fn missing_value(key: &str) -> String {
    format!("missing value for '{key}'")
}

pub const TEST_ONE_COMMIT: &str = "you must specify exactly one commit to test";
pub const FAILED_TO_LOAD: &str = "failed to load bitmap indexes";
pub const PROGRESS_VERIFYING: &str = "Verifying bitmap entries";
pub const TEST_MISMATCH: &str = "mismatch in bitmap results";

pub fn test_header(n: usize) -> String {
    format!("Bitmap v1 test ({n} entries loaded, {n} total)")
}

pub fn test_found(hex: &str, bits: usize, checksum: u32) -> String {
    format!("Found bitmap for '{hex}'. {bits} bits / {checksum:08x} checksum")
}

pub fn test_located_pack(name: &str) -> String {
    format!("Located via pack '{name}'.")
}

pub fn test_located_midx(name: &str) -> String {
    format!("Located via multi-pack index '{name}'.")
}

pub fn no_indexed_bitmap(hex: &str) -> String {
    format!("commit '{hex}' doesn't have an indexed bitmap")
}

pub const MIDX_EMPTY: &str = "refusing to write multi-pack .bitmap without any objects";
pub const MIDX_WRITE_FAILED: &str = "could not write multi-pack bitmap";

pub fn could_not_open_pack(name: &str) -> String {
    format!("could not open pack {name}")
}
