//! Live sound for `rec-host play`.
//!
//! After every frame, the audio the machine played since the last one is rendered from its
//! device logs, up to the time they are final ([`Machine::audio_settled_ns`]), and queued for
//! the host's audio output, which `tinyaudio` feeds from its own thread. MIDI bytes sent to the
//! MPU-401 go to a host MIDI output through `midir` when the audio around them is heard. All of
//! this only reads the machine, so a played session still replays exactly.
//!
//! The queue also paces the emulator: [`LiveAudio::frame_cycles`] runs a little more or less
//! than a nominal frame so that about [`TARGET_QUEUE_NS`] of sound stays queued, which keeps
//! the emulated time in step with the sound card's clock.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use dos::Machine;
use dos::audio::{Renderer, SAMPLE_RATE_HZ};
use dos::clock::PIT_HZ;
use dos::mpu401::MidiParser;
use midir::{MidiOutput, MidiOutputConnection};
use tinyaudio::{OutputDevice, OutputDeviceParameters, run_output_device};

const NANOS_PER_SECOND: u64 = 1_000_000_000;
/// Frames the output device takes at a time (about 21 ms); it buffers two such blocks.
const DEVICE_FRAMES: usize = 1024;
/// How much sound the emulator keeps queued ahead of the device.
const TARGET_QUEUE_NS: u64 = 50_000_000;
/// More than this is dropped (only after the window stalled, as pacing keeps the queue short).
const MAX_QUEUE_NS: u64 = 250_000_000;
const FRAME_NS: u64 = NANOS_PER_SECOND / 70;

/// Interleaved stereo samples waiting for the output device.
type Queue = Arc<Mutex<VecDeque<i16>>>;

pub struct LiveAudio {
    renderer: Renderer,
    rendered: Vec<i16>,
    queue: Queue,
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
        let params = OutputDeviceParameters {
            sample_rate: SAMPLE_RATE_HZ as usize,
            channels_count: 2,
            channel_sample_count: DEVICE_FRAMES,
        };
        let shared = queue.clone();
        let device = run_output_device(params, move |data: &mut [f32]| {
            let mut queue = shared.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            for out in data.iter_mut() {
                *out = queue.pop_front().map_or(0.0, |s| f32::from(s) / 32768.0);
            }
        });
        let device = device.map_err(|e| eprintln!("rec-host: no sound: {e}")).ok();
        LiveAudio { renderer: Renderer::default(), rendered: Vec::new(), queue, device, midi: Midi::new(midi_port) }
    }

    fn queued_ns(&self) -> u64 {
        let samples = self.queue.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).len();
        (samples / 2) as u64 * NANOS_PER_SECOND / u64::from(SAMPLE_RATE_HZ)
    }

    /// Cycles to run in the next frame: `nominal` (a 70 Hz frame), adjusted to keep the sound
    /// queue near its target. Without an audio device, just `nominal`.
    pub fn frame_cycles(&self, nominal: u64) -> u64 {
        if self.device.is_none() {
            return nominal;
        }
        let error = TARGET_QUEUE_NS as i64 - self.queued_ns() as i64;
        let run_ns = (FRAME_NS as i64 + error / 4).clamp(FRAME_NS as i64 / 2, FRAME_NS as i64 * 2) as u64;
        (u128::from(nominal) * u128::from(run_ns) / u128::from(FRAME_NS)).max(1) as u64
    }

    /// Renders and queues what the machine played since the last call, and sends the MIDI
    /// bytes that are due.
    pub fn update(&mut self, m: &Machine) {
        self.rendered.clear();
        self.renderer.render(&m.audio_log(), m.audio_settled_ns(), &mut self.rendered);
        let heard_ns = if self.device.is_some() {
            let max_samples = (MAX_QUEUE_NS * u64::from(SAMPLE_RATE_HZ) / NANOS_PER_SECOND) as usize * 2;
            let mut queue = self.queue.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            queue.extend(&self.rendered);
            let excess = queue.len().saturating_sub(max_samples);
            queue.drain(..excess);
            drop(queue);
            let device_ns = DEVICE_FRAMES as u64 * NANOS_PER_SECOND / u64::from(SAMPLE_RATE_HZ);
            self.renderer.rendered_ns().saturating_sub(self.queued_ns() + device_ns)
        } else {
            m.elapsed_ns()
        };
        let heard_tick = (u128::from(heard_ns) * u128::from(PIT_HZ) / u128::from(NANOS_PER_SECOND)) as u64;
        self.midi.send_until(&m.devices.mpu.midi_log, heard_tick);
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
            let output = MidiOutput::new("rec-host").map_err(|e| e.to_string())?;
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
            let connection = output.connect(port, "rec-host").map_err(|e| e.to_string())?;
            Ok::<_, String>((name, connection))
        })();
        match result {
            Ok((name, connection)) => {
                eprintln!("rec-host: MIDI goes to {name}");
                self.connection = Some(connection);
            }
            Err(e) => eprintln!("rec-host: MIDI isn't played: {e}"),
        }
    }

    /// Sends the logged MIDI bytes up to PIT tick `until`.
    fn send_until(&mut self, log: &[dos::mpu401::MidiByte], until: u64) {
        let due = log[self.next..].iter().take_while(|b| b.pit_tick <= until).count();
        if due == 0 {
            return;
        }
        if !self.tried {
            self.open();
        }
        for byte in &log[self.next..self.next + due] {
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
