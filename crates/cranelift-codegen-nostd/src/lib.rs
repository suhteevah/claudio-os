//! Cranelift code generation library.
#![deny(missing_docs)]
// Display feature requirements in the documentation when building on docs.rs
#![cfg_attr(docsrs, feature(doc_auto_cfg))]
#![no_std]
// Various bits and pieces of this crate might only be used for one platform or
// another, but it's not really too useful to learn about that all the time. On
// CI we build at least one version of this crate with `--features all-arch`
// which means we'll always detect truly dead code, otherwise if this is only
// built for one platform we don't have to worry too much about trimming
// everything down.
#![cfg_attr(not(feature = "all-arch"), allow(dead_code))]
#![expect(clippy::allow_attributes_without_reason, reason = "crate not migrated")]

#[allow(unused_imports)] // #[macro_use] is required for no_std
#[macro_use]
extern crate alloc;


#[cfg(not(feature = "std"))]
use hashbrown::{hash_map, HashMap};
