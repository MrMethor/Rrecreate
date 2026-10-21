//! The cache (DESIGN.md §9, D44): files addressed by their SHA-256, per user and outside git:
//! program images, Ghidra exports, snapshots, traces. `RRC_CACHE` names its folder; otherwise
//! `rrc.local.toml` does, or the user's cache folder (`%LOCALAPPDATA%\rrecreate\cache`,
//! `$XDG_CACHE_HOME/rrecreate` or `~/.cache/rrecreate`).

use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::repo::Local;

pub struct Cache {
    dir: PathBuf,
}

impl Cache {
    pub fn open(local: &Local) -> Result<Cache, String> {
        let dir = std::env::var_os("RRC_CACHE")
            .map(PathBuf::from)
            .or_else(|| local.cache.clone())
            .or_else(default_dir)
            .ok_or("no cache folder: set RRC_CACHE")?;
        Cache::at(&dir)
    }

    pub fn at(dir: &Path) -> Result<Cache, String> {
        let objects = dir.join("objects");
        std::fs::create_dir_all(&objects).map_err(|e| format!("{}: {e}", objects.display()))?;
        Ok(Cache { dir: dir.to_path_buf() })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Stores `bytes`; returns their hash.
    pub fn put(&self, bytes: &[u8]) -> Result<String, String> {
        let hash = sha256(bytes);
        let path = self.path(&hash);
        if !path.is_file() {
            let parent = path.parent().expect("an object's folder");
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
            // Written aside and renamed, so a reader never sees half an object.
            let partial = path.with_extension(format!("partial-{}", std::process::id()));
            std::fs::write(&partial, bytes).map_err(|e| format!("{}: {e}", partial.display()))?;
            std::fs::rename(&partial, &path).map_err(|e| format!("{}: {e}", path.display()))?;
        }
        Ok(hash)
    }

    /// A writer that puts objects on a thread of its own, for many objects in a row (a recording's
    /// frames) while the caller goes on.
    pub fn writer(&self) -> Writer {
        let (sender, receiver) = std::sync::mpsc::channel::<Vec<u8>>();
        let dir = self.dir.clone();
        let thread = std::thread::spawn(move || {
            let cache = Cache::at(&dir)?;
            receiver.iter().try_for_each(|bytes| cache.put(&bytes).map(|_| ()))
        });
        Writer { sender, thread }
    }

    pub fn get(&self, hash: &str) -> Result<Vec<u8>, String> {
        let path = self.path(hash);
        std::fs::read(&path).map_err(|_| format!("{hash} isn't in the cache ({})", self.dir.display()))
    }

    pub fn has(&self, hash: &str) -> bool {
        self.path(hash).is_file()
    }

    /// Remembers `text` under `key`: what an earlier run made from inputs `key` stands for (the
    /// analysis of a program, say), so another repository or a fresh clone can reuse it.
    pub fn remember(&self, kind: &str, key: &str, text: &str) -> Result<(), String> {
        let dir = self.dir.join("index").join(kind);
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let path = dir.join(key);
        std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn recall(&self, kind: &str, key: &str) -> Option<String> {
        std::fs::read_to_string(self.dir.join("index").join(kind).join(key)).ok()
    }

    pub fn path(&self, hash: &str) -> PathBuf {
        let (folder, rest) = hash.split_at(2.min(hash.len()));
        self.dir.join("objects").join(folder).join(rest)
    }
}

pub fn sha256(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

pub fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1 << 16];
    loop {
        let n = file.read(&mut buffer).map_err(|e| format!("{}: {e}", path.display()))?;
        if n == 0 {
            return Ok(hex(&hasher.finalize()));
        }
        hasher.update(&buffer[..n]);
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Puts objects into the cache on a thread of its own ([`Cache::writer`]).
pub struct Writer {
    sender: std::sync::mpsc::Sender<Vec<u8>>,
    thread: std::thread::JoinHandle<Result<(), String>>,
}

impl Writer {
    /// Stores `bytes` (soon); returns their hash.
    pub fn put(&self, bytes: Vec<u8>) -> String {
        let hash = sha256(&bytes);
        // A writer that stopped says why at `finish`.
        let _ = self.sender.send(bytes);
        hash
    }

    /// Waits until everything is stored.
    pub fn finish(self) -> Result<(), String> {
        drop(self.sender);
        self.thread.join().map_err(|_| "the cache's writer failed".to_owned())?
    }
}

fn default_dir() -> Option<PathBuf> {
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
    fn objects_are_found_by_their_hash() {
        let dir = std::env::temp_dir().join(format!("rrc-cache-{}", std::process::id()));
        let cache = Cache::at(&dir).unwrap();
        let hash = cache.put(b"abc").unwrap();
        assert_eq!(hash, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(cache.put(b"abc").unwrap(), hash, "storing again is harmless");
        assert_eq!(cache.get(&hash).unwrap(), b"abc");
        assert!(cache.has(&hash) && !cache.has(&sha256(b"abd")));
        assert_eq!(sha256_file(&cache.path(&hash)).unwrap(), hash);
        assert_eq!(cache.recall("analysis", "k"), None);
        cache.remember("analysis", "k", "text").unwrap();
        assert_eq!(cache.recall("analysis", "k").as_deref(), Some("text"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
