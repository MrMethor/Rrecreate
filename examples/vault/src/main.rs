//! The vault game of `testbins` (`testbins/src/vault.c`), ported from power-on (M5): its data read from
//! the person's copy of the game (`VAULT.DAT`, through `rrc_runtime::original::file`) and decoded with the
//! toolkit's codecs, its two loops compared as one session (DESIGN.md §12).
//!
//! Before its first tick the original reads the archive's directory, takes the map's and the picture's
//! buffers from the heap and shows the title, its palette and picture loaded: `start`. Then a tick begins
//! where a loop's `wait_tick` returns: at `title` (VAULT.EXE+00744) a step is `title_pass`, at `play`
//! (VAULT.EXE+0075A) `play_pass`. The inputs are the timer count (INT 08h's, 70 Hz) and the key states
//! (INT 09h's).
//!
//! What it shows (M6, D107) is drawn apart from the game: the simulation's scene (the title's picture and
//! palette, the level's map and the player) and `Screen`, which draws it as `show_title` and the play loop's
//! `draw` draw it, its own record of what it drew as theirs is (`drawn_level`, `drawn_x`, `drawn_y`).
//! `VAULT_BUG=colour` leaves a bug in: walls are drawn in colour 41.
//!
//! Each function names the original's it reimplements by its ID (DESIGN.md §6.6), which `rrc compare`
//! finds when that one wrote a value the port has otherwise.

use rrc_runtime::codecs::{Lzss, runs};
use rrc_runtime::original::file;
use rrc_runtime::present::{Frame, Presentation, Shows};
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

/// What there is to draw: the title's palette and picture, the scene (0 the title, 1 play), the level's map
/// and the player.
struct Scene<'a> {
    palette: &'a [u8],
    picture: &'a [u8],
    scene: u8,
    level: u8,
    map: Option<&'a [u8]>,
    player: (u8, u8),
}

impl Shows for Vault {
    type Scene<'a> = Scene<'a>;

    fn scene(&self) -> Scene<'_> {
        Scene {
            palette: &self.palette,
            picture: &self.picture,
            scene: self.scene,
            level: self.level,
            map: self.map.as_deref(),
            player: (self.player.x, self.player.y),
        }
    }
}

/// The screen, mode 13h: the level's cells are 6 × 6, the map's top left at (64, 4).
#[derive(Default)]
struct Screen {
    /// What `draw` last drew: the level, and the player's cell.
    drawn_level: u8,
    drawn: (u8, u8),
}

impl Screen {
    /// `draw_cell` (VAULT.EXE+004DD): a cell in its colour (floor and start 16, a wall 40, the exit 47), the
    /// player's 15.
    fn cell(&self, scene: &Scene, frame: &mut Frame, x: u8, y: u8) {
        const COLOURS: [u8; 4] = [16, 40, 47, 16];
        let map = scene.map.expect("a level is loaded in play");
        let colour = match map[usize::from(y) * CELLS + usize::from(x)] & 3 {
            _ if (x, y) == scene.player => 15,
            1 if std::env::var("VAULT_BUG").is_ok_and(|b| b == "colour") => 41,
            cell => COLOURS[usize::from(cell)],
        };
        for j in 0..6 {
            for i in 0..6 {
                frame.set(usize::from(x) * 6 + i + 64, usize::from(y) * 6 + j + 4, colour);
            }
        }
    }
}

impl Presentation<Vault> for Screen {
    /// `show_title` (VAULT.EXE+0045D): the palette, then the picture. From a given screen, what `draw` drew
    /// last isn't known: its next pass draws the whole level, which leaves the same pixels.
    fn start(&mut self, scene: &Scene, frame: &mut Frame, given: bool) {
        if given {
            return;
        }
        frame.palette.copy_from_slice(scene.palette);
        frame.pixels.copy_from_slice(scene.picture);
    }

    /// `draw` (VAULT.EXE+0058A), after each play pass: the level's cells when it changes, else the player's
    /// old and new cell.
    fn draw(&mut self, pass: &str, scene: &Scene, frame: &mut Frame) {
        if pass != "play" || scene.scene != 1 {
            return;
        }
        if self.drawn_level != scene.level {
            for y in 0..CELLS as u8 {
                for x in 0..CELLS as u8 {
                    self.cell(scene, frame, x, y);
                }
            }
            self.drawn_level = scene.level;
        } else if self.drawn != scene.player {
            self.cell(scene, frame, self.drawn.0, self.drawn.1);
            self.cell(scene, frame, scene.player.0, scene.player.1);
        }
        self.drawn = scene.player;
    }
}

fn main() -> std::process::ExitCode {
    rrc_runtime::main_drawn::<Vault, Screen>()
}
