//! Windows unwind tables for guest code (stretch goal of S8).
//!
//! Not implemented yet; see S8.md, "Debugging".

use crate::loader::Object;

pub fn register(_o: &Object) -> Result<usize, String> {
    Err("not implemented".into())
}
