//! The toy game of `testbins` (`testbins/src/toy.c`), ported: the game's state and its pass, as a
//! simulation `rrc compare` checks against the original tick by tick (DESIGN.md §6, §7).
//!
//! A tick begins where the original's `wait_tick` returns. Its inputs are the timer count (INT 08h's,
//! 70 Hz) and the key states (INT 09h's); one step is `update`. The original's `draw` only writes the
//! screen, which this port doesn't show. Arithmetic is the original's: 16-bit, wrapping. The
//! original's pointer to the nearest enemy is an index here, its trail on the heap an array, and its
//! status bits a structure (mapping.toml).
//!
//! `TOY_BUG=1` leaves a bug in: a bounce doesn't count; `TOY_BUG=trail`, another: the trail keeps y.
//! `rrc`'s tests use them to see a divergence found, and who wrote the original's value.

use rrc_runtime::{Simulation, Tick};
use serde::{Deserialize, Serialize};

const ENEMIES: usize = 6;
const FIELD_W: i16 = 320 * 16;
const FIELD_H: i16 = 200 * 16;
const MAX_ELAPSED: u16 = 4;
const PLAYER_SPEED: u16 = 24;
const BOMB_RADIUS: i32 = 40 * 16;
const HIT_RANGE: i16 = 8 * 16;
const TRAIL: usize = 32;

const KEY_LEFT: usize = 0x4B;
const KEY_RIGHT: usize = 0x4D;
const KEY_UP: usize = 0x48;
const KEY_DOWN: usize = 0x50;
const KEY_SPACE: usize = 0x39;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Toy {
    last_count: u16,
    elapsed: u16,
    pass_count: u16,
    player: Player,
    score: u16,
    lives: u8,
    bombs: u8,
    bomb_held: u8,
    game_over: u8,
    rng_seed: u32,
    enemies: [Enemy; ENEMIES],
    /// The nearest enemy alive.
    target: Option<usize>,
    /// The player's x at the last passes, by pass count.
    trail: [i16; TRAIL],
    status: Status,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Status {
    moving: bool,
    /// 0 left, 1 right, 2 up, 3 down: the last way the player moved.
    facing: u8,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Player {
    x: i16,
    y: i16,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Enemy {
    x: i16,
    y: i16,
    dx: i16,
    dy: i16,
    alive: u8,
    hits: u8,
}

impl Toy {
    /// The generator: a 32-bit linear congruential one, its high word modulo `range`.
    fn random(&mut self, range: u16) -> u16 {
        self.rng_seed = self.rng_seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        (self.rng_seed >> 16) as u16 % range
    }

    fn spawn(&mut self, i: usize) {
        let x = self.random(320).wrapping_mul(16) as i16;
        let y = self.random(100).wrapping_mul(16) as i16;
        let dx = match self.random(9) as i16 - 4 {
            0 => 3,
            dx => dx,
        };
        let dy = self.random(5) as i16 + 1;
        self.enemies[i] = Enemy { x, y, dx, dy, alive: 1, hits: 0 };
    }
}

impl Simulation for Toy {
    fn start() -> Toy {
        let mut toy = Toy {
            player: Player { x: FIELD_W / 2, y: FIELD_H - 32 },
            lives: 3,
            bombs: 3,
            rng_seed: 20_261_026,
            ..Toy::default()
        };
        for i in 0..ENEMIES {
            toy.spawn(i);
        }
        toy
    }

    fn step(&mut self, tick: &Tick) {
        let timer_count = tick.inputs.u16("timer_count");
        let key = |code| tick.inputs.u8_at("key_down", code) != 0;

        self.elapsed = timer_count.wrapping_sub(self.last_count).min(MAX_ELAPSED);
        self.last_count = timer_count;
        self.pass_count = self.pass_count.wrapping_add(1);

        let moved = PLAYER_SPEED.wrapping_mul(self.elapsed) as i16;
        let player = &mut self.player;
        self.status.moving = false;
        for (code, facing) in [(KEY_LEFT, 0), (KEY_RIGHT, 1), (KEY_UP, 2), (KEY_DOWN, 3)] {
            if key(code) {
                match facing {
                    0 => player.x = player.x.wrapping_sub(moved),
                    1 => player.x = player.x.wrapping_add(moved),
                    2 => player.y = player.y.wrapping_sub(moved),
                    _ => player.y = player.y.wrapping_add(moved),
                }
                self.status = Status { moving: true, facing };
            }
        }
        player.x = player.x.clamp(0, FIELD_W - 16);
        player.y = player.y.clamp(0, FIELD_H - 16);
        let kept = if bug("trail") { player.y } else { player.x };
        self.trail[usize::from(self.pass_count) % TRAIL] = kept;

        if key(KEY_SPACE) && self.bomb_held == 0 && self.bombs > 0 {
            self.bombs -= 1;
            for e in &mut self.enemies {
                let dx = i32::from(e.x.wrapping_sub(self.player.x));
                let dy = i32::from(e.y.wrapping_sub(self.player.y));
                if e.alive != 0 && dx * dx + dy * dy < BOMB_RADIUS * BOMB_RADIUS {
                    e.alive = 0;
                    self.score = self.score.wrapping_add(10);
                }
            }
        }
        self.bomb_held = tick.inputs.u8_at("key_down", KEY_SPACE);

        let elapsed = self.elapsed as i16;
        for i in 0..ENEMIES {
            if self.enemies[i].alive == 0 {
                if self.random(32) == 0 {
                    self.spawn(i);
                }
                continue;
            }
            let (player, e) = (&self.player, &mut self.enemies[i]);
            e.x = e.x.wrapping_add(e.dx.wrapping_mul(elapsed));
            e.y = e.y.wrapping_add(e.dy.wrapping_mul(elapsed));
            if e.x < 0 || e.x > FIELD_W - 16 {
                e.dx = e.dx.wrapping_neg();
                e.x = e.x.wrapping_add(2i16.wrapping_mul(e.dx).wrapping_mul(elapsed));
                if !bug("1") {
                    e.hits = e.hits.wrapping_add(1);
                }
            }
            if e.y > FIELD_H - 16 {
                e.y = 0;
                self.score = self.score.wrapping_add(1);
            }
            if distance(e.x, player.x) < HIT_RANGE && distance(e.y, player.y) < HIT_RANGE {
                e.alive = 0;
                self.lives = self.lives.wrapping_sub(1);
                if self.lives == 0 {
                    self.game_over = 1;
                }
            }
        }

        // The first of the nearest, by the distances' sum.
        let near = |e: &Enemy| i32::from(distance(e.x, self.player.x)) + i32::from(distance(e.y, self.player.y));
        self.target = (0..ENEMIES).filter(|&i| self.enemies[i].alive != 0).min_by_key(|&i| near(&self.enemies[i]));
        self.score = self.score.wrapping_add(self.elapsed);
    }
}

/// Whether `TOY_BUG` asks for this bug.
fn bug(which: &str) -> bool {
    std::env::var("TOY_BUG").is_ok_and(|b| b == which)
}

fn distance(a: i16, b: i16) -> i16 {
    if a > b { a.wrapping_sub(b) } else { b.wrapping_sub(a) }
}

fn main() -> std::process::ExitCode {
    rrc_runtime::main::<Toy>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_start_spawns_six_enemies_from_the_seed() {
        let toy = Toy::start();
        assert!(toy.enemies.iter().all(|e| e.alive == 1 && e.dx != 0 && (1..=5).contains(&e.dy)));
        assert_eq!((toy.player.x, toy.player.y, toy.lives), (2560, 3168, 3));
        // The original's first enemy at its first tick (rrc trace play --tick 1): F008 3000 0400 0300.
        assert_eq!((toy.enemies[0].x, toy.enemies[0].y, toy.enemies[0].dx, toy.enemies[0].dy), (0x08F0, 0x30, 4, 3));
    }
}
