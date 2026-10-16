//! Machine model for the Recreate host (PROJECT.md §5.2).
//!
//! Phase 0 holds the pieces shared with the Python tooling: function identifiers and
//! real-mode address helpers. The memory image, `CpuBackend` trait, snapshots and the
//! virtual clock arrive in Phase 1.

pub mod addr;
pub mod function_id;

pub use function_id::{FunctionId, ParseFunctionIdError};
