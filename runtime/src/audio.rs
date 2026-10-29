//! The sound and music formats DOS games commonly used (M5, D105): a port plays the game's sounds and
//! music from these, and `rrc asset` extracts with the same. Creative's Voice File (VOC) holds sampled
//! sound; Miles' Extended MIDI (XMIDI, `.XMI`) holds music, which becomes standard MIDI.

/// A sampled sound: unsigned 8-bit mono samples at `rate` per second.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Voice {
    pub rate: u32,
    pub samples: Vec<u8>,
}

/// A Creative Voice File's samples (8-bit, mono), at the rate of its first block of sound: its blocks of
/// sound (1, 9), their continuations (2) and silences (3), past its header. Other codings (Creative's ADPCM,
/// 16-bit, stereo) aren't read: an error says which.
pub fn voc(file: &[u8]) -> Result<Voice, String> {
    if !file.starts_with(b"Creative Voice File\x1A") {
        return Err("not a Creative Voice File".into());
    }
    let word = |at: usize| file.get(at..at + 2).map(|w| usize::from(u16::from_le_bytes([w[0], w[1]])));
    let mut at = word(20).ok_or("a Voice File's header cut short")?;
    let (mut rate, mut samples) = (None, Vec::new());
    // A block 8's rate, for the block 1 after it.
    let mut extended: Option<u32> = None;
    while let Some(&kind) = file.get(at) {
        if kind == 0 {
            break;
        }
        let len = file.get(at + 1..at + 4).ok_or("a block's length cut short")?;
        let len = usize::from(len[0]) | usize::from(len[1]) << 8 | usize::from(len[2]) << 16;
        let body =
            file.get(at + 4..at + 4 + len).ok_or_else(|| format!("block {kind} at +{at:X} runs past the file"))?;
        match kind {
            1 => {
                let [divisor, codec, ..] = body[..] else { return Err("a block 1 cut short".into()) };
                if codec != 0 {
                    return Err(format!("a block of codec {codec} (only 8-bit samples are read)"));
                }
                let block_rate = extended.take().unwrap_or(1_000_000 / (256 - u32::from(divisor)));
                rate.get_or_insert(block_rate);
                samples.extend_from_slice(&body[2..]);
            }
            2 => samples.extend_from_slice(body),
            3 => {
                let [low, high, divisor, ..] = body[..] else { return Err("a block 3 cut short".into()) };
                rate.get_or_insert(1_000_000 / (256 - u32::from(divisor)));
                samples.extend(std::iter::repeat_n(0x80, usize::from(u16::from_le_bytes([low, high])) + 1));
            }
            8 => {
                let [low, high, _, mode, ..] = body[..] else { return Err("a block 8 cut short".into()) };
                if mode != 0 {
                    return Err("a stereo block (only mono is read)".into());
                }
                extended = Some(256_000_000 / (65536 - u32::from(u16::from_le_bytes([low, high]))));
            }
            9 => {
                let head = body.get(..12).ok_or("a block 9 cut short")?;
                let block_rate = u32::from_le_bytes([head[0], head[1], head[2], head[3]]);
                let (bits, channels, codec) = (head[4], head[5], u16::from_le_bytes([head[6], head[7]]));
                if (bits, channels, codec) != (8, 1, 0) {
                    return Err(format!(
                        "a block of {bits} bits, {channels} channels, codec {codec} (8, 1, 0 are read)"
                    ));
                }
                rate.get_or_insert(block_rate);
                samples.extend_from_slice(&body[12..]);
            }
            // Markers, text, repeats: nothing to hear.
            _ => {}
        }
        at += 4 + len;
    }
    Ok(Voice { rate: rate.ok_or("no block of sound")?, samples })
}

/// XMIDI's sequences as standard MIDI files (format 0, 60 ticks a quarter at 120 a minute: XMIDI's 120
/// ticks a second). XMIDI gives a note its length where it starts; here it ends with a note off. A delay is
/// its bytes below 80h added up. XMIDI's tempo events go (its ticks are time already); the rest stays.
pub fn midi_from_xmi(xmi: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    let events: Vec<&[u8]> = forms(xmi)?.into_iter().filter_map(|form| chunk(form, b"EVNT")).collect();
    if events.is_empty() {
        return Err("no XMIDI sequence (FORM XMID with EVNT)".into());
    }
    events.into_iter().map(midi_from_events).collect()
}

/// The sequences' FORM XMID bodies: in a CAT after a FORM XDIR, or one alone.
fn forms(xmi: &[u8]) -> Result<Vec<&[u8]>, String> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some((id, body, next)) = iff(xmi, at) {
        match (&id[..], body.get(..4)) {
            (b"FORM", Some(b"XMID")) => out.push(&body[4..]),
            (b"CAT ", Some(b"XMID")) => {
                let mut inner = 4;
                while let Some((id, form, next)) = iff(body, inner) {
                    if &id[..] == b"FORM" && form.starts_with(b"XMID") {
                        out.push(&form[4..]);
                    }
                    inner = next;
                }
            }
            _ => {}
        }
        at = next;
    }
    if out.is_empty() && !xmi.starts_with(b"FORM") {
        return Err("not an XMIDI file".into());
    }
    Ok(out)
}

/// An IFF chunk at `at`: its id, its body, and where the next begins (bodies are padded to even).
fn iff(bytes: &[u8], at: usize) -> Option<([u8; 4], &[u8], usize)> {
    let id: [u8; 4] = bytes.get(at..at + 4)?.try_into().ok()?;
    let len = u32::from_be_bytes(bytes.get(at + 4..at + 8)?.try_into().ok()?) as usize;
    let body = bytes.get(at + 8..at + 8 + len)?;
    Some((id, body, at + 8 + len + (len & 1)))
}

/// A FORM XMID's chunk `id`.
fn chunk<'a>(form: &'a [u8], id: &[u8; 4]) -> Option<&'a [u8]> {
    let mut at = 0;
    while let Some((found, body, next)) = iff(form, at) {
        if &found == id {
            return Some(body);
        }
        at = next;
    }
    None
}

/// A MIDI variable-length number at `*at`.
fn vlq(bytes: &[u8], at: &mut usize) -> Result<u32, String> {
    let mut value = 0u32;
    for _ in 0..4 {
        let b = *bytes.get(*at).ok_or("a number cut short")?;
        *at += 1;
        value = value << 7 | u32::from(b & 0x7F);
        if b & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err("a number of more than 4 bytes".into())
}

fn put_vlq(out: &mut Vec<u8>, value: u32) {
    let mut bytes = vec![(value & 0x7F) as u8];
    let mut v = value >> 7;
    while v > 0 {
        bytes.push((v & 0x7F) as u8 | 0x80);
        v >>= 7;
    }
    out.extend(bytes.iter().rev());
}

/// An EVNT chunk as a standard MIDI file.
fn midi_from_events(evnt: &[u8]) -> Result<Vec<u8>, String> {
    // (time, order: note offs before the rest at a time, then as they came; the event's bytes)
    let mut events: Vec<(u32, u8, usize, Vec<u8>)> = Vec::new();
    let (mut time, mut at) = (0u32, 0);
    while at < evnt.len() {
        let status = evnt[at];
        if status < 0x80 {
            while at < evnt.len() && evnt[at] < 0x80 {
                time += u32::from(evnt[at]);
                at += 1;
            }
            continue;
        }
        at += 1;
        let data = |n: usize, at: &mut usize| -> Result<Vec<u8>, String> {
            let bytes = evnt.get(*at..*at + n).ok_or("an event cut short")?.to_vec();
            *at += n;
            Ok(bytes)
        };
        let n = events.len();
        match status & 0xF0 {
            0x90 => {
                let d = data(2, &mut at)?;
                let length = vlq(evnt, &mut at)?;
                let channel = status & 0x0F;
                if d[1] == 0 {
                    events.push((time, 0, n, vec![0x80 | channel, d[0], 0x40]));
                } else {
                    events.push((time, 1, n, vec![status, d[0], d[1]]));
                    events.push((time + length, 0, n, vec![0x80 | channel, d[0], 0x40]));
                }
            }
            0x80 | 0xA0 | 0xB0 | 0xE0 => events.push((time, 1, n, [vec![status], data(2, &mut at)?].concat())),
            0xC0 | 0xD0 => events.push((time, 1, n, [vec![status], data(1, &mut at)?].concat())),
            _ if status == 0xFF => {
                let kind = data(1, &mut at)?[0];
                let len = vlq(evnt, &mut at)?;
                let body = data(len as usize, &mut at)?;
                if kind == 0x2F {
                    break;
                }
                if kind != 0x51 {
                    let mut e = vec![0xFF, kind];
                    put_vlq(&mut e, len);
                    e.extend(body);
                    events.push((time, 1, n, e));
                }
            }
            _ if status == 0xF0 || status == 0xF7 => {
                let len = vlq(evnt, &mut at)?;
                let mut e = vec![status];
                put_vlq(&mut e, len);
                e.extend(data(len as usize, &mut at)?);
                events.push((time, 1, n, e));
            }
            _ => return Err(format!("status {status:02X} at +{:X} of EVNT", at - 1)),
        }
    }
    events.sort_by_key(|(time, order, n, _)| (*time, *order, *n));
    // 500000 µs a quarter: 120 a minute, 60 ticks a quarter, 120 ticks a second.
    let mut track = vec![0x00, 0xFF, 0x51, 0x03, 0x07, 0xA1, 0x20];
    let mut last = 0;
    for (time, _, _, bytes) in &events {
        put_vlq(&mut track, time - last);
        track.extend(bytes);
        last = *time;
    }
    track.extend([0x00, 0xFF, 0x2F, 0x00]);
    let mut midi = b"MThd\0\0\0\x06\0\0\0\x01\0\x3C".to_vec();
    midi.extend(b"MTrk");
    midi.extend((track.len() as u32).to_be_bytes());
    midi.extend(track);
    Ok(midi)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_voice_file_s_samples_and_rate() {
        let mut file = b"Creative Voice File\x1A\x1A\x00\x0A\x01\x29\x11".to_vec();
        // Block 1: 3 samples at divisor 156 (10000 a second); a silence of 2; a continuation; the end.
        file.extend([1, 5, 0, 0, 156, 0, 10, 20, 30]);
        file.extend([3, 3, 0, 0, 1, 0, 156]);
        file.extend([2, 1, 0, 0, 40, 0]);
        let voice = voc(&file).unwrap();
        assert_eq!(voice, Voice { rate: 10_000, samples: vec![10, 20, 30, 0x80, 0x80, 40] });
        assert!(voc(b"RIFF").is_err());
    }

    #[test]
    fn xmidi_s_notes_end_with_note_offs() {
        // FORM XDIR (1 sequence), CAT XMID, FORM XMID with EVNT: a program change, a note of 60 ticks
        // (60 = 3Ch), a delay of 127 + 3, a tempo (dropped), the end.
        let evnt = [0xC0, 5, 0x90, 60, 100, 0x3C, 0x7F, 0x03, 0xFF, 0x51, 3, 1, 2, 3, 0xFF, 0x2F, 0];
        let form = [&b"XMID"[..], b"EVNT", &(evnt.len() as u32).to_be_bytes(), &evnt, &[0]].concat();
        let cat = [&b"XMID"[..], b"FORM", &(form.len() as u32 - 1).to_be_bytes(), &form].concat();
        let xmi =
            [&b"FORM\0\0\0\x0EXDIRINFO\0\0\0\x02\x01\0"[..], b"CAT ", &(cat.len() as u32).to_be_bytes(), &cat].concat();
        let midi = midi_from_xmi(&xmi).unwrap();
        assert_eq!(midi.len(), 1);
        let track = [
            &[0x00, 0xFF, 0x51, 0x03, 0x07, 0xA1, 0x20][..],
            &[0x00, 0xC0, 5],
            &[0x00, 0x90, 60, 100],
            &[0x3C, 0x80, 60, 0x40],
            &[0x00, 0xFF, 0x2F, 0x00],
        ]
        .concat();
        let expected =
            [&b"MThd\0\0\0\x06\0\0\0\x01\0\x3CMTrk"[..], &(track.len() as u32).to_be_bytes(), &track].concat();
        assert_eq!(midi[0], expected);
        assert!(midi_from_xmi(b"MThd").is_err());
    }
}
