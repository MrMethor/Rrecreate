//! Rendering the sound devices' logs to PCM, after (or alongside) a run.
//!
//! The devices only record what the program did, with timestamps: OPL register writes
//! ([`crate::opl::Opl::writes`]), Sound Blaster DMA samples and DAC writes
//! ([`crate::sblaster::SoundBlaster::pcm`], `dac`) and the PC speaker's waveform
//! ([`crate::pit::Pit::speaker_log`]). Rendering is separate so it can't change
//! the machine's behavior, and two runs can be compared by their logs alone. [`Renderer`]
//! renders a log in pieces as a run goes on, for live output. FM synthesis uses
//! the `nuked-opl3` crate (a pure-Rust port of Nuked OPL3, LGPL-2.1-or-later), at the chip's
//! native 49 716 Hz, which is also the output rate. Mixer volumes and filters aren't applied,
//! except to the speaker: its on/off waveform is averaged exactly over each output sample
//! (which plays tones, and samples sent by pulse-width modulation or by toggling port 61h),
//! then filtered like DOSBox Staging's speaker (`pcspeaker_impulse.cpp`: third-order
//! Butterworth high-pass at 120 Hz and low-pass at 4.3 kHz, standing in for the cone).

use crate::hash::Fnv64;
use nuked_opl3::Opl3Chip;
use std::hash::Hasher;

use crate::cdrom::{CdImage, CdSegment, PCM_FRAMES_PER_SECTOR, PCM_RATE_HZ};
use crate::clock::PIT_HZ;
use crate::opl::OplWrite;
use crate::pit::{SpeakerEvent, Wave};
use crate::record::Log;
use crate::sblaster::{DacWrite, PcmSegment};

/// The OPL3's own sample rate (14.318 MHz / 288), used for all rendered audio.
pub const SAMPLE_RATE_HZ: u32 = 49_716;

const NANOS_PER_SECOND: u128 = 1_000_000_000;

/// What the sound devices recorded during a run.
#[derive(Debug, Clone, Copy)]
pub struct AudioLog<'a> {
    pub opl: &'a Log<OplWrite>,
    pub pcm: &'a Log<PcmSegment>,
    pub dac: &'a Log<DacWrite>,
    pub speaker: &'a Log<SpeakerEvent>,
    /// CD audio: each PLAY, and the image its samples come from.
    pub cd: &'a Log<CdSegment>,
    pub cd_image: Option<&'a CdImage>,
    /// When the log starts (a machine loaded from a snapshot logs from then on); rendering
    /// starts there.
    pub from_ns: u64,
}

/// Nothing logged.
impl Default for AudioLog<'_> {
    fn default() -> Self {
        static OPL: Log<OplWrite> = Log::new();
        static PCM: Log<PcmSegment> = Log::new();
        static DAC: Log<DacWrite> = Log::new();
        static SPEAKER: Log<SpeakerEvent> = Log::new();
        static CD: Log<CdSegment> = Log::new();
        AudioLog { opl: &OPL, pcm: &PCM, dac: &DAC, speaker: &SPEAKER, cd: &CD, cd_image: None, from_ns: 0 }
    }
}

impl AudioLog<'_> {
    pub fn is_empty(&self) -> bool {
        self.opl.is_empty()
            && self.pcm.is_empty()
            && self.dac.is_empty()
            && self.speaker.is_empty()
            && self.cd.is_empty()
    }

    /// A fingerprint of everything the devices played, for comparing runs.
    pub fn hash(&self) -> u64 {
        let mut h = Fnv64::default();
        for w in self.opl {
            h.write(&w.nanos.to_le_bytes());
            h.write(&w.register.to_le_bytes());
            h.write(&[w.value]);
        }
        for segment in self.pcm {
            // A stream's pieces count as one segment.
            if segment.first_frame == 0 {
                h.write(&segment.start_ns.to_le_bytes());
                h.write(&segment.rate_hz.to_le_bytes());
                h.write(&[u8::from(segment.stereo)]);
            }
            for sample in &segment.samples {
                h.write(&sample.to_le_bytes());
            }
        }
        for d in self.dac {
            h.write(&d.nanos.to_le_bytes());
            h.write(&[d.value]);
        }
        for e in self.speaker {
            h.write(&e.pit_tick.to_le_bytes());
            let (kind, a, b, c) = match e.wave {
                Wave::Steady(level) => (0, u64::from(level), 0, 0),
                Wave::LowUntil(t) => (1, t, 0, 0),
                Wave::PulseAt(t) => (2, t, 0, 0),
                Wave::Periodic { origin, period, high } => (3, origin, period, high),
            };
            h.write(&[kind]);
            h.write(&a.to_le_bytes());
            h.write(&b.to_le_bytes());
            h.write(&c.to_le_bytes());
        }
        // The samples come from the image (an input): what played, and when, is the log.
        for s in self.cd {
            h.write(&s.start_ns.to_le_bytes());
            h.write(&s.sector.to_le_bytes());
            h.write(&s.length.to_le_bytes());
            h.write(&s.end_ns().to_le_bytes());
            h.write(&s.volume);
        }
        h.finish()
    }

    /// Renders the audio from the log's start up to `until_ns` as interleaved stereo samples at
    /// [`SAMPLE_RATE_HZ`].
    pub fn render(&self, until_ns: u64) -> Vec<i16> {
        let mut out = Vec::new();
        Renderer::default().render(self, until_ns, &mut out);
        out
    }
}

/// The output frame playing at `nanos` (the first one at or after it).
fn frame_at_nanos(nanos: u64) -> usize {
    (u128::from(nanos) * u128::from(SAMPLE_RATE_HZ)).div_ceil(NANOS_PER_SECOND) as usize
}

/// The time of output frame `frame`.
fn frame_nanos(frame: usize) -> u64 {
    (frame as u128 * NANOS_PER_SECOND / u128::from(SAMPLE_RATE_HZ)) as u64
}

/// Renders a log a piece at a time while a run goes on (`clockwork play` does this every
/// frame). It keeps the OPL chip and its place in each log, and gives exactly the samples
/// [`AudioLog::render`] gives for the whole log, as long as each piece ends no later than the
/// time up to which the log is final (`Machine::audio_settled_ns`).
#[derive(Default)]
pub struct Renderer {
    /// The next output frame.
    frame: usize,
    /// Created at the first register write.
    chip: Option<Opl3Chip>,
    /// The next OPL write to apply.
    opl: usize,
    /// The first PCM segment that may still be playing.
    pcm: usize,
    /// The first DAC write that may still be playing.
    dac: usize,
    /// The speaker event in effect at `frame`.
    speaker: usize,
    /// The first CD segment that may still be playing.
    cd: usize,
    speaker_filter: SpeakerFilter,
}

impl Renderer {
    /// Where rendering has got to: rendering up to this time gives the same frames.
    pub fn rendered_ns(&self) -> u64 {
        (self.frame as u128 * NANOS_PER_SECOND).div_ceil(u128::from(SAMPLE_RATE_HZ)) as u64
    }

    /// Appends the audio from where the last call stopped up to `until_ns` to `out`, as
    /// interleaved stereo samples at [`SAMPLE_RATE_HZ`].
    pub fn render(&mut self, log: &AudioLog, until_ns: u64, out: &mut Vec<i16>) {
        self.frame = self.frame.max(frame_at_nanos(log.from_ns));
        let end = (u128::from(until_ns) * u128::from(SAMPLE_RATE_HZ) / NANOS_PER_SECOND) as usize;
        if end <= self.frame {
            return;
        }
        let mut mix = vec![0i32; (end - self.frame) * 2];
        self.render_opl(log.opl, &mut mix);
        self.render_pcm(log.pcm, &mut mix);
        self.render_dac(log.dac, &mut mix);
        self.render_speaker(log.speaker, &mut mix);
        if let Some(image) = log.cd_image {
            self.render_cd(log.cd, image, &mut mix);
        }
        self.frame = end;
        out.extend(mix.into_iter().map(|s| s.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16));
    }

    fn render_opl(&mut self, writes: &Log<OplWrite>, mix: &mut [i32]) {
        let mut sample = [0i16; 2];
        for (i, out) in mix.chunks_exact_mut(2).enumerate() {
            let now = frame_nanos(self.frame + i);
            while let Some(w) = writes.get(self.opl).filter(|w| w.nanos <= now) {
                let chip = self.chip.get_or_insert_with(|| Opl3Chip::new(SAMPLE_RATE_HZ));
                chip.write_register_buffered(w.register, w.value);
                self.opl += 1;
            }
            let Some(chip) = &mut self.chip else { continue };
            let _ = chip.generate(&mut sample);
            out[0] += i32::from(sample[0]);
            out[1] += i32::from(sample[1]);
        }
    }

    fn render_pcm(&mut self, segments: &Log<PcmSegment>, mix: &mut [i32]) {
        let (first_frame, end) = (self.frame, self.frame + mix.len() / 2);
        let mut all_done = true;
        let from = self.pcm;
        for (index, segment) in segments.iter_from(from).enumerate().map(|(i, s)| (from + i, s)) {
            let channels = if segment.stereo { 2 } else { 1 };
            let length = segment.samples.len() / channels;
            let rate = u128::from(segment.rate_hz);
            // Where the piece starts in its stream (a frame there may still be the last one of
            // the piece before).
            let start = u128::from(segment.first_frame) * NANOS_PER_SECOND / rate.max(1);
            let first = frame_at_nanos(segment.start_ns + start as u64);
            if first >= end {
                break;
            }
            let mut done = length == 0 || segment.rate_hz == 0;
            if !done {
                for frame in first.max(first_frame)..end {
                    // Position in the stream's samples, nearest earlier sample.
                    let t = u128::from(frame_nanos(frame).saturating_sub(segment.start_ns));
                    let Some(source) = ((t * rate / NANOS_PER_SECOND) as u64).checked_sub(segment.first_frame) else {
                        continue;
                    };
                    let source = source as usize;
                    if source >= length {
                        done = true;
                        break;
                    }
                    let (left, right) = if segment.stereo {
                        (segment.samples[source * 2], segment.samples[source * 2 + 1])
                    } else {
                        (segment.samples[source], segment.samples[source])
                    };
                    let at = (frame - first_frame) * 2;
                    mix[at] += i32::from(left);
                    mix[at + 1] += i32::from(right);
                }
            }
            // The last segment can still grow, so it is never passed.
            all_done &= done && index + 1 < segments.len();
            if all_done {
                self.pcm = index + 1;
            }
        }
    }

    fn render_cd(&mut self, segments: &Log<CdSegment>, image: &CdImage, mix: &mut [i32]) {
        let (first_frame, end) = (self.frame, self.frame + mix.len() / 2);
        for segment in segments.iter_from(self.cd) {
            let (from, until) = (frame_at_nanos(segment.start_ns), frame_at_nanos(segment.end_ns()));
            if from >= end {
                break;
            }
            let (from, until) = (from.max(first_frame), until.min(end));
            if from >= until {
                continue;
            }
            // The disc's PCM frame playing at an output frame (nearest earlier).
            let source = |frame: usize| {
                let t = u128::from(frame_nanos(frame).saturating_sub(segment.start_ns));
                u64::from(segment.sector) * u64::from(PCM_FRAMES_PER_SECTOR)
                    + (t * u128::from(PCM_RATE_HZ) / NANOS_PER_SECOND) as u64
            };
            let first = source(from);
            let samples = image.audio(first, (source(until - 1) - first + 1) as usize);
            let [left, right] = segment.volume.map(i32::from);
            for frame in from..until {
                let at = (source(frame) - first) as usize * 2;
                let out = (frame - first_frame) * 2;
                mix[out] += i32::from(samples[at]) * left / 255;
                mix[out + 1] += i32::from(samples[at + 1]) * right / 255;
            }
        }
        while self.cd < segments.len()
            && frame_at_nanos(segments[self.cd].end_ns()) <= end
            && self.cd + 1 < segments.len()
        {
            self.cd += 1;
        }
    }

    fn render_dac(&mut self, writes: &Log<DacWrite>, mix: &mut [i32]) {
        // Each DAC byte plays until the next one, for at most 1 ms (programs write the DAC at
        // their sample rate; a longer gap means playback stopped).
        let (first_frame, end) = (self.frame, self.frame + mix.len() / 2);
        let until = |i: usize| {
            let held = writes[i].nanos + 1_000_000;
            frame_at_nanos(writes.get(i + 1).map_or(held, |next| next.nanos.min(held)))
        };
        let from = self.dac;
        for (i, write) in writes.iter_from(from).enumerate().map(|(i, w)| (from + i, w)) {
            let from = frame_at_nanos(write.nanos);
            if from >= end {
                break;
            }
            let value = (i32::from(write.value) - 128) << 8;
            for frame in from.max(first_frame)..until(i).min(end) {
                let at = (frame - first_frame) * 2;
                mix[at] += value;
                mix[at + 1] += value;
            }
        }
        while self.dac < writes.len() && until(self.dac) <= end {
            self.dac += 1;
        }
    }

    fn render_speaker(&mut self, events: &Log<SpeakerEvent>, mix: &mut [i32]) {
        // Time in sub-ticks: a PIT tick is SAMPLE_RATE_HZ of them, an output frame PIT_HZ.
        let frame_length = PIT_HZ;
        for (i, out) in mix.chunks_exact_mut(2).enumerate() {
            let start = (self.frame + i) as u64 * frame_length;
            let high = speaker_high(events, &mut self.speaker, start, start + frame_length);
            let value = self.speaker_filter.run(SPEAKER_LEVEL * high as f64 / frame_length as f64) as i32;
            out[0] += value;
            out[1] += value;
        }
    }
}

/// The speaker pushed out, before filtering (a square wave comes out at about half of it).
const SPEAKER_LEVEL: f64 = 16_000.0;

/// How long the speaker is high (pushed out) in sub-ticks `from..to` (see `render_speaker`),
/// starting the search at `index`, which it moves to the event in effect at `from`.
fn speaker_high(events: &Log<SpeakerEvent>, index: &mut usize, from: u64, to: u64) -> u64 {
    let scale = u64::from(SAMPLE_RATE_HZ);
    let begins = |i: usize| events[i].pit_tick * scale;
    if events.is_empty() {
        return 0;
    }
    while *index + 1 < events.len() && begins(*index + 1) <= from {
        *index += 1;
    }
    // Before the first event the speaker is at rest.
    let (mut i, mut at) = (*index, from.max(begins(*index)));
    let mut high = 0;
    while at < to {
        let until = if i + 1 < events.len() { begins(i + 1).min(to) } else { to };
        // High sub-ticks before `x`: whole ticks, then the part of the tick `x` is in.
        let wave = events[i].wave;
        let before = |x: u64| wave.high_ticks(x / scale) * scale + (x % scale) * u64::from(wave.level_at(x / scale));
        high += before(until) - before(at);
        (i, at) = (i + 1, until);
    }
    high
}

/// One section of an IIR filter: y = b0·x + b1·x1 + b2·x2 − a1·y1 − a2·y2.
#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
struct Section {
    b: [f64; 3],
    a: [f64; 2],
    x: [f64; 2],
    y: [f64; 2],
}

impl Section {
    fn run(&mut self, x: f64) -> f64 {
        let mut y = self.b[0] * x + self.b[1] * self.x[0] + self.b[2] * self.x[1]
            - self.a[0] * self.y[0]
            - self.a[1] * self.y[1];
        // Flush what has died away, so silence doesn't run on subnormal numbers.
        if y.abs() < 1e-9 {
            y = 0.0;
        }
        self.x = [x, self.x[0]];
        self.y = [y, self.y[0]];
        y
    }
}

/// Third-order Butterworth filters (a first-order and a second-order section with Q = 1),
/// made by the bilinear transform with prewarping.
fn butterworth3(cutoff_hz: f64, high_pass: bool) -> [Section; 2] {
    let k = libm::tan(std::f64::consts::PI * cutoff_hz / f64::from(SAMPLE_RATE_HZ));
    let a1 = (k - 1.0) / (k + 1.0);
    let first = if high_pass {
        Section { b: [1.0 / (k + 1.0), -1.0 / (k + 1.0), 0.0], a: [a1, 0.0], ..Section::default() }
    } else {
        Section { b: [k / (k + 1.0), k / (k + 1.0), 0.0], a: [a1, 0.0], ..Section::default() }
    };
    let norm = 1.0 / (1.0 + k + k * k);
    let a = [2.0 * (k * k - 1.0) * norm, (1.0 - k + k * k) * norm];
    let b = if high_pass { [norm, -2.0 * norm, norm] } else { [k * k * norm, 2.0 * k * k * norm, k * k * norm] };
    [first, Section { b, a, ..Section::default() }]
}

/// What stands for the speaker's cone (DOSBox Staging's `pcspeaker_filter = on`).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct SpeakerFilter {
    sections: [Section; 4],
}

impl Default for SpeakerFilter {
    fn default() -> Self {
        let [h1, h2] = butterworth3(120.0, true);
        let [l1, l2] = butterworth3(4300.0, false);
        Self { sections: [h1, h2, l1, l2] }
    }
}

impl SpeakerFilter {
    fn run(&mut self, x: f64) -> f64 {
        self.sections.iter_mut().fold(x, |x, section| section.run(x))
    }
}

/// A 16-bit stereo WAV file of `samples` (interleaved) at [`SAMPLE_RATE_HZ`].
pub fn wav(samples: &[i16]) -> Vec<u8> {
    let data_bytes = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + samples.len() * 2);
    out.extend(b"RIFF");
    out.extend((36 + data_bytes).to_le_bytes());
    out.extend(b"WAVEfmt ");
    out.extend(16u32.to_le_bytes());
    out.extend(1u16.to_le_bytes()); // PCM
    out.extend(2u16.to_le_bytes()); // channels
    out.extend(SAMPLE_RATE_HZ.to_le_bytes());
    out.extend((SAMPLE_RATE_HZ * 4).to_le_bytes());
    out.extend(4u16.to_le_bytes());
    out.extend(16u16.to_le_bytes());
    out.extend(b"data");
    out.extend(data_bytes.to_le_bytes());
    for sample in samples {
        out.extend(sample.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sine-ish tone on OPL channel 0: modulator silent, carrier at full level, key on.
    fn tone(at: u64) -> Vec<OplWrite> {
        [(0x20, 0x01), (0x40, 0x3F), (0x60, 0xF0), (0x80, 0x77), (0x23, 0x01), (0x43, 0x00), (0x63, 0xF0), (0x83, 0x77)]
            .into_iter()
            .chain([(0xC0, 0x31), (0xA0, 0x98), (0xB0, 0x31)])
            .map(|(register, value)| OplWrite { nanos: at, register, value })
            .collect()
    }

    #[test]
    fn opl_writes_make_sound_from_their_time_on() {
        let writes = tone(10_000_000).into();
        let log = AudioLog { opl: &writes, ..AudioLog::default() };
        let samples = log.render(30_000_000);
        assert_eq!(samples.len(), 2 * (SAMPLE_RATE_HZ as usize * 3 / 100));
        let before = &samples[..2 * 400];
        assert!(before.iter().all(|&s| s == 0), "silent before the key-on at 10 ms");
        let peak = samples.iter().map(|s| s.unsigned_abs()).max().unwrap();
        assert!(peak > 1000, "a tone after it: peak {peak}");
    }

    #[test]
    fn pcm_segments_play_at_their_time_and_rate() {
        let segment =
            PcmSegment { start_ns: 1_000_000, rate_hz: 10_000, stereo: false, first_frame: 0, samples: vec![1000; 10] };
        let pcm = vec![segment].into();
        let log = AudioLog { pcm: &pcm, ..AudioLog::default() };
        let samples = log.render(5_000_000);
        let frame = |ms_tenths: u64| samples[(ms_tenths as usize * SAMPLE_RATE_HZ as usize / 10_000) * 2];
        assert_eq!(frame(5), 0, "0.5 ms: before the segment");
        assert_eq!(frame(15), 1000, "1.5 ms: playing");
        assert_eq!(frame(25), 0, "2.5 ms: 10 samples at 10 kHz are over");
    }

    fn square(origin: u64, period: u32) -> Wave {
        Wave::Periodic { origin, period, high: period.div_ceil(2) }
    }

    #[test]
    fn speaker_square_wave() {
        // 1 kHz tone (divisor 1193) from tick 0, off after 20 ms.
        let events = vec![
            SpeakerEvent { pit_tick: 0, wave: square(0, 1193) },
            SpeakerEvent { pit_tick: PIT_HZ / 50, wave: Wave::Steady(false) },
        ]
        .into();
        let log = AudioLog { speaker: &events, ..AudioLog::default() };
        let samples = log.render(100_000_000);
        let left: Vec<i16> = samples.iter().step_by(2).copied().collect();
        let tone = &left[497..994]; // 10-20 ms
        let edges = tone.windows(2).filter(|w| w[0] < 0 && w[1] >= 0).count();
        assert_eq!(edges, 10, "10 periods in 10 ms");
        let peak = tone.iter().map(|s| s.unsigned_abs()).max().unwrap();
        assert!((6000..12000).contains(&peak), "about half the level each way: {peak}");
        assert!(left[left.len() - 500..].iter().all(|&s| s == 0), "silent after it's turned off");
    }

    #[test]
    fn speaker_pulse_width_modulation_plays_the_duty_cycle() {
        // A 16 kHz carrier (75 ticks) whose duty cycle follows a 500 Hz sine: what a program
        // writing a mode 0 count per sample gives. What comes out is the sine, not the carrier.
        let period = 75u64;
        let events: Log<SpeakerEvent> = (0..1600u64)
            .map(|i| {
                let phase = 2.0 * std::f64::consts::PI * 500.0 * (i * period) as f64 / PIT_HZ as f64;
                let low = (period as f64 * (0.5 + 0.4 * libm::sin(phase))) as u64;
                SpeakerEvent { pit_tick: i * period, wave: Wave::LowUntil(i * period + low) }
            })
            .collect::<Vec<_>>()
            .into();
        let log = AudioLog { speaker: &events, ..AudioLog::default() };
        let left: Vec<f64> = log.render(90_000_000).into_iter().step_by(2).map(f64::from).collect();
        let window = &left[SAMPLE_RATE_HZ as usize / 50..SAMPLE_RATE_HZ as usize / 50 * 4]; // 20-80 ms
        let (mut re, mut im) = (0.0, 0.0);
        for (n, x) in window.iter().enumerate() {
            let phase = 2.0 * std::f64::consts::PI * 500.0 * n as f64 / f64::from(SAMPLE_RATE_HZ);
            (re, im) = (re + x * libm::cos(phase), im + x * libm::sin(phase));
        }
        let length = window.len() as f64;
        let tone_power = 2.0 * (re * re + im * im) / (length * length);
        let power = window.iter().map(|x| x * x).sum::<f64>() / length;
        assert!(tone_power > 0.95 * power, "500 Hz: {tone_power:.0} of {power:.0}");
        assert!(tone_power.sqrt() > 3000.0, "loud enough: {tone_power:.0}");
    }

    #[test]
    fn speaker_toggled_through_port_61h() {
        // Bit 1 toggled every 300 ticks (about 2 kHz) gives a tone like channel 2's.
        let events: Log<SpeakerEvent> = (0..100u64)
            .map(|i| SpeakerEvent { pit_tick: 300 * i, wave: Wave::Steady(i % 2 == 0) })
            .collect::<Vec<_>>()
            .into();
        let log = AudioLog { speaker: &events, ..AudioLog::default() };
        let left: Vec<i16> = log.render(25_000_000).into_iter().step_by(2).collect();
        let tone = &left[SAMPLE_RATE_HZ as usize / 200..SAMPLE_RATE_HZ as usize / 50]; // 5-20 ms
        let crossings = tone.windows(2).filter(|w| w[0] < 0 && w[1] >= 0).count();
        assert!((29..=31).contains(&crossings), "1989 Hz for 15 ms: {crossings}");
    }

    #[test]
    fn rendering_in_pieces_matches_rendering_at_once() {
        let mut opl = tone(3_000_000);
        opl.push(OplWrite { nanos: 21_000_000, register: 0xB0, value: 0x11 }); // key off
        let opl = opl.into();
        let pcm = vec![
            PcmSegment {
                start_ns: 2_000_000,
                rate_hz: 11_025,
                stereo: false,
                first_frame: 0,
                samples: (0..100).map(|i| i * 50).collect(),
            },
            // A stream in two pieces.
            PcmSegment {
                start_ns: 15_000_000,
                rate_hz: 22_050,
                stereo: true,
                first_frame: 0,
                samples: vec![-3000; 200],
            },
            PcmSegment {
                start_ns: 15_000_000,
                rate_hz: 22_050,
                stereo: true,
                first_frame: 100,
                samples: vec![2000; 100],
            },
        ]
        .into();
        let dac: Log<DacWrite> = (0..40)
            .map(|i| DacWrite { nanos: 5_000_000 + i * 90_000, value: (i * 6) as u8 })
            .collect::<Vec<_>>()
            .into();
        let speaker = vec![
            SpeakerEvent { pit_tick: PIT_HZ / 200, wave: square(PIT_HZ / 200, 600) },
            SpeakerEvent { pit_tick: PIT_HZ / 50, wave: Wave::Steady(false) },
            SpeakerEvent { pit_tick: PIT_HZ / 40, wave: Wave::LowUntil(PIT_HZ / 40 + 30) },
            SpeakerEvent { pit_tick: PIT_HZ / 40 + 50, wave: square(PIT_HZ / 40 + 50, 2000) },
        ]
        .into();
        let log = AudioLog { opl: &opl, pcm: &pcm, dac: &dac, speaker: &speaker, ..AudioLog::default() };
        let whole = log.render(40_000_000);
        for step in [1_000_000, 7_333_333, 14_285_714] {
            let mut renderer = Renderer::default();
            let mut pieces = Vec::new();
            let mut until = 0;
            while until < 40_000_000 {
                until = (until + step).min(40_000_000);
                renderer.render(&log, until, &mut pieces);
            }
            assert_eq!(pieces.len(), whole.len());
            assert!(pieces == whole, "pieces of {step} ns differ");
            assert_eq!(log.render(renderer.rendered_ns()).len(), whole.len());
        }
        assert!(whole.iter().any(|&s| s != 0));
    }

    #[test]
    fn a_growing_log_renders_like_the_final_one() {
        // The last PCM segment grows and DAC writes arrive while rendering goes on.
        let mut pcm = Log::from(vec![PcmSegment {
            start_ns: 0,
            rate_hz: 10_000,
            stereo: false,
            first_frame: 0,
            samples: vec![1000; 50],
        }]);
        let mut dac = Log::from(vec![DacWrite { nanos: 1_000_000, value: 200 }]);
        let mut renderer = Renderer::default();
        let mut pieces = Vec::new();
        renderer.render(&AudioLog { pcm: &pcm, dac: &dac, ..AudioLog::default() }, 4_000_000, &mut pieces);
        pcm.last_mut().unwrap().samples.extend([2000; 50]);
        dac.push(DacWrite { nanos: 4_500_000, value: 50 });
        let log = AudioLog { pcm: &pcm, dac: &dac, ..AudioLog::default() };
        renderer.render(&log, 12_000_000, &mut pieces);
        assert!(pieces == log.render(12_000_000));
    }

    #[test]
    fn a_stream_in_pieces_renders_and_hashes_as_one_segment() {
        let samples: Vec<i16> = (0..5000).map(|i| (i % 300) as i16 * 20).collect();
        let segment = |first_frame: usize, samples: &[i16]| PcmSegment {
            start_ns: 1_234_567,
            rate_hz: 22_050,
            stereo: false,
            first_frame: first_frame as u64,
            samples: samples.to_vec(),
        };
        let whole = Log::from(vec![segment(0, &samples)]);
        let pieces = samples.chunks(crate::sblaster::PIECE_FRAMES).enumerate();
        let pieces = Log::from(pieces.map(|(i, c)| segment(i * crate::sblaster::PIECE_FRAMES, c)).collect::<Vec<_>>());
        assert_eq!(pieces.len(), 5);
        let (a, b) =
            (AudioLog { pcm: &whole, ..AudioLog::default() }, AudioLog { pcm: &pieces, ..AudioLog::default() });
        assert!(a.render(300_000_000) == b.render(300_000_000));
        assert_eq!(a.hash(), b.hash());
    }

    #[test]
    fn wav_header() {
        let file = wav(&[1, -1]);
        assert_eq!(&file[..4], b"RIFF");
        assert_eq!(&file[8..16], b"WAVEfmt ");
        assert_eq!(u32::from_le_bytes(file[24..28].try_into().unwrap()), SAMPLE_RATE_HZ);
        assert_eq!(file.len(), 48);
    }
}
