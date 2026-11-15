//! The analysis tools' operations of `clockwork serve` (`docs/protocol.md`): watches, call tracking and its logs,
//! coverage, bounds checks, uninitialized reads, contract inference, file tracing, entries, screenshots and the
//! loaded image. They look at a run and never change what the machine does, so Clockwork's source hash (D76) leaves
//! this file out (D173): a change to a tool doesn't make every session's recording stale. An operation that runs the
//! machine, feeds its input or reads what a recording keeps (`run`, `tick`, `input`, `read`, `frames`, `sound`,
//! `late_inputs`, ...) belongs in `serve.rs`.

use std::collections::BTreeMap;
use std::ops::Range;

use dos::Machine;
use serde_json::{Value, json};

use crate::serve::{flag, hit, number, physical, physical_spans, ranges, text, touch_runs};

/// The tools' operations.
pub const OPS: &[&str] = &[
    "calls",
    "call_log",
    "call_counts",
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
    "uninit_unwritten",
    "uninit_end",
    "entries",
    "entries_end",
    "screenshot",
    "image",
];

/// How the request's addresses read and what answers name them by (`serve.rs`'s).
pub(crate) struct Names<'a> {
    pub addr: &'a dyn Fn(&Machine, &str) -> Result<u32, String>,
    pub name: &'a dyn Fn(u32) -> String,
    pub name_physical: &'a dyn Fn(u32) -> String,
    pub image: Range<u32>,
}

/// One of [`OPS`] on machine `m`; `watched`, the watch's spans and the linear address each was watched at.
pub(crate) fn handle(
    op: &str,
    r: &Value,
    m: &mut Machine,
    watched: &mut Vec<(Range<u32>, u32)>,
    names: &Names,
) -> Result<Value, String> {
    let (addr, name, name_physical, image) = (names.addr, names.name, names.name_physical, &names.image);
    match op {
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
            watched.clear();
            for (at, len) in ranges(r, "ranges", "len")? {
                let linear = addr(m, &at)?;
                let mut offset = 0;
                for span in physical_spans(m, linear, len)? {
                    watched.push((span.clone(), linear + offset));
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
                watch.limit = (limit as usize).min(dos::watch::MAX_LIMIT);
            }
            watch.program_only = flag(r, "program")?.unwrap_or(false);
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
            Ok(json!({"hits": hits, "dropped": watch.dropped, "host": watch.host}))
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
            // From a snapshot: what nothing had written by then, from `uninit_unwritten` on the run it came from (D150).
            match r.get("unwritten").and_then(Value::as_str) {
                Some(file) => {
                    let text = std::fs::read_to_string(file).map_err(|e| format!("{file}: {e}"))?;
                    let pairs: Vec<(u32, u32)> = serde_json::from_str(&text).map_err(|e| format!("{file}: {e}"))?;
                    more.extend(pairs.into_iter().map(|(at, len)| at..at.saturating_add(len)));
                    m.resume_uninit(&more);
                }
                None => m.start_uninit(&more),
            }
            Ok(json!({}))
        }
        "uninit_unwritten" => {
            let file = r.get("file").and_then(Value::as_str).ok_or("missing \"file\"")?;
            let ranges = m.unwritten();
            let pairs: Vec<(u32, u32)> = ranges.iter().map(|r| (r.start, r.end - r.start)).collect();
            let text = serde_json::to_string(&pairs).map_err(|e| e.to_string())?;
            std::fs::write(file, text).map_err(|e| format!("{file}: {e}"))?;
            Ok(json!({"ranges": pairs.len()}))
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
        "entries" => {
            let list = r.get("at").and_then(Value::as_array).ok_or("missing \"at\": a list of addresses")?;
            let at = list
                .iter()
                .map(|p| addr(m, p.as_str().ok_or("\"at\" lists addresses")?))
                .collect::<Result<Vec<_>, _>>()?;
            m.start_entries(at);
            // The entries whose runs from interrupt handlers count too (D174).
            if let Some(list) = r.get("interrupts").and_then(Value::as_array) {
                let mut interrupts = list
                    .iter()
                    .map(|p| addr(m, p.as_str().ok_or("\"interrupts\" lists addresses")?))
                    .collect::<Result<Vec<_>, _>>()?;
                interrupts.sort_unstable();
                if let Some(entries) = m.probes.entries.as_deref_mut() {
                    entries.interrupts = interrupts;
                }
            }
            Ok(json!({}))
        }
        "entries_end" => {
            let entries = m.take_entries().ok_or("no entries kept")?;
            let hits: Vec<Value> = entries
                .hits
                .iter()
                .map(|h| {
                    let g = &h.regs;
                    json!({"cycle": h.cycle, "at": name(h.at), "regs": {
                        "eax": g.gpr[0], "ecx": g.gpr[1], "edx": g.gpr[2], "ebx": g.gpr[3],
                        "esp": g.gpr[4], "ebp": g.gpr[5], "esi": g.gpr[6], "edi": g.gpr[7],
                        "es": g.es(), "cs": g.cs(), "ss": g.ss(), "ds": g.ds(), "flags": g.flags}})
                })
                .collect();
            Ok(json!({"entries": hits, "dropped": entries.dropped}))
        }
        "screenshot" => {
            let file = std::path::PathBuf::from(text(r, "file")?);
            let (w, h, rgb) = m.screen_rgb();
            let png = file.extension().is_some_and(|e| e.eq_ignore_ascii_case("png"));
            let bytes = if png { crate::screenshot::png_image(w, h, &rgb) } else { crate::screenshot::bmp(w, h, &rgb) };
            std::fs::write(&file, bytes).map_err(|e| format!("{}: {e}", file.display()))?;
            Ok(json!({"width": w, "height": h, "frame": m.frames.last().map(|f| f.number)}))
        }
        "image" => {
            let dir = std::path::PathBuf::from(text(r, "dir")?);
            Ok(json!({ "wrote": crate::run::dump_image(m, &dir)? }))
        }
        _ => Err(format!("unknown op {op:?}")),
    }
}
