//! The game's files and its versions (D30): a version the port supports is its files by SHA-256. The
//! hashes of the person's copy are kept in `.rrc/files.tsv` with each file's size and modification
//! time, so checking a large game again reads only what changed.

use std::collections::BTreeMap;
use std::path::Path;

use crate::cache::sha256_file;
use crate::repo::Version;

/// The files in `dir` and its folders, by `/`-separated path, with their hashes.
pub fn hash_folder(dir: &Path, state: &Path) -> Result<BTreeMap<String, String>, String> {
    let known_path = state.join("files.tsv");
    let known: BTreeMap<String, (String, String)> = std::fs::read_to_string(&known_path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let [path, stamp, hash] = line.split('\t').collect::<Vec<_>>()[..] else { return None };
            Some((path.to_owned(), (stamp.to_owned(), hash.to_owned())))
        })
        .collect();
    let mut files = BTreeMap::new();
    let mut seen = String::new();
    for (path, full) in walk(dir)? {
        let meta = std::fs::metadata(&full).map_err(|e| format!("{}: {e}", full.display()))?;
        let modified = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok());
        let stamp = format!("{}:{}", meta.len(), modified.map_or(0, |d| d.as_nanos()));
        let hash = match known.get(&path) {
            Some((s, hash)) if *s == stamp => hash.clone(),
            _ => sha256_file(&full)?,
        };
        seen.push_str(&format!("{path}\t{stamp}\t{hash}\n"));
        files.insert(path, hash);
    }
    std::fs::write(&known_path, seen).map_err(|e| format!("{}: {e}", known_path.display()))?;
    Ok(files)
}

/// Every file under `dir`, sorted, as (`/`-separated path, full path).
fn walk(dir: &Path) -> Result<Vec<(String, std::path::PathBuf)>, String> {
    let mut out = Vec::new();
    let mut pending = vec![(String::new(), dir.to_path_buf())];
    while let Some((prefix, folder)) = pending.pop() {
        let entries = std::fs::read_dir(&folder).map_err(|e| format!("{}: {e}", folder.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("{}: {e}", folder.display()))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
            let kind = entry.file_type().map_err(|e| format!("{path}: {e}"))?;
            if kind.is_dir() {
                pending.push((path, entry.path()));
            } else if kind.is_file() {
                out.push((path, entry.path()));
            }
        }
    }
    out.sort();
    Ok(out)
}

/// The first version all of whose files `files` has (paths compare without case, as DOS's do).
pub fn identify<'a>(versions: &'a [Version], files: &BTreeMap<String, String>) -> Option<&'a Version> {
    versions.iter().find(|v| differences(v, files).is_empty())
}

/// The files of `version` that `files` lacks or holds with other contents.
pub fn differences<'a>(version: &'a Version, files: &BTreeMap<String, String>) -> Vec<&'a str> {
    let by_name: BTreeMap<String, &String> = files.iter().map(|(p, h)| (p.to_ascii_uppercase(), h)).collect();
    version
        .files
        .iter()
        .filter(|(path, hash)| by_name.get(&path.to_ascii_uppercase()) != Some(hash))
        .map(|(path, _)| path.as_str())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_is_identified_by_its_files() {
        let dir = std::env::temp_dir().join(format!("rrc-game-{}", std::process::id()));
        let (game, state) = (dir.join("game"), dir.join("state"));
        std::fs::create_dir_all(game.join("data")).unwrap();
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(game.join("GAME.EXE"), b"MZ").unwrap();
        std::fs::write(game.join("data").join("LEVEL1.DAT"), b"level").unwrap();
        let files = hash_folder(&game, &state).unwrap();
        assert_eq!(files.keys().collect::<Vec<_>>(), ["GAME.EXE", "data/LEVEL1.DAT"]);
        assert_eq!(hash_folder(&game, &state).unwrap(), files, "from the kept hashes");
        let version = Version { name: "1.0".into(), files: files.clone() };
        let other = Version { name: "1.1".into(), files: BTreeMap::from([("game.exe".into(), "00".repeat(32))]) };
        let versions = [other, version];
        assert_eq!(identify(&versions, &files).map(|v| v.name.as_str()), Some("1.0"));
        assert_eq!(differences(&versions[0], &files), ["game.exe"]);
        std::fs::remove_dir_all(&dir).ok();
    }
}
