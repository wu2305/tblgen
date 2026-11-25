//! tblgen — offline `.tbl` <-> spec-format v1 converter.

pub mod check;
pub mod decompile;
pub mod gen;
pub mod model;
pub mod new;
pub mod project;
pub mod schema;
pub mod script;

pub use check::{check, check_file, CheckReport};
pub use decompile::decompile;
pub use gen::{generate, validate_spec};
pub use model::Diag;
