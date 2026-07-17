//! Syntactic frontend: parsing facade, imports, names, index, CFG
//! (DESIGN §5.1 pipeline stages before the abstract interpreter).
//!
//! TODO(phase-1): `parser`, `imports`, `names`, `index`.
//! TODO(phase-2): `cfg`.

pub mod cfg;
pub mod imports;
pub mod index;
pub mod names;
pub mod parser;
