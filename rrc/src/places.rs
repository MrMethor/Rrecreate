//! A session's address space (D55, D57): where the session's run puts the game's program, and
//! Clockwork's addresses (`+OFF` in the image `+OFF` counts from, `@LINEAR` elsewhere) for IDs and back.
//!
//! The game's program is the module of IDs (`[original] module`, else the program started). A 16-bit
//! one is loaded where `+OFF` counts from, unless a launcher starts it: then the session's run finds
//! where it goes, and Clockwork is told to count `+OFF` from there ([`prepare`]). An LE program's IDs
//! name its objects, which the session's run finds where its extender puts them.

use std::cell::OnceCell;

use ids::Id;
use serde_json::{Value, json};

use crate::cache::{Cache, sha256};
use crate::clockwork::{self, Address, Clockwork};
use crate::knowledge::parse_id;
use crate::repo::Repo;
use crate::session::Session;

/// Whether the game's program is an LE executable (a flat program under a DOS extender), whose IDs
/// name its objects (`GAME.EXE#2+00010`).
pub fn is_le(repo: &Repo) -> bool {
    le_object_sizes(repo).is_some()
}

/// The sizes of the game's program's LE objects, in its object table (`None`: it isn't LE).
pub fn le_object_sizes(repo: &Repo) -> Option<Vec<u32>> {
    let file = std::fs::read(repo.game_dir().ok()?.join(repo.module_file())).ok()?;
    let word = |at: usize| Some(u32::from_le_bytes(file.get(at..at + 4)?.try_into().ok()?) as usize);
    let header = word(0x3C)?;
    if !file.starts_with(b"MZ") || file.get(header..header + 2) != Some(b"LE") {
        return None;
    }
    let (table, count) = (header + word(header + 0x40)?, word(header + 0x44)?);
    (0..count).map(|i| word(table + 24 * i).map(|size| size as u32)).collect()
}

/// Whether a launcher starts the game's program, a 16-bit one: its image is wherever the session's
/// run loads it ([`placement`]), and Clockwork counts `+OFF` from there ([`prepare`]).
pub fn launched_16bit(repo: &Repo) -> bool {
    repo.module() != repo.program_module() && !is_le(repo)
}

/// After Clockwork loaded the session (from power-on or a snapshot): when a launcher starts a 16-bit
/// game, `+OFF` counts from where the run loads the game, and `loaded` (what loading answered) says so.
pub fn prepare(clockwork: &mut Clockwork, loaded: &mut Value, repo: &Repo, session: &Session) -> Result<(), String> {
    if launched_16bit(repo) {
        clockwork.rebase(loaded, placement(repo, session)?.image)?;
    }
    Ok(())
}

/// Where a session's run puts the game's program: its load image (linear base, size) and, for an LE
/// program, its objects (linear base, size and LE flags: 4 executable; in the LE's order).
#[derive(Debug, Clone, PartialEq)]
pub struct Placement {
    pub image: (u32, u32),
    pub objects: Vec<(u32, u32, u32)>,
}

/// Where the session's run puts the game's program: found once, by running the session until the
/// program runs (and its extender has placed its objects), and kept by the run and the build of `rrc`
/// that found it (`index/places/<key>`). Clockwork is deterministic, so every run of the session puts
/// it there. The program is found by name, running or having started the one that runs (an extender's
/// loader), so another program a launcher runs first doesn't count (Daggerfall's runs REPORT.EXE
/// before FALL.EXE).
pub fn placement(repo: &Repo, session: &Session) -> Result<Placement, String> {
    let cache = Cache::open(&repo.local)?;
    let run = crate::snapshots::run_identity(repo, session)?;
    let key = sha256(format!("{run}\n{}\n{}", repo.module_file(), crate::cache::own_build()?).as_bytes());
    let parse = |text: &str| -> Option<Placement> {
        let hex = |t: &str| u32::from_str_radix(t, 16).ok();
        let mut placement = Placement { image: (0, 0), objects: Vec::new() };
        for line in text.lines() {
            let (what, fields) = line.split_once(' ')?;
            let mut fields = fields.split(':').map(hex);
            match what {
                "image" => placement.image = (fields.next()??, fields.next()??),
                "object" => placement.objects.push((fields.next()??, fields.next()??, fields.next()??)),
                _ => return None,
            }
        }
        Some(placement)
    };
    if let Some(placement) = cache.recall("places", &key).as_deref().and_then(parse) {
        return Ok(placement);
    }
    let (mut clockwork, _) = Clockwork::original(repo, &session.options(repo))?;
    let (module, le) = (repo.module(), is_le(repo));
    let length_ms = (session.seconds * 1000.0) as u64;
    let mut ms = 0;
    loop {
        ms = (ms + 50).min(length_ms);
        let ran = clockwork.ask("run", json!({ "until_ms": ms }))?;
        let answer = clockwork.ask("objects", json!({ "program": module }))?;
        let objects = answer["objects"].as_array();
        if !answer["program"].is_null() && (!le || objects.is_some()) {
            let (base, size) = (clockwork::linear(&answer, "image")?, clockwork::number(&answer, "image_size")?);
            let mut text = format!("image {base:X}:{size:X}\n");
            for o in objects.into_iter().flatten() {
                let (at, size, flags) =
                    (clockwork::linear(o, "at")?, clockwork::number(o, "size")?, clockwork::number(o, "flags")?);
                text.push_str(&format!("object {at:X}:{size:X}:{flags:X}\n"));
            }
            cache.remember("places", &key, &text)?;
            return parse(&text).ok_or_else(|| "Clockwork's answer can't be read".into());
        }
        if ms >= length_ms || ran["stop"].as_str() != Some("time limit reached") {
            return Err(match le {
                true => {
                    format!("{module} places no LE objects in this session: IDs with #N are an LE program's objects")
                }
                false => format!("{module} doesn't run in this session"),
            });
        }
    }
}

/// Clockwork's addresses for a session's IDs, and IDs for Clockwork's addresses.
pub(crate) struct Places<'a> {
    repo: &'a Repo,
    session: &'a Session,
    /// The game's program.
    module: String,
    /// The module `+OFF` counts in: the program started, but a 16-bit game's that a launcher starts.
    image_module: String,
    le: bool,
    /// The LE program's objects in the session, once looked up.
    objects: OnceCell<Vec<(u32, u32, u32)>>,
}

impl<'a> Places<'a> {
    pub(crate) fn new(repo: &'a Repo, session: &'a Session) -> Places<'a> {
        let image_module = if launched_16bit(repo) { repo.module() } else { repo.program_module() };
        Places { repo, session, module: repo.module(), image_module, le: is_le(repo), objects: OnceCell::new() }
    }

    /// The LE program's objects in the session (linear base, size, LE flags); none for a 16-bit one.
    pub(crate) fn objects(&self) -> Result<&[(u32, u32, u32)], String> {
        if !self.le {
            return Ok(&[]);
        }
        if self.objects.get().is_none() {
            let found = placement(self.repo, self.session)?.objects;
            let _ = self.objects.set(found);
        }
        Ok(self.objects.get().expect("just set"))
    }

    /// Clockwork's address for `id`: `+OFF` in the image, `@LINEAR` in an object or outside the images.
    pub(crate) fn address(&self, id: &str) -> Result<String, String> {
        let id = parse_id(id)?;
        let objects = if id.part().is_some() && id.module() == self.module { self.objects()? } else { &[] };
        address(&self.module, &id, self.le.then_some(objects))
    }

    /// The ID of a Clockwork address (`+OFF`, `@LINEAR`), if it has one: in the image, in an LE object,
    /// or memory outside them (`MEM+...`). Needs the objects looked up first ([`Places::objects`]).
    pub(crate) fn id(&self, address: &str) -> Option<Id> {
        let objects = self.objects.get().map_or(&[][..], Vec::as_slice);
        id_of(&self.module, &self.image_module, objects, Address::parse(address)?)
    }

    /// Whether `address` is the program's own: in its image, or in an LE program's objects (whose
    /// image is the extender's real-mode part); not memory elsewhere.
    pub(crate) fn in_program(&self, address: &str) -> bool {
        self.id(address)
            .is_some_and(|id| id.module() == self.module && !id.is_memory() && (!self.le || id.part().is_some()))
    }
}

/// Clockwork's address for `id` in `module`, given its LE objects if it is an LE program.
fn address(module: &str, id: &Id, objects: Option<&[(u32, u32, u32)]>) -> Result<String, String> {
    if id.is_memory() {
        return Ok(Address::Linear(id.offset()).to_string());
    }
    if id.module() != module {
        return Err(format!("{id}: not in {module}, the program traced"));
    }
    let Some(n) = id.part() else { return Ok(Address::Image(id.offset()).to_string()) };
    let Some(objects) = objects else { return Err(format!("{id}: {module} has no objects")) };
    match usize::from(n).checked_sub(1).and_then(|i| objects.get(i)) {
        Some(&(base, size, _)) if id.offset() < size => Ok(Address::Linear(base.wrapping_add(id.offset())).to_string()),
        Some(&(_, size, _)) => Err(format!("{id}: past object {n}'s end ({size:X}h bytes)")),
        None => Err(format!("{id}: the program has {} objects", objects.len())),
    }
}

/// The ID of `address` in `module`, whose LE objects are `objects` (none for a 16-bit program), and
/// whose `+OFF` counts in `image_module`'s image.
fn id_of(module: &str, image_module: &str, objects: &[(u32, u32, u32)], address: Address) -> Option<Id> {
    match address {
        Address::Linear(linear) => {
            let object = objects.iter().position(|&(base, size, _)| linear.wrapping_sub(base) < size);
            match object {
                Some(i) => Id::new(module, Some(i as u16 + 1), linear - objects[i].0).ok(),
                None => Some(Id::memory(linear)),
            }
        }
        Address::Image(offset) => Id::new(image_module, None, offset).ok(),
        Address::Physical(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OBJECTS: [(u32, u32, u32); 2] = [(0x435000, 0x3181, 0x2045), (0x439000, 0x10590, 0x2043)];

    #[test]
    fn ids_are_clockwork_s_addresses() {
        let at = |id: &str, objects: Option<&[(u32, u32, u32)]>| address("GAME.EXE", &id.parse().unwrap(), objects);
        assert_eq!(at("GAME.EXE+20002", None).unwrap(), "+20002");
        assert_eq!(at("MEM+0046C", None).unwrap(), "@46C");
        assert_eq!(at("GAME.EXE#2+00010", Some(&OBJECTS)).unwrap(), "@439010", "in object 2");
        assert!(at("GAME.EXE#1+03181", Some(&OBJECTS)).is_err(), "past its end");
        assert!(at("GAME.EXE#3+00010", Some(&OBJECTS)).is_err() && at("GAME.EXE#1+00010", None).is_err());
        assert!(at("OTHER.EXE+20002", None).is_err());
    }

    #[test]
    fn clockwork_s_addresses_are_ids() {
        let id = |address: &str, objects: &[(u32, u32, u32)], image: &str| {
            id_of("GAME.EXE", image, objects, Address::parse(address).unwrap()).map(|id| id.to_string())
        };
        assert_eq!(id("@439010", &OBJECTS, "GAME.EXE").as_deref(), Some("GAME.EXE#2+00010"));
        assert_eq!(id("@46C", &OBJECTS, "GAME.EXE").as_deref(), Some("MEM+0046C"));
        assert_eq!(id("+00120", &OBJECTS, "GAME.EXE").as_deref(), Some("GAME.EXE+00120"));
        assert_eq!(id("+00120", &[], "LAUNCH.EXE").as_deref(), Some("LAUNCH.EXE+00120"), "the launcher's image");
        assert_eq!(id("%1000", &[], "GAME.EXE"), None, "physical memory has no ID");
    }
}
