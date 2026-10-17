//! The emulated drive C: an in-memory copy of the game folder (PROJECT.md §12: programs only
//! see a project-local virtual drive).
//!
//! File contents are shared (`Arc`) between snapshots and copied only when written, so cloning
//! a machine is cheap and every write stays inside the emulation. Paths are canonical DOS paths:
//! uppercase, `\`-separated, relative to the root, e.g. `DATA\LEVEL1.DAT`; the root is `""`.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::Path;
use std::sync::Arc;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Vfs {
    files: BTreeMap<String, Arc<Vec<u8>>>,
    dirs: BTreeSet<String>,
    /// Paths created, written, renamed or deleted since loading.
    changed: BTreeSet<String>,
}

impl std::hash::Hash for Vfs {
    /// Files as loaded are the run's input, the same in every run of a program, so they count
    /// by name and size; files changed since loading count with their contents. (Hashing every
    /// byte of a game's data took a large part of a short run.)
    fn hash<H: std::hash::Hasher>(&self, h: &mut H) {
        for (path, data) in &self.files {
            path.hash(h);
            data.len().hash(h);
            if self.changed.contains(path) {
                data.hash(h);
            }
        }
        self.dirs.hash(h);
        self.changed.hash(h);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u32,
}

impl Vfs {
    pub fn new() -> Self {
        let mut vfs = Self::default();
        vfs.dirs.insert(String::new());
        vfs
    }

    /// Loads a host folder. Names are upper-cased; entries whose names don't fit DOS 8.3 rules
    /// are still loaded (and reported) but can't be opened by DOS programs.
    pub fn from_host_dir(root: &Path) -> io::Result<(Self, Vec<String>)> {
        let mut vfs = Self::new();
        let mut warnings = Vec::new();
        load_dir(&mut vfs, root, "", &mut warnings)?;
        vfs.changed.clear();
        Ok((vfs, warnings))
    }

    /// Adds or replaces a file, creating its parent folders. For building a drive: this doesn't
    /// count as a change made by the program.
    pub fn insert_file(&mut self, path: &str, data: Vec<u8>) {
        let mut parent = String::new();
        for component in path.split('\\').collect::<Vec<_>>().split_last().map_or(&[][..], |(_, dirs)| dirs) {
            parent = join(&parent, component);
            self.dirs.insert(parent.clone());
        }
        self.files.insert(path.to_owned(), Arc::new(data));
    }

    pub fn file(&self, path: &str) -> Option<&[u8]> {
        self.files.get(path).map(|data| data.as_slice())
    }

    pub fn is_file(&self, path: &str) -> bool {
        self.files.contains_key(path)
    }

    pub fn is_dir(&self, path: &str) -> bool {
        self.dirs.contains(path)
    }

    pub fn parent_exists(&self, path: &str) -> bool {
        self.is_dir(parent_of(path))
    }

    /// Creates an empty file or truncates an existing one.
    pub fn create(&mut self, path: &str) {
        self.files.insert(path.to_owned(), Arc::new(Vec::new()));
        self.changed.insert(path.to_owned());
    }

    /// Writes `data` at `pos`, extending the file with zeros if `pos` is past its end.
    pub fn write_at(&mut self, path: &str, pos: usize, data: &[u8]) -> bool {
        let Some(file) = self.files.get_mut(path) else { return false };
        let contents = Arc::make_mut(file);
        if contents.len() < pos + data.len() {
            contents.resize(pos + data.len(), 0);
        }
        contents[pos..pos + data.len()].copy_from_slice(data);
        self.changed.insert(path.to_owned());
        true
    }

    pub fn set_len(&mut self, path: &str, len: usize) -> bool {
        let Some(file) = self.files.get_mut(path) else { return false };
        Arc::make_mut(file).resize(len, 0);
        self.changed.insert(path.to_owned());
        true
    }

    pub fn remove_file(&mut self, path: &str) -> bool {
        let removed = self.files.remove(path).is_some();
        if removed {
            self.changed.insert(path.to_owned());
        }
        removed
    }

    pub fn rename_file(&mut self, from: &str, to: &str) -> bool {
        if self.files.contains_key(to) || self.dirs.contains(to) || !self.parent_exists(to) {
            return false;
        }
        let Some(data) = self.files.remove(from) else { return false };
        self.files.insert(to.to_owned(), data);
        self.changed.insert(from.to_owned());
        self.changed.insert(to.to_owned());
        true
    }

    pub fn make_dir(&mut self, path: &str) -> bool {
        if path.is_empty() || self.files.contains_key(path) || !self.parent_exists(path) {
            return false;
        }
        let created = self.dirs.insert(path.to_owned());
        if created {
            self.changed.insert(path.to_owned());
        }
        created
    }

    pub fn remove_dir(&mut self, path: &str) -> bool {
        if path.is_empty() || !self.list(path).is_some_and(|entries| entries.is_empty()) {
            return false;
        }
        self.dirs.remove(path);
        self.changed.insert(path.to_owned());
        true
    }

    /// Entries of a folder, sorted by name; `None` if it isn't a folder.
    pub fn list(&self, dir: &str) -> Option<Vec<DirEntry>> {
        if !self.is_dir(dir) {
            return None;
        }
        // Paths are sorted, so a folder's entries are the run of paths starting with its prefix
        // (DOS programs list folders often: FindNext lists the folder again on every call).
        let prefix = if dir.is_empty() { String::new() } else { format!("{dir}\\") };
        fn child<'a>(prefix: &str, path: &'a str) -> Option<&'a str> {
            path.strip_prefix(prefix).filter(|name| !name.is_empty() && !name.contains('\\'))
        }
        let from = (std::ops::Bound::Included(prefix.as_str()), std::ops::Bound::Unbounded);
        let dirs = self.dirs.range::<str, _>(from).map_while(|p| p.starts_with(prefix.as_str()).then_some(p));
        let files =
            self.files.range::<str, _>(from).map_while(|(p, data)| p.starts_with(prefix.as_str()).then_some((p, data)));
        let mut entries: Vec<DirEntry> = dirs
            .filter_map(|p| child(&prefix, p))
            .map(|name| DirEntry { name: name.to_owned(), is_dir: true, size: 0 })
            .chain(files.filter_map(|(p, data)| {
                child(&prefix, p).map(|name| DirEntry {
                    name: name.to_owned(),
                    is_dir: false,
                    size: u32::try_from(data.len()).unwrap_or(u32::MAX),
                })
            }))
            .collect();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Some(entries)
    }

    pub fn changed(&self) -> impl Iterator<Item = &str> {
        self.changed.iter().map(String::as_str)
    }
}

pub fn parent_of(path: &str) -> &str {
    path.rsplit_once('\\').map_or("", |(parent, _)| parent)
}

pub fn name_of(path: &str) -> &str {
    path.rsplit_once('\\').map_or(path, |(_, name)| name)
}

pub fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() { name.to_owned() } else { format!("{dir}\\{name}") }
}

/// Whether `name` (already upper-cased) is a valid DOS 8.3 name.
pub fn is_valid_83(name: &str) -> bool {
    let (base, ext) = name.split_once('.').unwrap_or((name, ""));
    let ok_char = |c: char| c.is_ascii_alphanumeric() || "!#$%&'()-@^_`{}~".contains(c);
    !base.is_empty() && base.len() <= 8 && ext.len() <= 3 && base.chars().chain(ext.chars()).all(ok_char)
}

fn load_dir(vfs: &mut Vfs, host: &Path, dos: &str, warnings: &mut Vec<String>) -> io::Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(host)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry.file_name().to_string_lossy().to_uppercase();
        let path = join(dos, &name);
        if !is_valid_83(&name) {
            warnings.push(format!("{path}: not a DOS 8.3 name; DOS programs can't open it"));
        }
        let kind = entry.file_type()?;
        if kind.is_dir() {
            vfs.dirs.insert(path.clone());
            load_dir(vfs, &entry.path(), &path, warnings)?;
        } else if kind.is_file() {
            if vfs.files.contains_key(&path) {
                warnings.push(format!("{path}: two host files map to this name; keeping the first"));
                continue;
            }
            vfs.files.insert(path, Arc::new(std::fs::read(entry.path())?));
        } else {
            warnings.push(format!("{path}: skipped (not a regular file)"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_on_write_between_clones() {
        let mut a = Vfs::new();
        a.insert_file("DATA\\X.DAT", b"hello".to_vec());
        let b = a.clone();
        assert!(a.write_at("DATA\\X.DAT", 7, b"!"));
        assert_eq!(a.file("DATA\\X.DAT"), Some(&b"hello\0\0!"[..]));
        assert_eq!(b.file("DATA\\X.DAT"), Some(&b"hello"[..]));
        assert!(a.is_dir("DATA"));
    }

    #[test]
    fn listing_rename_and_dirs() {
        let mut v = Vfs::new();
        v.insert_file("A.TXT", vec![1, 2]);
        v.insert_file("SUB\\B.TXT", vec![]);
        let names: Vec<_> = v.list("").unwrap().into_iter().map(|e| (e.name, e.is_dir, e.size)).collect();
        assert_eq!(names, vec![("A.TXT".into(), false, 2), ("SUB".into(), true, 0)]);
        assert!(v.rename_file("A.TXT", "SUB\\C.TXT"));
        assert!(!v.rename_file("SUB\\C.TXT", "NOPE\\C.TXT"));
        assert!(!v.remove_dir("SUB"), "not empty");
        assert!(v.make_dir("NEW"));
        assert!(v.remove_dir("NEW"));
        assert!(v.changed().any(|p| p == "SUB\\C.TXT"));
    }

    #[test]
    fn names() {
        assert!(is_valid_83("GAME.EXE"));
        assert!(is_valid_83("README"));
        assert!(!is_valid_83("LONGFILENAME.TXT"));
        assert!(!is_valid_83("A B.TXT"));
        assert_eq!(parent_of("A\\B\\C"), "A\\B");
        assert_eq!(name_of("A\\B\\C"), "C");
        assert_eq!(parent_of("C"), "");
    }
}
