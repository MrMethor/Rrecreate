//! Ported from DOSBox Staging `src/dos/cdrom_image.cpp` and `src/dos/cdrom.h` (GPL-2.0-or-later):
//! CD images, their track layout and their sectors. A CUE sheet lists BINARY files (raw or cooked
//! sectors, audio as 16-bit little-endian stereo at 44.1 kHz) and WAVE files (the same PCM; other
//! formats are refused, so audio needs no floating-point decoder); an ISO file is one data track.
//! The data track's ISO 9660 file system (DOSBox `drive_iso.cpp`) becomes files of the virtual
//! drive, which the CD drive shows ([`mount`]).
//!
//! The image is an input, like the game's files: it is read when needed and is not part of the
//! machine state. Track positions follow DOSBox's `AddTrack` (pregaps, several tracks in one
//! file, a lead-out track after the last).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::vfs::Vfs;

pub const RAW_SECTOR: u32 = 2352;
pub const COOKED_SECTOR: u32 = 2048;
pub const FRAMES_PER_SECOND: u32 = 75;
/// PCM frames (stereo samples) in one sector of audio.
pub const PCM_FRAMES_PER_SECTOR: u32 = 588;
pub const PCM_RATE_HZ: u32 = 44_100;
/// Between High Sierra sector numbers and Red Book addresses: the 2-second lead-in.
pub const FRAME_PADDING: u32 = 150;

/// The folder of the virtual drive that holds an image's files: not a valid DOS name, so only
/// the CD drive reaches it.
pub const IMAGE_ROOT: &str = "[CD]";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Track {
    pub number: u8,
    /// 0 for audio, 40h for data (the Q-channel's control and address byte).
    pub attr: u8,
    /// First sector (High Sierra numbering: 0 is at Red Book 00:02:00).
    pub start: u32,
    pub length: u32,
    pub sector_size: u32,
    pub mode2: bool,
    /// Bytes of its file before its first sector.
    pub skip: u64,
    file: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct TrackFile {
    path: PathBuf,
    /// Where the sectors (or the WAVE's samples) start in the file, and how many bytes there are.
    offset: u64,
    length: u64,
}

impl TrackFile {
    fn read(&self, at: u64, buffer: &mut [u8]) -> bool {
        if at + buffer.len() as u64 > self.length {
            return false;
        }
        let Ok(mut file) = File::open(&self.path) else { return false };
        file.seek(SeekFrom::Start(self.offset + at)).is_ok() && file.read_exact(buffer).is_ok()
    }
}

/// A file of the data track's file system: its DOS path, and its contents (None: a folder).
pub type IsoEntry = (String, Option<Vec<u8>>);

/// A CD image: its tracks (the lead-out last) and the files they come from.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CdImage {
    pub tracks: Vec<Track>,
    files: Vec<TrackFile>,
}

impl CdImage {
    /// A folder shown as a CD: one data track of `sectors`, with no sectors to read.
    pub fn folder(sectors: u32) -> CdImage {
        let data = Track {
            number: 1,
            attr: 0x40,
            start: 0,
            length: sectors,
            sector_size: COOKED_SECTOR,
            mode2: false,
            skip: 0,
            file: None,
        };
        let lead_out = Track {
            number: 2,
            attr: 0,
            start: sectors,
            length: 0,
            sector_size: COOKED_SECTOR,
            mode2: false,
            skip: 0,
            file: None,
        };
        CdImage { tracks: vec![data, lead_out], files: Vec::new() }
    }

    /// Opens a `.cue` sheet or an `.iso` file.
    pub fn open(path: &Path) -> Result<CdImage, String> {
        let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        let image = if extension == "cue" { Self::cue(path)? } else { Self::iso(path)? };
        if image.tracks.len() < 2 {
            return Err(format!("{}: no tracks", path.display()));
        }
        Ok(image)
    }

    fn iso(path: &Path) -> Result<CdImage, String> {
        let length = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?.len();
        let file = TrackFile { path: path.to_path_buf(), offset: 0, length };
        // Cooked sectors, or raw ones with the PVD's CD001 16 bytes into sector 16.
        let mut probe = [0u8; 6];
        let raw = !(file.read(16 * 2048, &mut probe) && &probe[1..6] == b"CD001")
            && file.read(16 * 2352 + 16, &mut probe)
            && &probe[1..6] == b"CD001";
        let sector_size = if raw { RAW_SECTOR } else { COOKED_SECTOR };
        let sectors = length.div_ceil(u64::from(sector_size)) as u32;
        let data = Track {
            number: 1,
            attr: 0x40,
            start: 0,
            length: sectors,
            sector_size,
            mode2: false,
            skip: 0,
            file: Some(0),
        };
        let lead_out =
            Track { number: 2, attr: 0, start: sectors, length: 0, sector_size, mode2: false, skip: 0, file: None };
        Ok(CdImage { tracks: vec![data, lead_out], files: vec![file] })
    }

    /// DOSBox `LoadCueSheet`.
    fn cue(path: &Path) -> Result<CdImage, String> {
        let text = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let text = String::from_utf8_lossy(&text);
        let folder = path.parent().unwrap_or(Path::new("."));
        let mut image = CdImage { tracks: Vec::new(), files: Vec::new() };
        let mut track: Option<Track> = None;
        let mut file: Option<usize> = None;
        let (mut shift, mut total_pregap, mut pregap, mut prestart) = (0u32, 0u32, 0u32, None::<u32>);
        let bad = |line: &str| format!("{}: can't read the line {line:?}", path.display());
        for line in text.lines() {
            let line = line.trim();
            let (keyword, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
            let rest = rest.trim();
            match keyword.to_ascii_uppercase().as_str() {
                "TRACK" => {
                    if let Some(t) = track.take() {
                        image.add(t, &mut shift, prestart, &mut total_pregap, pregap)?;
                    }
                    (pregap, prestart) = (0, None);
                    let (number, kind) = rest.split_once(char::is_whitespace).ok_or_else(|| bad(line))?;
                    let number: u8 = number.parse().map_err(|_| bad(line))?;
                    let (sector_size, attr, mode2) = match kind.trim().to_ascii_uppercase().as_str() {
                        "AUDIO" => (RAW_SECTOR, 0, false),
                        "MODE1/2048" => (COOKED_SECTOR, 0x40, false),
                        "MODE1/2352" => (RAW_SECTOR, 0x40, false),
                        "MODE2/2336" => (2336, 0x40, true),
                        "MODE2/2352" => (RAW_SECTOR, 0x40, true),
                        _ => return Err(format!("{}: track type {kind:?} is not supported", path.display())),
                    };
                    track = Some(Track { number, attr, start: 0, length: 0, sector_size, mode2, skip: 0, file });
                }
                "INDEX" => {
                    let (index, time) = rest.split_once(char::is_whitespace).ok_or_else(|| bad(line))?;
                    let frame = msf_text(time.trim()).ok_or_else(|| bad(line))?;
                    match (index.parse::<u32>().map_err(|_| bad(line))?, track.as_mut()) {
                        (1, Some(t)) => t.start = frame,
                        (0, _) => prestart = Some(frame),
                        _ => {}
                    }
                }
                "FILE" => {
                    if let Some(t) = track.take() {
                        image.add(t, &mut shift, prestart, &mut total_pregap, pregap)?;
                    }
                    let (name, kind) = match rest.strip_prefix('"') {
                        Some(quoted) => quoted.split_once('"').ok_or_else(|| bad(line))?,
                        None => rest.rsplit_once(char::is_whitespace).ok_or_else(|| bad(line))?,
                    };
                    let file_path =
                        find_file(folder, name).ok_or_else(|| format!("{}: no file {name}", path.display()))?;
                    let kind = kind.trim().to_ascii_uppercase();
                    let track_file = match kind.as_str() {
                        "BINARY" => {
                            let length = std::fs::metadata(&file_path).map_err(|e| format!("{name}: {e}"))?.len();
                            TrackFile { path: file_path, offset: 0, length }
                        }
                        "WAVE" => wave(&file_path)?,
                        _ => {
                            return Err(format!(
                                "{name}: {kind} tracks are not supported (BINARY and 16-bit 44.1 kHz stereo WAVE are)"
                            ));
                        }
                    };
                    image.files.push(track_file);
                    file = Some(image.files.len() - 1);
                }
                "PREGAP" => pregap = msf_text(rest).ok_or_else(|| bad(line))?,
                "" | "REM" | "CATALOG" | "CDTEXTFILE" | "FLAGS" | "ISRC" | "PERFORMER" | "POSTGAP" | "SONGWRITER"
                | "TITLE" => {}
                _ => return Err(bad(line)),
            }
        }
        let mut last = track.ok_or_else(|| format!("{}: no tracks", path.display()))?;
        let number = last.number;
        last.file = last.file.or(file);
        image.add(last, &mut shift, prestart, &mut total_pregap, pregap)?;
        let lead_out = Track {
            number: number + 1,
            attr: 0,
            start: 0,
            length: 0,
            sector_size: RAW_SECTOR,
            mode2: false,
            skip: 0,
            file: None,
        };
        image.add(lead_out, &mut shift, None, &mut total_pregap, 0)?;
        Ok(image)
    }

    /// DOSBox `AddTrack`: where a track starts on the disc, from its file position and pregaps.
    fn add(
        &mut self,
        mut current: Track,
        shift: &mut u32,
        prestart: Option<u32>,
        total_pregap: &mut u32,
        pregap: u32,
    ) -> Result<(), String> {
        let mut skip = 0;
        if let Some(prestart) = prestart {
            if prestart > current.start {
                return Err(format!("track {}: INDEX 00 after INDEX 01", current.number));
            }
            skip = current.start - prestart;
        }
        let Some(previous) = self.tracks.last_mut() else {
            current.skip = u64::from(skip) * u64::from(current.sector_size);
            current.start += pregap;
            *total_pregap = pregap;
            self.tracks.push(current);
            return Ok(());
        };
        if previous.file == current.file || current.file.is_none() && previous.file.is_none() {
            current.start += *shift;
            if previous.length == 0 {
                previous.length = (current.start + *total_pregap).saturating_sub(previous.start + skip);
            }
            current.skip = previous.skip
                + u64::from(previous.length) * u64::from(previous.sector_size)
                + u64::from(skip) * u64::from(current.sector_size);
            *total_pregap += pregap;
            current.start += *total_pregap;
        } else {
            let file_length = previous.file.map_or(0, |f| self.files[f].length);
            let bytes = file_length.saturating_sub(previous.skip);
            previous.length = bytes.div_ceil(u64::from(previous.sector_size)) as u32;
            current.start += previous.start + previous.length + pregap;
            current.skip = u64::from(skip) * u64::from(current.sector_size);
            *shift += previous.start + previous.length;
            *total_pregap = pregap;
        }
        let previous = self.tracks.last().expect("checked");
        if current.number <= 1
            || previous.number + 1 != current.number
            || current.start < previous.start + previous.length
        {
            return Err(format!("track {}: out of order or overlapping the one before", current.number));
        }
        self.tracks.push(current);
        Ok(())
    }

    /// The track holding `sector` (not the lead-out).
    pub fn track_at(&self, sector: u32) -> Option<&Track> {
        let playable = &self.tracks[..self.tracks.len() - 1];
        playable.iter().rev().find(|t| t.start <= sector).filter(|t| sector < t.start + t.length.max(1))
    }

    pub fn lead_out(&self) -> u32 {
        self.tracks.last().map_or(0, |t| t.start)
    }

    /// One data sector: its 2048 user bytes, or (raw) all 2352 as on the disc.
    pub fn read_sector(&self, sector: u32, raw: bool) -> Option<Vec<u8>> {
        let track = self.track_at(sector).filter(|t| t.attr == 0x40)?;
        let file = &self.files[track.file?];
        let at = track.skip + u64::from(sector - track.start) * u64::from(track.sector_size);
        let mut data = vec![0; track.sector_size as usize];
        if !file.read(at, &mut data) {
            return None;
        }
        Some(match (raw, track.sector_size) {
            (false, COOKED_SECTOR) => data,
            (false, 2336) => data[8..8 + 2048].to_vec(),
            (false, _) => {
                let start = if track.mode2 { 24 } else { 16 };
                data[start..start + 2048].to_vec()
            }
            (true, RAW_SECTOR) => data,
            (true, _) => {
                // Cooked on the image: the sync and header around the user data.
                let mut raw = vec![0u8; RAW_SECTOR as usize];
                raw[1..11].fill(0xFF);
                let msf = msf(sector + FRAME_PADDING);
                raw[12..15].copy_from_slice(&[bcd(msf.0), bcd(msf.1), bcd(msf.2)]);
                raw[15] = 1;
                raw[16..16 + 2048].copy_from_slice(&data[..2048]);
                raw
            }
        })
    }

    /// `frames` stereo samples of audio from PCM frame `first` of the disc (588 a sector), as
    /// interleaved i16; silence where there is no audio.
    pub fn audio(&self, first: u64, frames: usize) -> Vec<i16> {
        let mut out = vec![0i16; frames * 2];
        let mut done = 0;
        while done < frames {
            let at = first + done as u64;
            let sector = (at / u64::from(PCM_FRAMES_PER_SECTOR)) as u32;
            let Some(track) = self.track_at(sector) else { break };
            let track_end = u64::from(track.start + track.length) * u64::from(PCM_FRAMES_PER_SECTOR);
            let count = (frames - done).min((track_end.saturating_sub(at)) as usize);
            if count == 0 {
                break;
            }
            if track.attr == 0
                && let Some(file) = track.file.map(|f| &self.files[f])
            {
                let offset = track.skip + (at - u64::from(track.start) * u64::from(PCM_FRAMES_PER_SECTOR)) * 4;
                let available = file.length.saturating_sub(offset).min(count as u64 * 4) as usize;
                let mut bytes = vec![0u8; available & !3];
                if file.read(offset, &mut bytes) {
                    for (i, pair) in bytes.chunks_exact(2).enumerate() {
                        out[done * 2 + i] = i16::from_le_bytes([pair[0], pair[1]]);
                    }
                }
            }
            done += count;
        }
        out
    }

    /// The data track's ISO 9660 volume label.
    pub fn volume_label(&self) -> Option<String> {
        let pvd = self.read_sector(16, false)?;
        (&pvd[1..6] == b"CD001").then(|| String::from_utf8_lossy(&pvd[40..72]).trim().to_string())
    }

    /// The files of the data track's ISO 9660 file system, as (DOS path, contents), folders
    /// as (path, None).
    pub fn files(&self) -> Result<Vec<IsoEntry>, String> {
        let pvd = self.read_sector(16, false).ok_or("no data track: no ISO 9660 volume at sector 16")?;
        if &pvd[1..6] != b"CD001" {
            return Err("the data track has no ISO 9660 volume".into());
        }
        let root = &pvd[156..156 + 34];
        let mut out = Vec::new();
        self.directory(both_endian(&root[2..]), both_endian(&root[10..]), "", &mut out, 0)?;
        Ok(out)
    }

    fn directory(&self, extent: u32, size: u32, path: &str, out: &mut Vec<IsoEntry>, depth: u32) -> Result<(), String> {
        if depth > 16 {
            return Err("ISO 9660 folders nest too deep".into());
        }
        let mut data = Vec::new();
        for sector in extent..extent + size.div_ceil(COOKED_SECTOR) {
            data.extend(self.read_sector(sector, false).ok_or_else(|| format!("can't read sector {sector}"))?);
        }
        data.truncate(size as usize);
        let mut at = 0;
        while at < data.len() {
            let length = usize::from(data[at]);
            if length == 0 {
                // Records don't cross sectors: the rest of this one is padding.
                at = (at / COOKED_SECTOR as usize + 1) * COOKED_SECTOR as usize;
                continue;
            }
            let Some(record) = data.get(at..at + length) else { break };
            at += length;
            let name_length = usize::from(record[32]);
            let name = &record[33..33 + name_length.min(record.len() - 33)];
            if name == [0] || name == [1] {
                continue;
            }
            let mut name = String::from_utf8_lossy(name).to_ascii_uppercase();
            if let Some(i) = name.find(';') {
                name.truncate(i);
            }
            let name = name.trim_end_matches('.').to_string();
            let full = if path.is_empty() { name } else { format!("{path}\\{name}") };
            let (extent, size, directory) =
                (both_endian(&record[2..]), both_endian(&record[10..]), record[25] & 2 != 0);
            if directory {
                out.push((full.clone(), None));
                self.directory(extent, size, &full, out, depth + 1)?;
            } else {
                let mut contents = Vec::with_capacity(size as usize);
                for sector in extent..extent + size.div_ceil(COOKED_SECTOR) {
                    contents.extend(
                        self.read_sector(sector, false).ok_or_else(|| format!("{full}: can't read sector {sector}"))?,
                    );
                }
                contents.truncate(size as usize);
                out.push((full, Some(contents)));
            }
        }
        Ok(())
    }
}

/// Puts an image's files on the drive under [`IMAGE_ROOT`] for the CD drive to show; returns
/// that folder and the volume label.
pub fn mount(drive: &mut Vfs, image: &CdImage) -> Result<(String, String), String> {
    drive.insert_dir(IMAGE_ROOT);
    for (path, contents) in image.files()? {
        let path = format!("{IMAGE_ROOT}\\{path}");
        match contents {
            Some(data) => drive.insert_file(&path, data),
            None => drive.insert_dir(&path),
        }
    }
    Ok((IMAGE_ROOT.to_string(), image.volume_label().unwrap_or_default()))
}

/// The ISO 9660 both-endian 32-bit number at `bytes` (its little-endian half).
fn both_endian(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// A sector number as (minutes, seconds, frames).
pub fn msf(frames: u32) -> (u8, u8, u8) {
    (
        (frames / (60 * FRAMES_PER_SECOND)) as u8,
        (frames / FRAMES_PER_SECOND % 60) as u8,
        (frames % FRAMES_PER_SECOND) as u8,
    )
}

pub fn from_msf(minutes: u8, seconds: u8, frames: u8) -> u32 {
    (u32::from(minutes) * 60 + u32::from(seconds)) * FRAMES_PER_SECOND + u32::from(frames)
}

pub fn bcd(value: u8) -> u8 {
    ((value / 10) << 4) | (value % 10)
}

/// "MM:SS:FF" in a CUE sheet.
fn msf_text(text: &str) -> Option<u32> {
    let mut parts = text.split(':').map(|p| p.trim().parse::<u8>().ok());
    let (m, s, f) = (parts.next()??, parts.next()??, parts.next()??);
    Some(from_msf(m, s, f))
}

/// A file a CUE sheet names, next to the sheet (in any case, as DOSBox's `GetRealFileName`).
fn find_file(folder: &Path, name: &str) -> Option<PathBuf> {
    let name = name.replace('\\', "/");
    let direct = folder.join(&name);
    if direct.is_file() {
        return Some(direct);
    }
    let base = Path::new(&name).file_name()?.to_str()?.to_ascii_lowercase();
    std::fs::read_dir(folder)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.to_ascii_lowercase() == base))
}

/// A WAVE file's samples: only 16-bit stereo PCM at 44.1 kHz, as on a CD.
fn wave(path: &Path) -> Result<TrackFile, String> {
    let name = path.display();
    let mut file = File::open(path).map_err(|e| format!("{name}: {e}"))?;
    let mut header = [0u8; 12];
    file.read_exact(&mut header).map_err(|e| format!("{name}: {e}"))?;
    if &header[0..4] != b"RIFF" || &header[8..12] != b"WAVE" {
        return Err(format!("{name}: not a WAVE file"));
    }
    let mut at = 12u64;
    let mut format_ok = false;
    loop {
        let mut chunk = [0u8; 8];
        if file.read_exact(&mut chunk).is_err() {
            return Err(format!("{name}: no data chunk"));
        }
        let size = u64::from(u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]));
        match &chunk[0..4] {
            b"fmt " => {
                let mut fmt = [0u8; 16];
                file.read_exact(&mut fmt).map_err(|e| format!("{name}: {e}"))?;
                let (kind, channels) = (u16::from_le_bytes([fmt[0], fmt[1]]), u16::from_le_bytes([fmt[2], fmt[3]]));
                let rate = u32::from_le_bytes([fmt[4], fmt[5], fmt[6], fmt[7]]);
                let bits = u16::from_le_bytes([fmt[14], fmt[15]]);
                if (kind, channels, rate, bits) != (1, 2, PCM_RATE_HZ, 16) {
                    return Err(format!(
                        "{name}: {bits}-bit, {channels} channels at {rate} Hz (format {kind}): CD tracks must be 16-bit stereo PCM at 44.1 kHz"
                    ));
                }
                format_ok = true;
                file.seek(SeekFrom::Start(at + 8 + size + (size & 1))).map_err(|e| format!("{name}: {e}"))?;
            }
            b"data" => {
                if !format_ok {
                    return Err(format!("{name}: data before the format"));
                }
                return Ok(TrackFile { path: path.to_path_buf(), offset: at + 8, length: size });
            }
            _ => {
                file.seek(SeekFrom::Start(at + 8 + size + (size & 1))).map_err(|e| format!("{name}: {e}"))?;
            }
        }
        at += 8 + size + (size & 1);
    }
}

/// One PLAY: what the renderer mixes in (the drive's part of the audio log).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct CdSegment {
    pub start_ns: u64,
    pub sector: u32,
    pub length: u32,
    /// When a STOP (or another PLAY) cut it short.
    pub stop_ns: Option<u64>,
    /// Left and right volume (0-255).
    pub volume: [u8; 2],
}

impl CdSegment {
    /// When it ends: cut short, or at its last sector.
    pub fn end_ns(&self) -> u64 {
        let full = self.start_ns + u64::from(self.length) * 1_000_000_000 / u64::from(FRAMES_PER_SECOND);
        self.stop_ns.map_or(full, |stop| stop.min(full))
    }
}

/// The drive's audio as MSCDEX sees it (DOSBox `CMscdex` with `CDROM_Interface_Image`'s
/// player), kept by virtual time: a PLAY starts at a time, and where it has got to follows from
/// the time now.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct CdAudio {
    playing: Option<CdSegment>,
    /// Stopped while playing: RESUME (or a PLAY from there) goes on from this sector, for this
    /// many more.
    paused: Option<(u32, u32)>,
    /// Where the head is when nothing plays (the last SEEK).
    position: Option<u32>,
    /// Channel control (IOCTL output 03h): the input channel and volume of each output.
    pub channels: [(u8, u8); 4],
    pub segments: Vec<CdSegment>,
}

impl Default for CdAudio {
    fn default() -> Self {
        CdAudio {
            playing: None,
            paused: None,
            position: None,
            channels: [(0, 255), (1, 255), (2, 0), (3, 0)],
            segments: Vec::new(),
        }
    }
}

impl CdAudio {
    fn active(&self, now: u64) -> Option<&CdSegment> {
        self.playing.as_ref().filter(|s| now < s.end_ns())
    }

    /// Sound coming out now (not paused, not finished).
    pub fn is_playing(&self, now: u64) -> bool {
        self.active(now).is_some()
    }

    pub fn is_paused(&self) -> bool {
        self.paused.is_some()
    }

    /// The sector the head is at.
    pub fn current(&self, image: &CdImage, now: u64) -> u32 {
        if let Some(s) = self.playing.as_ref() {
            let played =
                (now.min(s.end_ns()).saturating_sub(s.start_ns)) * u64::from(FRAMES_PER_SECOND) / 1_000_000_000;
            return s.sector + (played as u32).min(s.length);
        }
        if let Some((sector, _)) = self.paused {
            return sector;
        }
        self.position.unwrap_or_else(|| image.tracks.iter().find(|t| t.attr == 0).map_or(0, |t| t.start))
    }

    /// PLAY AUDIO from `sector` for `length` sectors (DOSBox `PlayAudioSector`): from where a
    /// STOP paused, it resumes. False if there is no audio there.
    pub fn play(&mut self, image: &CdImage, mut sector: u32, mut length: u32, now: u64) -> bool {
        if let Some((from, rest)) = self.paused
            && from == sector
            && rest != 0
        {
            length = rest;
        }
        let Some(track) = image.track_at(sector).or_else(|| {
            // In the pregap before a track: from its start.
            image.tracks.iter().find(|t| t.start > sector && t.attr == 0)
        }) else {
            self.stop_completely(now);
            return false;
        };
        if track.attr != 0 || length == 0 {
            self.stop_completely(now);
            return false;
        }
        if sector < track.start {
            length = length.saturating_sub(track.start - sector);
            sector = track.start;
        }
        self.cut(now);
        let volume = self.volume();
        let segment = CdSegment { start_ns: now, sector, length, stop_ns: None, volume };
        self.playing = Some(segment);
        self.paused = None;
        self.segments.push(segment);
        true
    }

    /// STOP AUDIO: while playing it pauses (RESUME goes on); otherwise it forgets the position.
    pub fn stop(&mut self, image: &CdImage, now: u64) {
        if self.is_playing(now) {
            let at = self.current(image, now);
            let s = self.playing.expect("playing");
            self.paused = Some((at, (s.sector + s.length).saturating_sub(at)));
            self.cut(now);
            self.playing = None;
        } else {
            self.stop_completely(now);
        }
    }

    pub fn resume(&mut self, image: &CdImage, now: u64) -> bool {
        match self.paused {
            Some((sector, length)) => self.play(image, sector, length, now),
            None => false,
        }
    }

    pub fn seek(&mut self, sector: u32, now: u64) {
        self.stop_completely(now);
        self.position = Some(sector);
    }

    /// The last PLAY's start and length, while it plays or is paused (IOCTL 0Fh).
    pub fn last_play(&self, now: u64) -> Option<(u32, u32)> {
        match (self.paused, self.active(now)) {
            (Some(paused), _) => Some(paused),
            (None, Some(s)) => Some((s.sector, s.length)),
            _ => None,
        }
    }

    fn stop_completely(&mut self, now: u64) {
        self.cut(now);
        self.playing = None;
        self.paused = None;
        self.position = None;
    }

    /// Ends the segment playing at `now`.
    fn cut(&mut self, now: u64) {
        if let Some(s) = self.playing
            && now < s.end_ns()
            && let Some(last) = self.segments.last_mut()
            && *last == s
        {
            last.stop_ns = Some(now);
        }
    }

    /// Left and right output volume, from channel control (an output fed by input 0 or 1).
    fn volume(&self) -> [u8; 2] {
        let level = |output: usize| {
            let (input, volume) = self.channels[output];
            if input <= 1 { volume } else { 0 }
        };
        [level(0), level(1)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bios::callback;
    use crate::{Config, Machine};

    /// A directory record: extent, size, folder or file, name.
    fn record(extent: u32, size: u32, folder: bool, name: &[u8]) -> Vec<u8> {
        let length = 33 + name.len() + (name.len() + 1) % 2;
        let mut r = vec![0u8; length];
        r[0] = length as u8;
        r[2..6].copy_from_slice(&extent.to_le_bytes());
        r[6..10].copy_from_slice(&extent.to_be_bytes());
        r[10..14].copy_from_slice(&size.to_le_bytes());
        r[14..18].copy_from_slice(&size.to_be_bytes());
        r[25] = if folder { 2 } else { 0 };
        r[28] = 1;
        r[32] = name.len() as u8;
        r[33..33 + name.len()].copy_from_slice(name);
        r
    }

    /// 30 cooked sectors: an ISO 9660 volume with HELLO.TXT and DIR\INNER.TXT.
    fn iso_volume() -> Vec<u8> {
        let mut sectors = vec![vec![0u8; 2048]; 30];
        let pvd = &mut sectors[16];
        pvd[0] = 1;
        pvd[1..6].copy_from_slice(b"CD001");
        pvd[6] = 1;
        pvd[40..72].copy_from_slice(format!("{:<32}", "TESTDISC").as_bytes());
        pvd[80..84].copy_from_slice(&30u32.to_le_bytes());
        pvd[128..130].copy_from_slice(&2048u16.to_le_bytes());
        let root = record(18, 2048, true, &[0]);
        pvd[156..156 + 34].copy_from_slice(&root[..34]);
        sectors[17][0] = 0xFF;
        sectors[17][1..6].copy_from_slice(b"CD001");
        let mut dir: Vec<u8> = [record(18, 2048, true, &[0]), record(18, 2048, true, &[1])].concat();
        dir.extend(record(20, 11, false, b"HELLO.TXT;1"));
        dir.extend(record(19, 2048, true, b"DIR"));
        sectors[18][..dir.len()].copy_from_slice(&dir);
        let mut inner: Vec<u8> = [record(19, 2048, true, &[0]), record(18, 2048, true, &[1])].concat();
        inner.extend(record(21, 5, false, b"INNER.TXT;1"));
        sectors[19][..inner.len()].copy_from_slice(&inner);
        sectors[20][..11].copy_from_slice(b"hello disc!");
        sectors[21][..5].copy_from_slice(b"inner");
        sectors.concat()
    }

    /// The test's audio: frame n of a track is (n, -n) plus a track marker.
    fn pcm(frames: u32, marker: i16) -> Vec<u8> {
        (0..frames)
            .flat_map(|n| [(n as i16).wrapping_add(marker), (n as i16).wrapping_neg()])
            .flat_map(i16::to_le_bytes)
            .collect()
    }

    fn wave_file(data: &[u8]) -> Vec<u8> {
        let mut out = b"RIFF".to_vec();
        out.extend((36 + data.len() as u32).to_le_bytes());
        out.extend(b"WAVEfmt ");
        out.extend(16u32.to_le_bytes());
        out.extend(1u16.to_le_bytes());
        out.extend(2u16.to_le_bytes());
        out.extend(44_100u32.to_le_bytes());
        out.extend((44_100u32 * 4).to_le_bytes());
        out.extend(4u16.to_le_bytes());
        out.extend(16u16.to_le_bytes());
        out.extend(b"data");
        out.extend((data.len() as u32).to_le_bytes());
        out.extend(data);
        out
    }

    /// GAME.BIN: the volume as raw MODE1/2352 sectors (30), 2 sectors of pregap, then 75 sectors
    /// of audio (track 2); T3.WAV: 37 sectors of audio (track 3).
    fn disc(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("clockwork-cd-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut bin = Vec::new();
        for (i, sector) in iso_volume().chunks(2048).enumerate() {
            let mut raw = vec![0u8; 2352];
            raw[1..11].fill(0xFF);
            let (m, s, f) = msf(i as u32 + FRAME_PADDING);
            raw[12..15].copy_from_slice(&[bcd(m), bcd(s), bcd(f)]);
            raw[15] = 1;
            raw[16..16 + 2048].copy_from_slice(sector);
            bin.extend(raw);
        }
        bin.extend(vec![0u8; 2 * 2352]);
        bin.extend(pcm(75 * 588, 1000));
        std::fs::write(dir.join("GAME.BIN"), bin).unwrap();
        std::fs::write(dir.join("T3.WAV"), wave_file(&pcm(37 * 588, 2000))).unwrap();
        let cue = "FILE \"game.bin\" BINARY\r\n  TRACK 01 MODE1/2352\r\n    INDEX 01 00:00:00\r\n  TRACK 02 AUDIO\r\n    INDEX 00 00:00:30\r\n    INDEX 01 00:00:32\r\nFILE \"T3.WAV\" WAVE\r\n  TRACK 03 AUDIO\r\n    INDEX 01 00:00:00\r\n";
        std::fs::write(dir.join("GAME.CUE"), cue).unwrap();
        dir.join("GAME.CUE")
    }

    #[test]
    fn a_cue_sheet_lays_out_its_tracks_as_dosbox_does() {
        let image = CdImage::open(&disc("layout")).unwrap();
        let layout: Vec<_> = image.tracks.iter().map(|t| (t.number, t.attr, t.start, t.length)).collect();
        assert_eq!(layout, [(1, 0x40, 0, 30), (2, 0, 32, 75), (3, 0, 107, 37), (4, 0, 144, 0)]);
        assert_eq!(&image.read_sector(16, false).unwrap()[1..6], b"CD001");
        let raw = image.read_sector(16, true).unwrap();
        assert_eq!((raw.len(), raw[15], &raw[17..22]), (2352, 1, &b"CD001"[..]));
        assert_eq!(image.read_sector(40, false), None, "audio isn't data");
        // Track 2's frame 10, and track 3's frame 0 (from the WAV).
        let at = |sector: u32, frame: u64| u64::from(sector) * 588 + frame;
        assert_eq!(image.audio(at(32, 10), 1), [1010, -10]);
        assert_eq!(image.audio(at(107, 0), 2), [2000, 0, 2001, -1]);
        assert_eq!(image.audio(at(144, 0), 1), [0, 0], "past the lead-out: silence");
        let files = image.files().unwrap();
        let file = |name: &str| files.iter().find(|(n, _)| n == name).and_then(|(_, c)| c.clone());
        assert_eq!(file("HELLO.TXT").as_deref(), Some(&b"hello disc!"[..]));
        assert_eq!(file("DIR\\INNER.TXT").as_deref(), Some(&b"inner"[..]));
        assert_eq!(image.volume_label().as_deref(), Some("TESTDISC"));
        assert!(CdImage::open(&disc("layout").with_file_name("NONE.CUE")).is_err());
    }

    /// A machine with the image on D: (a program that loops, so time passes).
    fn machine(name: &str) -> Machine {
        let image = CdImage::open(&disc(name)).unwrap();
        let mut drive = Vfs::new();
        drive.insert_file("LOOP.COM", vec![0xEB, 0xFE]);
        let (root, label) = mount(&mut drive, &image).unwrap();
        let config = Config { cd_drive: Some('D'), cd_root: root, cd_label: label, ..Config::default() };
        let mut m = Machine::with_program(config, drive, "LOOP.COM", "").unwrap();
        m.cd_image = Some(std::sync::Arc::new(image));
        m
    }

    const REQUEST: u32 = 0x9000;
    const BUFFER: u32 = 0x9100;

    /// A device request (INT 2Fh AX=1510h): command, and what to put in the header; the status.
    fn request(m: &mut Machine, command: u8, fill: &[(u32, u32)]) -> u16 {
        m.mem.load(REQUEST, &[0; 0x20]);
        m.mem.write_u8(REQUEST + 2, command);
        m.mem.write_u16(REQUEST + 0x0E, (BUFFER & 0xF) as u16);
        m.mem.write_u16(REQUEST + 0x10, (BUFFER >> 4) as u16);
        for &(at, value) in fill {
            m.mem.write_u32(REQUEST + at, value);
        }
        crate::dos::call_service(m, callback::MULTIPLEX, |r| {
            r.set_ax(0x1510);
            r.set_cx(3);
            r.seg[cpu::seg::ES] = (REQUEST >> 4) as u16;
            r.set_bx((REQUEST & 0xF) as u16);
        });
        m.mem.read_u16(REQUEST + 3)
    }

    fn ioctl(m: &mut Machine, code: u8, argument: u8) -> Vec<u8> {
        m.mem.load(BUFFER, &[code, argument, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(request(m, 0x03, &[]) & 0x8000, 0, "IOCTL {code:02X}h");
        (0..11).map(|i| m.mem.read_u8(BUFFER + i)).collect()
    }

    #[test]
    fn mscdex_plays_the_audio_tracks_by_virtual_time() {
        let mut m = machine("play");
        // The files are on D:.
        m.mem.load(0x9200, b"D:\\DIR\\INNER.TXT\0");
        let (r, failed) = crate::dos::call_service(&mut m, callback::DOS, |r| {
            r.set_ax(0x3D00);
            r.seg[cpu::seg::DS] = 0x920;
            r.set_dx(0);
        });
        assert!(!failed, "open D:\\DIR\\INNER.TXT: error {}", r.ax());
        // Tracks 1-3, lead-out at sector 144 (00:03:69 with the 2-second lead-in).
        assert_eq!(&ioctl(&mut m, 0x0A, 0)[1..7], [1, 3, 69, 3, 0, 0]);
        assert_eq!(&ioctl(&mut m, 0x0B, 2)[2..7], [32 + 150 - 150, 2, 0, 0, 0], "track 2 at 00:02:32");
        assert_eq!(ioctl(&mut m, 0x0B, 1)[6], 0x40, "track 1 is data");
        // PLAY track 2 from its start for 75 sectors (one second), in High Sierra addressing.
        assert_eq!(request(&mut m, 0x84, &[(0x0E, 32), (0x12, 75)]) & 0x8300, 0x0300, "done, busy: playing");
        assert_eq!(ioctl(&mut m, 0x06, 0)[2] & 0x04, 0x04, "device status: audio playing");
        m.run(m.cycle_at_ms(500));
        let q = ioctl(&mut m, 0x0C, 0);
        assert_eq!((q[2], q[3]), (0x02, 1), "track 2, index 1");
        let relative = u32::from(q[5]) * 75 + u32::from(q[6]);
        assert!((36..=38).contains(&relative), "half a second in: {q:?}");
        // STOP pauses; RESUME goes on from there.
        assert_eq!(request(&mut m, 0x85, &[]) & 0x0200, 0);
        let status = ioctl(&mut m, 0x0F, 0);
        assert_eq!(status[1], 1, "paused");
        let at = ioctl(&mut m, 0x01, 0);
        let paused_at = u32::from_le_bytes([at[2], at[3], at[4], at[5]]);
        m.run(m.cycle_at_ms(300));
        assert_eq!(u32::from_le_bytes(ioctl(&mut m, 0x01, 0)[2..6].try_into().unwrap()), paused_at, "not moving");
        assert_eq!(request(&mut m, 0x88, &[]) & 0x0200, 0x0200);
        m.run(m.cycle_at_ms(2000));
        assert_eq!(request(&mut m, 0x85, &[]) & 0x0200, 0, "finished");
        assert!(!m.dos.cd_audio.is_playing(m.elapsed_ns()));
        // PLAY on the data track fails.
        assert_ne!(request(&mut m, 0x84, &[(0x0E, 0), (0x12, 10)]) & 0x8000, 0);
        // Raw sector read (READ LONG): the PVD.
        m.mem.load(BUFFER, &[0; 2352]);
        assert_eq!(request(&mut m, 0x80, &[(0x12, 1), (0x14, 16)]) & 0x8000, 0);
        assert_eq!((0..5).map(|i| m.mem.read_u8(BUFFER + 1 + i)).collect::<Vec<_>>(), b"CD001");
    }

    #[test]
    fn the_cd_audio_is_in_the_rendered_sound() {
        let mut m = machine("render");
        m.run(m.cycle_at_ms(100));
        request(&mut m, 0x84, &[(0x0E, 107), (0x12, 37)]); // track 3, half a second
        let started = m.elapsed_ns();
        m.run(m.cycle_at_ms(1000));
        let samples = m.audio_log().render(m.elapsed_ns());
        let frame =
            |ns: u64| ((u128::from(ns) * u128::from(crate::audio::SAMPLE_RATE_HZ)).div_ceil(1_000_000_000)) as usize;
        let (from, to) = (frame(started), frame(started + 493_000_000));
        assert!(samples[..from * 2].iter().all(|&s| s == 0), "silent before the PLAY");
        // Track 3's left channel is 2000 + n: around 2000 at the start, rising.
        let left = |f: usize| samples[f * 2];
        assert!((2000..2010).contains(&left(from + 1)), "{}", left(from + 1));
        assert!(left(to) > left(from + 1000));
        assert!(samples[frame(started + 520_000_000) * 2..].iter().all(|&s| s == 0), "silent after its 37 sectors");
        let hash = m.audio_log().hash();
        assert_ne!(hash, crate::audio::AudioLog::default().hash());
    }
}
