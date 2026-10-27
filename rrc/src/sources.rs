//! The port's code (D73, D74): where its files name an original's function by ID, as the code rules
//! ask of every function that reimplements one (DESIGN.md §6.6), or a quirk by its name. At a
//! divergence, `rrc compare` names the port's code for the original's function that wrote the
//! differing value, so both sides are found by the same ID whatever language the port is in.

use std::path::Path;

/// Folders of a port repository that hold no code.
const NOT_CODE: [&str; 8] = [".git", ".rrc", "target", "knowledge", "sessions", "notes", "node_modules", "cache"];
/// Files that aren't code, by extension: settings, tables, notes, data.
const NOT_CODE_FILES: [&str; 7] = ["toml", "tsv", "md", "json", "jsonl", "lock", "txt"];
/// Files read at most this big.
const MAX_FILE: u64 = 2 << 20;

/// Where the port's files name `what` (an ID, a quirk's name): (path from the repository's root, with
/// `/`, and line number), in path order, at most `max`.
pub fn naming(root: &Path, what: &str, max: usize) -> Vec<(String, usize)> {
    let mut found = Vec::new();
    walk(root, root, what, max, &mut found);
    found
}

fn walk(root: &Path, dir: &Path, id: &str, max: usize, found: &mut Vec<(String, usize)>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        if found.len() >= max {
            return;
        }
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if path.is_dir() {
            if !NOT_CODE.contains(&name.as_str()) {
                walk(root, &path, id, max, found);
            }
            continue;
        }
        let extension = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        if NOT_CODE_FILES.contains(&extension.as_str()) || path.metadata().map_or(true, |m| m.len() > MAX_FILE) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let relative = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
        for (i, line) in text.lines().enumerate() {
            if names(line, id) {
                found.push((relative.clone(), i + 1));
                if found.len() >= max {
                    return;
                }
            }
        }
    }
}

/// Whether `line` has `what` as a word of its own (`GAME.EXE+0A2F0` isn't in `GAME.EXE+0A2F01` or
/// `XGAME.EXE+0A2F0`, `overrun` isn't in `overrun_2`).
fn names(line: &str, what: &str) -> bool {
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    line.match_indices(what).any(|(at, _)| {
        !line[..at].chars().next_back().is_some_and(word) && !line[at + what.len()..].chars().next().is_some_and(word)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_port_s_code_is_found_by_the_original_s_ids() {
        let root = std::env::temp_dir().join(format!("rrc-sources-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for dir in ["src/game", "knowledge", "target"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        let write = |file: &str, text: &str| std::fs::write(root.join(file), text).unwrap();
        write("src/main.rs", "fn main() {}\n\n/// update (GAME.EXE+00114)\nfn update() {}\n");
        write("src/game/enemies.rs", "// GAME.EXE+001140 and XGAME.EXE+00114 are others\n// GAME.EXE+00114.\n");
        write("knowledge/functions.tsv", "GAME.EXE+00114\tupdate\n");
        write("target/copy.rs", "GAME.EXE+00114\n");
        write("notes.md", "GAME.EXE+00114\n");
        let id = "GAME.EXE+00114";
        assert_eq!(naming(&root, id, 10), [("src/game/enemies.rs".to_owned(), 2), ("src/main.rs".to_owned(), 3)]);
        assert_eq!(naming(&root, id, 1).len(), 1);
        assert!(naming(&root, "GAME.EXE+00200", 10).is_empty());
        write("src/quirks.rs", "if rrc_runtime::keeps(\"overrun\") {}\nkeeps(\"overrun_2\")\n");
        assert_eq!(naming(&root, "overrun", 10), [("src/quirks.rs".to_owned(), 1)]);
        std::fs::remove_dir_all(&root).ok();
    }
}
