//! Live sound for `clockwork play`.
//!
//! After every frame, the audio the machine played since the last one is rendered from its
//! device logs, up to the time they are final ([`Machine::audio_settled_ns`]), and queued for
//! the host's audio output, which `tinyaudio` feeds from its own thread. MIDI bytes sent to the
//! MPU-401 go to a host MIDI output through `midir` when the audio around them is heard. All of
//! this only reads the machine, so a played session still replays exactly.
//!
//! The queue also paces the emulator: [`LiveAudio::frame_cycles`] runs a little more or less
//! than a nominal frame so that a little more sound stays queued than the device takes at once
//! (its target), which keeps the emulated time in step with the sound card's clock. What it
//! measures is the sound queued and what the device took last that is still to play (D202): the
//! queue alone falls by a whole block each time the device takes one, and pacing by it ran each
//! shown frame 10 to 20 ms of the game's time, not 14.3, with 2048 samples' blocks.
//!
//! The device takes [`DEVICE_FRAMES`] samples a channel at a time, twice buffered, as a played
//! port's output does (Rrecreate D168, D199): Windows' DirectSound, emulated over its audio engine,
//! reads ahead of where it plays, and halves of 1024 (20 ms) were played before they were written,
//! the sound breaking up. `CLOCKWORK_SOUND_BUFFER` sets another (256 to 16384), which `rrc play
//! --sound-buffer` passes; it isn't an option, which would count in the source hash (D76) though
//! the window replays nothing.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use dos::Machine;
use dos::audio::{Renderer, SAMPLE_RATE_HZ};
use dos::clock::PIT_HZ;
use dos::mpu401::MidiParser;
use midir::{MidiOutput, MidiOutputConnection};
use tinyaudio::{OutputDevice, OutputDeviceParameters, run_output_device};

const NANOS_PER_SECOND: u64 = 1_000_000_000;
/// Frames the output device takes at a time by default (about 41 ms); it buffers two such blocks.
pub const DEVICE_FRAMES: usize = 2048;
/// The environment variable that sets another, and the frames it may set.
const BUFFER_VAR: &str = "CLOCKWORK_SOUND_BUFFER";
const BUFFER_FRAMES: std::ops::RangeInclusive<usize> = 256..=16384;
/// How much sound the emulator keeps queued ahead of the device at least, and more than half a
/// block above it: the queue falls by a block each time the device takes one.
const TARGET_QUEUE_NS: u64 = 50_000_000;
const QUEUE_MARGIN_NS: u64 = 30_000_000;
/// More than this is dropped (only after the window stalled, as pacing keeps the queue short):
/// 250 ms, or two blocks past the target.
const MAX_QUEUE_NS: u64 = 250_000_000;
const FRAME_NS: u64 = NANOS_PER_SECOND / 70;

/// Interleaved stereo samples waiting for the output device.
type Queue = Arc<Mutex<VecDeque<i16>>>;

pub struct LiveAudio {
    renderer: Renderer,
    rendered: Vec<i16>,
    queue: Queue,
    /// Samples a channel the device takes at a time (and how long they play); the queue kept ahead
    /// of it, and the most.
    frames: usize,
    block_ns: u64,
    target_ns: u64,
    max_ns: u64,
    /// When the device last took a block (the host's ns from `epoch`).
    epoch: Instant,
    taken: Arc<AtomicU64>,
    /// Stops the device's thread when dropped.
    device: Option<OutputDevice>,
    midi: Midi,
}

impl LiveAudio {
    /// Opens the host's audio output (sound is skipped, with a message, if that fails).
    /// `midi_port` picks the MIDI output by part of its name; MIDI output is opened when the
    /// program first sends MIDI.
    pub fn start(midi_port: Option<String>) -> LiveAudio {
        let queue = Queue::default();
        let frames = buffer_frames(std::env::var(BUFFER_VAR).ok().as_deref());
        let block_ns = frames as u64 * NANOS_PER_SECOND / u64::from(SAMPLE_RATE_HZ);
        let target_ns = TARGET_QUEUE_NS.max(block_ns / 2 + QUEUE_MARGIN_NS);
        let max_ns = MAX_QUEUE_NS.max(target_ns + 2 * block_ns);
        let params = OutputDeviceParameters {
            sample_rate: SAMPLE_RATE_HZ as usize,
            channels_count: 2,
            channel_sample_count: frames,
        };
        let shared = queue.clone();
        let (epoch, taken) = (Instant::now(), Arc::new(AtomicU64::new(0)));
        let took = taken.clone();
        let device = run_output_device(params, move |data: &mut [f32]| {
            let mut queue = shared.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            for out in data.iter_mut() {
                *out = queue.pop_front().map_or(0.0, |s| f32::from(s) / 32768.0);
            }
            took.store(epoch.elapsed().as_nanos() as u64, Ordering::Relaxed);
        });
        let device = device.map_err(|e| eprintln!("clockwork: no sound: {e}")).ok();
        LiveAudio {
            renderer: Renderer::default(),
            rendered: Vec::new(),
            queue,
            frames,
            block_ns,
            target_ns,
            max_ns,
            epoch,
            taken,
            device,
            midi: Midi::new(midi_port),
        }
    }

    fn queued_ns(&self) -> u64 {
        let samples = self.queue.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).len();
        (samples / 2) as u64 * NANOS_PER_SECOND / u64::from(SAMPLE_RATE_HZ)
    }

    /// The sound buffered: queued, and what the device took last still to play (the frame log's, D204).
    pub fn buffered_ns(&self) -> u64 {
        self.queued_ns() + self.playing_ns()
    }

    /// What the device took last that is still to play.
    fn playing_ns(&self) -> u64 {
        let since = (self.epoch.elapsed().as_nanos() as u64).saturating_sub(self.taken.load(Ordering::Relaxed));
        self.block_ns.saturating_sub(since)
    }

    /// Cycles to run in the next frame: `nominal` (a 70 Hz frame), adjusted to keep the sound
    /// queued near its target. Without an audio device, just `nominal`.
    pub fn frame_cycles(&self, nominal: u64) -> u64 {
        if self.device.is_none() {
            return nominal;
        }
        let run_ns = pace(self.target_ns, self.block_ns, self.queued_ns(), self.playing_ns());
        (u128::from(nominal) * u128::from(run_ns) / u128::from(FRAME_NS)).max(1) as u64
    }

    /// Renders and queues what the machine played since the last call, and sends the MIDI
    /// bytes that are due.
    pub fn update(&mut self, m: &Machine) {
        self.rendered.clear();
        self.renderer.render(&m.audio_log(), m.audio_settled_ns(), &mut self.rendered);
        let heard_ns = if self.device.is_some() {
            let max_samples = (self.max_ns * u64::from(SAMPLE_RATE_HZ) / NANOS_PER_SECOND) as usize * 2;
            let mut queue = self.queue.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            queue.extend(&self.rendered);
            let excess = queue.len().saturating_sub(max_samples);
            queue.drain(..excess);
            drop(queue);
            let device_ns = self.frames as u64 * NANOS_PER_SECOND / u64::from(SAMPLE_RATE_HZ);
            self.renderer.rendered_ns().saturating_sub(self.queued_ns() + device_ns)
        } else {
            m.elapsed_ns()
        };
        let heard_tick = (u128::from(heard_ns) * u128::from(PIT_HZ) / u128::from(NANOS_PER_SECOND)) as u64;
        self.midi.send_until(&m.devices.mpu.midi_log, heard_tick);
    }
}

/// The game's time to run in the next frame (ns), the sound queued `queued_ns` and what the device
/// took last still to play `playing_ns`: a nominal frame, adjusted toward keeping `target_ns` queued
/// on the average (the device's block plays out half of it, on the average).
fn pace(target_ns: u64, block_ns: u64, queued_ns: u64, playing_ns: u64) -> u64 {
    let error = (target_ns + block_ns / 2) as i64 - (queued_ns + playing_ns) as i64;
    (FRAME_NS as i64 + error / 4).clamp(FRAME_NS as i64 / 2, FRAME_NS as i64 * 2) as u64
}

/// The samples a channel the device takes at a time: `CLOCKWORK_SOUND_BUFFER`'s (`set`), if it is
/// a number in range, else the default (with a message where it was set otherwise).
fn buffer_frames(set: Option<&str>) -> usize {
    match set.map(|s| s.trim().parse::<usize>().ok().filter(|f| BUFFER_FRAMES.contains(f))) {
        Some(Some(frames)) => frames,
        Some(None) => {
            eprintln!("clockwork: {BUFFER_VAR}: samples a channel, 256 to 16384; {DEVICE_FRAMES} it is");
            DEVICE_FRAMES
        }
        None => DEVICE_FRAMES,
    }
}

/// The host MIDI output, opened when the first byte is due.
struct Midi {
    /// Which port to open (part of its name), or `None` for the first one.
    port: Option<String>,
    connection: Option<MidiOutputConnection>,
    tried: bool,
    /// The next byte of the MIDI log to send.
    next: usize,
    parser: MidiParser,
}

impl Midi {
    fn new(port: Option<String>) -> Midi {
        let disabled = port.as_deref().is_some_and(|p| p.eq_ignore_ascii_case("none"));
        Midi { port, connection: None, tried: disabled, next: 0, parser: MidiParser::default() }
    }

    fn open(&mut self) {
        self.tried = true;
        let result = (|| {
            let output = MidiOutput::new("clockwork").map_err(|e| e.to_string())?;
            let ports = output.ports();
            let wanted = self.port.as_deref().map(str::to_ascii_lowercase);
            let port = ports
                .iter()
                .find(|p| {
                    let name = output.port_name(p).unwrap_or_default().to_ascii_lowercase();
                    wanted.as_ref().is_none_or(|w| name.contains(w.as_str()))
                })
                .ok_or_else(|| match &self.port {
                    Some(p) => format!("no MIDI output named like {p:?}"),
                    None => "no MIDI output on this computer".to_string(),
                })?;
            let name = output.port_name(port).unwrap_or_default();
            let connection = output.connect(port, "clockwork").map_err(|e| e.to_string())?;
            Ok::<_, String>((name, connection))
        })();
        match result {
            Ok((name, connection)) => {
                eprintln!("clockwork: MIDI goes to {name}");
                self.connection = Some(connection);
            }
            Err(e) => eprintln!("clockwork: MIDI isn't played: {e}"),
        }
    }

    /// Sends the logged MIDI bytes up to PIT tick `until`.
    fn send_until(&mut self, log: &dos::record::Log<dos::mpu401::MidiByte>, until: u64) {
        let due = log.iter_from(self.next).take_while(|b| b.pit_tick <= until).count();
        if due == 0 {
            return;
        }
        if !self.tried {
            self.open();
        }
        for byte in log.iter_from(self.next).take(due) {
            if let Some(connection) = &mut self.connection {
                self.parser.push(byte.value, |message| {
                    let _ = connection.send(message);
                });
            }
        }
        self.next += due;
    }
}

impl Drop for Midi {
    /// Silences the synthesizer, which would otherwise hold the notes that were playing.
    fn drop(&mut self) {
        if let Some(connection) = &mut self.connection {
            for channel in 0..16u8 {
                let _ = connection.send(&[0xB0 | channel, 120, 0]); // all sound off
                let _ = connection.send(&[0xB0 | channel, 121, 0]); // reset all controllers
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sound_buffer_is_the_ports_or_the_one_set() {
        assert_eq!(buffer_frames(None), 2048, "about 41 ms, as a played port's (D168)");
        assert_eq!(buffer_frames(Some("4096")), 4096);
        assert_eq!(buffer_frames(Some("100")), 2048, "out of range");
        assert_eq!(buffer_frames(Some("lots")), 2048);
    }

    #[test]
    fn a_frame_runs_the_same_time_whenever_the_device_takes_a_block() {
        // A block of 2048 samples taken while the queue is at the target: the queue falls by the block,
        // and what plays from it makes up for that as it plays out (D202).
        let block = 2048 * NANOS_PER_SECOND / u64::from(SAMPLE_RATE_HZ);
        let target = 50_000_000;
        let level = target + block / 2;
        for elapsed in [0, block / 4, block / 2, block * 3 / 4, block] {
            let (queued, playing) = (level - block + elapsed, block - elapsed);
            assert_eq!(pace(target, block, queued, playing), FRAME_NS, "{elapsed} ns after the block");
        }
        // More queued than the target: a shorter frame; less: a longer one.
        assert!(pace(target, block, level + 10_000_000, 0) < FRAME_NS);
        assert!(pace(target, block, 0, 0) > FRAME_NS);
    }
}
