//! Snapshots on disk: the whole machine, so a run can go on later, or in another process, exactly
//! as it would have gone on. In memory, `Machine::clone` is the snapshot; this is its file form.
//!
//! The file: a header (`CLOCKWRK`, the format number, Clockwork's version, the machine's state
//! hash), then the state and the call tracker's stack if calls were tracked (an analysis tool's, not
//! state, kept so that a watch from the snapshot sees whole call chains), serialized with postcard
//! and deflated. The game's files are an input,
//! not state: a snapshot holds only what the program changed on the drive, and loading it takes the
//! drive as loaded (and the CD image, if any) again. Records (the sound logs, frames, console) are
//! history, not state ([`crate::record`]): a loaded machine starts them empty.
//!
//! DOSBox-X's save states broke between builds made with different compilers; here the format is
//! explicit, and a snapshot from another format or version is refused. As a last guard the loaded
//! machine's state hash must equal the saved one, so a mismatch can't load as a subtly different
//! machine.

use std::io::{Read, Write};

use crate::machine::Machine;
use crate::vfs::Vfs;

const MAGIC: &[u8; 8] = b"CLOCKWRK";

/// The format's number: changed whenever what a snapshot holds changes shape.
pub const FORMAT: u32 = 5;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Writes `m` as a snapshot.
pub fn save(m: &Machine, mut out: impl Write) -> Result<(), String> {
    let mut slim = m.clone();
    slim.drive = m.drive.changes_only();
    let state = postcard::to_stdvec(&(&slim, m.probes.calls.as_deref())).map_err(|e| format!("snapshot: {e}"))?;
    let io = |e: std::io::Error| format!("snapshot: {e}");
    out.write_all(MAGIC).map_err(io)?;
    out.write_all(&FORMAT.to_le_bytes()).map_err(io)?;
    out.write_all(&[VERSION.len() as u8]).map_err(io)?;
    out.write_all(VERSION.as_bytes()).map_err(io)?;
    out.write_all(&m.state_hash().to_le_bytes()).map_err(io)?;
    let mut deflate = flate2::write::DeflateEncoder::new(out, flate2::Compression::fast());
    deflate.write_all(&state).map_err(io)?;
    deflate.finish().map_err(io)?;
    Ok(())
}

/// Reads a snapshot. `drive` is the game's folder as loaded at the start of the run; `cd_image`
/// the disc image the drive showed, if one.
pub fn load(
    mut input: impl Read,
    drive: Vfs,
    cd_image: Option<std::sync::Arc<crate::cdrom::CdImage>>,
) -> Result<Machine, String> {
    let io = |e: std::io::Error| format!("snapshot: {e}");
    let mut magic = [0; 8];
    input.read_exact(&mut magic).map_err(io)?;
    if &magic != MAGIC {
        return Err("not a Clockwork snapshot".into());
    }
    let mut word = [0; 4];
    input.read_exact(&mut word).map_err(io)?;
    let format = u32::from_le_bytes(word);
    let mut length = [0; 1];
    input.read_exact(&mut length).map_err(io)?;
    let mut version = vec![0; usize::from(length[0])];
    input.read_exact(&mut version).map_err(io)?;
    let version = String::from_utf8_lossy(&version);
    if format != FORMAT || version != VERSION {
        return Err(format!(
            "a snapshot of Clockwork {version} (format {format}); this is {VERSION} (format {FORMAT})"
        ));
    }
    let mut hash = [0; 8];
    input.read_exact(&mut hash).map_err(io)?;
    let mut state = Vec::new();
    flate2::read::DeflateDecoder::new(input).read_to_end(&mut state).map_err(io)?;
    let (mut m, calls): (Machine, Option<crate::calls::Calls>) =
        postcard::from_bytes(&state).map_err(|e| format!("snapshot: {e}"))?;
    m.probes.calls = calls.map(Box::new);
    m.drive = std::mem::take(&mut m.drive).over(drive);
    m.cd_image = cd_image;
    m.restart_records();
    if m.state_hash() != u64::from_le_bytes(hash) {
        return Err("the snapshot doesn't restore the machine it was taken from (another game folder, or \
                    another build of Clockwork?)"
            .into());
    }
    Ok(m)
}
