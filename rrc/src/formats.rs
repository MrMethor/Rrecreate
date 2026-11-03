//! The game's file formats as knowledge (D78, D103, D116): `knowledge/formats.toml` says, for each kind of
//! file or archive member, what it is (an archive's members, tagged chunks, members one after another, a
//! picture and its frames, a palette, a map, data), the structure at its start (a type of `types.toml`), how
//! its bytes are coded (the runtime's codecs, which a port decodes with too), the function that reads it and
//! the provenance. `rrc asset` decodes by it.

use std::collections::BTreeMap;
use std::path::Path;

use rrc_runtime::codecs::{Lzss, lzhuf, patches, rle, runs, unpack_bits};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::knowledge::Knowledge;
use crate::values::{Reader, at_leaf, decode, size};

pub const FORMATS: &str = "formats.toml";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Formats {
    #[serde(default, rename = "format")]
    pub formats: Vec<Format>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// A directory of entries, each a member's name, place, size and coding.
    Archive,
    /// Tagged chunks one after another, each a member named by its tag.
    Chunks,
    /// Members one after another, named by their number from 0: each its `entry` (a structure, if it has one)
    /// and `size` bytes after it (a number, or the entry's field: `entry.length`).
    Sequence,
    /// `width` × `height` palette indices, with the `palette` it is shown in; `frames` of them, a member's
    /// path naming one (`RAT1.CFA/3`).
    Picture,
    /// Colours of three bytes (`bits` each: 6, the VGA DAC's, or 8).
    Palette,
    /// Cells, `width` × `height` if they are known.
    Map,
    /// Sampled sound: unsigned 8-bit mono samples at `rate` (`coded = "voc"`: a Creative Voice File's,
    /// at its rate).
    Sound,
    /// Music: XMIDI (`.XMI`), or a standard MIDI file.
    Music,
    #[default]
    Data,
}

impl Kind {
    fn text(self) -> &'static str {
        match self {
            Kind::Archive => "archive",
            Kind::Chunks => "chunks",
            Kind::Sequence => "sequence",
            Kind::Picture => "picture",
            Kind::Palette => "palette",
            Kind::Map => "map",
            Kind::Sound => "sound",
            Kind::Music => "music",
            Kind::Data => "data",
        }
    }
}

/// A number of a format: written, or a field of its header (`header.width`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Number {
    Is(u64),
    Field(String),
}

/// How bytes are coded: a codec by name (`stored`, `runs`, `rle`, `lzss`, `lzhuf`, `voc`), or one with its parameters
/// (`{codec = "lzss", fill = 0}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Coded {
    Named(String),
    With(Codec),
}

/// A codec and its parameters: LZSS's ring, fill, start and shortest match (`rrc_runtime::codecs::Lzss`;
/// those not given are lzss.c's).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Codec {
    pub codec: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ring: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shortest: Option<usize>,
    /// The coded bytes begin with how many they decode to: `u16` or `u32`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
}

impl Coded {
    fn codec(&self) -> Codec {
        match self {
            Coded::Named(name) => Codec { codec: name.clone(), ..Codec::default() },
            Coded::With(codec) => codec.clone(),
        }
    }
}

impl Codec {
    fn stored() -> Codec {
        Codec { codec: "stored".into(), ..Codec::default() }
    }

    /// Whether it is a codec there is, with parameters that can be.
    fn check(&self) -> Result<(), String> {
        if !CODECS.contains(&self.codec.as_str()) {
            return Err(format!("{}: no such codec ({})", self.codec, CODECS.join(", ")));
        }
        if let Some(prefix) = self.prefix.as_deref().filter(|p| !["u16", "u32"].contains(p)) {
            return Err(format!("a prefix is u16 or u32, not {prefix}"));
        }
        if self.codec == "lzss" {
            self.lzss()?;
        }
        Ok(())
    }

    /// Its LZSS: lzss.c's, with the parameters given.
    fn lzss(&self) -> Result<Lzss, String> {
        let d = Lzss::OKUMURA;
        let lzss = Lzss {
            ring: self.ring.unwrap_or(d.ring),
            fill: self.fill.unwrap_or(d.fill),
            start: self.start.unwrap_or(d.start),
            shortest: self.shortest.unwrap_or(d.shortest),
        };
        if !lzss.ring.is_power_of_two() || lzss.start >= lzss.ring {
            return Err(format!("lzss: a ring of {} from {} (a power of two, past the start)", lzss.ring, lzss.start));
        }
        Ok(lzss)
    }

    /// `packed` decoded, up to `size` bytes (or its prefix's).
    pub fn decode(&self, packed: &[u8], size: usize) -> Result<Vec<u8>, String> {
        self.check()?;
        let (packed, size) = match self.prefix.as_deref() {
            Some(width) => {
                let n = if width == "u16" { 2 } else { 4 };
                let prefix = packed.get(..n).ok_or("shorter than its size")?;
                let said = prefix.iter().rev().fold(0usize, |v, &b| v << 8 | usize::from(b));
                (&packed[n..], said.min(size))
            }
            None => (packed, size),
        };
        Ok(match self.codec.as_str() {
            "runs" => runs(packed, size),
            "rle" => rle(packed, size),
            "lzhuf" => lzhuf(packed, size),
            "lzss" => self.lzss()?.decode(packed, size),
            // A sound's samples: `Formats::open` reads them with their rate.
            "voc" => rrc_runtime::audio::voc(packed)?.samples,
            _ => packed[..packed.len().min(size)].to_vec(),
        })
    }
}

/// The codecs a format may name.
const CODECS: [&str; 6] = ["stored", "runs", "rle", "lzss", "lzhuf", "voc"];

/// A kind of file or member.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Format {
    pub name: String,
    /// The files it is: names in the game's folder (`DATA\*.IMG`), or an archive's members
    /// (`VAULT.DAT/LEVEL*`); `*` and `?` as DOS has them, in any case.
    pub files: Vec<String>,
    /// The format of those of this many bytes only (headerless textures among pictures with a header): a
    /// file's format is the first that names it and fits its size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    pub kind: Kind,
    /// The structure at its start (a type of `types.toml`), whose fields the numbers here may name
    /// (`header.width`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    /// How the bytes past the header are coded: one codec, or by a number of the header (`method`, its bits
    /// in `mask`) that `methods` gives the codec of.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coded: Option<Coded>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<u64>,
    /// How many bytes past the header are coded (else all): what follows them is the file's too (a palette).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub packed: Option<Number>,
    /// How many bytes they decode to (else as many as `width` × `height`, or as the codec gives); a sequence's
    /// members, how many bytes each after its entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<Number>,
    /// An archive's directory, or the first chunk or member: where it begins (`end`: the file's last bytes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<Number>,
    /// Where an archive's members begin when its entries have no `offset`: each follows the last (else
    /// past the header).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Number>,
    /// An archive's entries: how many.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<Number>,
    /// An archive's entry (a structure of `types.toml`): its `name`, `offset` and `size`, and if its members
    /// are coded, `packed` (their bytes in the file) and `method`, which `methods` gives the codec of. A
    /// sequence's: the structure each member begins with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub methods: BTreeMap<String, Coded>,
    /// Chunks: the tag's bytes (4), and the length after it (`u16` or `u32`) of the bytes after that.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<Number>,
    /// A picture's frames: how many (else 1). Each is at its place in `offsets` (the header's array of them,
    /// from the file's start: `header.offsets`) and coded alone; or, with `deltas = "patches"`, each after
    /// the first is the one before it patched (`rrc_runtime::codecs::patches`), the patches following the
    /// first's coded bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frames: Option<Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offsets: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deltas: Option<String>,
    /// A picture's pixels packed `pixel_bits` each (else 8), the first in a byte's high bits, `stride` bytes a
    /// row (else as many as its width takes): its coded bytes decode to `stride` × `height`. Packed in fewer
    /// than 8 bits, a pixel's value is a place in its table of colours, at `colours` from the file's start,
    /// when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pixel_bits: Option<Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stride: Option<Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colours: Option<Number>,
    /// A picture's palette: a file or member of kind `palette`, or `after`, the bytes after its coded ones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub palette: Option<String>,
    /// A sound's samples a second.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate: Option<Number>,
    /// A palette's bits per colour (6 or 8; 6), and a picture's palette `after`'s.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bits: Option<u8>,
    /// The function that reads it (an ID).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub read_by: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub provenance: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub evidence: String,
}

/// An archive's member: its name, where its bytes are, how many, how many decoded, how coded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub name: String,
    pub offset: usize,
    pub packed: usize,
    pub size: usize,
    pub codec: Codec,
}

/// Bytes as the steps of a decoding left them, each by the step.
pub type Stages = Vec<(String, Vec<u8>)>;

/// A file or member decoded: its bytes, the codecs that decoded them, its header's values, where its
/// bytes are in its file; a picture's frames, and the bytes before its decoding's last step.
#[derive(Debug, Clone)]
pub struct Opened {
    pub file: String,
    pub format: Option<Format>,
    /// Where in the file its bytes begin, and how many.
    pub offset: usize,
    pub packed: usize,
    pub codecs: Vec<String>,
    pub header: Option<Value>,
    pub bytes: Vec<u8>,
    /// The bytes after its coded ones.
    pub after: Vec<u8>,
    /// A sound's samples a second.
    pub rate: Option<u32>,
    /// A picture's frames in its bytes, one after another (1 when the path names one).
    pub frames: usize,
    /// One frame's bytes before its decoding's last step, each as a step left them (a CFA frame's after its
    /// runs, before its pixels are unpacked): what the original may hold of it instead.
    pub stages: Stages,
}

impl Formats {
    pub fn load(dir: &Path) -> Result<Formats, String> {
        let path = dir.join(FORMATS);
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Formats::default()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// The format of a file or member (`FILE/MEMBER`) of `len` bytes: the first that names it and fits its
    /// size.
    pub fn of(&self, path: &str, len: usize) -> Option<&Format> {
        self.formats
            .iter()
            .find(|f| f.bytes.is_none_or(|n| n == len as u64) && f.files.iter().any(|pattern| matches(pattern, path)))
    }

    /// What's wrong with them: a type `types.toml` lacks, a codec there isn't, an archive without its
    /// directory.
    pub fn problems(&self, k: &Knowledge) -> Vec<String> {
        let mut problems = Vec::new();
        let mut names = std::collections::BTreeSet::new();
        for f in &self.formats {
            let at = format!("{FORMATS}: {}", f.name);
            if !names.insert(&f.name) {
                problems.push(format!("{at}: named twice"));
            }
            if f.files.is_empty() {
                problems.push(format!("{at}: no files"));
            }
            for ty in f.header.iter().chain(&f.entry) {
                if let Err(e) = size(ty, &k.types) {
                    problems.push(format!("{at}: {e}"));
                }
            }
            if f.method.is_some() && (f.coded.is_some() || f.methods.is_empty()) {
                problems.push(format!("{at}: a method needs methods, and no coded"));
            }
            for coded in f.coded.iter().chain(f.methods.values()) {
                if let Err(e) = coded.codec().check() {
                    problems.push(format!("{at}: {e}"));
                }
            }
            match f.kind {
                Kind::Archive if f.directory.is_none() || f.count.is_none() || f.entry.is_none() => {
                    problems.push(format!("{at}: an archive needs its directory, count and entry"));
                }
                Kind::Picture if f.width.is_none() || f.height.is_none() => {
                    problems.push(format!("{at}: a picture needs its width and height"));
                }
                Kind::Sequence if f.size.is_none() => {
                    problems.push(format!("{at}: a sequence needs its members' size"));
                }
                _ => {}
            }
            let framed = [&f.frames, &f.pixel_bits, &f.stride, &f.colours].iter().any(|n| n.is_some())
                || f.offsets.is_some()
                || f.deltas.is_some();
            if framed && f.kind != Kind::Picture {
                problems.push(format!("{at}: frames, offsets, deltas, pixel_bits, stride and colours are a picture's"));
            }
            if let Some(deltas) = f.deltas.as_deref().filter(|d| *d != "patches") {
                problems.push(format!("{at}: deltas are patches, not {deltas}"));
            }
            if f.offsets.is_some() && f.deltas.is_some() {
                problems.push(format!("{at}: frames by their offsets or by deltas, not both"));
            }
            if let Some(id) = Some(&f.read_by).filter(|r| !r.is_empty())
                && let Err(e) = crate::knowledge::parse_id(id)
            {
                problems.push(format!("{at}: read_by {e}"));
            }
        }
        problems
    }

    /// A file of the game (`name`) or a member of an archive or chunked file (`FILE/MEMBER`; a member's member,
    /// `START.MIF/LEVL/MAP1`; `#2` for the second of a name), decoded by its format; a picture's frame by its
    /// number (`RAT1.CFA/3`).
    pub fn open(&self, k: &Knowledge, game: &Path, path: &str) -> Result<Opened, String> {
        let parts: Vec<&str> = path.split('/').collect();
        let file = parts[0];
        let bytes = rrc_runtime::original::file_in(game, file)?;
        let mut opened = Opened {
            file: file.to_owned(),
            format: None,
            offset: 0,
            packed: bytes.len(),
            codecs: Vec::new(),
            header: None,
            bytes,
            after: Vec::new(),
            rate: None,
            frames: 1,
            stages: Vec::new(),
        };
        // The path as the formats name it: each member by its name, without `#n`.
        let mut named = file.to_owned();
        let mut frame = None;
        for (i, &wanted) in parts.iter().enumerate().skip(1) {
            let container = self
                .of(&named, opened.bytes.len())
                .ok_or_else(|| format!("{named}: no format in {FORMATS} names it, so no members"))?;
            if container.kind == Kind::Picture && i + 1 == parts.len() {
                let n = wanted.parse::<usize>();
                frame =
                    Some(n.map_err(|_| format!("{path}: {named} is a picture, and {wanted} isn't one of its frames"))?);
                break;
            }
            let members = self.members(container, k, &opened.bytes)?;
            let m = find_member(&members, wanted).ok_or_else(|| {
                let names: Vec<&str> = members.iter().take(12).map(|m| m.name.as_str()).collect();
                format!("{named} has no member {wanted} (it has {})", names.join(", "))
            })?;
            let packed = opened.bytes.get(m.offset..m.offset + m.packed).ok_or_else(|| {
                format!("{path}: {} bytes at +{:X}, past its {} bytes", m.packed, m.offset, opened.bytes.len())
            })?;
            // Where it is in the file, while what holds it is as the file has it.
            if opened.codecs.is_empty() {
                opened.offset += m.offset;
                opened.packed = m.packed;
            }
            let decoded = m.codec.decode(packed, m.size)?;
            opened.bytes = decoded;
            if m.codec.codec != "stored" {
                opened.codecs.push(m.codec.codec.clone());
            }
            named = format!("{named}/{}", m.name);
        }
        let Some(format) = self.of(&named, opened.bytes.len()) else { return Ok(opened) };
        opened.format = Some(format.clone());
        if matches!(format.kind, Kind::Archive | Kind::Chunks | Kind::Sequence) {
            return Ok(opened);
        }
        // Its header, then the coded bytes (`packed` of them, or the rest), then what follows them.
        let whole = std::mem::take(&mut opened.bytes);
        let mut body = whole.clone();
        if let Some(ty) = &format.header {
            let n = size(ty, &k.types)? as usize;
            let head = body.get(..n).ok_or_else(|| format!("{path}: shorter than its header ({ty}, {n} bytes)"))?;
            opened.header = Some(decode(head, ty, &Reader::of(k))?);
            body.drain(..n);
        }
        let number = |n: &Option<Number>| resolve(n.as_ref(), opened.header.as_ref());
        let coded = number(&format.packed)?.unwrap_or(body.len()).min(body.len());
        let after = body.split_off(coded);
        let codec = match (&format.coded, &format.method) {
            (Some(coded), _) => Some(coded.codec()),
            (None, Some(method)) => {
                let value = resolve(Some(method), opened.header.as_ref())?.unwrap_or(0) as u64
                    & format.mask.unwrap_or(u64::MAX);
                let codec = format
                    .methods
                    .get(&value.to_string())
                    .ok_or_else(|| format!("{path}: its method {value} isn't in {}'s methods", format.name))?;
                Some(codec.codec())
            }
            (None, None) => None,
        };
        opened.rate = number(&format.rate)?.map(|r| r as u32);
        if format.kind == Kind::Picture {
            let frames = format.frames(opened.header.as_ref(), &whole, &body, &after, codec.as_ref())?;
            opened.codecs.extend(frames.steps);
            let count = frames.pixels.len();
            (opened.bytes, opened.stages) = match frame {
                Some(f) => {
                    let pixels = frames.pixels.get(f).cloned().ok_or_else(|| {
                        format!("{path}: {named} has {count} frame{} (from 0)", if count == 1 { "" } else { "s" })
                    })?;
                    (pixels, frames.stages[f].clone())
                }
                None if count == 1 => (frames.pixels.concat(), frames.stages.concat()),
                None => (frames.pixels.concat(), Vec::new()),
            };
            opened.frames = if frame.is_some() { 1 } else { count };
            opened.after = after;
            return Ok(opened);
        }
        let cells = format.dimensions(opened.header.as_ref())?.map(|(w, h)| w * h);
        if let Some(codec) = codec {
            let size = number(&format.size)?.or(cells).unwrap_or(usize::MAX);
            if codec.codec == "voc" {
                let voice = rrc_runtime::audio::voc(&body)?;
                opened.rate = opened.rate.or(Some(voice.rate));
                body = voice.samples;
            } else {
                body = codec.decode(&body, size)?;
            }
            if codec.codec != "stored" {
                opened.codecs.push(codec.codec);
            }
        }
        opened.bytes = body;
        opened.after = after;
        Ok(opened)
    }

    /// An archive's or a chunked file's members.
    pub fn members(&self, f: &Format, k: &Knowledge, bytes: &[u8]) -> Result<Vec<Member>, String> {
        let at = |what: &str| format!("{}: {what}", f.name);
        let header = match &f.header {
            Some(ty) => {
                let n = size(ty, &k.types)? as usize;
                let head = bytes.get(..n).ok_or_else(|| at("shorter than its header"))?;
                Some(decode(head, ty, &Reader::of(k))?)
            }
            None => None,
        };
        let at_end = f.directory == Some(Number::Field("end".into()));
        let start = if at_end { 0 } else { resolve(f.directory.as_ref(), header.as_ref())?.unwrap_or(0) };
        match f.kind {
            Kind::Archive => {
                let count = resolve(f.count.as_ref(), header.as_ref())?.ok_or_else(|| at("an archive's count"))?;
                let ty = f.entry.as_ref().ok_or_else(|| at("an archive's entry"))?;
                let n = size(ty, &k.types)? as usize;
                let start = if at_end {
                    bytes.len().checked_sub(count * n).ok_or_else(|| at("shorter than its directory"))?
                } else {
                    start
                };
                // Entries without an offset: each member after the last, from `data` (else past the header).
                let head = match &f.header {
                    Some(ty) => size(ty, &k.types)? as usize,
                    None => 0,
                };
                let mut next = resolve(f.data.as_ref(), header.as_ref())?.unwrap_or(head);
                let mut members = Vec::with_capacity(count);
                for i in 0..count {
                    let b = bytes
                        .get(start + i * n..start + (i + 1) * n)
                        .ok_or_else(|| at("its directory runs past its end"))?;
                    let e = decode(b, ty, &Reader::of(k))?;
                    let field = |name: &str| e.get(name).and_then(Value::as_u64).map(|v| v as usize);
                    let name = e.get("name").map(name_text).ok_or_else(|| at("its entry has no name"))?;
                    let size = field("size").ok_or_else(|| at("its entry has no size"))?;
                    let packed = field("packed").unwrap_or(size);
                    let offset = field("offset").unwrap_or(next);
                    next = offset + packed;
                    let codec = match e.get("method") {
                        Some(method) => {
                            let method = method.as_u64().unwrap_or(0).to_string();
                            f.methods
                                .get(&method)
                                .map(Coded::codec)
                                .ok_or_else(|| at(&format!("{name}'s method {method} isn't in methods")))?
                        }
                        None => Codec::stored(),
                    };
                    members.push(Member { name, offset, packed, size, codec });
                }
                Ok(members)
            }
            Kind::Sequence => {
                let head = match &f.entry {
                    Some(ty) => size(ty, &k.types)? as usize,
                    None => 0,
                };
                let (mut members, mut i) = (Vec::new(), start);
                while i < bytes.len() {
                    let n = members.len();
                    let entry = match &f.entry {
                        Some(ty) => {
                            let b = bytes
                                .get(i..i + head)
                                .ok_or_else(|| at(&format!("member {n}'s entry runs past its end")))?;
                            Some(decode(b, ty, &Reader::of(k))?)
                        }
                        None => None,
                    };
                    let size =
                        field(f.size.as_ref(), "entry", entry.as_ref())?.ok_or_else(|| at("a sequence's size"))?;
                    let mut len = head + size;
                    if len == 0 {
                        return Err(at(&format!("member {n} has no bytes")));
                    }
                    // Members of a size may end the file short (a file cut short); an entry says how many follow.
                    if i + len > bytes.len() {
                        if f.entry.is_some() {
                            return Err(at(&format!(
                                "member {n}: {len} bytes at +{i:X}, past its {} bytes",
                                bytes.len()
                            )));
                        }
                        len = bytes.len() - i;
                    }
                    members.push(Member {
                        name: n.to_string(),
                        offset: i,
                        packed: len,
                        size: len,
                        codec: Codec::stored(),
                    });
                    i += len;
                }
                Ok(members)
            }
            Kind::Chunks => {
                let tag = f.tag.unwrap_or(4);
                let width = match f.length.as_deref().unwrap_or("u16") {
                    "u16" => 2,
                    "u32" => 4,
                    other => return Err(at(&format!("a chunk's length is u16 or u32, not {other}"))),
                };
                let (mut members, mut i) = (Vec::new(), start);
                while i + tag + width <= bytes.len() {
                    let name = String::from_utf8_lossy(&bytes[i..i + tag]).trim_end_matches(['\0', ' ']).to_owned();
                    let len =
                        bytes[i + tag..i + tag + width].iter().rev().fold(0usize, |v, &b| v << 8 | usize::from(b));
                    let offset = i + tag + width;
                    members.push(Member { name, offset, packed: len, size: len, codec: Codec::stored() });
                    i = offset + len;
                }
                Ok(members)
            }
            kind => Err(at(&format!("a {} has no members", kind.text()))),
        }
    }
}

impl Format {
    pub fn kind_text(&self) -> &'static str {
        self.kind.text()
    }

    /// Its width and height, by its header's values.
    pub fn dimensions(&self, header: Option<&Value>) -> Result<Option<(usize, usize)>, String> {
        Ok(match (resolve(self.width.as_ref(), header)?, resolve(self.height.as_ref(), header)?) {
            (Some(w), Some(h)) => Some((w, h)),
            _ => None,
        })
    }
}

/// The member `wanted` names (`MAP1`, `MAP1#2` the second of that name), in any case.
fn find_member<'a>(members: &'a [Member], wanted: &str) -> Option<&'a Member> {
    let (name, nth) = match wanted.rsplit_once('#') {
        Some((name, n)) => (name, n.parse::<usize>().ok()?.checked_sub(1)?),
        None => (wanted, 0),
    };
    members.iter().filter(|m| m.name.eq_ignore_ascii_case(name)).nth(nth)
}

/// A name field's text: characters up to the first NUL.
fn name_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.trim_end_matches('\0').to_owned(),
        Value::Array(bytes) => bytes
            .iter()
            .map_while(|b| b.as_u64().filter(|&b| b != 0).map(|b| char::from(b as u8)))
            .collect::<String>()
            .trim_end()
            .to_owned(),
        other => other.to_string(),
    }
}

/// A number: as written, or the header's field it names.
fn resolve(n: Option<&Number>, header: Option<&Value>) -> Result<Option<usize>, String> {
    field(n, "header", header)
}

/// A number: as written, or the field of `root` it names (`header.width`, a sequence's `entry.length`).
fn field(n: Option<&Number>, root: &str, value: Option<&Value>) -> Result<Option<usize>, String> {
    match n {
        None => Ok(None),
        Some(Number::Is(v)) => Ok(Some(*v as usize)),
        Some(Number::Field(path)) => {
            let field = path.strip_prefix(root).ok_or_else(|| format!("{path}: a number or {root}.FIELD"))?;
            let value = value.ok_or_else(|| format!("{path}: the format has no {root}"))?;
            at_leaf(value, field)
                .and_then(Value::as_u64)
                .map(|v| Some(v as usize))
                .ok_or_else(|| format!("{path}: no such number in the {root}"))
        }
    }
}

/// The numbers of the header's array field `path` names (`header.offsets`).
fn numbers(path: &str, header: Option<&Value>) -> Result<Vec<usize>, String> {
    let field = path.strip_prefix("header").ok_or_else(|| format!("{path}: the header's field, header.FIELD"))?;
    let header = header.ok_or_else(|| format!("{path}: the format has no header"))?;
    let array = at_leaf(header, field).and_then(Value::as_array).ok_or_else(|| format!("{path}: no such array"))?;
    Ok(array.iter().map(|v| v.as_u64().unwrap_or(0) as usize).collect())
}

/// A picture's frames decoded: each one's pixels, and its bytes before the last step (`Opened::stages`); the
/// steps after its codec.
struct Frames {
    pixels: Vec<Vec<u8>>,
    stages: Vec<Stages>,
    steps: Vec<String>,
}

impl Format {
    /// Its frames, from the whole file's bytes (`whole`), its coded ones (`body`) and those after them.
    fn frames(
        &self,
        header: Option<&Value>,
        whole: &[u8],
        body: &[u8],
        after: &[u8],
        codec: Option<&Codec>,
    ) -> Result<Frames, String> {
        let number = |n: &Option<Number>| resolve(n.as_ref(), header);
        let (width, height) = self.dimensions(header)?.ok_or("a picture needs its width and height")?;
        let count = number(&self.frames)?.unwrap_or(1);
        let bits = number(&self.pixel_bits)?.unwrap_or(8);
        if !(1..=8).contains(&bits) {
            return Err(format!("{}: {bits} bits a pixel (1 to 8)", self.name));
        }
        let stride = number(&self.stride)?.unwrap_or((width * bits).div_ceil(8));
        let size = number(&self.size)?.unwrap_or(stride * height);
        let packed = bits < 8 || stride != width;
        let colours = match number(&self.colours)? {
            Some(at) if bits < 8 => {
                let table =
                    whole.get(at..).ok_or_else(|| format!("{}: its colours at +{at:X}, past its end", self.name))?;
                Some(&table[..table.len().min(1 << bits)])
            }
            _ => None,
        };
        // A frame's coded bytes to its pixels, and what its codec left before they were unpacked.
        let decode = |code: &[u8]| -> Result<(Stages, Vec<u8>), String> {
            let decoded = match codec {
                Some(c) => c.decode(code, size)?,
                None => code[..code.len().min(size)].to_vec(),
            };
            if !packed {
                return Ok((Vec::new(), decoded));
            }
            let pixels = unpack_bits(&decoded, bits as u8, (width, height), stride, colours);
            Ok((vec![(codec.map_or("stored", |c| c.codec.as_str()).to_owned(), decoded)], pixels))
        };
        let mut frames = Frames { pixels: Vec::new(), stages: Vec::new(), steps: Vec::new() };
        if let Some(c) = codec.filter(|c| c.codec != "stored") {
            frames.steps.push(c.codec.clone());
        }
        if packed {
            frames.steps.push(format!("{bits} bits a pixel"));
        }
        match (&self.offsets, self.deltas.as_deref()) {
            (Some(offsets), _) => {
                let places = numbers(offsets, header)?;
                for f in 0..count {
                    let at =
                        *places.get(f).ok_or_else(|| format!("frame {f}: {offsets} has {} places", places.len()))?;
                    let code = whole.get(at..).ok_or_else(|| format!("frame {f} at +{at:X}, past its end"))?;
                    let (stages, pixels) = decode(code)?;
                    frames.stages.push(stages);
                    frames.pixels.push(pixels);
                }
            }
            (None, Some(_)) => {
                let (stages, mut pixels) = decode(body)?;
                frames.stages.push(stages);
                frames.pixels.push(pixels.clone());
                let mut at = 0;
                for f in 1..count {
                    let chunk = after
                        .get(at..)
                        .filter(|c| c.len() >= 4)
                        .ok_or_else(|| format!("frame {f}'s patches, past its end"))?;
                    at += patches(&mut pixels, chunk);
                    frames.stages.push(Vec::new());
                    frames.pixels.push(pixels.clone());
                }
                if count > 1 {
                    frames.steps.push("patches".into());
                }
            }
            (None, None) if count <= 1 => {
                let (stages, pixels) = decode(body)?;
                frames.stages.push(stages);
                frames.pixels.push(pixels);
            }
            (None, None) => {
                return Err(format!("{}: {count} frames, without offsets or deltas to find them", self.name));
            }
        }
        Ok(frames)
    }
}

/// A DOS wildcard (`*`, `?`) matching a name, in any case, `\` and `/` alike.
pub fn matches(pattern: &str, name: &str) -> bool {
    let norm = |s: &str| s.to_ascii_uppercase().replace('\\', "/");
    let (p, n) = (norm(pattern), norm(name));
    fn go(p: &[u8], n: &[u8]) -> bool {
        match p.split_first() {
            None => n.is_empty(),
            Some((b'*', rest)) => (0..=n.len()).any(|i| !n[..i].contains(&b'/') && go(rest, &n[i..])),
            Some((b'?', rest)) => n.first().is_some_and(|&c| c != b'/') && go(rest, &n[1..]),
            Some((c, rest)) => n.first() == Some(c) && go(rest, &n[1..]),
        }
    }
    go(p.as_bytes(), n.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcards_match_as_dos_has_them() {
        assert!(matches("VAULT.DAT/LEVEL*", "vault.dat/level2"));
        assert!(matches("DATA\\*.IMG", "data/title.img"));
        assert!(!matches("*.IMG", "DATA/TITLE.IMG"), "a star stays within a folder");
        assert!(matches("LEVEL?.MAP", "LEVEL1.MAP") && !matches("LEVEL?.MAP", "LEVEL10.MAP"));
    }

    #[test]
    fn an_archive_s_members_are_found_and_decoded() {
        let k = Knowledge {
            types: toml::from_str(
                "[[struct]]\nname = \"entry\"\nsize = 8\nfields = [\
             {offset = 0, size = 4, name = \"name\", type = \"u8[4]\"},\
             {offset = 4, size = 1, name = \"offset\", type = \"u8\"},\
             {offset = 5, size = 1, name = \"packed\", type = \"u8\"},\
             {offset = 6, size = 1, name = \"size\", type = \"u8\"},\
             {offset = 7, size = 1, name = \"method\", type = \"u8\"}]\n",
            )
            .unwrap(),
            ..Knowledge::default()
        };
        let formats: Formats = toml::from_str(
            "[[format]]\nname = \"pack\"\nfiles = [\"PACK.DAT\"]\nkind = \"archive\"\ndirectory = 1\ncount = 2\n\
             entry = \"entry\"\nmethods = { 0 = \"stored\", 1 = \"runs\" }\n",
        )
        .unwrap();
        assert!(formats.problems(&k).is_empty(), "{:?}", formats.problems(&k));
        let mut pack = vec![2];
        pack.extend(b"ABC\0\x11\x02\x02\x00");
        pack.extend(b"RUNS\x13\x02\x05\x01");
        pack.extend([7, 9, 0xC5, 3]);
        let members = formats.members(&formats.formats[0], &k, &pack).unwrap();
        assert_eq!((members[0].name.as_str(), members[0].offset, members[0].size), ("ABC", 0x11, 2));
        assert_eq!(members[1].codec.codec, "runs");
        let dir = std::env::temp_dir().join(format!("rrc-formats-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("pack.dat"), &pack).unwrap();
        assert_eq!(formats.open(&k, &dir, "PACK.DAT/runs").unwrap().bytes, [3; 5]);
        assert_eq!(formats.open(&k, &dir, "PACK.DAT/ABC").unwrap().bytes, [7, 9]);
        assert!(formats.open(&k, &dir, "PACK.DAT/NONE").unwrap_err().contains("it has ABC, RUNS"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn chunks_are_members_by_their_tags() {
        let f = Format {
            name: "level".into(),
            files: vec!["*.MIF".into()],
            kind: Kind::Chunks,
            directory: Some(Number::Is(2)),
            ..Format::default()
        };
        let bytes = b"\0\0MHDR\x01\x00\x07MAP1\x02\x00\x01\x02MAP1\x00\x00";
        let members = Formats::default().members(&f, &Knowledge::default(), bytes).unwrap();
        let named: Vec<(&str, usize, usize)> = members.iter().map(|m| (m.name.as_str(), m.offset, m.size)).collect();
        assert_eq!(named, [("MHDR", 8, 1), ("MAP1", 15, 2), ("MAP1", 23, 0)]);
        assert_eq!(find_member(&members, "map1#2").map(|m| m.offset), Some(23));
    }

    #[test]
    fn a_member_s_member_is_decoded_by_its_header() {
        // OUTR holds INNR, whose header (a u16 size, a u16 method, a u16 count of coded bytes) says how many
        // run-length coded bytes follow; a palette of two colours comes after them.
        let k = Knowledge {
            types: toml::from_str(
                "[[struct]]\nname = \"head\"\nsize = 6\nfields = [\
                 {offset = 0, size = 2, name = \"size\", type = \"u16\"},\
                 {offset = 2, size = 2, name = \"flags\", type = \"u16\"},\
                 {offset = 4, size = 2, name = \"length\", type = \"u16\"}]\n",
            )
            .unwrap(),
            ..Knowledge::default()
        };
        let formats: Formats = toml::from_str(
            "[[format]]\nname = \"file\"\nfiles = [\"*.BIN\"]\nkind = \"chunks\"\n\
             [[format]]\nname = \"outer\"\nfiles = [\"*.BIN/OUTR\"]\nkind = \"chunks\"\n\
             [[format]]\nname = \"inner\"\nfiles = [\"*.BIN/OUTR/INNR\"]\nkind = \"data\"\nheader = \"head\"\n\
             size = \"header.size\"\nmethod = \"header.flags\"\nmask = 0x0F\nmethods = { 1 = \"runs\" }\n\
             packed = \"header.length\"\n",
        )
        .unwrap();
        assert!(formats.problems(&k).is_empty(), "{:?}", formats.problems(&k));
        let inner = [&[4, 0, 0x11, 0x01, 2, 0, 0xC4, 7][..], &[1, 2, 3, 4, 5, 6]].concat();
        let outer = [&b"INNR"[..], &(inner.len() as u16).to_le_bytes(), &inner].concat();
        let file = [&b"OUTR"[..], &(outer.len() as u16).to_le_bytes(), &outer].concat();
        let dir = std::env::temp_dir().join(format!("rrc-formats-nested-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("A.BIN"), &file).unwrap();
        let opened = formats.open(&k, &dir, "a.bin/outr/innr").unwrap();
        assert_eq!(opened.bytes, [7; 4]);
        assert_eq!(opened.after, [1, 2, 3, 4, 5, 6], "the bytes after the coded ones");
        assert_eq!((opened.offset, opened.packed, opened.codecs.as_slice()), (12, 14, &["runs".to_owned()][..]));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_archive_s_directory_may_end_it_and_a_sound_be_its_member() {
        // A count, the members one after another, then the directory: a name of 4 and a size each. SND is a
        // Creative Voice File; RUN is run-length coded after a u16 of its size.
        let k = Knowledge {
            types: toml::from_str(
                "[[struct]]\nname = \"entry\"\nsize = 6\nfields = [\
                 {offset = 0, size = 4, name = \"name\", type = \"u8[4]\"},\
                 {offset = 4, size = 2, name = \"size\", type = \"u16\"}]\n",
            )
            .unwrap(),
            ..Knowledge::default()
        };
        let formats: Formats = toml::from_str(
            "[[format]]\nname = \"pack\"\nfiles = [\"P.BSA\"]\nkind = \"archive\"\nheader = \"u16\"\ncount = \"header\"\n\
             directory = \"end\"\nentry = \"entry\"\n\
             [[format]]\nname = \"voice\"\nfiles = [\"P.BSA/SND\"]\nkind = \"sound\"\ncoded = \"voc\"\n\
             [[format]]\nname = \"runs\"\nfiles = [\"P.BSA/RUN\"]\nkind = \"data\"\n\
             coded = { codec = \"runs\", prefix = \"u16\" }\n",
        )
        .unwrap();
        assert!(formats.problems(&k).is_empty(), "{:?}", formats.problems(&k));
        let mut snd = b"Creative Voice File\x1A\x1A\x00\x0A\x01\x29\x11".to_vec();
        snd.extend([1, 4, 0, 0, 156, 0, 1, 2, 0]);
        let run = [3, 0, 0xC3, 9];
        let mut file = 2u16.to_le_bytes().to_vec();
        file.extend(&snd);
        file.extend(run);
        file.extend([&b"SND\0"[..], &(snd.len() as u16).to_le_bytes(), b"RUN\0", &4u16.to_le_bytes()].concat());
        let dir = std::env::temp_dir().join(format!("rrc-formats-end-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("P.BSA"), &file).unwrap();
        let sound = formats.open(&k, &dir, "P.BSA/SND").unwrap();
        assert_eq!((sound.offset, sound.bytes.as_slice(), sound.rate), (2, &[1, 2][..], Some(10_000)));
        let runs = formats.open(&k, &dir, "P.BSA/RUN").unwrap();
        assert_eq!((runs.offset, runs.bytes.as_slice()), (2 + snd.len(), &[9, 9, 9][..]));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn frames_at_their_places_unpack_their_bits_through_their_colours() {
        // A 4 × 2 picture of 2 bits a pixel, a byte a row, two frames: the header (width, height, row, bits,
        // frames, the frames' places), four colours, then each frame's counted runs.
        let k = Knowledge {
            types: toml::from_str(
                "[[struct]]\nname = \"ani\"\nsize = 12\nfields = [\
                 {offset = 0, size = 2, name = \"width\", type = \"u16\"},\
                 {offset = 2, size = 2, name = \"height\", type = \"u16\"},\
                 {offset = 4, size = 2, name = \"row\", type = \"u16\"},\
                 {offset = 6, size = 1, name = \"bits\", type = \"u8\"},\
                 {offset = 7, size = 1, name = \"frames\", type = \"u8\"},\
                 {offset = 8, size = 4, name = \"offsets\", type = \"u16[2]\"}]\n",
            )
            .unwrap(),
            ..Knowledge::default()
        };
        let formats: Formats = toml::from_str(
            "[[format]]\nname = \"ani\"\nfiles = [\"*.ANI\"]\nkind = \"picture\"\nheader = \"ani\"\n\
             width = \"header.width\"\nheight = \"header.height\"\nframes = \"header.frames\"\n\
             offsets = \"header.offsets\"\ncoded = \"rle\"\npixel_bits = \"header.bits\"\nstride = \"header.row\"\n\
             colours = 12\n",
        )
        .unwrap();
        assert!(formats.problems(&k).is_empty(), "{:?}", formats.problems(&k));
        let file = [&[4, 0, 2, 0, 1, 0, 2, 2, 16, 0, 19, 0][..], &[10, 20, 30, 40], &[0x01, 0x1B, 0xE4], &[0x81, 0xFF]]
            .concat();
        let dir = std::env::temp_dir().join(format!("rrc-formats-frames-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("A.ANI"), &file).unwrap();
        let first = formats.open(&k, &dir, "A.ANI/0").unwrap();
        assert_eq!(first.bytes, [10, 20, 30, 40, 40, 30, 20, 10]);
        assert_eq!(first.stages, [("rle".to_owned(), vec![0x1B, 0xE4])], "what the runs left, before the bits");
        assert_eq!((first.frames, first.codecs.as_slice()), (1, &["rle".to_owned(), "2 bits a pixel".to_owned()][..]));
        let all = formats.open(&k, &dir, "a.ani").unwrap();
        assert_eq!((all.frames, &all.bytes[8..]), (2, &[40; 8][..]), "the frames one after another");
        assert!(formats.open(&k, &dir, "A.ANI/2").unwrap_err().contains("has 2 frames"));
        assert!(formats.open(&k, &dir, "A.ANI/X").unwrap_err().contains("isn't one of its frames"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn frames_after_the_first_are_it_patched() {
        // A 3 × 1 picture of two frames: the first's two bytes of runs, then a frame's patch, at 1 a 9.
        let k = Knowledge {
            types: toml::from_str(
                "[[struct]]\nname = \"dlt\"\nsize = 8\nfields = [\
                 {offset = 0, size = 2, name = \"frames\", type = \"u16\"},\
                 {offset = 2, size = 2, name = \"width\", type = \"u16\"},\
                 {offset = 4, size = 2, name = \"height\", type = \"u16\"},\
                 {offset = 6, size = 2, name = \"length\", type = \"u16\"}]\n",
            )
            .unwrap(),
            ..Knowledge::default()
        };
        let formats: Formats = toml::from_str(
            "[[format]]\nname = \"dlt\"\nfiles = [\"*.DLT\"]\nkind = \"picture\"\nheader = \"dlt\"\n\
             width = \"header.width\"\nheight = \"header.height\"\nframes = \"header.frames\"\n\
             packed = \"header.length\"\ncoded = \"rle\"\ndeltas = \"patches\"\n",
        )
        .unwrap();
        assert!(formats.problems(&k).is_empty(), "{:?}", formats.problems(&k));
        let file = [&[2, 0, 3, 0, 1, 0, 2, 0][..], &[0x82, 5], &[7, 0, 1, 0, 1, 0, 1, 0, 9]].concat();
        let dir = std::env::temp_dir().join(format!("rrc-formats-deltas-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("B.DLT"), &file).unwrap();
        let all = formats.open(&k, &dir, "B.DLT").unwrap();
        assert_eq!((all.frames, all.bytes.as_slice()), (2, &[5, 5, 5, 5, 9, 5][..]));
        assert_eq!(all.codecs, ["rle", "patches"]);
        assert_eq!(formats.open(&k, &dir, "B.DLT/1").unwrap().bytes, [5, 9, 5]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_sequence_s_members_follow_one_another_and_a_format_may_be_of_a_size() {
        // C.SEQ: each member a byte of how many follow. D.SET: members of 3 bytes, the last cut short; those
        // of 3 bytes are textures.
        let formats: Formats = toml::from_str(
            "[[format]]\nname = \"seq\"\nfiles = [\"C.SEQ\"]\nkind = \"sequence\"\nentry = \"u8\"\nsize = \"entry\"\n\
             [[format]]\nname = \"set\"\nfiles = [\"D.SET\"]\nkind = \"sequence\"\nsize = 3\n\
             [[format]]\nname = \"texture\"\nfiles = [\"D.SET/*\"]\nbytes = 3\nkind = \"picture\"\nwidth = 3\nheight = 1\n",
        )
        .unwrap();
        let k = Knowledge::default();
        assert!(formats.problems(&k).is_empty(), "{:?}", formats.problems(&k));
        let members = formats.members(&formats.formats[0], &k, &[2, 7, 8, 1, 9]).unwrap();
        let placed: Vec<(&str, usize, usize)> = members.iter().map(|m| (m.name.as_str(), m.offset, m.packed)).collect();
        assert_eq!(placed, [("0", 0, 3), ("1", 3, 2)], "each member with its entry");
        assert!(formats.members(&formats.formats[0], &k, &[2, 7, 8, 5, 9]).unwrap_err().contains("past its 5 bytes"));
        let members = formats.members(&formats.formats[1], &k, &[1, 2, 3, 4, 5, 6, 7]).unwrap();
        assert_eq!(members.iter().map(|m| m.packed).collect::<Vec<_>>(), [3, 3, 1], "the last cut short");
        assert_eq!(formats.of("D.SET/0", 3).map(|f| f.name.as_str()), Some("texture"));
        assert_eq!(formats.of("D.SET/2", 1), None, "not of the texture's size");
        let wrong: Formats = toml::from_str(
            "[[format]]\nname = \"w\"\nfiles = [\"W\"]\nkind = \"data\"\nframes = 2\ndeltas = \"xor\"\n",
        )
        .unwrap();
        let problems = wrong.problems(&k).join("; ");
        assert!(problems.contains("are a picture's") && problems.contains("deltas are patches, not xor"), "{problems}");
    }
}
