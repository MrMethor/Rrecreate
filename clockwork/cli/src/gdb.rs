//! Ported from MAME `src/osd/modules/debugger/debuggdbstub.cpp` (BSD-3-Clause), in its design: the
//! i386 core registers as a target description, memory as the CPU addresses it, software
//! breakpoints, Ctrl-C.
//!
//! `clockwork gdb [--port N] DIR PROGRAM [run's options]`: the program, stopped at its entry, for a
//! debugger that speaks GDB's remote serial protocol (gdb, Ghidra's debugger), on 127.0.0.1:N
//! (1234 by default) (DESIGN.md §4.2, D7).
//! Registers are 32-bit; in real mode EIP is the offset in CS, so gdb's `$pc` is too (`x/i
//! $cs*16+$eip` shows the instruction). Addresses in `m`, `M` and breakpoints are linear (through the
//! page tables when paging is on). Watchpoints aren't offered: gdb then steps and compares, and
//! Clockwork's own watches (`clockwork serve`) say who wrote a value.
//!
//! A run under a debugger isn't a session: what the debugger does to it is input Clockwork doesn't
//! record.

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::ExitCode;

use dos::{Machine, Stop};

/// Emulated time a `continue` runs before it looks for a Ctrl-C.
const SLICE_CYCLES: u64 = 400_000;

const TARGET_XML: &str = r#"<?xml version="1.0"?>
<!DOCTYPE target SYSTEM "gdb-target.dtd">
<target version="1.0">
<architecture>i386</architecture>
<feature name="org.gnu.gdb.i386.core">
<reg name="eax" bitsize="32" type="int32" regnum="0"/>
<reg name="ecx" bitsize="32" type="int32"/>
<reg name="edx" bitsize="32" type="int32"/>
<reg name="ebx" bitsize="32" type="int32"/>
<reg name="esp" bitsize="32" type="data_ptr"/>
<reg name="ebp" bitsize="32" type="data_ptr"/>
<reg name="esi" bitsize="32" type="int32"/>
<reg name="edi" bitsize="32" type="int32"/>
<reg name="eip" bitsize="32" type="code_ptr"/>
<reg name="eflags" bitsize="32" type="int32"/>
<reg name="cs" bitsize="32" type="int32"/>
<reg name="ss" bitsize="32" type="int32"/>
<reg name="ds" bitsize="32" type="int32"/>
<reg name="es" bitsize="32" type="int32"/>
<reg name="fs" bitsize="32" type="int32"/>
<reg name="gs" bitsize="32" type="int32"/>
<reg name="st0" bitsize="80" type="i387_ext"/>
<reg name="st1" bitsize="80" type="i387_ext"/>
<reg name="st2" bitsize="80" type="i387_ext"/>
<reg name="st3" bitsize="80" type="i387_ext"/>
<reg name="st4" bitsize="80" type="i387_ext"/>
<reg name="st5" bitsize="80" type="i387_ext"/>
<reg name="st6" bitsize="80" type="i387_ext"/>
<reg name="st7" bitsize="80" type="i387_ext"/>
<reg name="fctrl" bitsize="32" type="int" group="float"/>
<reg name="fstat" bitsize="32" type="int" group="float"/>
<reg name="ftag" bitsize="32" type="int" group="float"/>
<reg name="fiseg" bitsize="32" type="int" group="float"/>
<reg name="fioff" bitsize="32" type="int" group="float"/>
<reg name="foseg" bitsize="32" type="int" group="float"/>
<reg name="fooff" bitsize="32" type="int" group="float"/>
<reg name="fop" bitsize="32" type="int" group="float"/>
</feature>
</target>
"#;

/// The registers' order, as the target description gives it: the selectors' indices in `seg`.
const SEGMENTS: [usize; 6] = [cpu::seg::CS, cpu::seg::SS, cpu::seg::DS, cpu::seg::ES, cpu::seg::FS, cpu::seg::GS];
/// Registers in all: 8 general, EIP, EFLAGS, 6 selectors, 8 FPU stack registers, 8 FPU others.
const REGISTERS: usize = 32;

/// `clockwork gdb [--port N] DIR PROGRAM [options]`.
pub fn cmd_gdb(args: &[String]) -> Result<ExitCode, String> {
    let (port, rest) = match args {
        [flag, port, rest @ ..] if flag == "--port" => (port.parse().map_err(|_| format!("bad port {port:?}"))?, rest),
        rest => (1234u16, rest),
    };
    let options = crate::options::parse_options(rest)?;
    let (m, program, _) = crate::run::prepare(&options)?;
    let listener = TcpListener::bind(("127.0.0.1", port)).map_err(|e| format!("port {port}: {e}"))?;
    eprintln!("clockwork: {program} stopped at its entry; gdb: target remote 127.0.0.1:{port}");
    let (stream, _) = listener.accept().map_err(|e| e.to_string())?;
    Stub::new(m, stream).serve().map_err(|e| format!("gdb: {e}"))?;
    Ok(ExitCode::SUCCESS)
}

/// One debugger's connection to a machine.
pub struct Stub {
    m: Machine,
    stream: TcpStream,
    breakpoints: BTreeSet<u32>,
    acks: bool,
}

impl Stub {
    pub fn new(mut m: Machine, stream: TcpStream) -> Stub {
        // Time goes on while the program waits for a key, as in a window.
        m.stop_on_input_wait = false;
        Stub { m, stream, breakpoints: BTreeSet::new(), acks: true }
    }

    /// Answers the debugger's packets until it kills the program, detaches or goes away.
    pub fn serve(&mut self) -> std::io::Result<()> {
        while let Some(packet) = self.packet()? {
            let reply = match packet.as_bytes().first() {
                Some(b'k') => return Ok(()),
                Some(b'D') => {
                    self.send("OK")?;
                    return Ok(());
                }
                Some(b'c') => self.resume(false)?,
                Some(b's') => self.resume(true)?,
                _ => self.answer(&packet),
            };
            self.send(&reply)?;
        }
        Ok(())
    }

    /// The reply to a packet that doesn't run the machine ("" for one this stub doesn't know).
    fn answer(&mut self, packet: &str) -> String {
        let m = &mut self.m;
        match packet {
            "?" => "S05".into(),
            "g" => hex(&registers(m)),
            "qC" => "QC1".into(),
            "qfThreadInfo" => "m1".into(),
            "qsThreadInfo" => "l".into(),
            "qAttached" => "1".into(),
            "QStartNoAckMode" => {
                self.acks = false;
                "OK".into()
            }
            p if p.starts_with("qSupported") => "PacketSize=4000;qXfer:features:read+;QStartNoAckMode+".into(),
            p if p.starts_with('H') => "OK".into(),
            p if p.starts_with("qXfer:features:read:target.xml:") => {
                let (offset, len) = pair(&p["qXfer:features:read:target.xml:".len()..]).unwrap_or((0, 0));
                let chunk = TARGET_XML.get(offset as usize..).unwrap_or("");
                let chunk = &chunk[..chunk.len().min(len as usize)];
                let more = (offset as usize + chunk.len()) < TARGET_XML.len();
                format!("{}{chunk}", if more { 'm' } else { 'l' })
            }
            p if p.starts_with('p') => match usize::from_str_radix(&p[1..], 16).ok().filter(|&n| n < REGISTERS) {
                Some(n) => hex(&register(m, n)),
                None => "E01".into(),
            },
            p if p.starts_with('P') => {
                let set =
                    p[1..].split_once('=').and_then(|(n, v)| Some((usize::from_str_radix(n, 16).ok()?, unhex(v)?)));
                match set {
                    Some((n, value)) if set_register(m, n, &value) => "OK".into(),
                    _ => "E01".into(),
                }
            }
            p if p.starts_with('m') => match pair(&p[1..]) {
                Some((at, len)) => {
                    let bytes: Vec<u8> = (0..len).map_while(|i| m.peek_linear_u8(at.wrapping_add(i))).collect();
                    if bytes.is_empty() && len > 0 { "E14".into() } else { hex(&bytes) }
                }
                None => "E01".into(),
            },
            p if p.starts_with('M') => {
                let write = p[1..].split_once(':').and_then(|(range, data)| Some((pair(range)?, unhex(data)?)));
                match write {
                    Some(((at, _), data)) if write_linear(m, at, &data) => "OK".into(),
                    _ => "E01".into(),
                }
            }
            p if p.starts_with("Z0,") || p.starts_with("Z1,") => match pair(&p[3..]) {
                Some((at, _)) => {
                    self.breakpoints.insert(at);
                    m.syncs.add(at, false);
                    "OK".into()
                }
                None => "E01".into(),
            },
            p if p.starts_with("z0,") || p.starts_with("z1,") => match pair(&p[3..]) {
                Some((at, _)) => {
                    self.breakpoints.remove(&at);
                    m.syncs.remove(at);
                    "OK".into()
                }
                None => "E01".into(),
            },
            _ => String::new(),
        }
    }

    /// Runs one instruction (`step`) or until a breakpoint, the program's end or a Ctrl-C; the stop
    /// reply.
    fn resume(&mut self, step: bool) -> std::io::Result<String> {
        loop {
            let stop = if step { self.m.run(1) } else { self.m.run_to_next_sync(SLICE_CYCLES) };
            match stop {
                Stop::CycleLimit if !step => {
                    if self.interrupted()? {
                        return Ok("S02".into());
                    }
                }
                Stop::CycleLimit | Stop::Sync { .. } => return Ok("S05".into()),
                Stop::Exited(code) => return Ok(format!("W{code:02x}")),
                Stop::Fault(_) => return Ok("S0b".into()),
                _ => return Ok("S05".into()),
            }
        }
    }

    /// Whether the debugger sent a Ctrl-C (byte 03h) while the machine ran.
    fn interrupted(&mut self) -> std::io::Result<bool> {
        self.stream.set_nonblocking(true)?;
        let mut byte = [0];
        let read = self.stream.read(&mut byte);
        self.stream.set_nonblocking(false)?;
        match read {
            Ok(1) => Ok(byte[0] == 0x03),
            Ok(_) => Err(std::io::ErrorKind::UnexpectedEof.into()),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// The next packet's payload (`None` when the debugger went away), acknowledged.
    fn packet(&mut self) -> std::io::Result<Option<String>> {
        let mut byte = [0];
        loop {
            if self.stream.read(&mut byte)? == 0 {
                return Ok(None);
            }
            if byte[0] == b'$' {
                break;
            }
            // `+`, `-` and a Ctrl-C while stopped need nothing.
        }
        let mut payload = Vec::new();
        loop {
            if self.stream.read(&mut byte)? == 0 {
                return Ok(None);
            }
            if byte[0] == b'#' {
                break;
            }
            payload.push(byte[0]);
        }
        let mut checksum = [0; 2];
        self.stream.read_exact(&mut checksum)?;
        if self.acks {
            self.stream.write_all(b"+")?;
        }
        Ok(Some(String::from_utf8_lossy(&payload).into_owned()))
    }

    fn send(&mut self, payload: &str) -> std::io::Result<()> {
        let sum = payload.bytes().fold(0u8, |sum, b| sum.wrapping_add(b));
        self.stream.write_all(format!("${payload}#{sum:02x}").as_bytes())?;
        if self.acks {
            let mut ack = [0];
            self.stream.read_exact(&mut ack)?;
        }
        Ok(())
    }
}

/// Register `n` as the target description lays it out: little-endian, 4 bytes or 10 (ST0-ST7).
fn register(m: &Machine, n: usize) -> Vec<u8> {
    let g = &m.cpu.regs;
    let fpu = m.cpu.fpu.as_ref();
    match n {
        0..=7 => g.gpr[n].to_le_bytes().to_vec(),
        8 => g.ip.to_le_bytes().to_vec(),
        9 => g.flags.to_le_bytes().to_vec(),
        10..=15 => u32::from(g.seg[SEGMENTS[n - 10]]).to_le_bytes().to_vec(),
        16..=23 => x87(fpu.and_then(|f| f.st((n - 16) as u8)).unwrap_or(0.0)).to_vec(),
        24 => u32::from(fpu.map_or(0x037F, |f| f.control_word())).to_le_bytes().to_vec(),
        25 => u32::from(fpu.map_or(0, |f| f.status_word())).to_le_bytes().to_vec(),
        26 => u32::from(fpu.map_or(0xFFFF, |f| f.tag_word())).to_le_bytes().to_vec(),
        _ => vec![0; 4],
    }
}

fn registers(m: &Machine) -> Vec<u8> {
    (0..REGISTERS).flat_map(|n| register(m, n)).collect()
}

/// Sets register `n` (the general registers, EIP and EFLAGS); whether it could.
fn set_register(m: &mut Machine, n: usize, value: &[u8]) -> bool {
    let Ok(value) = <[u8; 4]>::try_from(value) else { return false };
    let value = u32::from_le_bytes(value);
    match n {
        0..=7 => m.cpu.regs.gpr[n] = value,
        8 => m.cpu.regs.ip = value,
        9 => m.cpu.regs.flags = value,
        _ => return false,
    }
    true
}

/// Writes `data` at a linear address; whether every byte is mapped.
fn write_linear(m: &mut Machine, at: u32, data: &[u8]) -> bool {
    for (i, &byte) in data.iter().enumerate() {
        let linear = at.wrapping_add(i as u32);
        let Some(physical) = m.cpu.peek_translate(|a| m.mem.read_u32(a), linear) else { return false };
        m.write_u8(physical, byte);
    }
    true
}

/// A value as the x87's 80-bit extended format.
fn x87(value: f64) -> [u8; 10] {
    let bits = value.to_bits();
    let sign = (bits >> 63) as u16;
    let exponent = ((bits >> 52) & 0x7FF) as i32;
    let fraction = bits & ((1 << 52) - 1);
    let (exponent, mantissa) = match exponent {
        0 if fraction == 0 => (0, 0),
        // Subnormal: normalized, the extended format has the range for it.
        0 => {
            let shift = fraction.leading_zeros() - 11;
            ((1 - 1023 + 16383 - shift as i32) as u16, fraction << (shift + 11))
        }
        0x7FF => (0x7FFF, (1 << 63) | (fraction << 11)),
        e => ((e - 1023 + 16383) as u16, (1 << 63) | (fraction << 11)),
    };
    let mut out = [0; 10];
    out[..8].copy_from_slice(&mantissa.to_le_bytes());
    out[8..].copy_from_slice(&((sign << 15) | exponent).to_le_bytes());
    out
}

/// `ADDR,LEN` in hex.
fn pair(text: &str) -> Option<(u32, u32)> {
    let (a, b) = text.split_once(',')?;
    Some((u32::from_str_radix(a, 16).ok()?, u32::from_str_radix(b.split(';').next()?, 16).ok()?))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A debugger's side: sends a packet, reads the reply.
    struct Client(TcpStream);

    impl Client {
        fn ask(&mut self, payload: &str) -> String {
            let sum = payload.bytes().fold(0u8, |sum, b| sum.wrapping_add(b));
            self.0.write_all(format!("${payload}#{sum:02x}").as_bytes()).unwrap();
            let mut byte = [0];
            self.0.read_exact(&mut byte).unwrap();
            assert_eq!(byte[0], b'+', "acknowledged");
            let mut reply = Vec::new();
            loop {
                self.0.read_exact(&mut byte).unwrap();
                match byte[0] {
                    b'$' => {}
                    b'#' => break,
                    b => reply.push(b),
                }
            }
            let mut checksum = [0; 2];
            self.0.read_exact(&mut checksum).unwrap();
            self.0.write_all(b"+").unwrap();
            String::from_utf8(reply).unwrap()
        }
    }

    #[test]
    fn a_debugger_breaks_reads_and_steps() {
        // Forever: `inc word [count]; mov cx, 1000; loop $; call step; jmp`, `step` (100h + 0Eh) a
        // `ret`, `count` at 10Fh.
        let program = [0xFF, 0x06, 0x0F, 0x01, 0xB9, 0xE8, 0x03, 0xE2, 0xFE, 0xE8, 0x02, 0x00, 0xEB, 0xF2, 0xC3, 0, 0];
        let mut drive = dos::Vfs::new();
        drive.insert_file("T.COM", program.to_vec());
        let m = Machine::with_program(dos::Config::default(), drive, "T.COM", "").unwrap();
        let base = (u32::from(m.dos.psp) << 4) + 0x100;
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let stub = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            Stub::new(m, stream).serve().unwrap();
        });
        let mut gdb = Client(TcpStream::connect(("127.0.0.1", port)).unwrap());
        assert!(gdb.ask("qSupported:xmlRegisters=i386").starts_with("PacketSize=4000;qXfer:features:read+"));
        let xml = gdb.ask("qXfer:features:read:target.xml:0,4000");
        assert!(xml.starts_with('l') && xml.contains("org.gnu.gdb.i386.core"), "{xml}");
        assert_eq!(gdb.ask("?"), "S05");
        assert_eq!(gdb.ask(&format!("Z0,{:x},1", base + 0xE)), "OK");
        assert_eq!(gdb.ask("c"), "S05", "at the breakpoint");
        let registers = unhex(&gdb.ask("g")).unwrap();
        assert_eq!(registers.len(), 16 * 4 + 8 * 10 + 8 * 4);
        assert_eq!(u32::from_le_bytes(registers[32..36].try_into().unwrap()), 0x10E, "EIP, the offset in CS");
        assert_eq!(gdb.ask(&format!("m{:x},2", base + 0xF)), "0100", "the count, after one pass");
        assert_eq!(gdb.ask(&format!("z0,{:x},1", base + 0xE)), "OK");
        assert_eq!(gdb.ask("s"), "S05");
        assert_eq!(gdb.ask("p8"), "0c010000", "the step returned to the jmp at 10Ch");
        assert_eq!(gdb.ask(&format!("M{:x},2:0900", base + 0xF)), "OK");
        assert_eq!(gdb.ask(&format!("m{:x},2", base + 0xF)), "0900");
        assert_eq!(gdb.ask("vMustReplyEmpty"), "");
        gdb.0.write_all(b"$k#6b").unwrap();
        stub.join().unwrap();
    }

    #[test]
    fn values_read_as_the_x87_s_extended_format() {
        assert_eq!(x87(1.0), [0, 0, 0, 0, 0, 0, 0, 0x80, 0xFF, 0x3F]);
        assert_eq!(x87(-2.0), [0, 0, 0, 0, 0, 0, 0, 0x80, 0x00, 0xC0]);
        assert_eq!(x87(0.0), [0; 10]);
    }
}
