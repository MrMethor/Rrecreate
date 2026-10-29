//! `rrc asset PATH` (D78, D103, D105): a file of the game, or an archive's member (`FILE/MEMBER`), decoded
//! by its format (`knowledge/formats.toml`): an archive's members listed, a picture written as PNG with its
//! palette, a sound as WAV, music as MIDI, the decoded bytes kept in the cache. With `--session`, the check: where the session's original
//! read those bytes (file tracing), and whether its memory holds what this decoded, at moments after.

use std::process::ExitCode;

use serde_json::{Value, json};

use crate::analysis::Analysis;
use crate::args::Args;
use crate::cache::Cache;
use crate::clockwork::Clockwork;
use crate::files::{Access, made_by, traced};
use crate::formats::{FORMATS, Formats, Kind, Opened};
use crate::frames::Picture;
use crate::knowledge::Knowledge;
use crate::places::Places;
use crate::quirks::{When, code_text};
use crate::repo::Repo;
use crate::say;
use crate::session::Session;
use crate::trace::sync_points;
use crate::watch::Names;

const USAGE: &str = "asset FILE[/MEMBER] [--session NAME] [--out FILE] [--lines N]";
/// Members listed.
const SHOWN: usize = 40;
/// The bytes a search of the original's memory starts from.
const ANCHOR: usize = 8;
/// A place in memory and how many bytes there agree.
type Found = Option<(u32, usize)>;

/// The moments after a read when the memory is searched: 1 ms, then 1, 2, 4... ms more, to about 4 s.
const LATER: u32 = 12;

/// Conventional memory with the high memory area; extended memory read at once, and the most searched.
const CONVENTIONAL: u32 = 0x11_0000;
const CHUNK: u32 = 0x10_0000;
const MEMORY: u32 = 64 * CHUNK;

pub(crate) fn asset(repo: &Repo, args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args, &["--session", "--out", "--lines"], &[])?;
    let [path] = a.positionals(1, 1, USAGE)? else { unreachable!() };
    let shown: usize = a.value("--lines").map_or(Ok(SHOWN), str::parse).map_err(|_| "--lines is a count")?;
    let k = Knowledge::load(&repo.knowledge())?;
    let formats = Formats::load(&repo.knowledge())?;
    let game = repo.game_dir()?;
    let opened = formats.open(&k, game, path)?;
    let coded =
        if opened.codecs.is_empty() { String::new() } else { format!(", coded {}", opened.codecs.join(" then ")) };
    let what = match &opened.format {
        Some(f) => format!("{} ({})", f.name, f.kind_text()),
        None => format!("no format in {FORMATS} names it"),
    };
    say(
        "asset",
        format!(
            "{path}: {what}, +{:X} in {}, {} bytes{coded}, {} decoded",
            opened.offset,
            opened.file,
            opened.packed,
            opened.bytes.len()
        ),
    );
    if let Some(header) = &opened.header {
        say("header", header);
    }
    let cache = Cache::open(&repo.local)?;
    say("hash", cache.put(&opened.bytes)?);
    match opened.format.as_ref().map(|f| f.kind) {
        Some(Kind::Archive | Kind::Chunks) => {
            let format = opened.format.as_ref().expect("a format");
            let members = formats.members(format, &k, &opened.bytes)?;
            say("members", members.len());
            for m in members.iter().take(shown) {
                let of = formats.of(&format!("{path}/{}", m.name)).map(|f| format!(", {} ({})", f.name, f.kind_text()));
                say(
                    "member",
                    format!(
                        "{}: +{:X}, {} bytes, {}, {} decoded{}",
                        m.name,
                        m.offset,
                        m.packed,
                        m.codec.codec,
                        m.size,
                        of.unwrap_or_default()
                    ),
                );
            }
            if members.len() > shown {
                say("...", format!("{} more (--lines N)", members.len() - shown));
            }
        }
        Some(Kind::Picture) => {
            let format = opened.format.as_ref().expect("a format");
            let (width, height) = format.dimensions(opened.header.as_ref())?.expect("checked: a picture's size");
            // Its palette: the bytes after its coded ones, or a file's (as that file's format has it).
            let palette = match format.palette.as_deref() {
                Some("after") if opened.after.len() >= 768 => {
                    Some(("its own".to_owned(), opened.after.clone(), format.bits.unwrap_or(6)))
                }
                Some("after") => None,
                Some(p) => {
                    let pal = formats.open(&k, game, p)?;
                    let bits = pal.format.as_ref().and_then(|f| f.bits).unwrap_or(6);
                    Some((p.to_owned(), pal.bytes, bits))
                }
                None => None,
            };
            let picture = picture(&opened.bytes, width, height, palette.as_ref().map(|(_, p, bits)| (&p[..], *bits)))?;
            let out = written(repo, a.value("--out"), path, "png", &picture.png())?;
            let shown_with =
                palette.map_or("grey, as it has no palette".to_owned(), |(p, _, _)| format!("palette {p}"));
            say("picture", format!("{width} × {height}, {shown_with}"));
            say("png", crate::path_text(&out));
        }
        Some(Kind::Palette) => {
            let bits = opened.format.as_ref().and_then(|f| f.bits).unwrap_or(6);
            say("palette", format!("{} colours of {bits} bits", opened.bytes.len() / 3));
        }
        Some(Kind::Map) => {
            let format = opened.format.as_ref().expect("a format");
            if let Some((width, height)) = format.dimensions(opened.header.as_ref())? {
                say("map", format!("{width} × {height}"));
            }
        }
        Some(Kind::Sound) => {
            let rate = opened.rate.ok_or("a sound without its rate: give the format's rate, or code it voc")?;
            let out = written(repo, a.value("--out"), path, "wav", &wav(&opened.bytes, rate))?;
            say(
                "sound",
                format!(
                    "{} samples at {rate} a second, {:.2} s",
                    opened.bytes.len(),
                    opened.bytes.len() as f64 / f64::from(rate)
                ),
            );
            say("wav", crate::path_text(&out));
        }
        Some(Kind::Music) => {
            let sequences = if opened.bytes.starts_with(b"MThd") {
                vec![opened.bytes.clone()]
            } else {
                rrc_runtime::audio::midi_from_xmi(&opened.bytes)?
            };
            say("music", format!("{} sequence{}", sequences.len(), if sequences.len() == 1 { "" } else { "s" }));
            for (i, midi) in sequences.iter().enumerate() {
                let name = if sequences.len() == 1 { path.to_owned() } else { format!("{path}-{}", i + 1) };
                let out = written(repo, a.value("--out").filter(|_| sequences.len() == 1), &name, "mid", midi)?;
                say("midi", crate::path_text(&out));
            }
        }
        Some(Kind::Data) | None => {}
    }
    if let Some(name) = a.value("--session") {
        check(repo, name, &k, &opened)?;
    }
    Ok(ExitCode::SUCCESS)
}

/// An extraction written: to `out`, else to `.rrc/assets/` by its path; where.
fn written(
    repo: &Repo,
    out: Option<&str>,
    path: &str,
    extension: &str,
    bytes: &[u8],
) -> Result<std::path::PathBuf, String> {
    let out = match out {
        Some(out) => std::path::PathBuf::from(out),
        None => repo.state()?.join("assets").join(format!("{}.{extension}", path.replace(['/', '\\'], "-"))),
    };
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(&out, bytes).map_err(|e| format!("{}: {e}", out.display()))?;
    Ok(out)
}

/// Unsigned 8-bit mono samples as a WAV file.
fn wav(samples: &[u8], rate: u32) -> Vec<u8> {
    let len = samples.len() as u32;
    let mut out = b"RIFF".to_vec();
    out.extend((36 + len + (len & 1)).to_le_bytes());
    out.extend(b"WAVEfmt ");
    out.extend(16u32.to_le_bytes());
    // PCM, 1 channel, the rate, as many bytes a second, 1 byte a frame, 8 bits.
    out.extend(1u16.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(rate.to_le_bytes());
    out.extend(rate.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(8u16.to_le_bytes());
    out.extend(b"data");
    out.extend(len.to_le_bytes());
    out.extend(samples);
    if len & 1 == 1 {
        out.push(0);
    }
    out
}

/// A picture's indices in its palette's colours (6 bits to 8), or grey.
fn picture(pixels: &[u8], width: usize, height: usize, palette: Option<(&[u8], u8)>) -> Result<Picture, String> {
    if pixels.len() < width * height {
        return Err(format!("a {width} × {height} picture with {} bytes", pixels.len()));
    }
    let bits = palette.map_or(6, |(_, bits)| bits);
    let expand = |c: u8| if bits == 6 { (c << 2) | (c >> 4) } else { c };
    let rgb = pixels[..width * height]
        .iter()
        .flat_map(|&p| match palette.and_then(|(pal, _)| pal.get(3 * usize::from(p)..3 * usize::from(p) + 3)) {
            Some(c) => [expand(c[0]), expand(c[1]), expand(c[2])],
            None => [p; 3],
        })
        .collect();
    Ok(Picture { width, height, rgb })
}

/// Where the session's original read the asset's bytes, and whether its memory holds them decoded after
/// that (D103): the original is the check of a format, as of the port's loader.
fn check(repo: &Repo, name: &str, k: &Knowledge, opened: &Opened) -> Result<(), String> {
    let session = Session::load(repo, name)?;
    let (accesses, ms_per_cycle) = traced(repo, &session)?;
    let file = opened.file.to_uppercase().replace('\\', "/");
    let (start, end) = (opened.offset, opened.offset + opened.packed);
    let reads: Vec<&Access> = accesses
        .iter()
        .filter(|x| !x.write && x.file.to_uppercase().replace('\\', "/").ends_with(&file))
        .filter(|x| (x.offset as usize) < end && start < x.offset as usize + x.len as usize)
        .collect();
    let Some(first) = reads.first() else {
        say("original", format!("{name} doesn't read +{start:X} to +{end:X} of {} (rrc files {name})", opened.file));
        return Ok(());
    };
    // The read that completes them: all their bytes are read by then.
    let mut covered = vec![false; opened.packed];
    let mut last = first;
    for x in &reads {
        let from = (x.offset as usize).max(start) - start;
        let to = (x.offset as usize + x.len as usize).min(end) - start;
        covered[from..to].fill(true);
        last = x;
        if covered.iter().all(|&c| c) {
            break;
        }
    }
    let when = When::new(repo, &session);
    let tick = when.tick(last.cycle).ok_or_else(|| format!("{name} isn't recorded, or read it after its last tick"))?;
    let names = Names::new(k, repo, &session)?;
    let places = Places::new(repo, &session);
    let analysis = Analysis::load(repo)?;
    let code = |address: &str| code_text(&places, k, analysis.as_ref(), address);
    say("read", format!("{}, by {}", when.text(first.cycle), made_by(first, &names, &code)));

    // The machine at the read, from the last snapshot before its pass; then its memory searched at moments
    // after it, until it holds them all.
    let syncs = sync_points(k)?;
    let length_ms = (session.seconds * 1000.0) as u64;
    let (mut clockwork, _, from) = crate::watch::open(repo, &session, &syncs, tick)?;
    crate::watch::to_tick(&mut clockwork, tick, from.as_ref(), length_ms)?;
    let now = crate::clockwork::number(&clockwork.ask("regs", Value::Null)?, "cycles")?;
    if last.cycle > now {
        clockwork.ask("run", json!({ "cycles": last.cycle - now }))?;
    }
    let wanted = &opened.bytes;
    let mut best: Option<(u64, Seen)> = None;
    let mut after = 0;
    for step in std::iter::once(1).chain((0..LATER).map(|i| 1u64 << i)) {
        clockwork.ask("run", json!({ "ms": step }))?;
        after += step;
        let seen = seen(&mut clockwork, wanted, false)?;
        let whole = seen.whole(wanted.len());
        if best.as_ref().is_none_or(|(_, b)| seen.agrees() > b.agrees()) {
            best = Some((after, seen));
        }
        if whole {
            break;
        }
    }
    // Not all there: extended memory too, at the last moment.
    if best.as_ref().is_some_and(|(_, b)| !b.whole(wanted.len())) {
        let seen = seen(&mut clockwork, wanted, true)?;
        if best.as_ref().is_none_or(|(_, b)| seen.agrees() > b.agrees()) {
            best = Some((after, seen));
        }
    }
    let at_cycle = |ms: u64| last.cycle + (ms as f64 / ms_per_cycle.max(f64::MIN_POSITIVE)) as u64;
    // A place by its named data, in the program's image or outside it.
    let named = |at: u32, n: usize| {
        let address = places.linear_id(at).and_then(|id| places.address(&id.to_string()));
        address.map_or_else(|_| format!("@{at:X}"), |address| names.data(&address, n as u32))
    };
    let Some((ms, seen)) = best else { return Ok(()) };
    let moment = format!("{ms} ms after the read ({})", when.text(at_cycle(ms)));
    let n = wanted.len();
    match seen.found {
        Some((at, m)) if m == n => {
            say("original", format!("equal: {moment}, its memory holds these {n} bytes at {}", named(at, n)))
        }
        _ if seen.rows.is_some() => {
            let (at, row, apart, count) = seen.rows.expect("rows");
            say(
                "original",
                format!(
                    "equal: {moment}, its memory holds these {n} bytes as {count} rows of {row}, {apart} bytes apart, \
                     from {}",
                    named(at, row)
                ),
            )
        }
        Some((at, m)) => say(
            "original",
            format!(
                "differs: {moment}, its memory holds at most the first {m} of these {n} bytes, at {}; byte {m} is \
                 {:02X} there, {:02X} here",
                named(at, m),
                seen.next.unwrap_or(0),
                wanted[m]
            ),
        ),
        None => say(
            "original",
            format!("not found: up to {moment}, its memory nowhere holds the first {} of these bytes", n.min(ANCHOR)),
        ),
    }
    Ok(())
}

/// What a search of the memory saw: the longest start of the bytes (its place and length) and the byte
/// after it there; the rows, if they are all there in rows of that length (a map in a wider grid): the place,
/// the row's length, how far apart, how many.
struct Seen {
    found: Found,
    next: Option<u8>,
    rows: Option<(u32, usize, usize, usize)>,
}

impl Seen {
    fn whole(&self, n: usize) -> bool {
        self.rows.is_some() || self.found.is_some_and(|(_, m)| m == n)
    }

    fn agrees(&self) -> usize {
        if self.rows.is_some() { usize::MAX } else { self.found.map_or(0, |(_, m)| m) }
    }
}

/// The machine's memory by linear address, searched for `wanted`: conventional memory with the high memory
/// area, and with `extended`, extended memory a megabyte at a time, to the first that isn't there or reads all
/// FFh (past the machine's memory).
fn seen(clockwork: &mut Clockwork, wanted: &[u8], extended: bool) -> Result<Seen, String> {
    let mut read = |at: u32, len: u32| -> Result<Option<Vec<u8>>, String> {
        let read = clockwork.ask("read", json!({"ranges": [{"at": format!("@{at:X}"), "len": len}]}))?;
        Ok(read["data"][0].as_str().map(crate::trace::unhex))
    };
    let mut memory = read(0, CONVENTIONAL)?.unwrap_or_default();
    let mut at = CONVENTIONAL;
    while extended && at < MEMORY {
        let Some(bytes) = read(at, CHUNK)? else { break };
        if bytes.iter().all(|&b| b == 0xFF) {
            break;
        }
        memory.extend(bytes);
        at += CHUNK;
    }
    let found = longest(&memory, wanted);
    let next = found.and_then(|(at, m)| memory.get(at as usize + m).copied());
    let rows = match found {
        Some((at, m)) if m < wanted.len() => {
            rows(&memory, wanted, at as usize, m).map(|(apart, count)| (at, m, apart, count))
        }
        _ => None,
    };
    Ok(Seen { found, next, rows })
}

/// `wanted` in rows of `row` bytes from `at`, each the same distance after the last (within 64 KB): that
/// distance and how many rows.
fn rows(memory: &[u8], wanted: &[u8], at: usize, row: usize) -> Option<(usize, usize)> {
    let count = wanted.len().div_ceil(row);
    let second = &wanted[row..(2 * row).min(wanted.len())];
    let holds = |apart: usize| {
        (0..count).all(|k| {
            let part = &wanted[k * row..((k + 1) * row).min(wanted.len())];
            memory.get(at + k * apart..at + k * apart + part.len()) == Some(part)
        })
    };
    (row + 1..=0x10000)
        .filter(|apart| memory.get(at + apart..at + apart + second.len()) == Some(second))
        .find(|&apart| holds(apart))
        .map(|apart| (apart, count))
}

/// Where `memory` holds `wanted`, or the most of its start (at least `ANCHOR` bytes): the place and how
/// many bytes, the first such place.
fn longest(memory: &[u8], wanted: &[u8]) -> Found {
    let anchor = &wanted[..wanted.len().min(ANCHOR)];
    let first = *anchor.first()?;
    let mut best: Found = None;
    let mut i = 0;
    while let Some(p) = memory[i..].iter().position(|&b| b == first) {
        let at = i + p;
        let n = memory[at..].iter().zip(wanted).take_while(|(a, b)| a == b).count();
        if n >= anchor.len() && best.is_none_or(|(_, m)| n > m) {
            best = Some((at as u32, n));
            if n == wanted.len() {
                break;
            }
        }
        i = at + 1;
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_make_a_wav_file() {
        let w = wav(&[0x80, 0x90, 0x70], 11025);
        assert_eq!((&w[..4], &w[8..16], w.len()), (&b"RIFF"[..], &b"WAVEfmt "[..], 44 + 4));
        assert_eq!(u32::from_le_bytes(w[24..28].try_into().unwrap()), 11025);
        assert_eq!((&w[36..40], u32::from_le_bytes(w[40..44].try_into().unwrap())), (&b"data"[..], 3));
    }

    #[test]
    fn the_longest_start_is_found() {
        let memory = b"..ABCDEFGHxx..ABCDEFGHIJ..";
        assert_eq!(longest(memory, b"ABCDEFGHIJ"), Some((14, 10)));
        assert_eq!(longest(memory, b"ABCDEFGHIZ"), Some((14, 9)));
        assert_eq!(longest(memory, b"ABCDEFGZ"), None, "fewer than the anchor's bytes");
        assert_eq!(longest(memory, b"AB"), Some((2, 2)), "a short asset is its own anchor");
    }

    #[test]
    fn a_map_in_a_wider_grid_is_found_by_its_rows() {
        // Three rows of 10 in rows of 16, from 4.
        let wanted: Vec<u8> = (0..30).collect();
        let mut memory = vec![0xEE; 64];
        for k in 0..3 {
            memory[4 + 16 * k..4 + 16 * k + 10].copy_from_slice(&wanted[10 * k..10 * (k + 1)]);
        }
        assert_eq!(longest(&memory, &wanted), Some((4, 10)));
        assert_eq!(rows(&memory, &wanted, 4, 10), Some((16, 3)));
        memory[4 + 32 + 9] = 0;
        assert_eq!(rows(&memory, &wanted, 4, 10), None, "a row that differs");
    }
}
