//! The emulated PC for the Recreate host (PROJECT.md §5.2): devices, BIOS and DOS services as
//! high-level emulation, program loading and the deterministic run loop.
//!
//! These services become the native port's runtime later (§7.3).

pub mod bios;
pub mod clock;
pub mod dos;
pub mod keyboard;
mod machine;
mod mz;
pub mod pic;
pub mod pit;
pub mod time;
pub mod vfs;
pub mod vga;

pub use cpu::Model as CpuModel;
pub use dos::LoadError;
pub use machine::{Config, Devices, Frame, Machine, Stop, cp437_to_char};
pub use time::DateTime;
pub use vfs::Vfs;
