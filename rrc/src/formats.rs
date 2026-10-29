//! The game's file formats as knowledge (D78, D103): `knowledge/formats.toml` says, for each kind of file
//! or archive member, what it is (an archive's members, tagged chunks, a picture, a palette, a map, data),
//! the structure at its start (a type of `types.toml`), how its bytes are coded (the runtime's codecs, which
//! a port decodes with too), the function that reads it and the provenance. `rrc asset` decodes by it.

use std::collections::BTreeMap;
use std::path::Path;

use rrc_runtime::codecs::{Lzss, runs};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// A directory of entries, each a member's name, place, size and coding.
    Archive,
    /// Tagged chunks one after another, each a member named by its tag.
    Chunks,
    /// `width` × `height` palette indices, with the `palette` it is shown in.
    Picture,
    /// Colours of three bytes (`bits` each: 6, the VGA DAC's, or 8).
    Palette,
    /// `width` × `height` cells.
    Map,
    Data,
}

impl Kind {
    fn text(self) -> &'static str {
        match self {
            Kind::Archive => "archive",
            Kind::Chunks => "chunks",
            Kind::Picture => "picture",
            Kind::Palette => "palette",
            Kind::Map => "map",
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

/// How bytes are coded: a codec by name (`stored`, `runs`, `lzss`), or one with its parameters
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

    /// `packed` decoded, up to `size` bytes.
    pub fn decode(&self, packed: &[u8], size: usize) -> Result<Vec<u8>, String> {
        match self.codec.as_str() {
            "stored" => Ok(packed[..packed.len().min(size)].to_vec()),
            "runs" => Ok(runs(packed, size)),
            "lzss" => {
                let d = Lzss::OKUMURA;
                let lzss = Lzss {
                    ring: self.ring.unwrap_or(d.ring),
                    fill: self.fill.unwrap_or(d.fill),
                    start: self.start.unwrap_or(d.start),
                    shortest: self.shortest.unwrap_or(d.shortest),
                };
                if !lzss.ring.is_power_of_two() || lzss.start >= lzss.ring {
                    return Err(format!(
                        "lzss: a ring of {} from {} (a power of two, past the start)",
                        lzss.ring, lzss.start
                    ));
                }
                Ok(lzss.decode(packed, size))
            }
            other => Err(format!("{other}: no such codec (stored, runs, lzss)")),
        }
    }
}

/// A kind of file or member.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Format {
    pub name: String,
    /// The files it is: names in the game's folder (`DATA\*.IMG`), or an archive's members
    /// (`VAULT.DAT/LEVEL*`); `*` and `?` as DOS has them, in any case.
    pub files: Vec<String>,
    pub kind: Kind,
    /// The structure at its start (a type of `types.toml`), whose fields the numbers here may name
    /// (`header.width`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    /// How the bytes past the header are coded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coded: Option<Coded>,
    /// How many bytes they decode to (else as many as `width` × `height`, or as the codec gives).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<Number>,
    /// An archive's directory, or the first chunk: where it begins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<Number>,
    /// An archive's entries: how many.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<Number>,
    /// An archive's entry (a structure of `types.toml`): its `name`, `offset` and `size`, and if its members
    /// are coded, `packed` (their bytes in the file) and `method`, which `methods` gives the codec of.
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
    /// A picture's palette: a file or member of kind `palette`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub palette: Option<String>,
    /// A palette's bits per colour (6 or 8; 6).
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

/// A file or member decoded: its bytes, the codecs that decoded them, its header's values, where its
/// bytes are in its file.
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

    /// The format of a file or member (`FILE/MEMBER`): the first that names it.
    pub fn of(&self, path: &str) -> Option<&Format> {
        self.formats.iter().find(|f| f.files.iter().any(|pattern| matches(pattern, path)))
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
            for coded in f.coded.iter().chain(f.methods.values()) {
                if let Err(e) = coded.codec().decode(&[], 0) {
                    problems.push(format!("{at}: {e}"));
                }
            }
            match f.kind {
                Kind::Archive if f.directory.is_none() || f.count.is_none() || f.entry.is_none() => {
                    problems.push(format!("{at}: an archive needs its directory, count and entry"));
                }
                Kind::Picture | Kind::Map if f.width.is_none() || f.height.is_none() => {
                    problems.push(format!("{at}: a {} needs its width and height", f.kind.text()));
                }
                _ => {}
            }
            if let Some(id) = Some(&f.read_by).filter(|r| !r.is_empty())
                && let Err(e) = crate::knowledge::parse_id(id)
            {
                problems.push(format!("{at}: read_by {e}"));
            }
        }
        problems
    }

    /// A file of the game (`name`) or an archive's member (`FILE/MEMBER`, `#2` for the second of its name)
    /// decoded by its format.
    pub fn open(&self, k: &Knowledge, game: &Path, path: &str) -> Result<Opened, String> {
        let (file, member) = match path.split_once('/') {
            Some((file, member)) => (file, Some(member)),
            None => (path, None),
        };
        let bytes = rrc_runtime::original::file_in(game, file)?;
        let mut opened = match member {
            None => Opened {
                file: file.to_owned(),
                format: None,
                offset: 0,
                packed: bytes.len(),
                codecs: Vec::new(),
                header: None,
                bytes,
            },
            Some(wanted) => {
                let archive =
                    self.of(file).ok_or_else(|| format!("{file}: no format in {FORMATS} names it, so no members"))?;
                let members = self.members(archive, k, &bytes)?;
                let m = find_member(&members, wanted).ok_or_else(|| {
                    let names: Vec<&str> = members.iter().take(12).map(|m| m.name.as_str()).collect();
                    format!("{file} has no member {wanted} (it has {})", names.join(", "))
                })?;
                let packed = bytes.get(m.offset..m.offset + m.packed).ok_or_else(|| {
                    format!("{path}: {} bytes at +{:X}, past the file's {} bytes", m.packed, m.offset, bytes.len())
                })?;
                Opened {
                    file: file.to_owned(),
                    format: None,
                    offset: m.offset,
                    packed: m.packed,
                    codecs: vec![m.codec.codec.clone()].into_iter().filter(|c| c != "stored").collect(),
                    header: None,
                    bytes: m.codec.decode(packed, m.size)?,
                }
            }
        };
        let Some(format) = self.of(path) else { return Ok(opened) };
        opened.format = Some(format.clone());
        if matches!(format.kind, Kind::Archive | Kind::Chunks) {
            return Ok(opened);
        }
        // Its header, then the rest decoded.
        let mut body = opened.bytes.clone();
        if let Some(ty) = &format.header {
            let n = size(ty, &k.types)? as usize;
            let head = body.get(..n).ok_or_else(|| format!("{path}: shorter than its header ({ty}, {n} bytes)"))?;
            opened.header = Some(decode(head, ty, &Reader::of(k))?);
            body.drain(..n);
        }
        let number = |n: &Option<Number>| resolve(n.as_ref(), opened.header.as_ref());
        let cells = match (number(&format.width)?, number(&format.height)?) {
            (Some(w), Some(h)) => Some(w * h),
            _ => None,
        };
        if let Some(coded) = &format.coded {
            let codec = coded.codec();
            let size = number(&format.size)?.or(cells).unwrap_or(usize::MAX);
            body = codec.decode(&body, size)?;
            opened.codecs.push(codec.codec);
        }
        opened.bytes = body;
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
        let start = resolve(f.directory.as_ref(), header.as_ref())?.unwrap_or(0);
        match f.kind {
            Kind::Archive => {
                let count = resolve(f.count.as_ref(), header.as_ref())?.ok_or_else(|| at("an archive's count"))?;
                let ty = f.entry.as_ref().ok_or_else(|| at("an archive's entry"))?;
                let n = size(ty, &k.types)? as usize;
                let mut members = Vec::with_capacity(count);
                for i in 0..count {
                    let b = bytes
                        .get(start + i * n..start + (i + 1) * n)
                        .ok_or_else(|| at("its directory runs past its end"))?;
                    let e = decode(b, ty, &Reader::of(k))?;
                    let field = |name: &str| e.get(name).and_then(Value::as_u64).map(|v| v as usize);
                    let name = e.get("name").map(name_text).ok_or_else(|| at("its entry has no name"))?;
                    let offset = field("offset").ok_or_else(|| at("its entry has no offset"))?;
                    let size = field("size").ok_or_else(|| at("its entry has no size"))?;
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
                    members.push(Member { name, offset, packed: field("packed").unwrap_or(size), size, codec });
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
    match n {
        None => Ok(None),
        Some(Number::Is(v)) => Ok(Some(*v as usize)),
        Some(Number::Field(path)) => {
            let field = path.strip_prefix("header").ok_or_else(|| format!("{path}: a number or header.FIELD"))?;
            let header = header.ok_or_else(|| format!("{path}: the format has no header"))?;
            at_leaf(header, field)
                .and_then(Value::as_u64)
                .map(|v| Some(v as usize))
                .ok_or_else(|| format!("{path}: no such number in the header"))
        }
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
            header: None,
            coded: None,
            size: None,
            directory: Some(Number::Is(2)),
            count: None,
            entry: None,
            methods: BTreeMap::new(),
            tag: None,
            length: None,
            width: None,
            height: None,
            palette: None,
            bits: None,
            read_by: String::new(),
            provenance: String::new(),
            evidence: String::new(),
        };
        let bytes = b"\0\0MHDR\x01\x00\x07MAP1\x02\x00\x01\x02MAP1\x00\x00";
        let members = Formats::default().members(&f, &Knowledge::default(), bytes).unwrap();
        let named: Vec<(&str, usize, usize)> = members.iter().map(|m| (m.name.as_str(), m.offset, m.size)).collect();
        assert_eq!(named, [("MHDR", 8, 1), ("MAP1", 15, 2), ("MAP1", 23, 0)]);
        assert_eq!(find_member(&members, "map1#2").map(|m| m.offset), Some(23));
    }
}
