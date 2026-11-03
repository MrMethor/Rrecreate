//! The sound a presentation makes, played on the host's audio output: the OPL3's register writes synthesized
//! by Nuked OPL3 (at the chip's own 49 716 Hz, the output's rate), the PC speaker's tone as a square wave of the
//! timer's period and high time, and digitized sound at its rate, one at a time as a Sound Blaster's DMA plays
//! it. Each tick's events are taken up when the output next asks for samples: within a buffer's time of when
//! the tick ran.

use std::sync::{Arc, Mutex};

use nuked_opl3::Opl3Chip;
use rrc_runtime::present::{Sounds, Speaker};
use tinyaudio::{OutputDevice, OutputDeviceParameters, run_output_device};

/// The OPL3's own rate, and the output's.
const RATE: usize = 49_716;
/// The PC's timer: its channel 2 drives the speaker.
const PIT_HZ: f64 = 1_193_182.0;
/// The speaker's and digitized sound's loudness against the OPL's.
const SPEAKER: f32 = 6000.0;
const PCM: f32 = 64.0;
/// Samples a channel the output asks for at once: about 20 ms.
const BUFFER: usize = 1024;

/// The host's audio output, playing what [`Audio::play`] is given.
pub(crate) struct Audio {
    mixer: Arc<Mutex<Mixer>>,
    _device: OutputDevice,
}

impl Audio {
    pub(crate) fn start() -> Result<Audio, String> {
        let mixer = Arc::new(Mutex::new(Mixer::new()));
        let feeding = Arc::clone(&mixer);
        let params = OutputDeviceParameters { sample_rate: RATE, channels_count: 2, channel_sample_count: BUFFER };
        let device = run_output_device(params, move |out| {
            if let Ok(mut mixer) = feeding.lock() {
                mixer.fill(out);
            }
        })
        .map_err(|e| e.to_string())?;
        Ok(Audio { mixer, _device: device })
    }

    /// Whether the digitized sound last started still plays (or is about to start).
    pub(crate) fn pcm_playing(&self) -> bool {
        self.mixer.lock().is_ok_and(|m| m.pcm.is_some() || m.pending.iter().any(|s| !s.pcm.is_empty()))
    }

    /// A tick's sound, played from the output's next buffer.
    pub(crate) fn play(&self, sounds: &Sounds) {
        if sounds.is_empty() {
            return;
        }
        if let Ok(mut mixer) = self.mixer.lock() {
            mixer.pending.push(sounds.clone());
        }
    }
}

/// The devices sounding, and the ticks' sound not taken up yet.
struct Mixer {
    opl: Opl3Chip,
    /// The speaker's tone (its period and high time in the timer's ticks), and where in it it is.
    tone: Option<(f64, f64)>,
    phase: f64,
    /// The digitized sound playing: its samples (unsigned 8-bit; left and right when stereo), its rate,
    /// whether stereo, and where in it.
    pcm: Option<(Arc<Vec<u8>>, u32, bool, f64)>,
    pending: Vec<Sounds>,
}

impl Mixer {
    fn new() -> Mixer {
        Mixer { opl: Opl3Chip::new(RATE as u32), tone: None, phase: 0.0, pcm: None, pending: Vec::new() }
    }

    /// The pending ticks' events, then `out` (left, right, ...) filled.
    fn fill(&mut self, out: &mut [f32]) {
        for sounds in std::mem::take(&mut self.pending) {
            self.take(&sounds);
        }
        let mut sample = [0i16; 2];
        for frame in out.chunks_mut(2) {
            let _ = self.opl.generate(&mut sample);
            let mono = self.speaker() + self.digitized();
            for (o, s) in frame.iter_mut().zip(sample) {
                *o = ((f32::from(s) + mono) / 32768.0).clamp(-1.0, 1.0);
            }
        }
    }

    /// One tick's events, in their order by device.
    fn take(&mut self, sounds: &Sounds) {
        for &(register, value) in &sounds.opl {
            self.opl.write_register(register, value);
        }
        for event in &sounds.speaker {
            self.tone = match *event {
                Speaker::Tone { period, high } if period > 0 => Some((f64::from(period), f64::from(high))),
                _ => None,
            };
        }
        // A Sound Blaster plays one at a time: the last started.
        if let (Some(&(rate, stereo)), Some(samples)) = (sounds.pcm.last(), sounds.samples.last()) {
            self.pcm = Some((Arc::clone(samples), rate, stereo, 0.0));
        }
    }

    /// The speaker's next sample: a square wave of its tone, or silence.
    fn speaker(&mut self) -> f32 {
        let Some((period, high)) = self.tone else { return 0.0 };
        self.phase = (self.phase + PIT_HZ / RATE as f64) % period;
        if self.phase < high { SPEAKER } else { -SPEAKER }
    }

    /// The digitized sound's next sample (both channels', when stereo, as one).
    fn digitized(&mut self) -> f32 {
        let Some((samples, rate, stereo, at)) = &mut self.pcm else { return 0.0 };
        let width = if *stereo { 2 } else { 1 };
        let i = *at as usize * width;
        let Some(frame) = samples.get(i..i + width) else {
            self.pcm = None;
            return 0.0;
        };
        let value = frame.iter().map(|&s| f32::from(s) - 128.0).sum::<f32>() / width as f32;
        *at += f64::from(*rate) / RATE as f64;
        value * PCM
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tone_is_a_square_wave_of_its_period() {
        let mut mixer = Mixer::new();
        // About 523 Hz, half high: a cycle is 95 samples.
        let sounds = Sounds { speaker: vec![Speaker::Tone { period: 2281, high: 1141 }], ..Sounds::default() };
        mixer.take(&sounds);
        let wave: Vec<f32> = (0..190).map(|_| mixer.speaker()).collect();
        let flips = wave.windows(2).filter(|w| w[0] != w[1]).count();
        assert!((3..=5).contains(&flips), "two cycles flip about four times: {flips}");
        mixer.take(&Sounds { speaker: vec![Speaker::Steady(false)], ..Sounds::default() });
        assert_eq!(mixer.speaker(), 0.0, "off");
    }

    #[test]
    fn digitized_sound_plays_at_its_rate_then_stops() {
        let mut mixer = Mixer::new();
        let samples = Arc::new(vec![255u8, 0, 128]);
        mixer.take(&Sounds { pcm: vec![(RATE as u32, false)], samples: vec![samples], ..Sounds::default() });
        let played: Vec<f32> = (0..4).map(|_| mixer.digitized()).collect();
        assert_eq!(played, [127.0 * PCM, -128.0 * PCM, 0.0, 0.0]);
        assert!(mixer.pcm.is_none());
    }
}
