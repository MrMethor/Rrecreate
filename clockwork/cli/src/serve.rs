//! `clockwork serve`: Clockwork as a long-lived process that tools drive (DESIGN.md §4.2), speaking the
//! emulator protocol (`docs/protocol.md`, revision [`PROTOCOL`]): its core and the `dos-pc` platform's
//! extension. Requests are JSON objects, one per line, on stdin (or a TCP connection with `--port N`); each
//! gets one line back with the same `id`. A request that panics inside Clockwork gets an error, and the
//! machine is dropped (its invariants may not hold); copies in memory stay.
//!
//! Addresses: `+OFF` counts from `State::image` (the started program's image, or what `rebase` set);
//! memory a watch reports is named by the linear address it was watched at, other physical memory the same
//! way while paging is off (physical is linear then), `%PHYS` while it is on.

use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::ops::Range;

use dos::Machine;
use serde_json::{Value, json};

/// The protocol's revision this implements (`docs/protocol.md`): it changes with every change to the
/// protocol, and tools refuse an emulator whose revision isn't theirs.
pub const PROTOCOL: u64 = 7;

/// The machine this emulates: the protocol's core and the `dos-pc` extension.
pub const PLATFORM: &str = "dos-pc";

/// The operations that need a machine: an unknown one is answered as such whether a machine is
/// loaded or not.
pub const MACHINE_OPS: &[&str] = &[
    "save",
    "snapshot",
    "rebase",
    "run",
    "sync",
    "syncs",
    "edits",
    "read",
    "write",
    "hash",
    "objects",
    "regs",
    "input",
    "calls",
    "call_counts",
    "call_log",
    "frame_images",
    "take_frame_images",
    "coverage",
    "coverage_end",
    "watch",
    "watch_end",
    "files",
    "files_end",
    "contracts",
    "contracts_end",
    "bounds",
    "bounds_end",
    "uninit",
    "uninit_end",
    "late_inputs",
    "late_inputs_end",
    "frames",
    "sound",
    "image",
    "screenshot",
];

struct State {
    m: Option<Machine>,
    copies: BTreeMap<String, Machine>,
    /// The program's load image: where `+OFF` addresses count from, and its extent.
    image: Range<u32>,
    /// The watch's physical spans with the linear address each was watched at, to name what it
    /// reports as it was asked for when paging is on.
    watched: Vec<(Range<u32>, u32)>,
}

impl State {
    fn new() -> State {
        State { m: None, copies: BTreeMap::new(), image: 0..0, watched: Vec::new() }
    }
}

/// Serves requests from `input` until it ends or a `quit`.
pub fn session(input: impl BufRead, output: impl Write) -> std::io::Result<()> {
    serve(&mut State::new(), input, output)
}

/// Serves requests from `input` with the machine (and copies) in `state` until it ends or a `quit`.
fn serve(state: &mut State, input: impl BufRead, mut output: impl Write) -> std::io::Result<()> {
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
                (id, guarded(state, |state| handle(state, &op, &request)))
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

/// `clockwork ask --port N REQUEST...`: each request (a JSON object, or an op's name alone) to the
/// `serve` on that port, in one connection; prints each answer. Fails if one is an error.
pub fn cmd_ask(args: &[String]) -> Result<std::process::ExitCode, String> {
    let [flag, port, requests @ ..] = args else { return Err("ask --port N REQUEST...".into()) };
    if flag != "--port" || requests.is_empty() {
        return Err("ask --port N REQUEST...".into());
    }
    let port: u16 = port.parse().map_err(|_| format!("bad port {port:?}"))?;
    let answers = ask(port, requests)?;
    answers.iter().for_each(|a| println!("{a}"));
    let failed = answers.iter().any(|a| a.get("error").is_some());
    Ok(if failed { std::process::ExitCode::from(1) } else { std::process::ExitCode::SUCCESS })
}

/// The answers of the `serve` on `port` to `requests` (JSON objects, or ops' names), in one connection.
fn ask(port: u16, requests: &[String]) -> Result<Vec<Value>, String> {
    let stream = std::net::TcpStream::connect(("127.0.0.1", port))
        .map_err(|e| format!("no clockwork serve --port {port}: {e}"))?;
    let mut reader = std::io::BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
    let mut writer = stream;
    let mut answers = Vec::new();
    for request in requests {
        let request =
            if request.trim_start().starts_with('{') { request.clone() } else { json!({"op": request}).to_string() };
        writeln!(writer, "{request}").map_err(|e| e.to_string())?;
        let mut answer = String::new();
        reader.read_line(&mut answer).map_err(|e| e.to_string())?;
        if answer.is_empty() {
            return Err("the server closed the connection".into());
        }
        answers.push(serde_json::from_str(&answer).map_err(|e| format!("not JSON: {e}"))?);
    }
    Ok(answers)
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
            serve_tcp(listener)?;
        }
        _ => return Err("usage: clockwork serve [--port N]".into()),
    }
    Ok(std::process::ExitCode::SUCCESS)
}

/// One machine for every connection, one connection at a time: a tool may ask, close and come back
/// (`clockwork ask`), and finds the machine as it left it. `quit` ends a connection, not the machine.
fn serve_tcp(listener: std::net::TcpListener) -> Result<(), String> {
    let mut state = State::new();
    for stream in listener.incoming() {
        let stream = stream.map_err(|e| e.to_string())?;
        let reader = std::io::BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
        if let Err(e) = serve(&mut state, reader, stream) {
            eprintln!("clockwork: connection: {e}");
        }
    }
    Ok(())
}

fn handle(st: &mut State, op: &str, r: &Value) -> Result<Value, String> {
    match op {
        "hello" => {
            return Ok(json!({
                "protocol": PROTOCOL, "platform": PLATFORM, "emulator": "clockwork",
                "version": env!("CARGO_PKG_VERSION"),
            }));
        }
        "load" | "load_snapshot" => {
            let argv = strings(r, "argv")?;
            let mut options = crate::options::parse_options(&argv)?;
            if op == "load_snapshot" {
                options.load_snapshot = Some(text(r, "file")?.into());
            }
            let (m, _, _) = crate::run::prepare(&options)?;
            // The started program's image, from a snapshot too (a program it started may run then).
            st.image = m.image_range_of(m.first_psp());
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
                if let Some(m) = st.m.as_mut().filter(|m| m.frame_images.is_some()) {
                    reply["images"] = images(m);
                }
            }
            return Ok(reply);
        }
        _ => {}
    }
    let image = st.image.clone();
    let watched = st.watched.clone();
    let Some(m) = st.m.as_mut() else {
        return Err(match MACHINE_OPS.contains(&op) {
            true => "no machine: load one first".into(),
            false => format!("unknown op {op:?}"),
        });
    };
    let addr = |m: &Machine, s: &str| address(m, image.start, s);
    let name = |linear: u32| describe(&image, linear);
    let paging = m.cpu.cr0 & 0x8000_0000 != 0;
    // A watched byte by the linear address it was watched at; other physical memory as `%PHYS`.
    let name_physical = |p: u32| match watched.iter().find(|(span, _)| span.contains(&p)) {
        Some((span, linear)) => describe(&image, linear + (p - span.start)),
        None if paging => format!("%{p:X}"),
        None => describe(&image, p),
    };
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
            st.image = match r.get("at") {
                Some(_) => {
                    let start = addr(m, text(r, "at")?)?;
                    let len = r.get("len").and_then(Value::as_u64).ok_or("rebase needs \"len\" with \"at\"")?;
                    start..start.saturating_add(len as u32)
                }
                None => m.image_range(),
            };
            Ok(json!({"psp": m.dos.psp, "image": format!("@{:X}", st.image.start)}))
        }
        "run" => {
            let ips = m.config.instructions_per_second;
            let until_ms = number(r, "until_ms")?;
            let max = match until_ms {
                Some(ms) => m.cycle_at_ms(ms).saturating_sub(m.cpu.cycles),
                None => number(r, "max_ms")?.unwrap_or(60_000) * ips / 1000,
            };
            let sync = optional(r, "sync", Value::as_str, "an address or \"next\"")?;
            let n = number(r, "n")?.unwrap_or(1);
            let stop = if sync == Some("next") {
                let end = m.cpu.cycles + max;
                let mut stop = m.run_to_next_sync(max);
                for _ in 1..n {
                    if !matches!(stop, dos::Stop::Sync { .. }) {
                        break;
                    }
                    stop = m.run_to_next_sync(end.saturating_sub(m.cpu.cycles));
                }
                stop
            } else if let Some(at) = sync {
                let at = addr(m, at)?;
                m.run_to_sync(at, n, max)
            } else if let Some(ms) = number(r, "ms")? {
                m.run(ms * ips / 1000)
            } else if let Some(cycles) = number(r, "cycles")? {
                m.run(cycles)
            } else if until_ms.is_some() {
                m.run(max)
            } else {
                return Err("run needs ms, cycles, until_ms or sync".into());
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
            m.syncs.add(at, flag(r, "log")?.unwrap_or(false));
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
            if let Some(program) = optional(r, "program", Value::as_str, "a file name")? {
                m.syncs.set_program(at, Some(program.to_owned()));
            }
            Ok(json!({"at": name(at)}))
        }
        "syncs" => {
            let at = addr(m, text(r, "at")?)?;
            let point = m.syncs.get(at).ok_or("not a sync point")?;
            Ok(json!({"hits": point.hits, "cycles": point.cycles, "ticks": m.syncs.ticks()}))
        }
        "edits" => {
            let mut edits = Vec::new();
            for edit in r.get("edits").and_then(Value::as_array).ok_or("missing \"edits\" (a list)")? {
                let tick = edit.get("tick").and_then(Value::as_u64).filter(|&t| t > 0).ok_or("an edit's tick")?;
                let at = addr(m, text(edit, "at")?)?;
                edits.push(dos::sync::Edit { tick, at, bytes: unhex(text(edit, "hex")?)? });
            }
            m.syncs.set_edits(edits)?;
            Ok(json!({"ticks": m.syncs.ticks(), "pending": m.syncs.edits().len()}))
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
        "objects" => {
            // The running program's (or the named one's, if it runs or started the one that does)
            // load image, and its LE objects where its extender put them: null until then, or for a
            // program that isn't LE.
            let (psp, path) = match optional(r, "program", Value::as_str, "a file name")? {
                Some(program) => match m.process_named(program) {
                    Some((psp, path)) => (psp, path.to_owned()),
                    None => return Ok(json!({"program": null, "image": null, "objects": null})),
                },
                None => (m.dos.psp, m.dos.program.clone()),
            };
            let image = m.image_range_of(psp);
            let le = m.drive.file(&path).and_then(|file| dos::le::Le::parse(file).ok().flatten());
            let objects = le.and_then(|le| {
                let bases = le.bases(m)?;
                let objects = le
                    .objects
                    .iter()
                    .zip(bases)
                    .map(|(o, base)| json!({"at": format!("@{base:X}"), "size": o.size, "flags": o.flags}));
                Some(objects.collect::<Vec<_>>())
            });
            let at = format!("@{:X}", image.start);
            Ok(json!({"program": path, "image": at, "image_size": image.len(), "objects": objects}))
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
            // As --input-file reads them: blank lines and comments are skipped.
            let lines = strings(r, "lines")?;
            let scripts = lines.iter().map(|l| l.trim()).filter(|l| !l.is_empty() && !l.starts_with('#'));
            for script in scripts.map(crate::input_script::Script::parse).collect::<Result<Vec<_>, _>>()? {
                script.schedule(m)?;
            }
            Ok(json!({}))
        }
        "calls" => {
            m.track_calls(flag(r, "on")?.unwrap_or(true));
            if let Some(calls) = m.probes.calls.as_deref_mut() {
                calls.count(flag(r, "count")?.unwrap_or(false));
                let limit = r.get("limit").and_then(Value::as_u64).unwrap_or(100_000) as usize;
                calls.log(flag(r, "log")?.unwrap_or(false).then_some(limit));
            }
            Ok(json!({}))
        }
        "call_log" => {
            let log = m.probes.calls.as_deref_mut().and_then(|c| c.take_log());
            let log = log.ok_or(r#"no calls logged: calls {"log": true} first"#)?;
            let calls: Vec<Value> = log
                .calls
                .iter()
                .map(|c| {
                    let kind = if c.hardware {
                        "irq"
                    } else if c.interrupt {
                        "int"
                    } else {
                        "call"
                    };
                    json!([c.depth, name(c.site), name(c.entry), kind])
                })
                .collect();
            let start: Vec<String> = log.start.iter().map(|f| name(f.entry)).collect();
            Ok(json!({ "calls": calls, "dropped": log.dropped, "start": start }))
        }
        "call_counts" => {
            let counts = m.probes.calls.as_deref().and_then(|c| c.counts());
            let counts = counts.ok_or(r#"no calls counted: calls {"count": true} first"#)?;
            let calls: Vec<Value> = counts.iter().map(|(&target, &n)| json!([name(target), n])).collect();
            Ok(json!({ "calls": calls }))
        }
        "frame_images" => {
            m.record_frame_images(flag(r, "on")?.unwrap_or(true));
            Ok(json!({}))
        }
        "take_frame_images" => Ok(json!({ "images": images(m) })),
        "coverage" => {
            if flag(r, "on")?.unwrap_or(true) {
                m.start_coverage();
            } else {
                m.probes.coverage = None;
            }
            Ok(json!({}))
        }
        "coverage_end" => {
            let coverage = m.probes.coverage.take().ok_or("no coverage on")?;
            let executed: Vec<String> = coverage.executed().map(name).collect();
            let calls: Vec<Value> =
                coverage.calls.iter().map(|(&(site, target), &n)| json!([name(site), name(target), n])).collect();
            Ok(json!({ "executed": executed, "calls": calls }))
        }
        "watch" => {
            let mut physical_ranges = Vec::new();
            st.watched.clear();
            for (at, len) in ranges(r, "ranges", "len")? {
                let linear = addr(m, &at)?;
                let mut offset = 0;
                for span in physical_spans(m, linear, len)? {
                    st.watched.push((span.clone(), linear + offset));
                    offset += span.end - span.start;
                    physical_ranges.push(span);
                }
            }
            let reads = flag(r, "reads")?.unwrap_or(false);
            let summary = flag(r, "summary")?.unwrap_or(false);
            let mut watch = if summary {
                dos::watch::Watch::summary(physical_ranges, reads)
            } else {
                dos::watch::Watch::new(physical_ranges, reads)
            };
            if let Some(limit) = number(r, "limit")? {
                watch.limit = limit as usize;
            }
            m.start_watch(watch);
            Ok(json!({}))
        }
        "watch_end" => {
            let watch = m.take_watch().ok_or("no watch on")?;
            if let Some(touches) = &watch.touches {
                let services: BTreeMap<String, u64> =
                    watch.services.iter().map(|(id, n)| (format!("{id:02X}"), *n)).collect();
                let ports: BTreeMap<String, u64> =
                    watch.port_reads.iter().map(|(port, n)| (format!("{port:X}"), *n)).collect();
                let port_uses: Vec<Value> = watch
                    .port_uses
                    .iter()
                    .map(|(u, n)| {
                        json!({"port": format!("{:X}", u.port), "at": name(u.at), "write": u.write,
                               "interrupt": u.in_interrupt, "n": n})
                    })
                    .collect();
                let bytes = touch_runs(touches, &name, &name_physical);
                return Ok(json!({ "bytes": bytes, "services": services, "ports": ports, "port_uses": port_uses }));
            }
            let hits: Vec<Value> = watch.hits.iter().map(|h| hit(h, &name, &name_physical)).collect();
            Ok(json!({"hits": hits, "dropped": watch.dropped}))
        }
        "files" => {
            m.start_files();
            Ok(json!({}))
        }
        "files_end" => {
            let files = m.take_files().ok_or("no file tracing on")?;
            let accesses: Vec<Value> = files
                .iter()
                .map(|f| {
                    let chain: Vec<String> = f.chain.iter().map(|frame| name(frame.entry)).collect();
                    json!({"cycle": f.cycle, "write": f.write, "file": f.path, "offset": f.offset, "len": f.len,
                           "at": name(f.at), "by": name(f.caller), "chain": chain})
                })
                .collect();
            Ok(json!({ "accesses": accesses }))
        }
        "contracts" => {
            // For the image `+OFF` counts from (a launcher's game, after `rebase`).
            m.start_contracts_in(image.clone());
            Ok(json!({}))
        }
        "contracts_end" => {
            let contracts = m.probes.contracts.take().ok_or("no contract inference on")?;
            let mut found: Value = serde_json::from_str(&contracts.to_json("")).map_err(|e| e.to_string())?;
            // Each function by its address, not its offset in the image inference started with.
            let mut functions = found["functions"].take();
            for f in functions.as_array_mut().into_iter().flatten() {
                let offset = f["offset"].as_u64().unwrap_or(0) as u32;
                f["at"] = json!(name(contracts.base + offset));
            }
            Ok(json!({ "functions": functions }))
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
            let more_ranges = if r.get("more").is_some() { ranges(r, "more", "len")? } else { Vec::new() };
            for (at, len) in more_ranges {
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
        "late_inputs" => {
            let items = r.get("sources").and_then(Value::as_array).ok_or("missing \"sources\"")?;
            let mut sources = Vec::new();
            for item in items {
                let source = match spec(item, "sources")? {
                    Spec::Memory(at, len) => dos::late::LateSource::Memory { at: addr(m, &at)?, len },
                    Spec::Device(device) => dos::late::LateSource::Device(device),
                };
                let points = match optional(item, "sync", Value::as_array, "a list of addresses")? {
                    Some(points) => points
                        .iter()
                        .map(|p| addr(m, p.as_str().ok_or("sources: \"sync\" lists addresses")?))
                        .collect::<Result<_, _>>()?,
                    None => Vec::new(),
                };
                sources.push((source, points));
            }
            m.start_late_inputs(dos::late::LateInputs::new(sources));
            Ok(json!({}))
        }
        "late_inputs_end" => {
            let late = m.take_late_inputs().ok_or("no late-input check on")?;
            let findings: Vec<Value> = late
                .findings
                .iter()
                .map(|l| {
                    let bytes: Vec<Value> = l.bytes.iter().map(|(offset, value)| json!([offset, value])).collect();
                    json!({"tick": l.tick_cycle, "source": l.source, "cycle": l.cycle, "at": name(l.at), "bytes": bytes})
                })
                .collect();
            Ok(json!({"late": findings, "dropped": late.dropped}))
        }
        "screenshot" => {
            let file = std::path::PathBuf::from(text(r, "file")?);
            let (w, h, rgb) = m.screen_rgb();
            let png = file.extension().is_some_and(|e| e.eq_ignore_ascii_case("png"));
            let bytes = if png { crate::screenshot::png_image(w, h, &rgb) } else { crate::screenshot::bmp(w, h, &rgb) };
            std::fs::write(&file, bytes).map_err(|e| format!("{}: {e}", file.display()))?;
            Ok(json!({"width": w, "height": h, "frame": m.frames.last().map(|f| f.number)}))
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
            Ok(json!({ "wrote": crate::run::dump_image(m, &dir)? }))
        }
        _ => Err(format!("unknown op {op:?}")),
    }
}

/// The frames' pictures rendered since they were last taken, as `[[number, hex]]`.
fn images(m: &mut Machine) -> Value {
    json!(m.take_frame_images().iter().map(|(number, image)| json!([number, hex(image)])).collect::<Vec<_>>())
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
        dos::watch::By::Edit => json!({"edit": true}),
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
    let from = number(r, "from")?.ok_or("missing \"from\" (cycles)")?;
    let to = number(r, "to")?.unwrap_or(m.cpu.cycles);
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
                "written": t.written, "interrupt_writer": t.interrupt_writer.map(name),
                "interrupt_write_by": t.interrupt_write_by.map(name), "host": t.written_by_host,
            })
        })
        .collect()
}

/// An optional field of a request: absent (or null) is `None`; given as another type, an error, not
/// the default.
fn optional<'a, T>(
    r: &'a Value,
    key: &str,
    read: impl Fn(&'a Value) -> Option<T>,
    what: &str,
) -> Result<Option<T>, String> {
    match r.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => read(value).map(Some).ok_or_else(|| format!("{key:?} must be {what}")),
    }
}

fn flag(r: &Value, key: &str) -> Result<Option<bool>, String> {
    optional(r, key, Value::as_bool, "true or false")
}

/// A count: a whole number, which JSON may write as 16000.0.
fn number(r: &Value, key: &str) -> Result<Option<u64>, String> {
    let whole =
        |v: &Value| v.as_u64().or_else(|| v.as_f64().filter(|f| f.fract() == 0.0 && *f >= 0.0).map(|f| f as u64));
    optional(r, key, whole, "a whole number")
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
    items.iter().map(|item| spec(item, key)).collect()
}

/// One of [`specs`]' items (in the list `key`).
fn spec(item: &Value, key: &str) -> Result<Spec, String> {
    match item.get("device").and_then(Value::as_str) {
        Some(name) => dos::Device::parse(name).map(Spec::Device).ok_or_else(|| format!("no device {name:?}")),
        None => {
            let at = item.get("at").and_then(Value::as_str).ok_or_else(|| format!("{key}: each needs \"at\""))?;
            let len = item.get("len").and_then(Value::as_u64).ok_or_else(|| format!("{key}: each needs \"len\""))?;
            Ok(Spec::Memory(at.to_owned(), len as u32))
        }
    }
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

    /// A folder with T.COM, a program that waits forever.
    fn game(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("clockwork-serve-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("T.COM"), [0xEB, 0xFE]).unwrap();
        dir
    }

    fn ask_state(st: &mut State, request: Value) -> Result<Value, String> {
        let op = request["op"].as_str().unwrap().to_owned();
        guarded(st, |st| handle(st, &op, &request))
    }

    #[test]
    fn input_lines_read_as_a_file_does_and_a_time_past_is_refused() {
        let dir = game("input");
        let mut st = State::new();
        ask_state(&mut st, json!({"op": "load", "argv": [dir.to_str().unwrap(), "T.COM"]})).unwrap();
        ask_state(&mut st, json!({"op": "run", "until_ms": 100.0})).unwrap();
        let past = ask_state(&mut st, json!({"op": "input", "lines": ["50:a"]})).unwrap_err();
        assert!(past.contains("input at 50 ms: the machine is at 100 ms already"), "{past}");
        let lines = json!({"op": "input", "lines": ["# a comment", "", "200:a\r"]});
        assert!(ask_state(&mut st, lines).is_ok(), "a comment, a blank line and a CR are no keys");
        assert!(ask_state(&mut st, json!({"op": "run", "until_ms": 100.5})).is_err(), "a whole number of ms");
        ask_state(&mut st, json!({"op": "run", "until_ms": 400})).unwrap();
        let file = dir.join("screen.png");
        let shot = ask_state(&mut st, json!({"op": "screenshot", "file": file.to_str().unwrap()})).unwrap();
        assert_eq!((shot["width"].as_u64(), shot["height"].as_u64()), (Some(720), Some(400)), "{shot}");
        assert!(std::fs::read(&file).unwrap().starts_with(b"\x89PNG"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn connections_share_the_machine() {
        let dir = game("tcp");
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || serve_tcp(listener));
        let load = json!({"op": "load", "argv": [dir.to_str().unwrap(), "T.COM"]}).to_string();
        let first = ask(port, &[load, r#"{"op": "run", "ms": 50}"#.into(), "quit".into()]).unwrap();
        assert!(first.iter().all(|a| a["ok"] == true), "{first:?}");
        let second = ask(port, &["regs".into()]).unwrap();
        assert!(second[0]["cycles"].as_u64() > Some(0), "the machine the first connection ran: {second:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_panic_is_an_error_and_the_machine_goes() {
        let mut st = State::new();
        st.m = Some(Machine::new(dos::Config::default(), dos::Vfs::new()));
        let reply = guarded(&mut st, |_| panic!("an invariant broke"));
        assert!(reply.is_err_and(|e| e.contains("an invariant broke")));
        assert!(st.m.is_none());
        assert_eq!(guarded(&mut st, |_| Ok(json!(1))), Ok(json!(1)));
    }
}
