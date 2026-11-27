//! Test support: builders for zip archives, binary XML, resource tables
//! and signed APKs. Compiled only with the `testkit` feature, which this
//! crate's tests and andruna-sysd's tests enable; shipped code never uses
//! it.

pub mod apk;
pub mod arscw;
pub mod axmlw;
pub mod sign;
pub mod zipw;
