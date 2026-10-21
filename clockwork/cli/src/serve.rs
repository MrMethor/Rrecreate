//! `clockwork serve`: Clockwork as a long-lived process that tools drive (DESIGN.md §4.2). Requests
//! are JSON objects, one per line, on stdin (or a TCP connection with `--port N`); each gets one line
//! back with the same `id`: `{"id":…,"ok":true,…}` or `{"id":…,"error":"…"}`. A request that
//! panics inside Clockwork gets an error, and the machine is dropped (its invariants may not hold);
//! copies in memory stay.
//!
//! Addresses are `+OFF` (an offset in the program's load image, hex), `SEG:OFF` (as the CPU sees it
//! now) or `@LINEAR` (hex); answers name addresses the same way, `+OFF` where they fall in the image.
//! Memory a watch or an uninitialized read reports is physical: named the same way while paging is
//! off (physical is linear then), `%PHYS` while it is on.
//!
//! Operations:
//! - `load` `{"argv": [dir, program, options…]}`: a machine as `clockwork run` would start it (the
//!   same options: drives, CD, input scripts…); says where the program's image is and its size.
//!   `load_snapshot` `{"argv": [dir, options…], "file"}`: from a snapshot. `save` `{"file"}`: a
//!   snapshot on disk.
//! - `snapshot` / `restore` / `forget` `{"name"}`: copies of the machine in memory.
//! - `run` `{"ms"}` or `{"cycles"}`, or `{"sync": addr, "n": 1, "max_ms": 60000}`: runs on; says why it
//!   stopped and when. `{"sync": "next", "n": 1}` stops at the n-th next hit of any sync point: the next
//!   tick. `"until_ms"` instead of `"max_ms"` stops at that time from power-on, wherever the run is.
//!   `tick` `{"from", "ranges", "max_ms"}`: a tick in one request: runs to the next tick, then
//!   answers as `read` (`data`), `frames` and `sound` (since cycle `from`) would.
//! - `frames` `{"from", "to"}` (cycles; `to` defaults to now): the frames shown then, as their number,
//!   cycle, video mode and hash. `sound` `{"from", "to"}`: the sound events then: OPL writes (`[ns,
//!   register, value]`), PC speaker waveforms (`[pit_tick, wave]`), digitized sound starts (`[ns, rate,
//!   stereo]`), MIDI bytes (`[pit_tick, byte]`) and CD audio plays (`[ns, sector, sectors]`).
//! - `sync` `{"at", "log": false, "gate": [{"at", "len"}]}`: a sync point; with a gate (D48), a hit
//!   counts only when the gate's bytes changed since the point's last counted hit, and a stop there
//!   says how many hits it passed over before (`skipped`). `syncs`: each point's hits (and their
//!   cycles).
//! - `read` `{"ranges": [{"at", "len"} or {"device"}]}` (hex strings back), `write` `{"at", "hex"}`, `regs`,
//!   `hash` (the machine's state hash, as `clockwork run` prints it).
//!   A device (D49) is `mouse` (what INT 33h would report: `dos::mouse::Mouse::reported_state`) or
//!   `joystick` (the game port's sticks); a gate takes them too.
//! - `input` `{"lines": ["MS:KEYS", …]}`: scripted input, at emulated times from the start.
//! - `watch` `{"ranges": [{"at", "len"}], "reads": false}` … `watch_end`: who wrote (or read) each
//!   byte, with the instruction, the cycle and the call chain (at most `"limit"` accesses, 10000 by
//!   default). With `"summary": true`, who touched
//!   each byte instead, as runs of bytes touched alike: `{"at", "len", "read", "read_in_interrupt",
//!   "written", "interrupt_writer" (the handler's entry, or null), "host" (a service or device wrote)}`. `calls` `{"on": true}` tracks calls
//!   from now on, so a watch started later sees the whole chain.
//! - `bounds` `{"objects": [{"at", "size"}]}` … `bounds_end`; `uninit` `{"more": [{"at", "len"}]}` …
//!   `uninit_end`.
//! - `image` `{"dir"}`: the loaded image for analysis; `rebase`: image offsets count from the
//!   program running now (a child program the first one started).
//! - `quit`.

use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::ops::Range;

use dos::Machine;
use serde_json::{Value, json};

struct State {
    m: Option<Machine>,
    copies: BTreeMap<String, Machine>,
    /// The program's load image: where `+OFF` addresses count from, and its extent.
    image: Range<u32>,
}

/// Serves requests from `input` until it ends or a `quit`.
pub fn session(input: impl BufRead, mut output: impl Write) -> std::io::Result<()> {
    let mut state = State { m: None, copies: BTreeMap::new(), image: 0..0 };
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let (id, reply) = match serde_json::from_str::<Value>(&line) {
            Ok(request) => {
                let id = request.get("id").cloned().unwrap_or(Value::Null);
                let op = request.get("op").and_then(Value::as_str).unwrap_or("").to_owned();
                if op == "quit" {
                    writeln!(output, "{}", json!({"id": id, "ok": true}))?;
                    return Ok(());
                }
                (id, guarded(&mut state, |state| handle(state, &op, &request)))
            }
            Err(e) => (Value::Null, Err(format!("not JSON: {e}"))),
        };
        let mut answer = match reply {
            Ok(Value::Object(fields)) => Value::Object(fields),
            Ok(other) => json!({"value": other}),
            Err(message) => json!({"error": message}),
        };
        answer["id"] = id;
        if answer.get("error").is_none() {
            answer["ok"] = json!(true);
        }
        writeln!(output, "{answer}")?;
        output.flush()?;
    }
    Ok(())
}

/// Runs `f`, answering a panic with an error and dropping the machine.
fn guarded(st: &mut State, f: impl FnOnce(&mut State) -> Result<Value, String>) -> Result<Value, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&mut *st))).unwrap_or_else(|panic| {
        st.m = None;
        let message = panic
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| panic.downcast_ref::<String>().cloned())
            .unwrap_or_default();
        Err(format!("Clockwork panicked: {message}; the machine is gone (load or restore one)"))
    })
}

/// `clockwork serve [--port N]`.
pub fn cmd_serve(args: &[String]) -> Result<std::process::ExitCode, String> {
    match args {
        [] => {
            let stdin = std::io::stdin();
            session(stdin.lock(), std::io::stdout().lock()).map_err(|e| e.to_string())?;
        }
        [flag, port] if flag == "--port" => {
            let port: u16 = port.parse().map_err(|_| format!("bad port {port:?}"))?;
            let listener = std::net::TcpListener::bind(("127.0.0.1", port)).map_err(|e| e.to_string())?;
            eprintln!("clockwork: serving on 127.0.0.1:{port}");
            for stream in listener.incoming() {
                let stream = stream.map_err(|e| e.to_string())?;
                let reader = std::io::BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
                if let Err(e) = session(reader, stream) {
                    eprintln!("clockwork: connection: {e}");
                }
            }
        }
        _ => return Err("usage: clockwork serve [--port N]".into()),
    }
    Ok(std::process::ExitCode::SUCCESS)
}

fn handle(st: &mut State, op: &str, r: &Value) -> Result<Value, String> {
    match op {
        "load" | "load_snapshot" => {
            let argv = strings(r, "argv")?;
            let mut options = crate::options::parse_options(&argv)?;
            if op == "load_snapshot" {
                options.load_snapshot = Some(text(r, "file")?.into());
            }
            let (m, _, _) = crate::run::prepare(&options)?;
            st.image = m.image_range();
            st.copies.clear();
            let image = format!("@{:X}", st.image.start);
            let reply = json!({"psp": m.dos.psp, "image": image, "image_size": st.image.len(), "cycles": m.cpu.cycles});
            st.m = Some(m);
            return Ok(reply);
        }
        "forget" => {
            st.copies.remove(text(r, "name")?);
            return Ok(json!({}));
        }
        "restore" => {
            let name = text(r, "name")?;
            let copy = st.copies.get(name).ok_or_else(|| format!("no copy {name:?}"))?;
            st.m = Some(copy.clone());
            return Ok(json!({"cycles": copy.cpu.cycles}));
        }
        "tick" => {
            let mut run = r.clone();
            run["sync"] = json!("next");
            run["n"] = json!(1);
            let mut reply = handle(st, "run", &run)?;
            if reply.get("sync").is_some() {
                let span = json!({"from": r.get("from").ok_or("missing \"from\" (cycles)")?, "to": reply["cycles"]});
                if r.get("ranges").is_some() {
                    reply["data"] = handle(st, "read", r)?["data"].take();
                }
                reply["frames"] = handle(st, "frames", &span)?["frames"].take();
                reply["sound"] = handle(st, "sound", &span)?;
            }
            return Ok(reply);
        }
        _ => {}
    }
    let image = st.image.clone();
    let m = st.m.as_mut().ok_or("no machine: load one first")?;
    let addr = |m: &Machine, s: &str| address(m, image.start, s);
    let name = |linear: u32| describe(&image, linear);
    let paging = m.cpu.cr0 & 0x8000_0000 != 0;
    let name_physical = |p: u32| if paging { format!("%{p:X}") } else { describe(&image, p) };
    match op {
        "save" => {
            let path = text(r, "file")?;
            let file = std::fs::File::create(path).map_err(|e| format!("{path}: {e}"))?;
            dos::snapshot::save(m, std::io::BufWriter::new(file))?;
            Ok(json!({}))
        }
        "snapshot" => {
            st.copies.insert(text(r, "name")?.to_owned(), m.clone());
            Ok(json!({}))
        }
        "rebase" => {
            st.image = m.image_range();
            Ok(json!({"psp": m.dos.psp, "image": format!("@{:X}", st.image.start)}))
        }
        "run" => {
            let ips = m.config.instructions_per_second;
            let max = match r.get("until_ms").and_then(Value::as_u64) {
                Some(ms) => m.cycle_at_ms(ms).saturating_sub(m.cpu.cycles),
                None => r.get("max_ms").and_then(Value::as_u64).unwrap_or(60_000) * ips / 1000,
            };
            let stop = if r.get("sync").and_then(Value::as_str) == Some("next") {
                let end = m.cpu.cycles + max;
                let mut stop = m.run_to_next_sync(max);
                for _ in 1..r.get("n").and_then(Value::as_u64).unwrap_or(1) {
                    if !matches!(stop, dos::Stop::Sync { .. }) {
                        break;
                    }
                    stop = m.run_to_next_sync(end.saturating_sub(m.cpu.cycles));
                }
                stop
            } else if let Some(at) = r.get("sync").and_then(Value::as_str) {
                let at = addr(m, at)?;
                let n = r.get("n").and_then(Value::as_u64).unwrap_or(1);
                m.run_to_sync(at, n, max)
            } else if let Some(ms) = r.get("ms").and_then(Value::as_u64) {
                m.run(ms * ips / 1000)
            } else if let Some(cycles) = r.get("cycles").and_then(Value::as_u64) {
                m.run(cycles)
            } else {
                return Err("run needs ms, cycles or sync".into());
            };
            let mut reply =
                json!({"stop": crate::summary::stop_text(&stop), "cycles": m.cpu.cycles, "ms": m.elapsed_ms()});
            if let dos::Stop::Sync { address, hit } = stop {
                reply["sync"] = json!({"at": name(address), "hit": hit});
                if let Some(point) = m.syncs.get(address).filter(|p| p.gate.is_some()) {
                    reply["sync"]["skipped"] = json!(point.skipped);
                }
            }
            Ok(reply)
        }
        "sync" => {
            let at = addr(m, text(r, "at")?)?;
            m.syncs.add(at, r.get("log").and_then(Value::as_bool).unwrap_or(false));
            if r.get("gate").is_some() {
                let mut sources = Vec::new();
                for spec in specs(r, "gate")? {
                    sources.push(match spec {
                        Spec::Memory(from, len) => dos::sync::Source::Memory { at: addr(m, &from)?, len },
                        Spec::Device(device) => dos::sync::Source::Device(device),
                    });
                }
                m.syncs.gate(at, sources);
            }
            Ok(json!({"at": name(at)}))
        }
        "syncs" => {
            let at = addr(m, text(r, "at")?)?;
            let point = m.syncs.get(at).ok_or("not a sync point")?;
            Ok(json!({"hits": point.hits, "cycles": point.cycles}))
        }
        "read" => {
            let mut data = Vec::new();
            for spec in specs(r, "ranges")? {
                let bytes: Option<Vec<u8>> = match spec {
                    Spec::Memory(at, len) => {
                        let linear = addr(m, &at)?;
                        (0..len).map(|i| m.peek_linear_u8(linear.wrapping_add(i))).collect()
                    }
                    Spec::Device(device) => Some(m.device_state(device)),
                };
                data.push(bytes.map_or(Value::Null, |b| json!(hex(&b))));
            }
            Ok(json!({"data": data}))
        }
        "write" => {
            let linear = addr(m, text(r, "at")?)?;
            let bytes = unhex(text(r, "hex")?)?;
            for (i, byte) in bytes.into_iter().enumerate() {
                let physical = physical(m, linear.wrapping_add(i as u32))?;
                m.write_u8(physical, byte);
            }
            Ok(json!({}))
        }
        "hash" => Ok(json!({"state_hash": format!("{:016x}", m.state_hash())})),
        "regs" => {
            let g = &m.cpu.regs;
            Ok(json!({
                "eax": g.gpr[0], "ecx": g.gpr[1], "edx": g.gpr[2], "ebx": g.gpr[3],
                "esp": g.gpr[4], "ebp": g.gpr[5], "esi": g.gpr[6], "edi": g.gpr[7],
                "es": g.es(), "cs": g.cs(), "ss": g.ss(), "ds": g.ds(), "eip": g.ip, "flags": g.flags,
                "at": name(m.cpu.segment_base(cpu::seg::CS).wrapping_add(g.ip)),
                "protected": m.cpu.protected_mode(), "cycles": m.cpu.cycles,
            }))
        }
        "input" => {
            for line in strings(r, "lines")? {
                crate::input_script::Script::parse(&line)?.schedule(m);
            }
            Ok(json!({}))
        }
        "calls" => {
            m.track_calls(r.get("on").and_then(Value::as_bool).unwrap_or(true));
            Ok(json!({}))
        }
        "watch" => {
            let mut physical_ranges = Vec::new();
            for (at, len) in ranges(r, "ranges", "len")? {
                physical_ranges.extend(physical_spans(m, addr(m, &at)?, len)?);
            }
            let reads = r.get("reads").and_then(Value::as_bool).unwrap_or(false);
            let summary = r.get("summary").and_then(Value::as_bool).unwrap_or(false);
            let mut watch = if summary {
                dos::watch::Watch::summary(physical_ranges, reads)
            } else {
                dos::watch::Watch::new(physical_ranges, reads)
            };
            if let Some(limit) = r.get("limit").and_then(Value::as_u64) {
                watch.limit = limit as usize;
            }
            m.start_watch(watch);
            Ok(json!({}))
        }
        "watch_end" => {
            let watch = m.take_watch().ok_or("no watch on")?;
            if let Some(touches) = &watch.touches {
                return Ok(json!({ "bytes": touch_runs(touches, &name, &name_physical) }));
            }
            let hits: Vec<Value> = watch.hits.iter().map(|h| hit(h, &name, &name_physical)).collect();
            Ok(json!({"hits": hits, "dropped": watch.dropped}))
        }
        "bounds" => {
            let mut objects = Vec::new();
            for (at, size) in ranges(r, "objects", "size")? {
                objects.push(dos::bounds::Object { start: physical(m, addr(m, &at)?)?, size });
            }
            m.start_bounds(&objects);
            Ok(json!({}))
        }
        "bounds_end" => {
            let bounds = m.take_bounds().ok_or("no bounds checks on")?;
            let findings: Vec<Value> = bounds
                .findings()
                .iter()
                .map(|f| {
                    json!({"instruction": name(f.instruction), "object": f.object, "outside": f.outside,
                           "offsets": f.offsets, "cycle": f.first_cycle})
                })
                .collect();
            Ok(json!({"findings": findings}))
        }
        "uninit" => {
            let mut more = Vec::new();
            for (at, len) in ranges(r, "more", "len").unwrap_or_default() {
                more.extend(physical_spans(m, addr(m, &at)?, len)?);
            }
            m.start_uninit(&more);
            Ok(json!({}))
        }
        "uninit_end" => {
            let uninit = m.take_uninit().ok_or("no uninitialized-read tracking on")?;
            let reads: Vec<Value> = uninit
                .reads
                .iter()
                .map(|u| json!({"at": name_physical(u.address), "value": u.value, "instruction": name(u.instruction), "cycle": u.cycle}))
                .collect();
            Ok(json!({"reads": reads, "dropped": uninit.dropped}))
        }
        "frames" => {
            let (from, to) = span(m, r)?;
            let first = m.frames.partition_point(|f| f.cycle < from);
            let frames: Vec<Value> = m
                .frames
                .iter_from(first)
                .take_while(|f| f.cycle < to)
                .map(|f| json!([f.number, f.cycle, f.mode, format!("{:016x}", f.hash)]))
                .collect();
            Ok(json!({"frames": frames}))
        }
        "sound" => {
            let (from, to) = span(m, r)?;
            let clock = &m.devices.clock;
            let (ns, ticks) = (clock.nanos(from)..clock.nanos(to), clock.pit_ticks(from)..clock.pit_ticks(to));
            let log = m.audio_log();
            let opl = log.opl.iter_from(log.opl.partition_point(|w| w.nanos < ns.start));
            let opl: Vec<Value> =
                opl.take_while(|w| w.nanos < ns.end).map(|w| json!([w.nanos, w.register, w.value])).collect();
            let speaker = log.speaker.iter_from(log.speaker.partition_point(|e| e.pit_tick < ticks.start));
            let speaker: Vec<Value> =
                speaker.take_while(|e| e.pit_tick < ticks.end).map(|e| json!([e.pit_tick, e.wave])).collect();
            // A stream's pieces carry its start; the first one is where it starts.
            let pcm = log.pcm.iter_from(log.pcm.partition_point(|s| s.start_ns < ns.start));
            let pcm = pcm.take_while(|s| s.start_ns < ns.end).filter(|s| s.first_frame == 0);
            let pcm: Vec<Value> = pcm.map(|s| json!([s.start_ns, s.rate_hz, s.stereo])).collect();
            let midi = &m.devices.mpu.midi_log;
            let midi = midi.iter_from(midi.partition_point(|b| b.pit_tick < ticks.start));
            let midi: Vec<Value> =
                midi.take_while(|b| b.pit_tick < ticks.end).map(|b| json!([b.pit_tick, b.value])).collect();
            let cd = log.cd.iter_from(log.cd.partition_point(|s| s.start_ns < ns.start));
            let cd: Vec<Value> =
                cd.take_while(|s| s.start_ns < ns.end).map(|s| json!([s.start_ns, s.sector, s.length])).collect();
            Ok(json!({"opl": opl, "speaker": speaker, "pcm": pcm, "midi": midi, "cd": cd}))
        }
        "image" => {
            let dir = std::path::PathBuf::from(text(r, "dir")?);
            crate::run::dump_image(m, &dir)?;
            Ok(json!({}))
        }
        _ => Err(format!("unknown op {op:?}")),
    }
}

/// A linear address from `+OFF`, `SEG:OFF` or `@LINEAR`.
fn address(m: &Machine, image: u32, s: &str) -> Result<u32, String> {
    let hex = |t: &str| u32::from_str_radix(t, 16).map_err(|_| format!("bad address {s:?}"));
    if let Some(offset) = s.strip_prefix('+') {
        Ok(image.wrapping_add(hex(offset)?))
    } else if let Some(linear) = s.strip_prefix('@') {
        hex(linear)
    } else if let Some((segment, offset)) = s.split_once(':') {
        let segment = u16::from_str_radix(segment, 16).map_err(|_| format!("bad address {s:?}"))?;
        m.peek_address(segment, hex(offset)?)
            .map(|(linear, _)| linear)
            .ok_or_else(|| format!("no segment {segment:04X} now"))
    } else {
        Err(format!("bad address {s:?}: +OFF, SEG:OFF or @LINEAR"))
    }
}

/// `+OFF` in the program's image, `@LINEAR` elsewhere.
fn describe(image: &Range<u32>, linear: u32) -> String {
    if image.contains(&linear) { format!("+{:05X}", linear - image.start) } else { format!("@{linear:X}") }
}

fn physical(m: &Machine, linear: u32) -> Result<u32, String> {
    m.cpu.peek_translate(|at| m.mem.read_u32(at), linear).ok_or_else(|| format!("@{linear:X} isn't mapped"))
}

/// The physical ranges `len` bytes at `linear` occupy (contiguous runs).
fn physical_spans(m: &Machine, linear: u32, len: u32) -> Result<Vec<Range<u32>>, String> {
    let mut spans: Vec<Range<u32>> = Vec::new();
    for i in 0..len {
        let p = physical(m, linear.wrapping_add(i))?;
        match spans.last_mut() {
            Some(span) if span.end == p => span.end += 1,
            _ => spans.push(p..p + 1),
        }
    }
    Ok(spans)
}

fn hit(h: &dos::watch::Hit, name: &impl Fn(u32) -> String, name_physical: &impl Fn(u32) -> String) -> Value {
    let by = match h.by {
        dos::watch::By::Instruction { linear, .. } => json!({"instruction": name(linear)}),
        dos::watch::By::Service { id, ax } => json!({"service": format!("{id:02X}"), "ax": format!("{ax:04X}")}),
        dos::watch::By::Device => json!({"device": true}),
    };
    let chain: Vec<Value> = h
        .chain
        .iter()
        .map(|f| json!({"entry": name(f.entry), "from": name(f.from), "interrupt": f.interrupt}))
        .collect();
    json!({
        "at": name_physical(h.address), "kind": if h.kind == dos::watch::Kind::Write { "write" } else { "read" },
        "value": h.value, "old": h.old, "cycle": h.cycle, "by": by, "chain": chain,
    })
}

/// `{"from", "to"}` in cycles; `to` defaults to now.
fn span(m: &Machine, r: &Value) -> Result<(u64, u64), String> {
    let from = r.get("from").and_then(Value::as_u64).ok_or("missing \"from\" (cycles)")?;
    let to = r.get("to").and_then(Value::as_u64).unwrap_or(m.cpu.cycles);
    Ok((from, to))
}

/// A summary watch's bytes as runs of consecutive bytes touched alike.
fn touch_runs(
    touches: &BTreeMap<u32, dos::watch::Touch>,
    name: &impl Fn(u32) -> String,
    name_physical: &impl Fn(u32) -> String,
) -> Vec<Value> {
    let mut runs: Vec<(u32, u32, dos::watch::Touch)> = Vec::new();
    for (&at, &touch) in touches {
        match runs.last_mut() {
            Some((start, len, t)) if *start + *len == at && *t == touch => *len += 1,
            _ => runs.push((at, 1, touch)),
        }
    }
    runs.iter()
        .map(|(at, len, t)| {
            json!({
                "at": name_physical(*at), "len": len, "read": t.read, "read_in_interrupt": t.read_in_interrupt,
                "written": t.written, "interrupt_writer": t.interrupt_writer.map(name), "host": t.written_by_host,
            })
        })
        .collect()
}

fn text<'a>(r: &'a Value, key: &str) -> Result<&'a str, String> {
    r.get(key).and_then(Value::as_str).ok_or_else(|| format!("missing {key:?}"))
}

fn strings(r: &Value, key: &str) -> Result<Vec<String>, String> {
    let items = r.get(key).and_then(Value::as_array).ok_or_else(|| format!("missing {key:?}"))?;
    items.iter().map(|v| v.as_str().map(String::from).ok_or_else(|| format!("{key}: strings only"))).collect()
}

/// What `read` and a gate take: memory, or an input device's state.
enum Spec {
    Memory(String, u32),
    Device(dos::Device),
}

/// `[{"at": …, "len": n} or {"device": "mouse" | "joystick"}]`.
fn specs(r: &Value, key: &str) -> Result<Vec<Spec>, String> {
    let items = r.get(key).and_then(Value::as_array).ok_or_else(|| format!("missing {key:?}"))?;
    items
        .iter()
        .map(|item| match item.get("device").and_then(Value::as_str) {
            Some(name) => dos::Device::parse(name).map(Spec::Device).ok_or_else(|| format!("no device {name:?}")),
            None => {
                let at = item.get("at").and_then(Value::as_str).ok_or_else(|| format!("{key}: each needs \"at\""))?;
                let len =
                    item.get("len").and_then(Value::as_u64).ok_or_else(|| format!("{key}: each needs \"len\""))?;
                Ok(Spec::Memory(at.to_owned(), len as u32))
            }
        })
        .collect()
}

/// `[{"at": …, <size>: n}]` as (address, n) pairs.
fn ranges(r: &Value, key: &str, size: &str) -> Result<Vec<(String, u32)>, String> {
    let items = r.get(key).and_then(Value::as_array).ok_or_else(|| format!("missing {key:?}"))?;
    items
        .iter()
        .map(|item| {
            let at = item.get("at").and_then(Value::as_str).ok_or_else(|| format!("{key}: each needs \"at\""))?;
            let n = item.get(size).and_then(Value::as_u64).ok_or_else(|| format!("{key}: each needs {size:?}"))?;
            Ok((at.to_owned(), n as u32))
        })
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

fn unhex(s: &str) -> Result<Vec<u8>, String> {
    if s.len() % 2 != 0 {
        return Err(format!("odd hex {s:?}"));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| format!("bad hex {s:?}")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panic_is_an_error_and_the_machine_goes() {
        let mut st = State { m: None, copies: BTreeMap::new(), image: 0..0 };
        st.m = Some(Machine::new(dos::Config::default(), dos::Vfs::new()));
        let reply = guarded(&mut st, |_| panic!("an invariant broke"));
        assert!(reply.is_err_and(|e| e.contains("an invariant broke")));
        assert!(st.m.is_none());
        assert_eq!(guarded(&mut st, |_| Ok(json!(1))), Ok(json!(1)));
    }
}
