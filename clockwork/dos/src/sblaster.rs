//! Creative Sound Blaster (1.x, 2.0, Pro 1, Pro 2, 16): the DSP with its command set, DMA
//! playback (8- and 16-bit PCM, ADPCM, mono and stereo, single-cycle and auto-init), direct DAC
//! output, interrupts and the mixer.
//!
//! Ported from DOSBox Staging `src/hardware/audio/soundblaster.cpp` (GPL-2.0-or-later). DOSBox
//! pulls DMA data as its mixer asks for audio; here playback runs on virtual time instead: the
//! DMA channel is read at the exact byte rate, and the end-of-block interrupt fires when the
//! last byte of the block is due. Decoded samples go to [`SoundBlaster::pcm`] with their start
//! time, for `audio.rs` to render. Not emulated: recording (input reads as silence), SB MIDI,
//! the SB16 ASP and ESS extensions, and mixer volumes in the rendered output.

use std::collections::{BTreeSet, VecDeque};

use crate::memory::Memory;

use crate::dma::Dma;
use crate::opl::OplMode;

const NANOS_PER_SECOND: u128 = 1_000_000_000;
/// Samples that follow the last PCM segment within this much time continue it.
const SEGMENT_SLACK_NS: u64 = 1_000_000;
/// DMA playback is read in steps of about this much time (DOSBox's mixer reads it every
/// millisecond), so the PCM log and the DMA count follow playback closely.
const READ_STEP_HZ: u64 = 1000;
/// DMA transfer units per sample in 14-bit fixed point (DOSBox's `SbShift`).
const SHIFT: u32 = 14;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
pub enum SbType {
    Sb1 = 1,
    SbPro1 = 2,
    Sb2 = 3,
    SbPro2 = 4,
    Sb16 = 6,
}

/// Card type and resources, as in the `BLASTER` variable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct SbConfig {
    pub kind: SbType,
    pub base: u16,
    pub irq: u8,
    pub dma8: u8,
    pub dma16: u8,
}

impl Default for SbConfig {
    /// DOSBox's defaults: an SB16 at 220h, IRQ 7, DMA 1, high DMA 5.
    fn default() -> Self {
        Self { kind: SbType::Sb16, base: 0x220, irq: 7, dma8: 1, dma16: 5 }
    }
}

impl SbConfig {
    /// The `BLASTER` environment variable DOS programs read the settings from.
    pub fn blaster_variable(&self) -> String {
        let mut value = format!("A{:x} I{} D{}", self.base, self.irq, self.dma8);
        if self.kind == SbType::Sb16 {
            value += &format!(" H{}", self.dma16);
        }
        value + &format!(" T{}", self.kind as u8)
    }

    /// The FM chips on this card.
    pub fn opl_mode(&self) -> OplMode {
        match self.kind {
            SbType::Sb1 | SbType::Sb2 => OplMode::Opl2,
            SbType::SbPro1 => OplMode::DualOpl2,
            SbType::SbPro2 | SbType::Sb16 => OplMode::Opl3,
        }
    }
}

/// Samples played by DMA: signed 16-bit, interleaved left/right in stereo. A stream that plays on
/// without a gap from `start_ns` is logged in pieces of at most [`PIECE_FRAMES`] frames, so copies
/// of a machine share all but the last.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct PcmSegment {
    /// When the stream started.
    pub start_ns: u64,
    pub rate_hz: u32,
    pub stereo: bool,
    /// Frames of the stream in the pieces before this one.
    pub first_frame: u64,
    pub samples: Vec<i16>,
}

/// The most frames a [`PcmSegment`] holds.
pub const PIECE_FRAMES: usize = 1024;

/// A byte written to the DAC directly (command 10h); it plays until the next one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct DacWrite {
    pub nanos: u64,
    pub value: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
enum DspState {
    Reset,
    /// Reset released; ready (with AAh to read) at the given time.
    ResetWait(u64),
    Normal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
enum Mode {
    None,
    Dac,
    Dma,
    DmaPause,
    DmaMasked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
enum DmaMode {
    None,
    Adpcm2,
    Adpcm3,
    Adpcm4,
    Pcm8,
    Pcm16,
    /// 16-bit samples through the 8-bit DMA channel.
    Pcm16Aliased,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
struct DmaState {
    mode: DmaMode,
    autoinit: bool,
    stereo: bool,
    signed: bool,
    /// Started with a pre-SB16 command (stereo comes from the mixer and halves the rate).
    old_style: bool,
    /// Transfer units per sample (×2 in stereo), in 14-bit fixed point.
    mul: u32,
    single_size: u32,
    auto_size: u32,
    /// Units left in the current block.
    left: u32,
    channel: u8,
    /// Playback position: units consumed since `anchor_ns`.
    anchor_ns: u64,
    consumed: u64,
    /// A sample left over from an odd-sized stereo read.
    dangling: Option<i16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
struct Mixer {
    index: u8,
    master: [u8; 2],
    dac: [u8; 2],
    fm: [u8; 2],
    cd: [u8; 2],
    line: [u8; 2],
    mic: u8,
    stereo: bool,
    filter: bool,
    /// Registers stored without effect (SB Pro input control, SB16 3Bh-47h).
    other: Vec<u8>,
}

impl Mixer {
    fn new() -> Self {
        Self {
            index: 0,
            master: [31; 2],
            dac: [31; 2],
            fm: [31; 2],
            cd: [31; 2],
            line: [0; 2],
            mic: 0,
            stereo: false,
            filter: true,
            other: vec![0; 0x48],
        }
    }
}

/// Bytes of parameters each DSP command takes (DOSBox `dsp_cmd_len_sb`, with its "Wari hack"
/// for 15h-17h).
#[rustfmt::skip]
const COMMAND_LENGTH_SB: [u8; 256] = [
    0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0,
    1,0,0,0, 2,2,2,2, 0,0,0,0, 0,0,0,0,
    0,0,0,0, 2,0,0,0, 0,0,0,0, 0,0,0,0,
    0,0,0,0, 0,0,0,0, 1,0,0,0, 0,0,0,0,
    1,2,2,0, 0,0,0,0, 2,0,0,0, 0,0,0,0,
    0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0,
    0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0,
    0,0,0,0, 2,2,2,2, 0,0,0,0, 0,0,0,0,
    2,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0,
    0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0,
    0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0,
    0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0,
    0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0,
    0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0,
    1,0,1,0, 1,0,0,0, 0,0,0,0, 0,0,0,0,
    0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0,
];

/// The SB16 version (DOSBox `dsp_cmd_len_sb16`): adds the ASP commands and B0h-CFh.
#[rustfmt::skip]
const COMMAND_LENGTH_SB16: [u8; 256] = [
    0,0,0,0, 1,2,0,0, 1,0,0,0, 0,0,2,1,
    1,0,0,0, 2,2,2,2, 0,0,0,0, 0,0,0,0,
    0,0,0,0, 2,0,0,0, 0,0,0,0, 0,0,0,0,
    0,0,0,0, 0,0,0,0, 1,0,0,0, 0,0,0,0,
    1,2,2,0, 0,0,0,0, 2,0,0,0, 0,0,0,0,
    0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0,
    0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0,
    0,0,0,0, 2,2,2,2, 0,0,0,0, 0,0,0,0,
    2,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0,
    0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0,
    0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0,
    3,3,3,3, 3,3,3,3, 3,3,3,3, 3,3,3,3,
    3,3,3,3, 3,3,3,3, 3,3,3,3, 3,3,3,3,
    0,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,0,
    1,0,1,0, 1,0,0,0, 0,0,0,0, 0,0,0,0,
    0,0,0,0, 0,0,0,0, 0,1,0,0, 0,0,0,0,
];

/// Command E2h's checksum steps (DOSBox `e2_incr_table`).
const E2_STEPS: [[i32; 9]; 4] = [
    [0x01, -0x02, -0x04, 0x08, -0x10, 0x20, 0x40, -0x80, -106],
    [-0x01, 0x02, -0x04, 0x08, 0x10, -0x20, 0x40, -0x80, 165],
    [-0x01, 0x02, 0x04, -0x08, 0x10, -0x20, -0x40, 0x80, -151],
    [0x01, -0x02, 0x04, -0x08, -0x10, 0x20, -0x40, 0x80, 90],
];

const OUTPUT_FIFO: usize = 64;
const DEFAULT_RATE_HZ: u32 = 22050;

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct SoundBlaster {
    pub config: SbConfig,
    irq: u8,
    dma8: u8,
    dma16: u8,
    state: DspState,
    mode: Mode,
    command: u8,
    command_length: u8,
    parameters: Vec<u8>,
    output: VecDeque<u8>,
    last_output: u8,
    test_register: u8,
    write_status_counter: u8,
    freq_hz: u32,
    speaker: bool,
    dma: DmaState,
    adpcm_reference: u8,
    adpcm_step: u8,
    adpcm_have_reference: bool,
    pending_irq8: bool,
    pending_irq16: bool,
    /// Interrupt line changes for the PIC: Some(true) raise, Some(false) withdraw.
    pub(crate) irq_change: Option<bool>,
    e2_value: i32,
    e2_count: u32,
    /// Command E2h's result, written by DMA once the channel is unmasked.
    e2_pending: Option<u8>,
    /// Command 24h (recording): fills the buffer with silence once the channel is unmasked.
    adc_pending: bool,
    /// When an interrupt requested by F2h or 80h is due.
    timed_irq_ns: Option<u64>,
    mixer: Mixer,
    pub pcm: crate::record::Record<crate::record::Log<PcmSegment>>,
    pub dac: crate::record::Record<crate::record::Log<DacWrite>>,
    /// Things programs used that aren't emulated (each message once).
    pub unsupported: BTreeSet<String>,
}

impl SoundBlaster {
    pub fn new(config: SbConfig) -> Self {
        let mut sb = Self {
            config,
            irq: config.irq,
            dma8: config.dma8,
            dma16: config.dma16,
            state: DspState::Normal,
            mode: Mode::None,
            command: 0,
            command_length: 0,
            parameters: Vec::new(),
            output: VecDeque::new(),
            last_output: 0xAA,
            test_register: 0,
            write_status_counter: 0,
            freq_hz: DEFAULT_RATE_HZ,
            speaker: false,
            dma: DmaState {
                mode: DmaMode::None,
                autoinit: false,
                stereo: false,
                signed: false,
                old_style: true,
                mul: 0,
                single_size: 0,
                auto_size: 0,
                left: 0,
                channel: config.dma8,
                anchor_ns: 0,
                consumed: 0,
                dangling: None,
            },
            adpcm_reference: 0,
            adpcm_step: 0,
            adpcm_have_reference: false,
            pending_irq8: false,
            pending_irq16: false,
            irq_change: None,
            e2_value: 0xAA,
            e2_count: 0,
            e2_pending: None,
            adc_pending: false,
            timed_irq_ns: None,
            mixer: Mixer::new(),
            pcm: Default::default(),
            dac: Default::default(),
            unsupported: BTreeSet::new(),
        };
        sb.reset_dsp();
        sb.speaker = config.kind == SbType::Sb16;
        sb
    }

    /// The interrupt line the card uses now (the SB16 mixer can change it).
    pub fn irq(&self) -> u8 {
        self.irq
    }

    pub fn decodes(&self, port: u16) -> bool {
        (self.config.base..self.config.base + 0x10).contains(&port)
    }

    fn note(&mut self, message: String) {
        self.unsupported.insert(message);
    }

    fn raise_irq(&mut self, sixteen_bit: bool) {
        let pending = if sixteen_bit { &mut self.pending_irq16 } else { &mut self.pending_irq8 };
        if !*pending {
            *pending = true;
            self.irq_change = Some(true);
        }
    }

    fn lower_irq(&mut self) {
        self.irq_change = Some(false);
    }

    fn reset_dsp(&mut self) {
        self.lower_irq();
        self.mode = Mode::None;
        self.output.clear();
        self.command = 0;
        self.command_length = 0;
        self.parameters.clear();
        self.write_status_counter = 0;
        let channel = self.dma8;
        self.dma = DmaState {
            mode: DmaMode::None,
            autoinit: false,
            stereo: false,
            signed: false,
            old_style: true,
            mul: 0,
            single_size: 0,
            auto_size: 0,
            left: 0,
            channel,
            anchor_ns: 0,
            consumed: 0,
            dangling: None,
        };
        self.adpcm_reference = 0;
        self.adpcm_step = 0;
        self.adpcm_have_reference = false;
        self.freq_hz = DEFAULT_RATE_HZ;
        self.e2_value = 0xAA;
        self.e2_count = 0;
        self.pending_irq8 = false;
        self.pending_irq16 = false;
        self.timed_irq_ns = None;
        // Speaker output stays on across resets on the SB16; older cards start with it off.
        self.speaker = self.config.kind == SbType::Sb16;
    }

    /// DMA transfer units per second for the current transfer.
    fn unit_rate(&self) -> u64 {
        let frame_rate = if self.dma.old_style && self.dma.stereo { self.freq_hz / 2 } else { self.freq_hz };
        (u64::from(frame_rate) * u64::from(self.dma.mul)) >> SHIFT
    }

    /// Frames per second of the samples being produced.
    fn frame_rate(&self) -> u32 {
        if self.dma.old_style && self.dma.stereo { self.freq_hz / 2 } else { self.freq_hz }
    }

    /// Restarts the playback clock at `now` (after a rate or format change).
    fn anchor(&mut self, now: u64) {
        self.dma.anchor_ns = now;
        self.dma.consumed = 0;
    }

    /// When the next millisecond of DMA playback is due, the current block ends, or another
    /// timed action is due.
    pub fn next_event(&self) -> Option<u64> {
        let mut next = self.timed_irq_ns;
        if let DspState::ResetWait(at) = self.state {
            next = Some(next.map_or(at, |n| n.min(at)));
        }
        let rate = self.unit_rate();
        if self.mode == Mode::Dma && self.dma.mode != DmaMode::None && rate > 0 {
            let step = (rate / READ_STEP_HZ).max(1).min(u64::from(self.dma.left.max(1)));
            let units = self.dma.consumed + step;
            let at = self.dma.anchor_ns
                + (u128::from(units) * NANOS_PER_SECOND).div_ceil(u128::from(rate)).min(u128::from(u64::MAX / 2))
                    as u64;
            next = Some(next.map_or(at, |n| n.min(at)));
        }
        next
    }

    /// Up to when the PCM log is final at `now`: samples logged later all start after it.
    /// Playback being read is logged a step behind, and samples that continue a segment can
    /// be placed up to [`SEGMENT_SLACK_NS`] early.
    pub fn settled_ns(&self, now: u64) -> u64 {
        let rate = self.unit_rate();
        let mut settled = now;
        if self.mode == Mode::Dma && self.dma.mode != DmaMode::None && rate > 0 {
            let played =
                self.dma.anchor_ns + (u128::from(self.dma.consumed) * NANOS_PER_SECOND / u128::from(rate)) as u64;
            settled = settled.min(played);
        }
        settled.saturating_sub(SEGMENT_SLACK_NS)
    }

    /// Brings the card up to time `now`: finishes resets, delivers timed interrupts, and plays
    /// the DMA data that is due.
    pub fn update(&mut self, now: u64, dma: &mut Dma, mem: &mut Memory) {
        if let DspState::ResetWait(at) = self.state
            && now >= at
        {
            self.output.clear();
            self.output.push_back(0xAA);
            self.state = DspState::Normal;
        }
        if let Some(at) = self.timed_irq_ns
            && now >= at
        {
            self.timed_irq_ns = None;
            self.raise_irq(false);
        }
        let channel = &mut dma.channels[usize::from(self.dma8)];
        if let Some(value) = self.e2_pending
            && !channel.masked
        {
            channel.write(mem, &[value]);
            self.e2_pending = None;
        }
        if self.adc_pending && !channel.masked {
            let silence = vec![128u8; self.dma.left as usize];
            channel.write(mem, &silence);
            self.adc_pending = false;
            self.raise_irq(false);
        }
        self.play(now, dma, mem);
    }

    fn play(&mut self, now: u64, dma: &mut Dma, mem: &Memory) {
        if self.dma.mode == DmaMode::None || !matches!(self.mode, Mode::Dma | Mode::DmaMasked) {
            return;
        }
        let masked = dma.channels[usize::from(self.dma.channel)].masked;
        if masked {
            self.mode = Mode::DmaMasked;
            return;
        }
        if self.mode == Mode::DmaMasked {
            self.mode = Mode::Dma;
            self.anchor(now);
        }
        let rate = self.unit_rate();
        if rate == 0 {
            return;
        }
        loop {
            let elapsed = u128::from(now.saturating_sub(self.dma.anchor_ns));
            let due = (elapsed * u128::from(rate) / NANOS_PER_SECOND) as u64;
            let wanted = due.saturating_sub(self.dma.consumed).min(u64::from(self.dma.left));
            if wanted == 0 {
                break;
            }
            let start_ns =
                self.dma.anchor_ns + (u128::from(self.dma.consumed) * NANOS_PER_SECOND / u128::from(rate)) as u64;
            let mut bytes = Vec::new();
            let channel = &mut dma.channels[usize::from(self.dma.channel)];
            let read = channel.read(mem, wanted as usize, &mut bytes) as u32;
            self.dma.consumed += u64::from(read);
            self.dma.left -= read;
            self.log_samples(start_ns, &bytes);
            if self.dma.left == 0 {
                self.raise_irq(self.dma.mode >= DmaMode::Pcm16);
                if !self.dma.autoinit {
                    if self.dma.single_size == 0 {
                        self.mode = Mode::None;
                        self.dma.mode = DmaMode::None;
                        break;
                    }
                    // A single-cycle transfer queued while auto-init was running.
                    self.dma.left = self.dma.single_size;
                    self.dma.single_size = 0;
                } else {
                    if self.dma.auto_size == 0 {
                        self.mode = Mode::None;
                        break;
                    }
                    self.dma.left = self.dma.auto_size;
                }
            }
            if u64::from(read) < wanted {
                // The DMA controller reached the end of a single-cycle transfer and masked
                // the channel: playback waits for the program to set it up again.
                self.mode = Mode::DmaMasked;
                break;
            }
        }
    }

    fn adpcm_portion(&mut self, portion: i32, adjust: &[u8], scale: &[i8]) -> u8 {
        let i = (portion + i32::from(self.adpcm_step)).clamp(0, adjust.len() as i32 - 1) as usize;
        self.adpcm_step = self.adpcm_step.wrapping_add(adjust[i]);
        self.adpcm_reference = (i32::from(self.adpcm_reference) + i32::from(scale[i])).clamp(0, 255) as u8;
        self.adpcm_reference
    }

    /// Decodes the DMA bytes of the current format into signed 16-bit samples and appends them
    /// to the PCM log (nothing is heard while the speaker is off on pre-SB16 cards).
    fn log_samples(&mut self, start_ns: u64, bytes: &[u8]) {
        let mut samples: Vec<i16> = Vec::with_capacity(bytes.len() * 2);
        let u8_sample = |b: u8| (i16::from(b) - 128) << 8;
        match self.dma.mode {
            DmaMode::Pcm8 => samples
                .extend(bytes.iter().map(|&b| if self.dma.signed { i16::from(b as i8) << 8 } else { u8_sample(b) })),
            DmaMode::Pcm16 | DmaMode::Pcm16Aliased => samples.extend(bytes.chunks_exact(2).map(|w| {
                let word = u16::from_le_bytes([w[0], w[1]]);
                if self.dma.signed { word as i16 } else { (word ^ 0x8000) as i16 }
            })),
            DmaMode::Adpcm2 | DmaMode::Adpcm3 | DmaMode::Adpcm4 => {
                let mut bytes = bytes;
                if self.adpcm_have_reference && !bytes.is_empty() {
                    self.adpcm_have_reference = false;
                    self.adpcm_reference = bytes[0];
                    self.adpcm_step = 0;
                    bytes = &bytes[1..];
                }
                for &b in bytes {
                    let decoded: Vec<u8> = match self.dma.mode {
                        DmaMode::Adpcm2 => (0..4)
                            .map(|k| {
                                self.adpcm_portion(i32::from((b >> (6 - 2 * k)) & 3), &ADPCM2_ADJUST, &ADPCM2_SCALE)
                            })
                            .collect(),
                        DmaMode::Adpcm3 => vec![
                            self.adpcm_portion(i32::from((b >> 5) & 7), &ADPCM3_ADJUST, &ADPCM3_SCALE),
                            self.adpcm_portion(i32::from((b >> 2) & 7), &ADPCM3_ADJUST, &ADPCM3_SCALE),
                            self.adpcm_portion(i32::from((b & 3) << 1), &ADPCM3_ADJUST, &ADPCM3_SCALE),
                        ],
                        _ => vec![
                            self.adpcm_portion(i32::from(b >> 4), &ADPCM4_ADJUST, &ADPCM4_SCALE),
                            self.adpcm_portion(i32::from(b & 0x0F), &ADPCM4_ADJUST, &ADPCM4_SCALE),
                        ],
                    };
                    samples.extend(decoded.into_iter().map(u8_sample));
                }
            }
            DmaMode::None => {}
        }
        if self.config.kind != SbType::Sb16 && !self.speaker {
            return;
        }
        let stereo = self.dma.stereo && !matches!(self.dma.mode, DmaMode::Adpcm2 | DmaMode::Adpcm3 | DmaMode::Adpcm4);
        if stereo {
            if let Some(first) = self.dma.dangling.take() {
                samples.insert(0, first);
            }
            if samples.len() % 2 == 1 {
                self.dma.dangling = samples.pop();
            }
        }
        if samples.is_empty() {
            return;
        }
        let rate_hz = self.frame_rate();
        let channels = if stereo { 2 } else { 1 };
        let (mut start_ns, mut first_frame) = (start_ns, 0);
        if let Some(last) = self.pcm.last_mut()
            && last.rate_hz == rate_hz
            && last.stereo == stereo
        {
            let frames = last.first_frame + (last.samples.len() / channels) as u64;
            let end = last.start_ns + (u128::from(frames) * NANOS_PER_SECOND / u128::from(rate_hz)) as u64;
            // Continue the stream when these samples follow on without a gap (1 ms slack).
            if start_ns.abs_diff(end) <= SEGMENT_SLACK_NS {
                let room = (PIECE_FRAMES * channels).saturating_sub(last.samples.len()).min(samples.len());
                last.samples.extend(samples.drain(..room));
                start_ns = last.start_ns;
                first_frame = last.first_frame + (last.samples.len() / channels) as u64;
            }
        }
        while !samples.is_empty() {
            let rest = samples.split_off((PIECE_FRAMES * channels).min(samples.len()));
            let frames = (samples.len() / channels) as u64;
            self.pcm.push(PcmSegment { start_ns, rate_hz, stereo, first_frame, samples });
            first_frame += frames;
            samples = rest;
        }
    }

    fn start_dma(&mut self, now: u64, mode: DmaMode, autoinit: bool, stereo: bool, old_style: bool) {
        // Starting a transfer clears pending interrupts.
        self.pending_irq8 = false;
        self.pending_irq16 = false;
        self.lower_irq();
        let mut mul = match mode {
            DmaMode::Adpcm2 => (1 << SHIFT) / 4,
            DmaMode::Adpcm3 => (1 << SHIFT) / 3,
            DmaMode::Adpcm4 => (1 << SHIFT) / 2,
            DmaMode::Pcm8 | DmaMode::Pcm16 => 1 << SHIFT,
            DmaMode::Pcm16Aliased => (1 << SHIFT) * 2,
            DmaMode::None => return,
        };
        if self.mode >= Mode::Dma && self.dma.autoinit && !autoinit {
            // From auto-init into single-cycle: the switch happens at the end of the block.
        } else if !autoinit {
            self.dma.left = self.dma.single_size;
            self.dma.single_size = 0;
        } else {
            self.dma.left = self.dma.auto_size;
        }
        if stereo {
            mul *= 2;
        }
        self.dma.autoinit = autoinit;
        self.dma.mode = mode;
        self.dma.stereo = stereo;
        self.dma.old_style = old_style;
        self.dma.mul = mul;
        self.dma.dangling = None;
        // Playback starts when the DMA channel is (or already is) unmasked.
        self.mode = Mode::DmaMasked;
        self.anchor(now);
    }

    fn prepare_dma_old(&mut self, now: u64, mode: DmaMode, autoinit: bool) {
        self.dma.signed = false;
        if !autoinit {
            self.dma.single_size = 1 + u32::from(self.parameters[0]) + (u32::from(self.parameters[1]) << 8);
        }
        self.dma.channel = self.dma8;
        let stereo = self.mixer.stereo;
        self.start_dma(now, mode, autoinit, stereo, true);
    }

    fn prepare_dma_new(&mut self, now: u64, mode: DmaMode, length: u32, autoinit: bool, stereo: bool) {
        let (mode, length) = if mode == DmaMode::Pcm16 && self.dma16 > 7 {
            // No 16-bit channel: 16-bit samples go through the 8-bit channel, two bytes each.
            self.dma.channel = self.dma8;
            (DmaMode::Pcm16Aliased, length * 2)
        } else {
            self.dma.channel = if mode == DmaMode::Pcm16 { self.dma16 } else { self.dma8 };
            (mode, length)
        };
        if autoinit {
            self.dma.auto_size = length;
        } else {
            self.dma.single_size = length;
        }
        self.start_dma(now, mode, autoinit, stereo, false);
    }

    fn set_rate(&mut self, now: u64, freq_hz: u32) {
        if self.freq_hz != freq_hz && self.dma.mode != DmaMode::None {
            self.anchor(now);
        }
        self.freq_hz = freq_hz;
    }

    fn add_output(&mut self, value: u8) {
        if self.output.len() < OUTPUT_FIFO {
            self.output.push_back(value);
        }
    }

    fn set_output(&mut self, values: &[u8]) {
        self.output.clear();
        for &value in values {
            self.add_output(value);
        }
    }

    fn version(&self) -> [u8; 2] {
        match self.config.kind {
            SbType::Sb1 => [1, 5],
            SbType::Sb2 => [2, 1],
            SbType::SbPro1 => [3, 0],
            SbType::SbPro2 => [3, 2],
            SbType::Sb16 => [4, 5],
        }
    }

    fn sb16(&mut self) -> bool {
        let ok = self.config.kind == SbType::Sb16;
        if !ok {
            self.note(format!("Sound Blaster DSP command {:02X}h needs an SB16", self.command));
        }
        ok
    }

    fn sb2_or_later(&mut self) -> bool {
        let ok = self.config.kind > SbType::Sb1;
        if !ok {
            self.note(format!("Sound Blaster DSP command {:02X}h needs an SB 2.0 or later", self.command));
        }
        ok
    }

    fn word_parameter(&self, at: usize) -> u32 {
        1 + u32::from(self.parameters[at]) + (u32::from(self.parameters[at + 1]) << 8)
    }

    fn execute(&mut self, now: u64) {
        let parameters: [u8; 3] = std::array::from_fn(|i| self.parameters.get(i).copied().unwrap_or(0));
        let p = |i: usize| parameters[i];
        match self.command {
            0x04 if self.config.kind != SbType::Sb16 => {
                // DSP status (pre-SB16).
                let value = match self.config.kind {
                    SbType::Sb2 => 0x88,
                    SbType::SbPro1 | SbType::SbPro2 => 0x7B,
                    _ => 0xFF,
                };
                self.set_output(&[value]);
            }
            0x04 | 0x05 | 0x08 | 0x0E | 0x0F | 0xF9 => {
                self.note(format!("Sound Blaster 16 ASP command {:02X}h", self.command));
                if matches!(self.command, 0x08 | 0x0F | 0xF9) {
                    self.add_output(0);
                }
            }
            0x10 => {
                self.mode = Mode::Dac;
                if self.config.kind == SbType::Sb16 || self.speaker {
                    self.dac.push(DacWrite { nanos: now, value: p(0) });
                }
            }
            0x14 | 0x15 | 0x91 => self.prepare_dma_old(now, DmaMode::Pcm8, false),
            0x1C | 0x90 => {
                if self.sb2_or_later() {
                    self.prepare_dma_old(now, DmaMode::Pcm8, true);
                }
            }
            0x16 | 0x17 => {
                self.adpcm_have_reference |= self.command == 0x17;
                self.prepare_dma_old(now, DmaMode::Adpcm2, false);
            }
            0x74 | 0x75 => {
                self.adpcm_have_reference |= self.command == 0x75;
                self.prepare_dma_old(now, DmaMode::Adpcm4, false);
            }
            0x76 | 0x77 => {
                self.adpcm_have_reference |= self.command == 0x77;
                self.prepare_dma_old(now, DmaMode::Adpcm3, false);
            }
            0x7D => {
                if self.sb2_or_later() {
                    self.adpcm_have_reference = true;
                    self.prepare_dma_old(now, DmaMode::Adpcm4, true);
                }
            }
            0x20 => self.add_output(0x7F), // direct ADC: silence
            0x24 => {
                self.dma.left = self.word_parameter(0);
                self.dma.signed = false;
                self.adc_pending = true;
            }
            0x40 => self.set_rate(now, 1_000_000 / (256 - u32::from(p(0)))),
            0x41 | 0x42 => {
                if self.sb16() {
                    self.set_rate(now, (u32::from(p(0)) << 8) | u32::from(p(1)));
                }
            }
            0x48 => {
                if self.sb2_or_later() {
                    self.dma.auto_size = self.word_parameter(0);
                }
            }
            0x80 => {
                // Silence for a number of samples, then an interrupt.
                let samples = u64::from(self.word_parameter(0));
                self.timed_irq_ns = Some(now + samples * 1_000_000_000 / u64::from(self.freq_hz.max(1)));
            }
            0xB0..=0xCF => {
                if self.sb16() {
                    self.dma.signed = p(0) & 0x10 != 0;
                    let mode = if self.command & 0x10 != 0 { DmaMode::Pcm16 } else { DmaMode::Pcm8 };
                    let length = 1 + u32::from(p(1)) + (u32::from(p(2)) << 8);
                    self.prepare_dma_new(now, mode, length, self.command & 0x04 != 0, p(0) & 0x20 != 0);
                }
            }
            0xD0 | 0xD5 => {
                if self.command == 0xD0 || self.sb16() {
                    self.mode = Mode::DmaPause;
                }
            }
            0xD4 | 0xD6 => {
                if (self.command == 0xD4 || self.sb16()) && self.mode == Mode::DmaPause {
                    self.mode = Mode::DmaMasked;
                }
            }
            0xD1 => self.set_speaker(true),
            0xD3 => self.set_speaker(false),
            0xD8 => {
                if self.sb2_or_later() {
                    let status = if self.speaker { 0xFF } else { 0x00 };
                    self.set_output(&[status]);
                }
            }
            0xD9 | 0xDA => {
                if (self.command == 0xDA || self.sb16()) && self.sb2_or_later() {
                    self.dma.autoinit = false;
                }
            }
            0xE0 => self.set_output(&[!p(0)]),
            0xE1 => {
                let version = self.version();
                self.set_output(&version);
            }
            0xE2 => {
                // DMA identification: a checksum of the parameter, written by DMA.
                let step = &E2_STEPS[(self.e2_count % 4) as usize];
                for (bit, &delta) in step.iter().take(8).enumerate() {
                    if p(0) & (1 << bit) != 0 {
                        self.e2_value += delta;
                    }
                }
                self.e2_value += step[8];
                self.e2_count += 1;
                self.e2_pending = Some(self.e2_value as u8);
            }
            0xE3 => {
                self.output.clear();
                for &b in b"COPYRIGHT (C) CREATIVE TECHNOLOGY LTD, 1992.\0" {
                    self.add_output(b);
                }
            }
            0xE4 => self.test_register = p(0),
            0xE8 => {
                let value = self.test_register;
                self.set_output(&[value]);
            }
            0xF2 => self.timed_irq_ns = Some(now + 10_000),
            0xF3 => {
                if self.sb16() {
                    self.raise_irq(true);
                }
            }
            0xF8 => self.set_output(&[0]),
            0x30 | 0x31 | 0x34..=0x38 => self.note("Sound Blaster MIDI is not emulated".to_owned()),
            command => self.note(format!("Sound Blaster DSP command {command:02X}h is not emulated")),
        }
        self.command = 0;
        self.command_length = 0;
        self.parameters.clear();
    }

    fn set_speaker(&mut self, on: bool) {
        // The SB16 ignores speaker commands: its output is always on.
        if self.config.kind != SbType::Sb16 {
            self.speaker = on;
        }
    }

    fn write_dsp(&mut self, now: u64, value: u8) {
        if self.command == 0 {
            self.command = value;
            let table = if self.config.kind == SbType::Sb16 { &COMMAND_LENGTH_SB16 } else { &COMMAND_LENGTH_SB };
            self.command_length = table[usize::from(value)];
            self.parameters.clear();
            if self.command_length == 0 {
                self.execute(now);
            }
        } else {
            self.parameters.push(value);
            if self.parameters.len() >= usize::from(self.command_length) {
                self.execute(now);
            }
        }
    }

    /// The write-status busy bit: busy while a reset is in progress, and on 7 of 8 reads
    /// during DMA with more than 3 ms of data left (DOSBox `write_buffer_at_capacity`).
    fn write_busy(&mut self) -> bool {
        if self.state != DspState::Normal {
            return true;
        }
        self.write_status_counter = self.write_status_counter.wrapping_add(1);
        if self.write_status_counter % 8 == 0 || self.dma.mode == DmaMode::None {
            return false;
        }
        u64::from(self.dma.left) > self.unit_rate() * 3 / 1000
    }

    /// A port read at time `now`.
    pub fn read(&mut self, port: u16, now: u64, dma: &mut Dma, mem: &mut Memory) -> u8 {
        self.update(now, dma, mem);
        self.read_port(port)
    }

    /// A port write at time `now`. Playback set up by the write starts right away if the DMA
    /// channel is already unmasked.
    pub fn write(&mut self, port: u16, value: u8, now: u64, dma: &mut Dma, mem: &mut Memory) {
        self.update(now, dma, mem);
        self.write_port(port, value, now);
        self.update(now, dma, mem);
    }

    fn read_port(&mut self, port: u16) -> u8 {
        match port - self.config.base {
            0x04 => self.mixer.index,
            0x05 => self.read_mixer(),
            0x06 => 0xFF,
            0x0A => {
                if let Some(value) = self.output.pop_front() {
                    self.last_output = value;
                }
                self.last_output
            }
            0x0C => {
                if self.write_busy() {
                    0xFF
                } else {
                    0x7F
                }
            }
            0x0E => {
                // Reading the read-status port acknowledges an 8-bit interrupt.
                if self.pending_irq8 {
                    self.pending_irq8 = false;
                    self.lower_irq();
                }
                if self.output.is_empty() { 0x7F } else { 0xFF }
            }
            0x0F => {
                self.pending_irq16 = false;
                0xFF
            }
            _ => 0xFF,
        }
    }

    fn write_port(&mut self, port: u16, value: u8, now: u64) {
        match port - self.config.base {
            0x06 => {
                if value & 1 != 0 && self.state != DspState::Reset {
                    self.reset_dsp();
                    self.state = DspState::Reset;
                } else if value & 1 == 0 && self.state == DspState::Reset {
                    self.state = DspState::ResetWait(now + 20_000);
                }
            }
            0x0C => self.write_dsp(now, value),
            0x04 => self.mixer.index = value,
            0x05 => self.write_mixer(now, value),
            _ => {}
        }
    }

    fn set_stereo(&mut self, now: u64, stereo: bool) {
        if self.dma.stereo != stereo && self.dma.mode != DmaMode::None {
            if stereo {
                self.dma.mul *= 2;
            } else {
                self.dma.mul /= 2;
            }
            self.anchor(now);
        }
        self.dma.stereo = stereo;
    }

    fn pro_volume(&self, value: u8) -> [u8; 2] {
        let low = if self.config.kind == SbType::Sb16 { 1 } else { 3 };
        [((value & 0xF0) >> 3) | low, ((value & 0x0F) << 1) | low]
    }

    fn read_pro_volume(&self, volume: [u8; 2]) -> u8 {
        let pro = matches!(self.config.kind, SbType::SbPro1 | SbType::SbPro2);
        ((volume[0] & 0x1E) << 3) | ((volume[1] & 0x1E) >> 1) | if pro { 0x11 } else { 0 }
    }

    fn write_mixer(&mut self, now: u64, value: u8) {
        let sb16 = self.config.kind == SbType::Sb16;
        let mono = |v: u8| (v & 0x0F) | (v << 4);
        match self.mixer.index {
            0x00 => self.mixer = Mixer { index: 0, ..Mixer::new() },
            0x02 => self.mixer.master = self.pro_volume(mono(value)),
            0x04 => self.mixer.dac = self.pro_volume(value),
            0x06 => self.mixer.fm = self.pro_volume(mono(value)),
            0x08 => self.mixer.cd = self.pro_volume(mono(value)),
            0x0A => {
                if self.config.kind == SbType::Sb2 {
                    self.mixer.dac = [((value & 0x06) << 2) | 3; 2];
                } else {
                    self.mixer.mic = ((value & 0x07) << 2) | if sb16 { 1 } else { 3 };
                }
            }
            0x0E => {
                self.mixer.stereo = value & 0x02 != 0;
                if self.config.kind == SbType::SbPro2 {
                    self.mixer.filter = value & 0x20 == 0;
                }
                self.set_stereo(now, self.mixer.stereo);
            }
            0x22 => self.mixer.master = self.pro_volume(value),
            0x26 => self.mixer.fm = self.pro_volume(value),
            0x28 => self.mixer.cd = self.pro_volume(value),
            0x2E => self.mixer.line = self.pro_volume(value),
            0x30..=0x3A if sb16 => {
                let level = value >> 3;
                match self.mixer.index {
                    0x30 => self.mixer.master[0] = level,
                    0x31 => self.mixer.master[1] = level,
                    0x32 => self.mixer.dac[0] = level,
                    0x33 => self.mixer.dac[1] = level,
                    0x34 => self.mixer.fm[0] = level,
                    0x35 => self.mixer.fm[1] = level,
                    0x36 => self.mixer.cd[0] = level,
                    0x37 => self.mixer.cd[1] = level,
                    0x38 => self.mixer.line[0] = level,
                    0x39 => self.mixer.line[1] = level,
                    _ => self.mixer.mic = level,
                }
            }
            0x80 if sb16 => {
                self.irq = [(1, 2), (2, 5), (4, 7), (8, 10)]
                    .iter()
                    .find(|&&(bit, _)| value & bit != 0)
                    .map_or(0xFF, |&(_, irq)| irq);
            }
            0x81 if sb16 => {
                self.dma8 =
                    [(1, 0), (2, 1), (8, 3)].iter().find(|&&(bit, _)| value & bit != 0).map_or(0xFF, |&(_, d)| d);
                self.dma16 = [(0x20, 5), (0x40, 6), (0x80, 7)]
                    .iter()
                    .find(|&&(bit, _)| value & bit != 0)
                    .map_or(0xFF, |&(_, d)| d);
            }
            index => {
                let pro = matches!(self.config.kind, SbType::SbPro1 | SbType::SbPro2);
                if (pro && index == 0x0C) || (sb16 && (0x3B..=0x47).contains(&index)) {
                    self.mixer.other[usize::from(index)] = value;
                }
            }
        }
    }

    fn read_mixer(&mut self) -> u8 {
        let sb16 = self.config.kind == SbType::Sb16;
        let m = &self.mixer;
        match m.index {
            0x00 => 0x00,
            0x02 => (m.master[1] >> 1) & 0x0E,
            0x04 => self.read_pro_volume(m.dac),
            0x06 => (m.fm[1] >> 1) & 0x0E,
            0x08 => (m.cd[1] >> 1) & 0x0E,
            0x0A => {
                if self.config.kind == SbType::Sb2 {
                    m.dac[0] >> 2
                } else {
                    (m.mic >> 2) & if sb16 { 7 } else { 6 }
                }
            }
            0x0E => 0x11 | if m.stereo { 0x02 } else { 0 } | if m.filter { 0 } else { 0x20 },
            0x22 => self.read_pro_volume(m.master),
            0x26 => self.read_pro_volume(m.fm),
            0x28 => self.read_pro_volume(m.cd),
            0x2E => self.read_pro_volume(m.line),
            0x30..=0x3A if sb16 => {
                let level = match m.index {
                    0x30 => m.master[0],
                    0x31 => m.master[1],
                    0x32 => m.dac[0],
                    0x33 => m.dac[1],
                    0x34 => m.fm[0],
                    0x35 => m.fm[1],
                    0x36 => m.cd[0],
                    0x37 => m.cd[1],
                    0x38 => m.line[0],
                    0x39 => m.line[1],
                    _ => m.mic,
                };
                level << 3
            }
            0x80 if sb16 => match self.irq {
                2 => 0x01,
                5 => 0x02,
                7 => 0x04,
                10 => 0x08,
                _ => 0,
            },
            0x81 if sb16 => {
                let low = match self.dma8 {
                    0 => 0x01,
                    1 => 0x02,
                    3 => 0x08,
                    _ => 0,
                };
                let high = match self.dma16 {
                    5 => 0x20,
                    6 => 0x40,
                    7 => 0x80,
                    _ => 0,
                };
                low | high
            }
            0x82 if sb16 => u8::from(self.pending_irq8) | (u8::from(self.pending_irq16) << 1) | 0x20,
            index => {
                let pro = matches!(self.config.kind, SbType::SbPro1 | SbType::SbPro2);
                if (pro && index == 0x0C) || (sb16 && (0x3B..=0x47).contains(&index)) {
                    m.other[usize::from(index)]
                } else {
                    0x0A
                }
            }
        }
    }
}

#[rustfmt::skip]
const ADPCM2_SCALE: [i8; 24] = [
    0, 1, 0, -1, 1, 3, -1, -3, 2, 6, -2, -6, 4, 12, -4, -12, 8, 24, -8, -24, 6, 48, -16, -48,
];
#[rustfmt::skip]
const ADPCM2_ADJUST: [u8; 24] = [
    0, 4, 0, 4, 252, 4, 252, 4, 252, 4, 252, 4, 252, 4, 252, 4, 252, 4, 252, 4, 252, 0, 252, 0,
];
#[rustfmt::skip]
const ADPCM3_SCALE: [i8; 40] = [
    0, 1, 2, 3, 0, -1, -2, -3,
    1, 3, 5, 7, -1, -3, -5, -7,
    2, 6, 10, 14, -2, -6, -10, -14,
    4, 12, 20, 28, -4, -12, -20, -28,
    5, 15, 25, 35, -5, -15, -25, -35,
];
#[rustfmt::skip]
const ADPCM3_ADJUST: [u8; 40] = [
    0, 0, 0, 8, 0, 0, 0, 8,
    248, 0, 0, 8, 248, 0, 0, 8,
    248, 0, 0, 8, 248, 0, 0, 8,
    248, 0, 0, 8, 248, 0, 0, 8,
    248, 0, 0, 0, 248, 0, 0, 0,
];
#[rustfmt::skip]
const ADPCM4_SCALE: [i8; 64] = [
    0, 1, 2, 3, 4, 5, 6, 7, 0, -1, -2, -3, -4, -5, -6, -7,
    1, 3, 5, 7, 9, 11, 13, 15, -1, -3, -5, -7, -9, -11, -13, -15,
    2, 6, 10, 14, 18, 22, 26, 30, -2, -6, -10, -14, -18, -22, -26, -30,
    4, 12, 20, 28, 36, 44, 52, 60, -4, -12, -20, -28, -36, -44, -52, -60,
];
#[rustfmt::skip]
const ADPCM4_ADJUST: [u8; 64] = [
    0, 0, 0, 0, 0, 16, 16, 16, 0, 0, 0, 0, 0, 16, 16, 16,
    240, 0, 0, 0, 0, 16, 16, 16, 240, 0, 0, 0, 0, 16, 16, 16,
    240, 0, 0, 0, 0, 16, 16, 16, 240, 0, 0, 0, 0, 16, 16, 16,
    240, 0, 0, 0, 0, 0, 0, 0, 240, 0, 0, 0, 0, 0, 0, 0,
];

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: u16 = 0x220;

    fn card(kind: SbType) -> (SoundBlaster, Dma, Memory) {
        (SoundBlaster::new(SbConfig { kind, ..SbConfig::default() }), Dma::new(), Memory::new())
    }

    fn command(sb: &mut SoundBlaster, dma: &mut Dma, mem: &mut Memory, now: u64, bytes: &[u8]) {
        for &b in bytes {
            sb.write(BASE + 0x0C, b, now, dma, mem);
        }
    }

    #[test]
    fn reset_then_version() {
        let (mut sb, mut dma, mut mem) = card(SbType::SbPro2);
        sb.write(BASE + 6, 1, 0, &mut dma, &mut mem);
        sb.write(BASE + 6, 0, 1_000, &mut dma, &mut mem);
        assert_eq!(sb.read(BASE + 0x0E, 10_000, &mut dma, &mut mem) & 0x80, 0, "no data until 20 µs after the reset");
        assert_eq!(sb.read(BASE + 0x0E, 30_000, &mut dma, &mut mem) & 0x80, 0x80);
        assert_eq!(sb.read(BASE + 0x0A, 30_000, &mut dma, &mut mem), 0xAA);
        command(&mut sb, &mut dma, &mut mem, 40_000, &[0xE1]);
        let version = [0; 2].map(|_| sb.read(BASE + 0x0A, 40_000, &mut dma, &mut mem));
        assert_eq!(version, [3, 2]);
        assert_eq!(sb.config.blaster_variable(), "A220 I7 D1 T4");
    }

    #[test]
    fn single_cycle_dma_plays_at_the_sample_rate_and_interrupts_at_the_end() {
        let (mut sb, mut dma, mut mem) = card(SbType::Sb16);
        for i in 0..100u32 {
            mem.write_u8(0x1_0000 + i, 0x80 + i as u8);
        }
        // DMA channel 1: 100 bytes at 1000:0000.
        for (port, value) in
            [(0x0A, 5), (0x0C, 0), (0x0B, 0x49), (0x02, 0), (0x02, 0), (0x83, 1), (0x03, 99), (0x03, 0), (0x0A, 1)]
        {
            dma.write(port, value);
        }
        command(&mut sb, &mut dma, &mut mem, 0, &[0x40, 156]); // 10 kHz
        command(&mut sb, &mut dma, &mut mem, 0, &[0x14, 99, 0]); // 100 bytes
        sb.update(5_000_000, &mut dma, &mut mem); // 5 ms: 50 bytes
        assert_eq!(sb.pcm.iter().map(|s| s.samples.len()).sum::<usize>(), 50);
        assert_eq!(sb.irq_change, Some(false), "starting the transfer withdrew old interrupts");
        assert_eq!(sb.next_event(), Some(6_000_000), "the next millisecond is read at 6 ms");
        sb.update(9_000_000, &mut dma, &mut mem);
        assert_eq!(sb.next_event(), Some(10_000_000), "the block ends at 10 ms");
        sb.update(10_000_000, &mut dma, &mut mem);
        assert_eq!(sb.irq_change, Some(true));
        let samples: Vec<i16> = sb.pcm.iter().flat_map(|s| s.samples.iter().copied()).collect();
        assert_eq!(samples.len(), 100);
        assert_eq!((samples[0], samples[99]), (0, 99 << 8));
        assert_eq!(sb.pcm.len(), 1, "one continuous segment");
        assert_eq!(sb.pcm[0].rate_hz, 10_000);
        assert_eq!(sb.read(BASE + 0x0E, 10_000_000, &mut dma, &mut mem) & 0x80, 0);
        assert_eq!(sb.irq_change, Some(false), "reading 22Eh acknowledged the interrupt");
    }

    #[test]
    fn e2_writes_its_checksum_by_dma() {
        let (mut sb, mut dma, mut mem) = card(SbType::Sb16);
        for (port, value) in
            [(0x0A, 5), (0x0C, 0), (0x0B, 0x45), (0x02, 0x00), (0x02, 0x50), (0x83, 0), (0x03, 0), (0x03, 0), (0x0A, 1)]
        {
            dma.write(port, value);
        }
        command(&mut sb, &mut dma, &mut mem, 0, &[0xE2, 0xA5]);
        // E2 steps for count 0: bits 0, 2, 5, 7 of A5h, then the constant.
        let expected = (0xAA + 0x01 - 0x04 + 0x20 - 0x80 - 106) as u8;
        assert_eq!(mem.read_u8(0x5000), expected);
    }

    #[test]
    fn sb16_mixer_reports_resources() {
        let (mut sb, mut dma, mut mem) = card(SbType::Sb16);
        sb.write(BASE + 4, 0x80, 0, &mut dma, &mut mem);
        assert_eq!(sb.read(BASE + 5, 0, &mut dma, &mut mem), 0x04, "IRQ 7");
        sb.write(BASE + 4, 0x81, 0, &mut dma, &mut mem);
        assert_eq!(sb.read(BASE + 5, 0, &mut dma, &mut mem), 0x22, "DMA 1 and 5");
        assert_eq!(sb.config.blaster_variable(), "A220 I7 D1 H5 T6");
    }
}
