//! The emulator protocol's revision this implements (`docs/protocol.md`), apart from `serve.rs`: Clockwork's source
//! hash leaves this file out (D173), since a revision for a tool's operation changes no run; a change to an
//! operation that runs the machine changes `serve.rs` itself.

/// The protocol's revision: it changes with every change to the protocol, and tools refuse an emulator whose
/// revision isn't theirs.
pub const PROTOCOL: u64 = 18;
