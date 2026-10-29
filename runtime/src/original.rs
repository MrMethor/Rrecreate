//! The person's copy of the game, for what a port loads from it instead of copying it into its source
//! (D82): the original program's static data (tables its code reads, such as a sine table) and the
//! game's files. `rrc` gives a port both when it runs it: `RRC_PROGRAM`, the program as the analysis
//! unpacked it, and `RRC_GAME`, the game's folder. Run otherwise (the port's own `cargo test`), the
//! port finds them as `rrc` would (D91): the port repository above the current folder, its analysis
//! (`.rrc/analysis.toml`), its cache and its game (`rrc.local.toml`, or `RRC_CACHE`).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The original program's load image: what its IDs' offsets count in (`ACD.EXE+46A3C` is offset
/// `0x46A3C`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    image: Vec<u8>,
}

impl Program {
    /// An MZ program's load image: the file past its header, as long as the header says.
    pub fn from_mz(bytes: &[u8]) -> Result<Program, String> {
        let word = |at: usize| bytes.get(at..at + 2).map(|w| usize::from(u16::from_le_bytes([w[0], w[1]])));
        if !(bytes.starts_with(b"MZ") || bytes.starts_with(b"ZM")) {
            return Err("not an MZ program".into());
        }
        let (last, pages, header) = (word(2).unwrap_or(0), word(4).unwrap_or(0), word(8).unwrap_or(0) * 16);
        let size = if last == 0 { pages * 512 } else { pages.saturating_sub(1) * 512 + last };
        let image = bytes.get(header..size.min(bytes.len())).ok_or("the program is shorter than its header")?;
        Ok(Program { image: image.to_vec() })
    }

    /// `len` bytes at `offset`. Panics past the image: static data is in it.
    pub fn bytes(&self, offset: u32, len: usize) -> &[u8] {
        let start = offset as usize;
        self.image.get(start..start + len).unwrap_or_else(|| {
            panic!("+{offset:05X}, {len} bytes: past the program's image ({} bytes)", self.image.len())
        })
    }

    /// `n` words at `offset`.
    pub fn u16s(&self, offset: u32, n: usize) -> Vec<u16> {
        self.bytes(offset, 2 * n).chunks_exact(2).map(|w| u16::from_le_bytes([w[0], w[1]])).collect()
    }

    /// `n` signed words at `offset`.
    pub fn i16s(&self, offset: u32, n: usize) -> Vec<i16> {
        self.bytes(offset, 2 * n).chunks_exact(2).map(|w| i16::from_le_bytes([w[0], w[1]])).collect()
    }
}

/// The original program, as `rrc` gave it (`RRC_PROGRAM`). Panics, saying why, when there is none: a
/// port that reads it runs through `rrc`, after `rrc analyze`.
pub fn program() -> &'static Program {
    static PROGRAM: OnceLock<Result<Program, String>> = OnceLock::new();
    let loaded = PROGRAM.get_or_init(|| {
        let path = match std::env::var_os("RRC_PROGRAM") {
            Some(path) => PathBuf::from(path),
            None => repository_program()?,
        };
        let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        Program::from_mz(&bytes)
    });
    match loaded {
        Ok(program) => program,
        Err(e) => panic!("{e}"),
    }
}

/// The game's folder: `RRC_GAME`, else the port repository's (`rrc.local.toml`'s `game`), if there is one.
pub fn game_dir() -> Option<PathBuf> {
    std::env::var_os("RRC_GAME").map(PathBuf::from).or_else(|| {
        let root = repository(&std::env::current_dir().ok()?)?;
        Some(PathBuf::from(setting(&root.join("rrc.local.toml"), "game")?))
    })
}

/// A file of the person's copy of the game (M5), by the name the program opens it with: a DOS path from
/// the game's folder, in any case, with `\` or `/` (`DATA\LEVEL1.MAP`).
pub fn file(name: &str) -> Result<Vec<u8>, String> {
    let dir = game_dir()
        .ok_or("the game's files: run the port through rrc, or inside its port repository (rrc.local.toml's game)")?;
    file_in(&dir, name)
}

/// A file of the game in `dir`, by the name the program opens it with (`file`; `rrc asset` reads the
/// game's files so too).
pub fn file_in(dir: &Path, name: &str) -> Result<Vec<u8>, String> {
    let path = find(dir, name)?;
    std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))
}

/// `name`'s path under `dir`, each part matched without regard to case, as DOS matches it.
fn find(dir: &Path, name: &str) -> Result<PathBuf, String> {
    let mut path = dir.to_path_buf();
    for part in name.split(['\\', '/']).filter(|p| !p.is_empty()) {
        let exact = path.join(part);
        if exact.exists() {
            path = exact;
            continue;
        }
        let entries = std::fs::read_dir(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let found = entries
            .filter_map(Result::ok)
            .find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(part))
            .ok_or_else(|| format!("{name}: the game's folder has no {part} in {}", path.display()))?;
        path = found.path();
    }
    Ok(path)
}

/// The program as the port repository's analysis unpacked it, in the cache: what `rrc` would give.
fn repository_program() -> Result<PathBuf, String> {
    let here = std::env::current_dir().map_err(|e| e.to_string())?;
    let no = "the port reads the original program: run it through rrc, or inside its port repository after rrc analyze";
    let root = repository(&here).ok_or(no)?;
    let hash = setting(&root.join(".rrc").join("analysis.toml"), "program").ok_or(no)?;
    let cache = std::env::var_os("RRC_CACHE")
        .map(PathBuf::from)
        .or_else(|| setting(&root.join("rrc.local.toml"), "cache").map(PathBuf::from))
        .or_else(default_cache)
        .ok_or("no cache folder: set RRC_CACHE")?;
    let (folder, rest) = hash.split_at(2.min(hash.len()));
    Ok(cache.join("objects").join(folder).join(rest))
}

/// The port repository `dir` is in: the folder with `rrc.toml`, from `dir` up.
fn repository(dir: &Path) -> Option<PathBuf> {
    dir.ancestors().find(|d| d.join("rrc.toml").is_file()).map(Path::to_path_buf)
}

/// A top-level string of a TOML file.
fn setting(file: &Path, key: &str) -> Option<String> {
    let table: toml::Table = std::fs::read_to_string(file).ok()?.parse().ok()?;
    table.get(key)?.as_str().map(str::to_owned)
}

/// The user's cache folder, as `rrc` has it (`%LOCALAPPDATA%\rrecreate\cache`, `$XDG_CACHE_HOME/rrecreate` or
/// `~/.cache/rrecreate`).
fn default_cache() -> Option<PathBuf> {
    let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    if cfg!(windows) {
        return var("LOCALAPPDATA").map(|d| d.join("rrecreate").join("cache"));
    }
    var("XDG_CACHE_HOME").or_else(|| var("HOME").map(|h| h.join(".cache"))).map(|d| d.join("rrecreate"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_mz_program_s_image_is_past_its_header() {
        // Two paragraphs of header, 4 bytes of image: 36 bytes, 1 page whose last has 36.
        let mut mz = vec![0u8; 32];
        mz[..2].copy_from_slice(b"MZ");
        mz[2..4].copy_from_slice(&36u16.to_le_bytes());
        mz[4..6].copy_from_slice(&1u16.to_le_bytes());
        mz[8..10].copy_from_slice(&2u16.to_le_bytes());
        mz.extend([0x92, 0x01, 0xFE, 0xFF, 0xAA]);
        let program = Program::from_mz(&mz).unwrap();
        assert_eq!(program.u16s(0, 2), [0x0192, 0xFFFE]);
        assert_eq!(program.i16s(2, 1), [-2], "the byte past the header's length isn't the image's");
        assert!(Program::from_mz(b"PK").is_err());
    }

    #[test]
    fn a_port_repository_s_settings_are_found_from_inside_it() {
        let root = std::env::temp_dir().join(format!("rrc-runtime-repo-{}", std::process::id()));
        let inside = root.join("src").join("deep");
        std::fs::create_dir_all(&inside).unwrap();
        std::fs::create_dir_all(root.join(".rrc")).unwrap();
        std::fs::write(root.join("rrc.toml"), "[game]\nname = \"t\"\n").unwrap();
        std::fs::write(root.join(".rrc").join("analysis.toml"), "program = \"ab12cd\"\nentry = 0\n").unwrap();
        std::fs::write(root.join("rrc.local.toml"), "game = 'C:\\Games\\T'\ncache = \"D:/c\"\n").unwrap();
        assert_eq!(repository(&inside), Some(root.clone()));
        assert_eq!(setting(&root.join(".rrc").join("analysis.toml"), "program").as_deref(), Some("ab12cd"));
        assert_eq!(setting(&root.join("rrc.local.toml"), "game").as_deref(), Some("C:\\Games\\T"));
        assert_eq!(repository(&std::env::temp_dir().join("no-port-here-at-all")), None);
        std::fs::remove_dir_all(&root).ok();
    }
}
