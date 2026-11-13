//! The sound a presentation makes, played on the host's audio output: the OPL3's register writes synthesized
//! by Nuked OPL3 (at the chip's own 49 716 Hz, the output's rate), the PC speaker's tone as a square wave of the
//! timer's period and high time, and digitized sound at its rate, one at a time as a Sound Blaster's DMA plays
//! it, each sample held until the next as its DAC holds it, then through the card's output filter (D169). Each
//! tick's events are taken up when the output next asks for samples: within a buffer's time of when the tick ran.

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
/// Samples a channel the output asks for at once, by default: about 41 ms, which it buffers twice (D168). Windows'
/// DirectSound, emulated over its audio engine, reads ahead of where it plays: with halves of 1024 (20 ms) on a USB
/// interface a half was played before it was written, and the sound broke up as an audio buffer set too small does.
pub(crate) const BUFFER: usize = 2048;

/// The Sound Blaster's analog output filter its digitized sound went through (D169): a 2nd-order low-pass at the
/// model's cutoff, as DOSBox Staging models each card (`soundblaster.cpp`, `configure_sb_filter_for_model`: the
/// values; the filter here is a plain Butterworth section); or none, the samples as they are held.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Filter {
    None,
    Sb1,
    Sb2,
    /// The Sound Blaster Pro's (1 and 2), which Arena's driver is for: the default.
    #[default]
    SbPro,
}

impl Filter {
    /// By its name on the command line.
    pub fn named(name: &str) -> Option<Filter> {
        Some(match name {
            "none" => Filter::None,
            "sb1" => Filter::Sb1,
            "sb2" => Filter::Sb2,
            "sbpro" => Filter::SbPro,
            _ => return None,
        })
    }

    /// Its cutoff, in Hz.
    fn cutoff(self) -> Option<f64> {
        match self {
            Filter::None => None,
            Filter::Sb1 => Some(3800.0),
            Filter::Sb2 => Some(4800.0),
            Filter::SbPro => Some(3200.0),
        }
    }
}

/// A 2nd-order Butterworth low-pass at the output's rate (the bilinear transform's), run sample by sample.
#[derive(Debug, Clone, Copy)]
struct LowPass {
    b: [f64; 3],
    a: [f64; 2],
    x: [f64; 2],
    y: [f64; 2],
}

impl LowPass {
    fn new(cutoff: f64) -> LowPass {
        let k = (std::f64::consts::PI * cutoff / RATE as f64).tan();
        let root2 = std::f64::consts::SQRT_2;
        let norm = 1.0 / (1.0 + root2 * k + k * k);
        let b0 = k * k * norm;
        LowPass {
            b: [b0, 2.0 * b0, b0],
            a: [2.0 * (k * k - 1.0) * norm, (1.0 - root2 * k + k * k) * norm],
            x: [0.0; 2],
            y: [0.0; 2],
        }
    }

    fn run(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.b[1] * self.x[0] + self.b[2] * self.x[1]
            - self.a[0] * self.y[0]
            - self.a[1] * self.y[1];
        self.x = [x, self.x[0]];
        self.y = [y, self.y[0]];
        y
    }
}

/// The host's audio output, playing what [`Audio::play`] is given.
pub(crate) struct Audio {
    mixer: Arc<Mutex<Mixer>>,
    _device: OutputDevice,
}

impl Audio {
    /// The output, asked for `frames` samples a channel at a time, its digitized sound through `filter`.
    pub(crate) fn start(frames: usize, filter: Filter) -> Result<Audio, String> {
        let mixer = Arc::new(Mutex::new(Mixer::with(filter)));
        let feeding = Arc::clone(&mixer);
        let params = OutputDeviceParameters { sample_rate: RATE, channels_count: 2, channel_sample_count: frames };
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
    /// The card's output filter, which runs on whatever the DAC holds, silence too.
    filter: Option<LowPass>,
    pending: Vec<Sounds>,
}

impl Mixer {
    #[cfg(test)]
    fn new() -> Mixer {
        Mixer::with(Filter::None)
    }

    fn with(filter: Filter) -> Mixer {
        Mixer {
            opl: Opl3Chip::new(RATE as u32),
            tone: None,
            phase: 0.0,
            pcm: None,
            filter: filter.cutoff().map(LowPass::new),
            pending: Vec::new(),
        }
    }

    /// The pending ticks' events, then `out` (left, right, ...) filled.
    fn fill(&mut self, out: &mut [f32]) {
        for sounds in std::mem::take(&mut self.pending) {
            self.take(&sounds);
        }
        let mut sample = [0i16; 2];
        for frame in out.chunks_mut(2) {
            let _ = self.opl.generate(&mut sample);
            let held = self.digitized();
            let digitized = match &mut self.filter {
                Some(filter) => filter.run(f64::from(held)) as f32,
                None => held,
            };
            let mono = self.speaker() + digitized;
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

    /// The amplitude of a sine of `hz` through the SB Pro's filter, against its own.
    fn gain(hz: f64) -> f64 {
        let mut filter = LowPass::new(Filter::SbPro.cutoff().unwrap());
        let wave = |i: usize| (2.0 * std::f64::consts::PI * hz * i as f64 / RATE as f64).sin();
        (0..RATE).map(|i| filter.run(wave(i))).skip(RATE / 2).fold(0.0, |m: f64, y| m.max(y.abs()))
    }

    #[test]
    fn the_sound_blaster_pro_s_filter_keeps_the_low_and_cuts_the_high() {
        assert!((gain(500.0) - 1.0).abs() < 0.02, "500 Hz passes: {}", gain(500.0));
        let at_cutoff = gain(3200.0);
        assert!((at_cutoff - std::f64::consts::FRAC_1_SQRT_2).abs() < 0.02, "-3 dB at the cutoff: {at_cutoff}");
        assert!(gain(11_000.0) < 0.1, "a held sample's steps above the band are cut: {}", gain(11_000.0));
        assert_eq!(Filter::named("sbpro"), Some(Filter::SbPro));
        assert_eq!(Filter::named("none").and_then(|f| f.cutoff()), None);
        assert_eq!(Filter::named("sb16"), None, "not modelled");
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
