// SPDX-License-Identifier: MIT
//! Checks of `.gitmodules` blobs, shared by `fsck` and `index-pack
//! --strict`. They live in `rgit-submodule`, where `fetch` uses them too.

pub(crate) use rgit_submodule::fsck::check;
