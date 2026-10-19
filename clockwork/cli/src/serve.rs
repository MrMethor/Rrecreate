//! `clockwork serve`: Clockwork as a long-lived process that tools drive (DESIGN.md §4.2). Requests
//! are JSON objects, one per line, on stdin (or a TCP connection with `--port N`); each gets one line
//! back with the same `id`: `{"id":…,"ok":true,…}` or `{"id":…,"error":"…"}`.
//!
//! Addresses are `+OFF` (an offset in the program's load image, hex), `SEG:OFF` (as the CPU sees it
//! now) or `@LINEAR` (hex); answers name addresses the same way, `+OFF` where they fall in the image.
//! Memory a watch or an uninitialized read reports is physical: named the same way while paging is
//! off (physical is linear then), `%PHYS` while it is on.
//!
//! Operations:
//! - `load` `{"argv": [dir, program, options…]}`: a machine as `clockwork run` would start it (the
//!   same options: drives, CD, input scripts…). `load_snapshot` `{"argv": [dir, options…],
//!   "file"}`: from a snapshot. `save` `{"file"}`: a snapshot on disk.
//! - `snapshot` / `restore` / `forget` `{"name"}`: copies of the machine in memory.
//! - `run` `{"ms"}` or `{"cycles"}`, or `{"sync": addr, "n": 1, "max_ms": 60000}`: runs on; says why it
//!   stopped and when.
//! - `sync` `{"at", "log": false}`: a sync point; `syncs`: each point's hits (and their cycles).
//! - `read` `{"ranges": [{"at", "len"}]}` (hex strings back), `write` `{"at", "hex"}`, `regs`.
//! - `input` `{"lines": ["MS:KEYS", …]}`: scripted input, at emulated times from the start.
//! - `watch` `{"ranges": [{"at", "len"}], "reads": false}` … `watch_end`: who wrote (or read) each
//!   byte, with the instruction, the cycle and the call chain.
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
                (id, handle(&mut state, &op, &request))
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
            let mut options = crate::parse_options(&argv)?;
            if op == "load_snapshot" {
                options.load_snapshot = Some(text(r, "file")?.into());
            }
            let (m, _, _, base) = crate::prepare(&options)?;
            st.image = base..image_end(&m);
            st.copies.clear();
            let reply = json!({"psp": m.dos.psp, "image": format!("@{base:X}"), "cycles": m.cpu.cycles});
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
            st.image = (u32::from(m.dos.psp) + 0x10) << 4..image_end(m);
            Ok(json!({"psp": m.dos.psp, "image": format!("@{:X}", st.image.start)}))
        }
        "run" => {
            let ips = m.config.instructions_per_second;
            let stop = if let Some(at) = r.get("sync").and_then(Value::as_str) {
                let at = addr(m, at)?;
                let n = r.get("n").and_then(Value::as_u64).unwrap_or(1);
                let max = r.get("max_ms").and_then(Value::as_u64).unwrap_or(60_000) * ips / 1000;
                m.run_to_sync(at, n, max)
            } else if let Some(ms) = r.get("ms").and_then(Value::as_u64) {
                m.run(ms * ips / 1000)
            } else if let Some(cycles) = r.get("cycles").and_then(Value::as_u64) {
                m.run(cycles)
            } else {
                return Err("run needs ms, cycles or sync".into());
            };
            let mut reply = json!({"stop": crate::stop_text(&stop), "cycles": m.cpu.cycles, "ms": m.elapsed_ms()});
            if let dos::Stop::Sync { address, hit } = stop {
                reply["sync"] = json!({"at": name(address), "hit": hit});
            }
            Ok(reply)
        }
        "sync" => {
            let at = addr(m, text(r, "at")?)?;
            m.syncs.add(at, r.get("log").and_then(Value::as_bool).unwrap_or(false));
            Ok(json!({"at": name(at)}))
        }
        "syncs" => {
            let at = addr(m, text(r, "at")?)?;
            let point = m.syncs.get(at).ok_or("not a sync point")?;
            Ok(json!({"hits": point.hits, "cycles": point.cycles}))
        }
        "read" => {
            let mut data = Vec::new();
            for range in ranges(r, "ranges", "len")? {
                let linear = addr(m, &range.0)?;
                let bytes: Option<Vec<u8>> = (0..range.1).map(|i| m.peek_linear_u8(linear.wrapping_add(i))).collect();
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
        "watch" => {
            let mut physical_ranges = Vec::new();
            for (at, len) in ranges(r, "ranges", "len")? {
                physical_ranges.extend(physical_spans(m, addr(m, &at)?, len)?);
            }
            let reads = r.get("reads").and_then(Value::as_bool).unwrap_or(false);
            m.start_watch(dos::watch::Watch::new(physical_ranges, reads));
            Ok(json!({}))
        }
        "watch_end" => {
            let watch = m.take_watch().ok_or("no watch on")?;
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
        "image" => {
            let dir = std::path::PathBuf::from(text(r, "dir")?);
            crate::dump_image(m, &dir)?;
            Ok(json!({}))
        }
        _ => Err(format!("unknown op {op:?}")),
    }
}

/// End of the program's block (where the load image's extent ends).
fn image_end(m: &Machine) -> u32 {
    let psp = u32::from(m.dos.psp);
    (psp + u32::from(m.mem.read_u16((psp.wrapping_sub(1) << 4) + 3))) << 4
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

fn text<'a>(r: &'a Value, key: &str) -> Result<&'a str, String> {
    r.get(key).and_then(Value::as_str).ok_or_else(|| format!("missing {key:?}"))
}

fn strings(r: &Value, key: &str) -> Result<Vec<String>, String> {
    let items = r.get(key).and_then(Value::as_array).ok_or_else(|| format!("missing {key:?}"))?;
    items.iter().map(|v| v.as_str().map(String::from).ok_or_else(|| format!("{key}: strings only"))).collect()
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
