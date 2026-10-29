//! `rrc asset PATH` (D78, D103): a file of the game, or an archive's member (`FILE/MEMBER`), decoded by
//! its format (`knowledge/formats.toml`): an archive's members listed, a picture written as PNG with its
//! palette, the decoded bytes kept in the cache. With `--session`, the check: where the session's original
//! read those bytes (file tracing), and whether its memory holds what this decoded, at the tick after.

use std::process::ExitCode;

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

const USAGE: &str = "asset FILE[/MEMBER] [--session NAME] [--out FILE.png] [--lines N]";
/// Members listed.
const SHOWN: usize = 40;
/// The bytes a search of the original's memory starts from.
const ANCHOR: usize = 8;
/// A place in memory and how many bytes there agree.
type Found = Option<(u32, usize)>;

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
            let palette = match &format.palette {
                Some(p) => Some((p.as_str(), formats.open(&k, game, p)?)),
                None => None,
            };
            let picture = picture(&opened.bytes, width, height, palette.as_ref().map(|(_, p)| p))?;
            let out = match a.value("--out") {
                Some(out) => std::path::PathBuf::from(out),
                None => repo.state()?.join("assets").join(format!("{}.png", path.replace(['/', '\\'], "-"))),
            };
            if let Some(dir) = out.parent() {
                std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            }
            std::fs::write(&out, picture.png()).map_err(|e| format!("{}: {e}", out.display()))?;
            let shown_with = palette.map_or("grey, as no palette is named".to_owned(), |(p, _)| format!("palette {p}"));
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
        Some(Kind::Data) | None => {}
    }
    if let Some(name) = a.value("--session") {
        check(repo, name, &k, &opened)?;
    }
    Ok(ExitCode::SUCCESS)
}

/// A picture's indices in its palette's colours (6 bits to 8), or grey.
fn picture(pixels: &[u8], width: usize, height: usize, palette: Option<&Opened>) -> Result<Picture, String> {
    if pixels.len() < width * height {
        return Err(format!("a {width} × {height} picture with {} bytes", pixels.len()));
    }
    let bits = palette.and_then(|p| p.format.as_ref()).and_then(|f| f.bits).unwrap_or(6);
    let expand = |c: u8| if bits == 6 { (c << 2) | (c >> 4) } else { c };
    let rgb = pixels[..width * height]
        .iter()
        .flat_map(|&p| match palette.and_then(|pal| pal.bytes.get(3 * usize::from(p)..3 * usize::from(p) + 3)) {
            Some(c) => [expand(c[0]), expand(c[1]), expand(c[2])],
            None => [p; 3],
        })
        .collect();
    Ok(Picture { width, height, rgb })
}

/// Where the session's original read the asset's bytes, and whether its memory holds them decoded at the
/// tick after (D103): the original is the check of a format, as of the port's loader.
fn check(repo: &Repo, name: &str, k: &Knowledge, opened: &Opened) -> Result<(), String> {
    let session = Session::load(repo, name)?;
    let (accesses, _) = traced(repo, &session)?;
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

    let syncs = sync_points(k)?;
    let length_ms = (session.seconds * 1000.0) as u64;
    let (mut clockwork, _, from) = crate::watch::open(repo, &session, &syncs, tick + 1)?;
    crate::watch::to_tick(&mut clockwork, tick + 1, from.as_ref(), length_ms)?;
    let wanted = &opened.bytes;
    let (memory, found) = searched(&mut clockwork, wanted)?;
    // A place by its named data, in the program's image or outside it.
    let named = |at: u32, n: usize| {
        let address = places.linear_id(at).and_then(|id| places.address(&id.to_string()));
        address.map_or_else(|_| format!("@{at:X}"), |address| names.data(&address, n as u32))
    };
    match found {
        Some((at, n)) if n == wanted.len() => {
            say("original", format!("equal: at tick {tick} its memory holds these {n} bytes at {}", named(at, n)))
        }
        Some((at, n)) => say(
            "original",
            format!(
                "differs: at tick {tick} its memory holds the first {n} of these {} bytes at {}; byte {n} is {:02X} \
                 there, {:02X} here",
                wanted.len(),
                named(at, n),
                memory[at as usize + n],
                wanted[n]
            ),
        ),
        None => say(
            "original",
            format!(
                "not found: at tick {tick} its memory nowhere holds the first {} of these bytes",
                wanted.len().min(ANCHOR)
            ),
        ),
    }
    Ok(())
}

/// The machine's memory by linear address, searched for `wanted` ([`longest`]): conventional memory with
/// the high memory area, then, if they aren't all there, extended memory a megabyte at a time, to the first
/// that isn't there or reads all FFh (past the machine's memory).
fn searched(clockwork: &mut Clockwork, wanted: &[u8]) -> Result<(Vec<u8>, Found), String> {
    let mut read = |at: u32, len: u32| -> Result<Option<Vec<u8>>, String> {
        let read = clockwork.ask("read", serde_json::json!({"ranges": [{"at": format!("@{at:X}"), "len": len}]}))?;
        Ok(read["data"][0].as_str().map(crate::trace::unhex))
    };
    let mut memory = read(0, CONVENTIONAL)?.unwrap_or_default();
    let mut found = longest(&memory, wanted);
    if found.is_some_and(|(_, n)| n == wanted.len()) {
        return Ok((memory, found));
    }
    let mut at = CONVENTIONAL;
    while at < MEMORY {
        let Some(bytes) = read(at, CHUNK)? else { break };
        if bytes.iter().all(|&b| b == 0xFF) {
            break;
        }
        memory.extend(bytes);
        at += CHUNK;
    }
    if memory.len() > CONVENTIONAL as usize {
        found = longest(&memory, wanted);
    }
    Ok((memory, found))
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
    fn the_longest_start_is_found() {
        let memory = b"..ABCDEFGHxx..ABCDEFGHIJ..";
        assert_eq!(longest(memory, b"ABCDEFGHIJ"), Some((14, 10)));
        assert_eq!(longest(memory, b"ABCDEFGHIZ"), Some((14, 9)));
        assert_eq!(longest(memory, b"ABCDEFGZ"), None, "fewer than the anchor's bytes");
        assert_eq!(longest(memory, b"AB"), Some((2, 2)), "a short asset is its own anchor");
    }
}
