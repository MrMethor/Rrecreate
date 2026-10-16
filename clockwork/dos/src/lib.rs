//! The emulated PC for the Recreate host (PROJECT.md §5.2): devices, BIOS and DOS services as
//! high-level emulation, program loading and the deterministic run loop.
//!
//! These services become the native port's runtime later (§7.3).

pub mod audio;
pub mod bios;
pub mod clock;
pub mod cmos;
pub mod dma;
pub mod dos;
pub mod ems;
pub mod fonts;
pub mod keyboard;
mod machine;
pub mod mouse;
pub mod mpu401;
mod mz;
pub mod opl;
pub mod pic;
pub mod pit;
pub mod sblaster;
pub mod time;
pub mod trace;
pub mod vfs;
pub mod vga;
pub mod vga_draw;
pub mod vga_params;

pub use cpu::Model as CpuModel;
pub use dos::LoadError;
pub use machine::{Config, Devices, Frame, Input, Machine, Stop, cp437_to_char};
pub use time::DateTime;
pub use vfs::Vfs;
