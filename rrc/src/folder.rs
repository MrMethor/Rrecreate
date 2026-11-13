//! The game's folder as a session was recorded (D170). A run's start depends on more than the version's
//! files: what else the folder holds (saves, a game's running files left by the last play, a launcher's
//! files) is what the program finds, lists and deletes, and a play outside the toolkit changes it. So a
//! session records the folder it ran on: every file, by path and SHA-256 (the session file's `[folder]`),
//! with their contents kept in the cache. Every run of the session, the original's and the port's, is
//! given that folder, built in the cache from them, and sees no other file; the person's folder is only
//! where a file the cache lost is found again.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::cache::{Cache, sha256};
use crate::repo::Repo;
use crate::session::Session;

/// What a folder without files is listed as: its path ends in `/`, and this stands where a file's hash is.
const FOLDER: &str = "folder";

/// The game's folder as it is now, its files' contents put in the cache: what a session recorded now runs on.
pub fn take(repo: &Repo) -> Result<BTreeMap<String, String>, String> {
    let dir = repo.game_dir()?;
    let mut files = crate::game::hash_folder(dir, &repo.state()?)?;
    let cache = Cache::open(&repo.local)?;
    for (path, hash) in &files {
        if !cache.has(hash) {
            let full = host(dir, path);
            let bytes = std::fs::read(&full).map_err(|e| format!("{}: {e}", full.display()))?;
            if cache.put(&bytes)? != *hash {
                return Err(format!("{}: it changed while the game's folder was read", full.display()));
            }
        }
    }
    for folder in crate::game::empty_folders(dir)? {
        files.insert(format!("{folder}/"), FOLDER.to_owned());
    }
    Ok(files)
}

/// The files `session`'s run sees: those its root's recording kept, or (recorded before D170) the game's folder's.
pub fn files(repo: &Repo, session: &Session) -> Result<BTreeMap<String, String>, String> {
    let root = session.root(repo)?;
    match root.files.is_empty() {
        true => crate::game::hash_folder(repo.game_dir()?, &repo.state()?),
        false => Ok(root.files),
    }
}

/// The folder `session` runs on: its root's recorded folder, built in the cache; or (recorded before D170) the
/// game's folder.
pub fn of(repo: &Repo, session: &Session) -> Result<PathBuf, String> {
    let root = session.root(repo)?;
    if root.files.is_empty() {
        return Ok(repo.game_dir()?.to_path_buf());
    }
    build(repo, &root.files).map_err(|e| {
        let name = session.from.as_ref().map_or("the session", |_| "its root session");
        format!("{e}: {name} can't be run as recorded (rrc record ROOT --folder records it on the folder as it is)")
    })
}

/// The paths where the game's folder now differs from `files` (a recording's): changed, added or gone.
pub fn differences(repo: &Repo, files: &BTreeMap<String, String>) -> Result<Vec<String>, String> {
    let dir = repo.game_dir()?;
    let mut now = crate::game::hash_folder(dir, &repo.state()?)?;
    for folder in crate::game::empty_folders(dir)? {
        now.insert(format!("{folder}/"), FOLDER.to_owned());
    }
    let mut paths: Vec<String> = files.iter().filter(|(p, h)| now.get(*p) != Some(h)).map(|(p, _)| p.clone()).collect();
    paths.extend(now.keys().filter(|p| !files.contains_key(*p)).cloned());
    paths.sort();
    Ok(paths)
}

/// The folder `files` lists, built once in the cache's `folders/` (by the list's hash) from the cache's
/// objects: hard links where the file system allows, else copies. Files the cache lost are taken from the
/// game's folder where it still holds them as recorded.
fn build(repo: &Repo, files: &BTreeMap<String, String>) -> Result<PathBuf, String> {
    let cache = Cache::open(&repo.local)?;
    let list: String = files.iter().map(|(path, hash)| format!("{path}\t{hash}\n")).collect();
    let root = cache.dir().join("folders");
    let dir = root.join(sha256(list.as_bytes()));
    if dir.is_dir() {
        return Ok(dir);
    }
    let lost: Vec<(&String, &String)> = files.iter().filter(|(_, h)| *h != FOLDER && !cache.has(h)).collect();
    if !lost.is_empty() {
        let game = repo.game_dir().ok();
        let now = match game {
            Some(dir) => crate::game::hash_folder(dir, &repo.state()?)?,
            None => BTreeMap::new(),
        };
        let mut gone = Vec::new();
        for (path, hash) in lost {
            match (game, now.get(path) == Some(hash)) {
                (Some(dir), true) => {
                    let full = host(dir, path);
                    cache.put(&std::fs::read(&full).map_err(|e| format!("{}: {e}", full.display()))?)?;
                }
                _ => gone.push(path.as_str()),
            }
        }
        if !gone.is_empty() {
            let more = if gone.len() > 3 { format!(" and {} more", gone.len() - 3) } else { String::new() };
            return Err(format!(
                "the game's folder as recorded: {}{more} not in the cache, and the folder now holds other contents",
                gone[..gone.len().min(3)].join(", ")
            ));
        }
    }
    // Built aside and renamed, so a folder that is there is whole.
    let partial = root.join(format!("partial-{}", std::process::id()));
    if partial.exists() {
        std::fs::remove_dir_all(&partial).map_err(|e| format!("{}: {e}", partial.display()))?;
    }
    for (path, hash) in files {
        let to = host(&partial, path);
        if hash == FOLDER {
            std::fs::create_dir_all(&to).map_err(|e| format!("{}: {e}", to.display()))?;
            continue;
        }
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        let from = cache.path(hash);
        if std::fs::hard_link(&from, &to).is_err() {
            std::fs::copy(&from, &to).map_err(|e| format!("{}: {e}", to.display()))?;
        }
    }
    std::fs::create_dir_all(&partial).map_err(|e| format!("{}: {e}", partial.display()))?;
    match std::fs::rename(&partial, &dir) {
        Ok(()) => Ok(dir),
        // Another rrc built it meanwhile.
        Err(_) if dir.is_dir() => {
            let _ = std::fs::remove_dir_all(&partial);
            Ok(dir)
        }
        Err(e) => Err(format!("{}: {e}", dir.display())),
    }
}

/// A listed path (`/`-separated) under `dir`.
fn host(dir: &Path, path: &str) -> PathBuf {
    path.split('/').filter(|part| !part.is_empty()).fold(dir.to_path_buf(), |at, part| at.join(part))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_runs_on_the_folder_it_was_recorded_on() {
        let dir = std::env::temp_dir().join(format!("rrc-folder-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (root, game, cache) = (dir.join("port"), dir.join("game"), dir.join("cache"));
        std::fs::create_dir_all(game.join("data")).unwrap();
        std::fs::create_dir_all(game.join("saves")).unwrap();
        std::fs::create_dir_all(root.join("sessions")).unwrap();
        std::fs::write(game.join("GAME.EXE"), b"MZ").unwrap();
        std::fs::write(game.join("data").join("LEVEL1.DAT"), b"level").unwrap();
        std::fs::write(game.join("RUN.64"), b"left by the last play").unwrap();
        std::fs::write(root.join("rrc.toml"), "[game]\nname = \"test\"\n[original]\nprogram = \"GAME.EXE\"\n").unwrap();
        std::fs::write(
            root.join("rrc.local.toml"),
            format!("game = {:?}\ncache = {:?}\n", game.to_string_lossy(), cache.to_string_lossy()),
        )
        .unwrap();
        let repo = Repo::load(&root).unwrap();
        let mut session = Session { seconds: 1.0, ..Session::default() };
        assert_eq!(of(&repo, &session).unwrap(), game, "recorded before D170: the folder as it is");

        session.files = take(&repo).unwrap();
        assert_eq!(session.files.keys().collect::<Vec<_>>(), ["GAME.EXE", "RUN.64", "data/LEVEL1.DAT", "saves/"]);
        session.save(&repo, "s").unwrap();
        // Another play changes the folder: the session's run doesn't see it.
        std::fs::remove_file(game.join("RUN.64")).unwrap();
        std::fs::write(game.join("OTHER.64"), b"left by another play").unwrap();
        assert_eq!(differences(&repo, &session.files).unwrap(), ["OTHER.64", "RUN.64"]);
        let built = of(&repo, &session).unwrap();
        assert!(built.starts_with(&cache));
        assert_eq!(std::fs::read(built.join("RUN.64")).unwrap(), b"left by the last play");
        assert_eq!(std::fs::read(built.join("data").join("LEVEL1.DAT")).unwrap(), b"level");
        assert!(built.join("saves").is_dir() && !built.join("OTHER.64").exists());
        assert_eq!(files(&repo, &session).unwrap(), session.files);
        // A session based on it runs on the same folder.
        let based = Session {
            from: Some(crate::session::Base { session: "s".into(), tick: 1, cycle: None }),
            ..Session::default()
        };
        assert_eq!(of(&repo, &based).unwrap(), built);

        // The cache lost the folder and a file: the file the game's folder still holds is taken from it again;
        // one it doesn't is named.
        std::fs::remove_dir_all(cache.join("folders")).unwrap();
        let level = session.files["data/LEVEL1.DAT"].clone();
        std::fs::remove_file(Cache::open(&repo.local).unwrap().path(&level)).unwrap();
        assert_eq!(std::fs::read(of(&repo, &session).unwrap().join("data").join("LEVEL1.DAT")).unwrap(), b"level");
        std::fs::remove_dir_all(cache.join("folders")).unwrap();
        let run = session.files["RUN.64"].clone();
        std::fs::remove_file(Cache::open(&repo.local).unwrap().path(&run)).unwrap();
        let error = of(&repo, &session).unwrap_err();
        assert!(error.contains("RUN.64 not in the cache"), "{error}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
