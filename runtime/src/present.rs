//! The presentation (DESIGN.md §6.1, D107): what a port draws, read from the simulation's scene, never
//! changing it. Its frame at a tick is what the original's screen holds at that tick's sync point (its memory
//! and palette, as Clockwork renders them), which `rrc compare` compares pixel by pixel. A presentation keeps
//! its own state (what it has drawn), as the original's drawing code keeps its own.

use std::io::{Read, Write};

/// A picture as an indexed video mode shows it: `width` × `height` palette indices, row by row from the top
/// left, and the palette, 256 × R, G, B as the VGA's DAC holds them (0 to 63).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub width: usize,
    pub height: usize,
    pub palette: Vec<u8>,
    pub pixels: Vec<u8>,
}

impl Frame {
    /// Black: every pixel 0, every colour black.
    pub fn new(width: usize, height: usize) -> Frame {
        Frame { width, height, palette: vec![0; 768], pixels: vec![0; width * height] }
    }

    /// The pixel at (`x`, `y`) set to `index`; nothing past the edges.
    pub fn set(&mut self, x: usize, y: usize, index: u8) {
        if x < self.width && y < self.height {
            self.pixels[y * self.width + x] = index;
        }
    }

    /// As a frame's picture (`FRM1`, raw-deflated; `docs/formats.md`, "Frame pictures"), as Clockwork writes
    /// the original's.
    pub fn encode(&self) -> Vec<u8> {
        let mut raw = Vec::with_capacity(9 + 768 + self.pixels.len());
        raw.extend(b"FRM1");
        raw.extend((self.width as u16).to_le_bytes());
        raw.extend((self.height as u16).to_le_bytes());
        raw.push(0);
        raw.extend(&self.palette);
        raw.extend(&self.pixels);
        let mut deflate = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
        deflate.write_all(&raw).expect("writing to memory");
        deflate.finish().expect("writing to memory")
    }

    /// A frame's picture (indexed; an RGB one, of a direct-colour mode, isn't a `Frame`).
    pub fn decode(bytes: &[u8]) -> Result<Frame, String> {
        let mut raw = Vec::new();
        flate2::read::DeflateDecoder::new(bytes)
            .read_to_end(&mut raw)
            .map_err(|e| format!("a frame's picture: {e}"))?;
        if raw.len() < 9 || &raw[..4] != b"FRM1" {
            return Err("not a frame's picture (FRM1)".into());
        }
        let word = |at: usize| usize::from(u16::from_le_bytes([raw[at], raw[at + 1]]));
        let (width, height) = (word(4), word(6));
        if raw[8] != 0 {
            return Err("a direct-colour picture, not an indexed one".into());
        }
        let body = &raw[9..];
        if body.len() != 768 + width * height {
            return Err(format!("a {width} × {height} picture of {} bytes", body.len()));
        }
        Ok(Frame { width, height, palette: body[..768].to_vec(), pixels: body[768..].to_vec() })
    }
}

/// The scene seam of a simulation (DESIGN.md §6.1): what there is to draw at a tick, as meaning (the map,
/// the player, the picture shown), which a presentation draws from. It borrows from the simulation.
pub trait Shows {
    type Scene<'a>
    where
        Self: 'a;
    fn scene(&self) -> Self::Scene<'_>;
}

/// The sound a pass made, by device, as the original's would be logged without its times (D108): the OPL's
/// register writes, the speaker's states, the digitized sounds started, the MIDI bytes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sounds {
    /// (register, value); an OPL3's second bank is 100h up.
    pub opl: Vec<(u16, u8)>,
    pub speaker: Vec<Speaker>,
    /// (rate, stereo) of each digitized sound started.
    pub pcm: Vec<(u32, bool)>,
    pub midi: Vec<u8>,
}

/// What the PC speaker does from a change on: steady (pushed out or not), or a tone of the timer's
/// channel 2 (its period and how long of it the cone is out, in timer ticks), or a one-shot pulse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Speaker {
    Steady(bool),
    Tone { period: u32, high: u32 },
    OneShot,
}

impl Sounds {
    pub fn is_empty(&self) -> bool {
        self.opl.is_empty() && self.speaker.is_empty() && self.pcm.is_empty() && self.midi.is_empty()
    }

    /// As a tick of the port's trace has them (`docs/formats.md`): only devices with sound.
    pub fn to_json(&self) -> serde_json::Value {
        let mut out = serde_json::Map::new();
        if !self.opl.is_empty() {
            out.insert("opl".into(), serde_json::json!(self.opl));
        }
        if !self.speaker.is_empty() {
            let speaker: Vec<serde_json::Value> = self
                .speaker
                .iter()
                .map(|s| match s {
                    Speaker::Steady(on) => serde_json::json!({ "steady": on }),
                    Speaker::Tone { period, high } => serde_json::json!({ "tone": [period, high] }),
                    Speaker::OneShot => serde_json::json!({ "one_shot": true }),
                })
                .collect();
            out.insert("speaker".into(), serde_json::Value::Array(speaker));
        }
        if !self.pcm.is_empty() {
            out.insert("pcm".into(), serde_json::json!(self.pcm));
        }
        if !self.midi.is_empty() {
            out.insert("midi".into(), serde_json::json!(self.midi));
        }
        serde_json::Value::Object(out)
    }
}

/// A presentation of a simulation's scenes: the frames and the sound a faithful port makes. It says
/// which it makes: one that doesn't draw (yet) or doesn't sound isn't compared there.
pub trait Presentation<S: Shows>: Default {
    /// Whether it draws: its frames are compared with the original's screen.
    const DRAWS: bool = true;
    /// Whether it sounds: its sound is compared with the original's.
    const SOUNDS: bool = false;

    /// Its own state at a transplant or a resume (D110): the original's values of what `mapping.toml`'s
    /// `[[present]]` maps there, at their fields' paths (`drawn.level`), before [`Presentation::start`]. A
    /// presentation that keeps none has nothing mapped there and is given nothing.
    fn given(&mut self, state: serde_json::Value) -> Result<(), String> {
        let fields: Vec<&String> = state.as_object().map(|o| o.keys().collect()).unwrap_or_default();
        Err(format!("the presentation takes no state of its own, but is given {fields:?} (mapping.toml's [[present]])"))
    }

    /// The screen at the session's first tick. From power-on, `frame` is black and this draws what the
    /// original's start-up drew; from a transplant or a saved state, `frame` is the original's screen then
    /// (`given`), and this takes up from it what it keeps of its own that [`Presentation::given`] didn't
    /// give.
    fn start(&mut self, scene: &S::Scene<'_>, frame: &mut Frame, given: bool) {
        let _ = (scene, frame, given);
    }

    /// The screen at the next tick's sync point: what the pass that began at a sync point named `pass` drew,
    /// from the scene after it, onto `frame`, the screen at the tick before.
    fn draw(&mut self, pass: &str, scene: &S::Scene<'_>, frame: &mut Frame) {
        let _ = (pass, scene, frame);
    }

    /// The sound made before the next tick's sync point: by the pass that began at a sync point named
    /// `pass`, or with none, by the start-up from power-on to the first tick; from the scene after it.
    fn sound(&mut self, pass: Option<&str>, scene: &S::Scene<'_>, sounds: &mut Sounds) {
        let _ = (pass, scene, sounds);
    }
}

/// What a run draws and sounds with: nothing, or a simulation's presentation.
pub(crate) trait Drawer<S> {
    fn given(&mut self, own: serde_json::Value) -> Result<(), String>;
    fn start(&mut self, state: &S, frame: &mut Frame, given: bool);
    fn draw(&mut self, pass: &str, state: &S, frame: &mut Frame);
    fn sound(&mut self, pass: Option<&str>, state: &S, sounds: &mut Sounds);
    /// Whether it draws, and whether it sounds.
    fn draws(&self) -> bool;
    fn sounds(&self) -> bool;
}

/// A port that doesn't draw or sound (yet).
pub(crate) struct Undrawn;

impl<S> Drawer<S> for Undrawn {
    fn given(&mut self, _: serde_json::Value) -> Result<(), String> {
        Ok(())
    }
    fn start(&mut self, _: &S, _: &mut Frame, _: bool) {}
    fn draw(&mut self, _: &str, _: &S, _: &mut Frame) {}
    fn sound(&mut self, _: Option<&str>, _: &S, _: &mut Sounds) {}
    fn draws(&self) -> bool {
        false
    }
    fn sounds(&self) -> bool {
        false
    }
}

/// A simulation's presentation, drawing and sounding from its scene.
pub(crate) struct Drawn<P>(pub P);

impl<S: Shows, P: Presentation<S>> Drawer<S> for Drawn<P> {
    fn given(&mut self, own: serde_json::Value) -> Result<(), String> {
        self.0.given(own)
    }
    fn start(&mut self, state: &S, frame: &mut Frame, given: bool) {
        self.0.start(&state.scene(), frame, given);
    }
    fn draw(&mut self, pass: &str, state: &S, frame: &mut Frame) {
        self.0.draw(pass, &state.scene(), frame);
    }
    fn sound(&mut self, pass: Option<&str>, state: &S, sounds: &mut Sounds) {
        self.0.sound(pass, &state.scene(), sounds);
    }
    fn draws(&self) -> bool {
        P::DRAWS
    }
    fn sounds(&self) -> bool {
        P::SOUNDS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_s_picture_goes_and_comes_back() {
        let mut frame = Frame::new(4, 2);
        frame.set(3, 1, 7);
        frame.set(9, 9, 1);
        frame.palette[21..24].copy_from_slice(&[63, 0, 32]);
        let back = Frame::decode(&frame.encode()).unwrap();
        assert_eq!(back, frame);
        assert_eq!(back.pixels, [0, 0, 0, 0, 0, 0, 0, 7]);
        assert!(Frame::decode(b"nothing").is_err());
    }

    #[test]
    fn sounds_are_written_by_device() {
        let sounds = Sounds {
            opl: vec![(0xB0, 0x35)],
            speaker: vec![Speaker::Tone { period: 2281, high: 1140 }, Speaker::Steady(false)],
            ..Sounds::default()
        };
        assert_eq!(
            sounds.to_json(),
            serde_json::json!({"opl": [[0xB0, 0x35]], "speaker": [{"tone": [2281, 1140]}, {"steady": false}]})
        );
        assert!(Sounds::default().is_empty() && !sounds.is_empty());
    }
}
