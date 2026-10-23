//! `clockwork serve`, driven as a tool would: requests in, one answer per line out.

use serde_json::{Value, json};

/// A COM program: `mov word [200h], 1234h; call setter; int 20h; setter: mov word [200h], 5678h;
/// ret`. Image offsets (from PSP:0100): main's write +00000, the return +00009, setter +0000B, the
/// variable +00100.
const PROGRAM: [u8; 18] =
    [0xC7, 0x06, 0x00, 0x02, 0x34, 0x12, 0xE8, 0x02, 0x00, 0xCD, 0x20, 0xC7, 0x06, 0x00, 0x02, 0x78, 0x56, 0xC3];

fn serve(requests: &[Value]) -> Vec<Value> {
    let input: String = requests.iter().map(|r| format!("{r}\n")).collect();
    let mut output = Vec::new();
    clockwork::serve::session(input.as_bytes(), &mut output).unwrap();
    String::from_utf8(output).unwrap().lines().map(|line| serde_json::from_str(line).unwrap()).collect()
}

#[test]
fn a_tool_finds_who_wrote_a_variable() {
    let dir = std::env::temp_dir().join(format!("clockwork-serve-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("W.COM"), PROGRAM).unwrap();
    let dir = dir.to_string_lossy().into_owned();
    let answers = serve(&[
        json!({"id": 1, "op": "load", "argv": [dir, "W.COM"]}),
        json!({"id": 2, "op": "sync", "at": "+0000B"}),
        json!({"id": 3, "op": "snapshot", "name": "start"}),
        json!({"id": 4, "op": "watch", "ranges": [{"at": "+00100", "len": 2}]}),
        json!({"id": 5, "op": "run", "ms": 100}),
        json!({"id": 6, "op": "watch_end"}),
        json!({"id": 7, "op": "syncs", "at": "+0000B"}),
        json!({"id": 8, "op": "restore", "name": "start"}),
        json!({"id": 9, "op": "run", "sync": "+0000B"}),
        json!({"id": 10, "op": "read", "ranges": [{"at": "+00100", "len": 2}]}),
        json!({"id": 11, "op": "regs"}),
        json!({"id": 12, "op": "frobnicate"}),
        json!({"id": 13, "op": "quit"}),
    ]);
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(answers.len(), 13, "{answers:#?}");
    for answer in &answers[..11] {
        assert_eq!(answer["ok"], true, "{answer}");
    }
    assert_eq!(answers[4]["stop"], "exited with code 0");
    let hits: Vec<(String, u64, Value)> = answers[5]["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| {
            (h["by"]["instruction"].as_str().unwrap().to_owned(), h["value"].as_u64().unwrap(), h["chain"].clone())
        })
        .collect();
    let in_setter = json!([{"entry": "+0000B", "from": "+00009", "interrupt": false}]);
    assert_eq!(
        hits,
        [
            ("+00000".into(), 0x34, json!([])),
            ("+00000".into(), 0x12, json!([])),
            ("+0000B".into(), 0x78, in_setter.clone()),
            ("+0000B".into(), 0x56, in_setter),
        ]
    );
    assert_eq!(answers[5]["hits"][0]["at"], "+00100");
    assert_eq!(answers[6]["hits"], 1);
    // From the copy: stopped at setter, after main's write.
    assert_eq!(answers[8]["sync"], json!({"at": "+0000B", "hit": 1}));
    assert_eq!(answers[9]["data"], json!(["3412"]));
    assert_eq!(answers[10]["at"], "+0000B");
    assert_eq!(answers[11]["error"], "unknown op \"frobnicate\"");
    assert_eq!(answers[11]["id"], 12);
}

/// A COM program: the PC speaker at 1 kHz (PIT channel 2), then forever `again: call step; jmp again`
/// with `step` (+00017) a `ret`; `again` is at +00012.
const TICKS: [u8; 24] = [
    0xB0, 0xB6, 0xE6, 0x43, 0xB0, 0xA9, 0xE6, 0x42, 0xB0, 0x04, 0xE6, 0x42, 0xE4, 0x61, 0x0C, 0x03, 0xE6, 0x61, 0xE8,
    0x02, 0x00, 0xEB, 0xFB, 0xC3,
];

#[test]
fn a_tool_steps_tick_by_tick_and_reads_frames_and_sound() {
    let dir = std::env::temp_dir().join(format!("clockwork-ticks-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("T.COM"), TICKS).unwrap();
    let dir = dir.to_string_lossy().into_owned();
    let answers = serve(&[
        json!({"id": 1, "op": "load", "argv": [dir, "T.COM"]}),
        json!({"id": 2, "op": "sync", "at": "+00012"}),
        json!({"id": 3, "op": "sync", "at": "+00017"}),
        json!({"id": 4, "op": "run", "sync": "next"}),
        json!({"id": 5, "op": "run", "sync": "next"}),
        json!({"id": 6, "op": "run", "sync": "next"}),
        json!({"id": 7, "op": "run", "sync": "next", "n": 3}),
        json!({"id": 8, "op": "tick", "from": 0, "ranges": [{"at": "+00000", "len": 2}]}),
        json!({"id": 9, "op": "run", "ms": 100}),
        json!({"id": 10, "op": "frames", "from": 0}),
        json!({"id": 11, "op": "sound", "from": 0}),
        json!({"id": 12, "op": "frames", "from": 0, "to": 0}),
    ]);
    std::fs::remove_dir_all(&dir).ok();
    let ticks: Vec<(&str, u64)> =
        answers[3..6].iter().map(|a| (a["sync"]["at"].as_str().unwrap(), a["sync"]["hit"].as_u64().unwrap())).collect();
    assert_eq!(ticks, [("+00012", 1), ("+00017", 1), ("+00012", 2)], "{answers:#?}");
    let (third, tick) = (&answers[6], &answers[7]);
    assert_eq!((&third["sync"]["at"], &third["sync"]["hit"]), (&json!("+00017"), &json!(3)), "3 ticks on: {third}");
    assert_eq!((&tick["sync"]["at"], &tick["sync"]["hit"]), (&json!("+00012"), &json!(4)));
    assert_eq!(tick["data"], json!(["B0B6"]), "the program's first bytes");
    assert_eq!(tick["sound"]["speaker"].as_array().unwrap().len(), 1, "the tone, since cycle 0");
    let frames = answers[9]["frames"].as_array().unwrap();
    assert!((6..=8).contains(&frames.len()), "70 frames a second: {frames:?}");
    assert_eq!(frames[0].as_array().unwrap().len(), 4, "number, cycle, mode, hash");
    let speaker = answers[10]["speaker"].as_array().unwrap();
    assert_eq!(speaker.len(), 1, "the tone starts: {speaker:?}");
    assert!(speaker[0][1].get("Periodic").is_some(), "a square wave: {speaker:?}");
    assert_eq!(answers[11]["frames"], json!([]));
}

#[test]
fn coverage_says_what_ran_and_the_calls_made() {
    let dir = std::env::temp_dir().join(format!("clockwork-coverage-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("W.COM"), PROGRAM).unwrap();
    let dir = dir.to_string_lossy().into_owned();
    let answers = serve(&[
        json!({"id": 1, "op": "load", "argv": [dir, "W.COM"]}),
        json!({"id": 2, "op": "coverage"}),
        json!({"id": 3, "op": "run", "ms": 100}),
        json!({"id": 4, "op": "coverage_end"}),
        json!({"id": 5, "op": "coverage_end"}),
    ]);
    std::fs::remove_dir_all(&dir).ok();
    let executed: Vec<&str> = answers[3]["executed"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    for at in ["+00000", "+00006", "+00009", "+0000B", "+00011"] {
        assert!(executed.contains(&at), "{at} in {executed:?}");
    }
    assert!(answers[3]["calls"].as_array().unwrap().contains(&json!(["+00006", "+0000B", 1])), "{}", answers[3]);
    assert_eq!(answers[4]["error"], "no coverage on", "it ended");
}

#[test]
fn a_field_of_the_wrong_type_is_refused_not_defaulted() {
    let dir = std::env::temp_dir().join(format!("clockwork-fields-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("W.COM"), PROGRAM).unwrap();
    let dir = dir.to_string_lossy().into_owned();
    let answers = serve(&[
        json!({"id": 1, "op": "load", "argv": [dir, "W.COM"]}),
        json!({"id": 2, "op": "run", "ms": "100"}),
        json!({"id": 3, "op": "watch", "ranges": [{"at": "+00100", "len": 2}], "reads": 1}),
        json!({"id": 4, "op": "run", "ms": 100, "n": null}),
    ]);
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(answers[1]["error"], "\"ms\" must be a whole number");
    assert_eq!(answers[2]["error"], "\"reads\" must be true or false");
    assert_eq!(answers[3]["stop"], "exited with code 0", "null is absent");
}

#[test]
fn hello_says_what_the_emulator_is() {
    let answers = serve(&[json!({"id": 1, "op": "hello"})]);
    assert_eq!(answers[0]["protocol"], clockwork::serve::PROTOCOL);
    assert_eq!((&answers[0]["platform"], &answers[0]["emulator"]), (&json!("dos-pc"), &json!("clockwork")));
    assert!(answers[0]["version"].is_string(), "{}", answers[0]);
}

#[test]
fn every_operation_is_known_with_or_without_a_machine() {
    let dir = std::env::temp_dir().join(format!("clockwork-ops-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("W.COM"), PROGRAM).unwrap();
    let dir = dir.to_string_lossy().into_owned();
    let ops = clockwork::serve::MACHINE_OPS;
    let without: Vec<Value> = ops.iter().map(|op| json!({"op": op})).collect();
    let mut with = vec![json!({"op": "load", "argv": [dir, "W.COM"]})];
    with.extend(without.iter().cloned());
    let (without, with) = (serve(&without), serve(&with));
    std::fs::remove_dir_all(&dir).ok();
    for (op, answer) in ops.iter().zip(&without) {
        assert_eq!(answer["error"], "no machine: load one first", "{op}");
    }
    for (op, answer) in ops.iter().zip(&with[1..]) {
        assert!(!answer["error"].as_str().unwrap_or("").starts_with("unknown op"), "{op}: {answer}");
    }
    assert_eq!(serve(&[json!({"op": "frobnicate"})])[0]["error"], "unknown op \"frobnicate\"", "without a machine");
}
