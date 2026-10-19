//! Foundations shared by Clockwork's crates: function identifiers,
//! real-mode addressing, the memory image and stable hashing.

pub mod addr;
pub mod function_id;
pub mod hash;
pub mod memory;

pub use function_id::{FunctionId, ParseFunctionIdError};
pub use memory::Memory;
