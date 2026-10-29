//! The vault game of `testbins` (`testbins/src/vault.c`), ported from power-on (M5): its data read from
//! the person's copy of the game (`VAULT.DAT`, through `rrc_runtime::original::file`) and decoded with the
//! toolkit's codecs, its two loops compared as one session (DESIGN.md §12).
//!
//! Before its first tick the original reads the archive's directory, takes the map's and the picture's
//! buffers from the heap and shows the title, its palette and picture loaded: `start`. Then a tick begins
//! where a loop's `wait_tick` returns: at `title` (VAULT.EXE+00744) a step is `title_pass`, at `play`
//! (VAULT.EXE+0075A) `play_pass` (the play loop's `draw` only writes the screen, which this port doesn't
//! show). The inputs are the timer count (INT 08h's, 70 Hz) and the key states (INT 09h's).
//!
//! Each function names the original's it reimplements by its ID (DESIGN.md §6.6), which `rrc compare`
//! finds when that one wrote a value the port has otherwise.

use rrc_runtime::codecs::{Lzss, runs};
use rrc_runtime::original::file;
use rrc_runtime::{Simulation, Tick};
use serde::{Deserialize, Serialize};

const ARCHIVE: &str = "VAULT.DAT";
const CELLS: usize = 32;
const WALL: u8 = 1;
const EXIT: u8 = 2;
const START: u8 = 3;
const LEVELS: u8 = 2;
const PICTURE: usize = 320 * 200;
const ENTRY: usize = 24;

const KEY_ENTER: usize = 0x1C;
/// The arrow keys, in the order a pass looks at them: left, right, up, down.
const ARROWS: [(usize, i8, i8); 4] = [(0x4B, -1, 0), (0x4D, 1, 0), (0x48, 0, -1), (0x50, 0, 1)];

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Vault {
    last_count: u16,
    pass_count: u16,
    /// 0 the title, 1 play, 2 over.
    scene: u8,
    title_passes: u16,
    level: u8,
    player: Player,
    moves: u16,
    bumps: u16,
    /// Bit per arrow key (left, right, up, down) down at the last pass.
    keys_held: u8,
    /// Members loaded.
    loads: u8,
    entries: u16,
    directory: Vec<Entry>,
    palette: Vec<u8>,
    /// The level's cells, by row, once a level is loaded; on the original's heap, which holds the heap's
    /// leftovers until then.
    map: Option<Vec<u8>>,
    /// The title, 320 x 200; on the original's heap.
    picture: Vec<u8>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Player {
    x: u8,
    y: u8,
}

/// A member of the archive: its name (NUL-padded), where its bytes are, how many, how many decoded, and
/// how they are coded (0 stored, 1 run-length, 2 LZSS).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Entry {
    name: [u8; 8],
    offset: u32,
    packed: u32,
    size: u32,
    method: u16,
    reserved: u16,
}

impl Entry {
    fn read(b: &[u8]) -> Entry {
        let dword = |at: usize| u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
        Entry {
            name: b[..8].try_into().unwrap(),
            offset: dword(8),
            packed: dword(12),
            size: dword(16),
            method: u16::from_le_bytes([b[20], b[21]]),
            reserved: u16::from_le_bytes([b[22], b[23]]),
        }
    }
}

/// Where a member goes.
enum Into {
    Palette,
    Map,
    Picture,
}

impl Vault {
    /// `load` (VAULT.EXE+0021F): member `name` decoded into its buffer, as many bytes as the buffer holds;
    /// nothing if the archive hasn't it.
    fn load(&mut self, name: &str, into: Into) -> bool {
        let mut wanted = [0; 8];
        wanted[..name.len()].copy_from_slice(name.as_bytes());
        let Some(e) = self.directory.iter().take(usize::from(self.entries)).find(|e| e.name == wanted) else {
            return false;
        };
        let archive = file(ARCHIVE).unwrap_or_else(|e| panic!("{e}"));
        let start = e.offset as usize;
        let packed = archive.get(start..(start + e.packed as usize).min(archive.len())).unwrap_or(&[]);
        let to = match into {
            Into::Palette => &mut self.palette,
            Into::Map => self.map.get_or_insert_with(|| vec![0; CELLS * CELLS]),
            Into::Picture => &mut self.picture,
        };
        let size = to.len();
        let decoded = match e.method {
            1 => runs(packed, size),
            2 => Lzss::OKUMURA.decode(packed, size),
            _ => packed[..packed.len().min(size)].to_vec(),
        };
        to[..decoded.len()].copy_from_slice(&decoded);
        self.loads = self.loads.wrapping_add(1);
        true
    }

    /// `load_level` (VAULT.EXE+00313): the level's map, the player on its start cell.
    fn load_level(&mut self) {
        let name = format!("LEVEL{}", self.level);
        self.load(&name, Into::Map);
        for (i, &cell) in self.map.iter().flatten().enumerate() {
            if cell == START {
                self.player = Player { x: (i % CELLS) as u8, y: (i / CELLS) as u8 };
            }
        }
    }

    /// `title_pass` (VAULT.EXE+00393): Enter starts the first level.
    fn title_pass(&mut self, tick: &Tick) {
        self.last_count = tick.inputs.u16("timer_count");
        self.pass_count = self.pass_count.wrapping_add(1);
        self.title_passes = self.title_passes.wrapping_add(1);
        if tick.inputs.u8_at("key_down", KEY_ENTER) != 0 {
            self.level = 1;
            self.load_level();
            self.scene = 1;
        }
    }

    /// `play_pass` (VAULT.EXE+003BC): a cell for each arrow key pressed since the last pass; a wall stops
    /// it and counts; the exit loads the next level, or ends the game after the last.
    fn play_pass(&mut self, tick: &Tick) {
        self.last_count = tick.inputs.u16("timer_count");
        self.pass_count = self.pass_count.wrapping_add(1);
        let mut held = 0;
        for (k, &(scan, dx, dy)) in ARROWS.iter().enumerate() {
            if tick.inputs.u8_at("key_down", scan) == 0 {
                continue;
            }
            held |= 1 << k;
            if self.keys_held & (1 << k) != 0 {
                continue;
            }
            let x = self.player.x.wrapping_add_signed(dx);
            let y = self.player.y.wrapping_add_signed(dy);
            let map = self.map.as_ref().expect("a level is loaded in play");
            let cell = map[usize::from(y) * CELLS + usize::from(x)];
            if cell == WALL {
                self.bumps = self.bumps.wrapping_add(1);
                continue;
            }
            self.player = Player { x, y };
            self.moves = self.moves.wrapping_add(1);
            if cell == EXIT {
                if self.level < LEVELS {
                    self.level += 1;
                    self.load_level();
                } else {
                    self.scene = 2;
                }
                break;
            }
        }
        self.keys_held = held;
    }
}

impl Simulation for Vault {
    /// `main` (VAULT.EXE+00637) before its first tick: the archive's directory, the buffers, the title.
    fn start() -> Vault {
        let archive = file(ARCHIVE).unwrap_or_else(|e| panic!("{e}"));
        let entries = u16::from_le_bytes([archive[4], archive[5]]);
        let mut directory = vec![Entry::default(); 8];
        for (i, e) in directory.iter_mut().enumerate().take(usize::from(entries)) {
            *e = Entry::read(&archive[6 + i * ENTRY..6 + (i + 1) * ENTRY]);
        }
        let mut vault =
            Vault { entries, directory, palette: vec![0; 768], picture: vec![0; PICTURE], ..Vault::default() };
        vault.load("PALETTE", Into::Palette);
        vault.load("TITLE", Into::Picture);
        vault
    }

    fn step(&mut self, tick: &Tick) {
        match tick.sync {
            "title" => self.title_pass(tick),
            "play" => self.play_pass(tick),
            _ => {}
        }
    }
}

fn main() -> std::process::ExitCode {
    rrc_runtime::main::<Vault>()
}
