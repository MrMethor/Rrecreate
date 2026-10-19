//! What `run` prints: a short summary for agents, or the same as JSON for tools.

use std::collections::BTreeSet;

use dos::hash::fnv64;
use dos::{Machine, Stop};
use serde_json::json;

pub(crate) struct Summary {
    stop: String,
    exit_code: Option<u8>,
    cycles: u64,
    elapsed_ms: u64,
    frames: usize,
    distinct_frames: usize,
    midi_bytes: usize,
    speaker_events: usize,
    opl_writes: usize,
    pcm_frames: usize,
    audio_hash: u64,
    /// Every frame's hash, in order.
    frames_hash: u64,
    timer: String,
    irqs: String,
    last_frame: Option<dos::Frame>,
    state_hash: u64,
    console_hash: u64,
    issues: Vec<String>,
    files_changed: Vec<String>,
    contract_lines: Vec<String>,
}

impl Summary {
    pub(crate) fn new(m: &Machine, stop: &Stop) -> Self {
        let stop_text = stop_text(stop);
        let mut issues: Vec<String> = m.diagnostics.iter().cloned().collect();
        if !m.devices.unknown_ports.is_empty() {
            let ports: Vec<String> = m.devices.unknown_ports.iter().map(|p| format!("{p:X}h")).collect();
            issues.push(format!("unemulated I/O ports: {}", ports.join(" ")));
        }
        Self {
            stop: stop_text,
            exit_code: m.exit_code(),
            cycles: m.cpu.cycles,
            elapsed_ms: m.elapsed_ms(),
            frames: m.frames.len(),
            distinct_frames: m.frames.iter().map(|f| f.hash).collect::<BTreeSet<_>>().len(),
            midi_bytes: m.devices.mpu.midi_log.len(),
            speaker_events: m.devices.pit.speaker_log.len(),
            opl_writes: m.audio_log().opl.len(),
            pcm_frames: m.audio_log().pcm.iter().map(|s| s.samples.len() / if s.stereo { 2 } else { 1 }).sum(),
            audio_hash: m.audio_log().hash(),
            frames_hash: {
                let mut h = dos::hash::Fnv64::default();
                for f in &m.frames {
                    std::hash::Hash::hash(&f.hash, &mut h);
                }
                std::hash::Hasher::finish(&h)
            },
            timer: timer_summary(m),
            irqs: irq_summary(m),
            last_frame: m.frames.last().copied(),
            state_hash: m.state_hash(),
            console_hash: fnv64(&m.console.to_vec()),
            issues,
            files_changed: m.drive.changed().map(String::from).collect(),
            contract_lines: m.probes.contracts.as_deref().map(contract_lines).unwrap_or_default(),
        }
    }

    pub(crate) fn print(&self, m: &Machine) {
        println!("stop      {} at {} ms ({} cycles)", self.stop, self.elapsed_ms, self.cycles);
        println!("console   {} bytes: {}", m.console.len(), escape_tail(&m.console.to_vec(), 200));
        match self.last_frame {
            Some(f) => println!(
                "frames    {} ({} distinct), last: mode {:02X}h hash {:016x}",
                self.frames, self.distinct_frames, f.mode, f.hash
            ),
            None => println!("frames    0"),
        }
        if self.midi_bytes + self.speaker_events + self.opl_writes + self.pcm_frames > 0 {
            println!(
                "audio     {} OPL writes, {} Sound Blaster frames, {} MIDI bytes, {} PC speaker changes, hash {:016x}",
                self.opl_writes, self.pcm_frames, self.midi_bytes, self.speaker_events, self.audio_hash
            );
        }
        println!("timer     {}", self.timer);
        println!("irqs      {}", self.irqs);
        println!("state     {:016x}", self.state_hash);
        if !self.files_changed.is_empty() {
            println!("files     changed: {}", self.files_changed.join(", "));
        }
        for issue in &self.issues {
            println!("issue     {issue}");
        }
        for line in &self.contract_lines {
            println!("{line}");
        }
        if let Some(lines) = m.screen_text() {
            let last = lines.iter().rposition(|l| !l.is_empty()).map_or(0, |i| i + 1);
            if last > 0 {
                println!("screen");
                for line in &lines[..last] {
                    println!("  |{line}");
                }
            }
        }
    }

    pub(crate) fn json(&self) -> String {
        let hex = |n: u64| format!("{n:016x}");
        let last_frame =
            self.last_frame.map(|f| json!({"number": f.number, "cycle": f.cycle, "mode": f.mode, "hash": hex(f.hash)}));
        json!({
            "stop": self.stop,
            "exit_code": self.exit_code,
            "cycles": self.cycles,
            "elapsed_ms": self.elapsed_ms,
            "frames": self.frames,
            "distinct_frames": self.distinct_frames,
            "midi_bytes": self.midi_bytes,
            "speaker_events": self.speaker_events,
            "opl_writes": self.opl_writes,
            "pcm_frames": self.pcm_frames,
            "audio_hash": hex(self.audio_hash),
            "frames_hash": hex(self.frames_hash),
            "last_frame": last_frame,
            "state_hash": hex(self.state_hash),
            "console_hash": hex(self.console_hash),
            "issues": self.issues,
            "files_changed": self.files_changed,
        })
        .to_string()
    }
}

/// Why a run stopped, in words.
pub(crate) fn stop_text(stop: &Stop) -> String {
    match stop {
        Stop::Exited(code) => format!("exited with code {code}"),
        Stop::CycleLimit => "time limit reached".into(),
        Stop::WaitingForInput => "waiting for keyboard input".into(),
        Stop::Halted => "halted with interrupts disabled".into(),
        Stop::Fault(exit) => format!("fault: {exit}"),
        Stop::Sync { address, hit } => format!("sync point @{address:X}, hit {hit}"),
    }
}

/// What contract inference saw.
fn contract_lines(contracts: &dos::contracts::Contracts) -> Vec<String> {
    let calls: u64 = contracts.functions.values().map(|c| c.calls + c.abnormal).sum();
    vec![format!("contracts {} functions called, {calls} calls", contracts.functions.len())]
}

/// PIT channel 0 rate and the owners of the timer vectors (INT 08h, INT 1Ch).
fn timer_summary(m: &Machine) -> String {
    let channel = &m.devices.pit.channels[0];
    let hz = dos::clock::PIT_HZ as f64 / channel.period() as f64;
    let vector = |n: u32| (m.mem.read_u16(n * 4 + 2), m.mem.read_u16(n * 4));
    let ((s8, o8), (s1c, o1c)) = (vector(8), vector(0x1C));
    format!(
        "channel 0 mode {} at {hz:.1} Hz; INT 08h -> {s8:04X}:{o8:04X}, INT 1Ch -> {s1c:04X}:{o1c:04X}",
        channel.mode
    )
}

/// How often each IRQ line was delivered, and which lines the PICs mask now.
fn irq_summary(m: &Machine) -> String {
    let taken: Vec<String> = m
        .probes
        .irq_counts
        .iter()
        .enumerate()
        .filter(|(_, n)| **n > 0)
        .map(|(line, n)| format!("{line}: {n}"))
        .collect();
    let imr = u16::from(m.devices.pic.imr()) | (u16::from(m.devices.pic2.imr()) << 8);
    let masked: Vec<String> = (0..16).filter(|line| imr & (1 << line) != 0).map(|line| line.to_string()).collect();
    format!(
        "taken {}; masked {}; vectors {:02X}h/{:02X}h",
        if taken.is_empty() { "none".into() } else { taken.join(", ") },
        if masked.is_empty() { "none".into() } else { masked.join(" ") },
        m.devices.pic.vector_base(),
        m.devices.pic2.vector_base()
    )
}

/// The last `max` bytes, quoted, with control characters escaped.
fn escape_tail(bytes: &[u8], max: usize) -> String {
    let start = bytes.len().saturating_sub(max);
    let mut out = String::from(if start > 0 { "...\"" } else { "\"" });
    for &b in &bytes[start..] {
        match b {
            b'\r' => out.push_str("\\r"),
            b'\n' => out.push_str("\\n"),
            b'"' => out.push_str("\\\""),
            0x20..=0x7E => out.push(char::from(b)),
            _ => out.push_str(&format!("\\x{b:02X}")),
        }
    }
    out.push('"');
    out
}
