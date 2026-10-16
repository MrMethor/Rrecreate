//! Rendering the sound devices' logs to PCM, after (or alongside) a run.
//!
//! The devices only record what the program did, with timestamps: OPL register writes
//! ([`crate::opl::Opl::writes`]), Sound Blaster DMA samples and DAC writes
//! ([`crate::sblaster::SoundBlaster::pcm`], `dac`) and PC speaker tones
//! ([`crate::pit::Pit::speaker_log`]). Rendering is separate so it can't change
//! the machine's behavior, and two runs can be compared by their logs alone. FM synthesis uses
//! the `nuked-opl3` crate (a pure-Rust port of Nuked OPL3, LGPL-2.1-or-later), at the chip's
//! native 49 716 Hz, which is also the output rate. Mixer volumes and filters aren't applied,
//! and the speaker plays plain square waves (no band limiting, no direct port 61h sample output).

use machine::hash::Fnv64;
use nuked_opl3::Opl3Chip;
use std::hash::Hasher;

use crate::clock::PIT_HZ;
use crate::opl::OplWrite;
use crate::pit::SpeakerEvent;
use crate::sblaster::{DacWrite, PcmSegment};

/// The OPL3's own sample rate (14.318 MHz / 288), used for all rendered audio.
pub const SAMPLE_RATE_HZ: u32 = 49_716;

const NANOS_PER_SECOND: u128 = 1_000_000_000;

/// What the sound devices recorded during a run.
#[derive(Debug, Clone, Copy, Default)]
pub struct AudioLog<'a> {
    pub opl: &'a [OplWrite],
    pub pcm: &'a [PcmSegment],
    pub dac: &'a [DacWrite],
    pub speaker: &'a [SpeakerEvent],
}

impl AudioLog<'_> {
    pub fn is_empty(&self) -> bool {
        self.opl.is_empty() && self.pcm.is_empty() && self.dac.is_empty() && self.speaker.is_empty()
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
            h.write(&segment.start_ns.to_le_bytes());
            h.write(&segment.rate_hz.to_le_bytes());
            h.write(&[u8::from(segment.stereo)]);
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
            h.write(&[u8::from(e.on)]);
            h.write(&e.divisor.to_le_bytes());
        }
        h.finish()
    }

    /// Renders `duration_ns` of audio as interleaved stereo samples at [`SAMPLE_RATE_HZ`].
    pub fn render(&self, duration_ns: u64) -> Vec<i16> {
        let frames = (u128::from(duration_ns) * u128::from(SAMPLE_RATE_HZ) / NANOS_PER_SECOND) as usize;
        let mut mix = vec![0i32; frames * 2];
        self.render_opl(&mut mix);
        self.render_pcm(&mut mix);
        self.render_dac(&mut mix);
        self.render_speaker(&mut mix);
        mix.into_iter().map(|s| s.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16).collect()
    }

    fn render_opl(&self, mix: &mut [i32]) {
        if self.opl.is_empty() {
            return;
        }
        let mut chip = Opl3Chip::new(SAMPLE_RATE_HZ);
        let mut writes = self.opl.iter().peekable();
        let mut sample = [0i16; 2];
        for (frame, out) in mix.chunks_exact_mut(2).enumerate() {
            let now = (frame as u128 * NANOS_PER_SECOND / u128::from(SAMPLE_RATE_HZ)) as u64;
            while let Some(w) = writes.next_if(|w| w.nanos <= now) {
                chip.write_register_buffered(w.register, w.value);
            }
            let _ = chip.generate(&mut sample);
            out[0] += i32::from(sample[0]);
            out[1] += i32::from(sample[1]);
        }
    }

    fn render_pcm(&self, mix: &mut [i32]) {
        let frames = mix.len() / 2;
        for segment in self.pcm {
            let channels = if segment.stereo { 2 } else { 1 };
            let length = segment.samples.len() / channels;
            if length == 0 || segment.rate_hz == 0 {
                continue;
            }
            let first = (u128::from(segment.start_ns) * u128::from(SAMPLE_RATE_HZ)).div_ceil(NANOS_PER_SECOND) as usize;
            for frame in first..frames {
                // Position in the segment's samples, nearest earlier sample.
                let t = (frame as u128 * NANOS_PER_SECOND / u128::from(SAMPLE_RATE_HZ))
                    .saturating_sub(u128::from(segment.start_ns));
                let source = (t * u128::from(segment.rate_hz) / NANOS_PER_SECOND) as usize;
                if source >= length {
                    break;
                }
                let (left, right) = if segment.stereo {
                    (segment.samples[source * 2], segment.samples[source * 2 + 1])
                } else {
                    (segment.samples[source], segment.samples[source])
                };
                mix[frame * 2] += i32::from(left);
                mix[frame * 2 + 1] += i32::from(right);
            }
        }
    }

    fn render_dac(&self, mix: &mut [i32]) {
        // Each DAC byte plays until the next one (at most 1 ms after the last).
        let frames = mix.len() / 2;
        for (i, write) in self.dac.iter().enumerate() {
            let until = self.dac.get(i + 1).map_or(write.nanos + 1_000_000, |next| next.nanos);
            let to_frame = |ns: u64| (u128::from(ns) * u128::from(SAMPLE_RATE_HZ)).div_ceil(NANOS_PER_SECOND) as usize;
            let value = (i32::from(write.value) - 128) << 8;
            for frame in to_frame(write.nanos)..to_frame(until).min(frames) {
                mix[frame * 2] += value;
                mix[frame * 2 + 1] += value;
            }
        }
    }
}

impl AudioLog<'_> {
    fn render_speaker(&self, mix: &mut [i32]) {
        const LEVEL: i32 = 8000;
        let frames = mix.len() / 2;
        let to_frame =
            |tick: u64| (u128::from(tick) * u128::from(SAMPLE_RATE_HZ)).div_ceil(u128::from(PIT_HZ)) as usize;
        for (i, event) in self.speaker.iter().enumerate() {
            if !event.on {
                continue;
            }
            let period = if event.divisor == 0 { 65536 } else { u64::from(event.divisor) };
            let end = self.speaker.get(i + 1).map_or(frames, |next| to_frame(next.pit_tick).min(frames));
            for frame in to_frame(event.pit_tick)..end {
                // Channel 2 in mode 3: a square wave, high for the first half of each period.
                let tick = (frame as u128 * u128::from(PIT_HZ) / u128::from(SAMPLE_RATE_HZ)) as u64;
                let value = if (tick % period) * 2 < period { LEVEL } else { -LEVEL };
                mix[frame * 2] += value;
                mix[frame * 2 + 1] += value;
            }
        }
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
        let writes = tone(10_000_000);
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
        let segment = PcmSegment { start_ns: 1_000_000, rate_hz: 10_000, stereo: false, samples: vec![1000; 10] };
        let pcm = [segment];
        let log = AudioLog { pcm: &pcm, ..AudioLog::default() };
        let samples = log.render(5_000_000);
        let frame = |ms_tenths: u64| samples[(ms_tenths as usize * SAMPLE_RATE_HZ as usize / 10_000) * 2];
        assert_eq!(frame(5), 0, "0.5 ms: before the segment");
        assert_eq!(frame(15), 1000, "1.5 ms: playing");
        assert_eq!(frame(25), 0, "2.5 ms: 10 samples at 10 kHz are over");
    }

    #[test]
    fn speaker_square_wave() {
        // 1 kHz tone (divisor 1193) from tick 0, off after 10 ms.
        let events = [
            SpeakerEvent { pit_tick: 0, on: true, divisor: 1193 },
            SpeakerEvent { pit_tick: PIT_HZ / 100, on: false, divisor: 1193 },
        ];
        let log = AudioLog { speaker: &events, ..AudioLog::default() };
        let samples = log.render(20_000_000);
        let left: Vec<i16> = samples.iter().step_by(2).copied().collect();
        let edges = left[..497].windows(2).filter(|w| w[0] < 0 && w[1] > 0).count();
        assert!((9..=10).contains(&edges), "about 10 periods in 10 ms: {edges}");
        assert!(left[600..].iter().all(|&s| s == 0), "silent after it's turned off");
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
