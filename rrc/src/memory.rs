//! Each tick's memory of a session's run, kept for `rrc search` (D76): what a search reads (the data
//! segment, an LE program's data objects, or a range given) at every tick of a span, as the first
//! tick's bytes and then each tick's changes, deflated, in the cache. A search whose ticks a kept span
//! covers reads them from there and runs no emulator; another reads its span from Clockwork and keeps
//! it. Spans are kept by the run (Clockwork's sources, the game, the session: D76), the sync points that
//! number its ticks, and what was asked to be read.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};

use crate::cache::{Cache, sha256};
use crate::repo::Repo;
use crate::session::Session;
use crate::trace::SyncPoint;

const MAGIC: &[u8] = b"RRCMEM1\n";
/// A span is kept only if it is at most this many bytes read in all (ranges times ticks): more is
/// read tick by tick and dropped.
pub const MAX_KEPT: u64 = 256 << 20;
/// Changed bytes this close are one change.
const GAP: usize = 8;

/// A kept span: its ticks, the ranges read (Clockwork's addresses, sizes) and its memory in the cache.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Span {
    pub first: u64,
    pub last: u64,
    pub ranges: Vec<(String, u32)>,
    pub hash: String,
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct Index {
    #[serde(default, rename = "span")]
    spans: Vec<Span>,
}

/// The key of a session's kept memory: its run, its sync points, and what is read (`data`, or the
/// range asked for).
pub fn key(repo: &Repo, session: &Session, syncs: &[SyncPoint], what: &str) -> Result<String, String> {
    let run = crate::snapshots::run_identity(repo, session)?;
    Ok(sha256(format!("memory 1\n{run}\n{}\n{what}", serde_json::json!(syncs)).as_bytes()))
}

/// A kept span holding ticks `first` to `last`, if one is.
pub fn kept(repo: &Repo, key: &str, first: u64, last: u64) -> Result<Option<Span>, String> {
    let cache = Cache::open(&repo.local)?;
    let Some(text) = cache.recall("memory", key) else { return Ok(None) };
    let index: Index = toml::from_str(&text).unwrap_or_default();
    Ok(index.spans.into_iter().find(|s| s.first <= first && last <= s.last && cache.has(&s.hash)))
}

/// Keeps a span: `memory` has each tick's ranges from `first` on (none where memory isn't mapped).
pub fn keep(
    repo: &Repo,
    key: &str,
    first: u64,
    ranges: &[(String, u32)],
    memory: &[Vec<Option<Vec<u8>>>],
) -> Result<Span, String> {
    let cache = Cache::open(&repo.local)?;
    let mut z = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
    z.write_all(&encode(memory)).map_err(|e| e.to_string())?;
    let hash = cache.put(&z.finish().map_err(|e| e.to_string())?)?;
    let span = Span { first, last: first + memory.len() as u64 - 1, ranges: ranges.to_vec(), hash };
    let mut index: Index = cache.recall("memory", key).and_then(|t| toml::from_str(&t).ok()).unwrap_or_default();
    index.spans.retain(|s| !(span.first <= s.first && s.last <= span.last));
    index.spans.push(span.clone());
    cache.remember("memory", key, &toml::to_string(&index).map_err(|e| e.to_string())?)?;
    Ok(span)
}

/// The kept span's memory at the ticks `wanted`: each range's bytes (empty where memory wasn't mapped).
pub fn load(repo: &Repo, span: &Span, wanted: &[u64]) -> Result<BTreeMap<u64, Vec<Vec<u8>>>, String> {
    let cache = Cache::open(&repo.local)?;
    let mut raw = Vec::new();
    flate2::read::DeflateDecoder::new(&cache.get(&span.hash)?[..])
        .read_to_end(&mut raw)
        .map_err(|e| format!("kept memory: {e}"))?;
    let wanted: BTreeSet<u64> = wanted.iter().copied().collect();
    let mut found = BTreeMap::new();
    decode(&raw, span.ranges.len(), |i, memory| {
        let tick = span.first + i as u64;
        if wanted.contains(&tick) {
            found.insert(tick, memory.iter().map(|m| m.clone().unwrap_or_default()).collect());
        }
    })?;
    Ok(found)
}

/// Each tick's ranges as bytes: the first tick whole, then what changed.
fn encode(memory: &[Vec<Option<Vec<u8>>>]) -> Vec<u8> {
    let ranges = memory.first().map_or(0, Vec::len);
    let mut out = MAGIC.to_vec();
    put(&mut out, ranges);
    put(&mut out, memory.len());
    let mut before: Vec<Option<&Vec<u8>>> = vec![None; ranges];
    for tick in memory {
        for (r, now) in tick.iter().enumerate().take(ranges) {
            match (before[r], now) {
                (_, None) => out.push(1),
                (Some(old), Some(new)) if old == new => out.push(0),
                (Some(old), Some(new)) if old.len() == new.len() => {
                    out.push(3);
                    let runs = changes(old, new);
                    put(&mut out, runs.len());
                    for (at, len) in runs {
                        put(&mut out, at);
                        put(&mut out, len);
                        out.extend_from_slice(&new[at..at + len]);
                    }
                }
                (_, Some(new)) => {
                    out.push(2);
                    put(&mut out, new.len());
                    out.extend_from_slice(new);
                }
            }
            before[r] = now.as_ref();
        }
    }
    out
}

/// Where `new` differs from `old`: (offset, length), runs closer than [`GAP`] joined.
fn changes(old: &[u8], new: &[u8]) -> Vec<(usize, usize)> {
    let mut runs: Vec<(usize, usize)> = Vec::new();
    for i in (0..new.len()).filter(|&i| old[i] != new[i]) {
        match runs.last_mut() {
            Some((at, len)) if i < *at + *len + GAP => *len = i + 1 - *at,
            _ => runs.push((i, 1)),
        }
    }
    runs
}

/// Each tick's ranges from [`encode`]'s bytes, given to `each` in order (none where unmapped).
fn decode(bytes: &[u8], ranges: usize, mut each: impl FnMut(usize, &[Option<Vec<u8>>])) -> Result<(), String> {
    if !bytes.starts_with(MAGIC) {
        return Err(damaged());
    }
    let mut c = Cursor { bytes, at: MAGIC.len() };
    if c.number()? != ranges {
        return Err(damaged());
    }
    let ticks = c.number()?;
    let mut now: Vec<Option<Vec<u8>>> = vec![None; ranges];
    for tick in 0..ticks {
        for range in now.iter_mut() {
            match c.take(1)?[0] {
                0 => {}
                1 => *range = None,
                2 => {
                    let len = c.number()?;
                    *range = Some(c.take(len)?.to_vec());
                }
                3 => {
                    let memory = range.as_mut().ok_or_else(damaged)?;
                    for _ in 0..c.number()? {
                        let (offset, len) = (c.number()?, c.number()?);
                        let new = c.take(len)?;
                        memory.get_mut(offset..offset + len).ok_or_else(damaged)?.copy_from_slice(new);
                    }
                }
                _ => return Err(damaged()),
            }
        }
        each(tick, &now);
    }
    Ok(())
}

fn damaged() -> String {
    "kept memory is damaged".into()
}

/// Reads [`encode`]'s bytes in order.
struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let slice = self.bytes.get(self.at..self.at + n).ok_or_else(damaged)?;
        self.at += n;
        Ok(slice)
    }

    fn number(&mut self) -> Result<usize, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().map_err(|_| damaged())?) as usize)
    }
}

fn put(out: &mut Vec<u8>, n: usize) {
    out.extend_from_slice(&(n as u32).to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_reads_back_tick_by_tick() {
        let tick = |a: &[u8], b: Option<&[u8]>| vec![Some(a.to_vec()), b.map(<[u8]>::to_vec)];
        let mut big = vec![0u8; 100];
        let memory =
            vec![tick(&[1, 2, 3, 4], Some(&big)), tick(&[1, 2, 3, 4], None), tick(&[1, 9, 3, 8], Some(&big)), {
                big[3] = 7;
                big[90] = 1;
                tick(&[1, 9, 3, 8], Some(&big))
            }];
        let bytes = encode(&memory);
        let mut back = Vec::new();
        decode(&bytes, 2, |_, now| back.push(now.to_vec())).unwrap();
        assert_eq!(back, memory);
        assert_eq!(changes(&[0; 20], &[0, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]), [(1, 4), (19, 1)]);
        assert!(decode(&bytes[..bytes.len() - 1], 2, |_, _| {}).is_err());
        assert!(decode(&bytes, 3, |_, _| {}).is_err());
    }
}
