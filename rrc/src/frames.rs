//! `rrc frame SESSION --tick N`: the picture the original showed by the end of a tick, as a PNG
//! (what image viewers and agents read), from the session's trace; with `--screen`, what its screen held at
//! the tick's sync point, which a port's frame is compared with (D107). Traces keep each frame's picture
//! in the cache as indexed pixels and the palette (D25; the format is in `docs/formats.md`).

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use crate::args::Args;
use crate::cache::Cache;
use crate::repo::Repo;
use crate::say;
use crate::session::Session;
use crate::trace::Trace;

pub(crate) fn frame(repo: &Repo, args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args, &["--tick", "--out"], &["--screen"])?;
    let [name] = a.positionals(1, 1, "frame SESSION --tick N [--screen] [--out FILE]")? else { unreachable!() };
    let tick: u64 = a.required("--tick")?.parse().map_err(|_| "--tick is a number")?;
    let session = Session::load(repo, name)?;
    let recorded = session.trace.ok_or_else(|| format!("{name} isn't recorded: rrc record {name}"))?;
    let cache = Cache::open(&repo.local)?;
    let trace = Trace::parse(&String::from_utf8_lossy(&cache.get(&recorded.hash)?))?;
    if tick == 0 || tick > trace.ticks.len() as u64 {
        return Err(format!("no tick {tick} (1 to {})", trace.ticks.len()));
    }
    if a.has("--screen") {
        let screen = trace.ticks.iter().take_while(|t| t.tick <= tick).filter_map(|t| t.screen.as_ref()).last();
        let hash = screen.ok_or_else(|| format!("the trace has no screen by tick {tick}: rrc record {name}"))?;
        let picture = Picture::decode(&cache.get(hash)?)?;
        let out = match a.value("--out") {
            Some(path) => PathBuf::from(path),
            None => repo.state()?.join("frames").join(format!("{name}-{tick}-screen.png")),
        };
        if let Some(dir) = out.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        std::fs::write(&out, picture.png()).map_err(|e| format!("{}: {e}", out.display()))?;
        say("screen", format!("at tick {tick}'s sync point, {} × {}", picture.width, picture.height));
        say("png", crate::path_text(&out));
        return Ok(ExitCode::SUCCESS);
    }
    let frame = trace.ticks.iter().take_while(|t| t.tick <= tick).flat_map(|t| &t.frames).last();
    let frame = frame.ok_or_else(|| format!("no frame was shown by tick {tick}"))?;
    let hash = frame[4].as_str().ok_or("the frame has no picture")?;
    let picture = Picture::decode(&cache.get(hash)?)?;
    let out = match a.value("--out") {
        Some(path) => PathBuf::from(path),
        None => repo.state()?.join("frames").join(format!("{name}-{tick}.png")),
    };
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(&out, picture.png()).map_err(|e| format!("{}: {e}", out.display()))?;
    let (number, cycle, mode) = (&frame[0], &frame[1], frame[2].as_u64().unwrap_or(0));
    say("frame", format!("{number} at cycle {cycle}, video mode {mode:X}h, {} × {}", picture.width, picture.height));
    say("png", crate::path_text(&out));
    Ok(ExitCode::SUCCESS)
}

/// A frame's picture, as RGB.
pub struct Picture {
    pub width: usize,
    pub height: usize,
    pub rgb: Vec<u8>,
}

impl Picture {
    /// Reads a picture as the cache keeps it (`docs/formats.md`).
    pub fn decode(bytes: &[u8]) -> Result<Picture, String> {
        let mut raw = Vec::new();
        flate2::read::DeflateDecoder::new(bytes).read_to_end(&mut raw).map_err(|e| format!("a picture: {e}"))?;
        if raw.len() < 9 || &raw[..4] != b"FRM1" {
            return Err("not a picture (FRM1)".into());
        }
        let word = |at: usize| usize::from(u16::from_le_bytes([raw[at], raw[at + 1]]));
        let (width, height) = (word(4), word(6));
        let rgb = match raw[8] {
            0 => {
                let (palette, pixels) = raw[9..].split_at_checked(768).ok_or("a picture without its palette")?;
                // The DAC's 6 bits to 8.
                let expand = |c: u8| (c << 2) | (c >> 4);
                pixels.iter().flat_map(|&p| palette[3 * usize::from(p)..][..3].iter().map(|&c| expand(c))).collect()
            }
            1 => raw[9..].to_vec(),
            kind => return Err(format!("a picture of kind {kind}")),
        };
        if rgb.len() != width * height * 3 {
            return Err(format!("a {width} × {height} picture with {} bytes of pixels", rgb.len()));
        }
        Ok(Picture { width, height, rgb })
    }

    /// An 8-bit RGB PNG.
    pub fn png(&self) -> Vec<u8> {
        let mut raw = Vec::with_capacity((self.width * 3 + 1) * self.height);
        for line in self.rgb.chunks(self.width * 3) {
            raw.push(0); // no filter
            raw.extend_from_slice(line);
        }
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(&raw).expect("writing to memory");
        let data = z.finish().expect("writing to memory");
        let mut header = Vec::with_capacity(13);
        header.extend((self.width as u32).to_be_bytes());
        header.extend((self.height as u32).to_be_bytes());
        header.extend([8, 2, 0, 0, 0]); // 8 bits, RGB, deflate, adaptive filtering, not interlaced
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        for (kind, body) in [(b"IHDR", &header[..]), (b"IDAT", &data[..]), (b"IEND", &[][..])] {
            out.extend((body.len() as u32).to_be_bytes());
            out.extend(kind);
            out.extend(body);
            let mut crc = crc32fast::Hasher::new();
            crc.update(kind);
            crc.update(body);
            out.extend(crc.finalize().to_be_bytes());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_indexed_picture_reads_as_rgb_through_its_palette() {
        let mut raw = b"FRM1\x02\x00\x01\x00\x00".to_vec();
        let mut palette = [0u8; 768];
        palette[3..6].copy_from_slice(&[63, 32, 0]);
        raw.extend(palette);
        raw.extend([1, 0]);
        let mut z = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
        z.write_all(&raw).unwrap();
        let picture = Picture::decode(&z.finish().unwrap()).unwrap();
        assert_eq!((picture.width, picture.height), (2, 1));
        assert_eq!(picture.rgb, [255, 130, 0, 0, 0, 0], "6-bit DAC values to 8 bits");
        assert!(picture.png().starts_with(b"\x89PNG"));
        assert!(Picture::decode(b"not deflate").is_err());
    }
}
