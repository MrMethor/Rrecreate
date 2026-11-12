//! The port's code (D73, D74, D117): where its files name an original's function by ID, as the code rules
//! ask of every function that reimplements one (docs/design.md §6.6), or a quirk by its name. At a
//! divergence, `rrc compare` names the port's code for the original's function that wrote the
//! differing value, so both sides are found by the same ID whatever language the port is in; `rrc
//! progress` counts the functions it names. An offset alone (`+0A2F0`, not part of an ID) names the
//! program's own.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Folders of a port repository that hold no code.
const NOT_CODE: [&str; 8] = [".git", ".rrc", "target", "knowledge", "sessions", "notes", "node_modules", "cache"];
/// Files that aren't code, by extension: settings, tables, notes, data.
const NOT_CODE_FILES: [&str; 7] = ["toml", "tsv", "md", "json", "jsonl", "lock", "txt"];
/// Files read at most this big.
const MAX_FILE: u64 = 2 << 20;

/// Where the port's files name `what` (an ID, a quirk's name), or `short` (the ID's `+OFFSET`, when it is the
/// program's own): (path from the repository's root, with `/`, and line number), in path order, at most `max`.
pub fn naming(root: &Path, what: &str, short: Option<&str>, max: usize) -> Vec<(String, usize)> {
    let mut found = Vec::new();
    each_file(root, root, &mut |relative, text| {
        for (i, line) in text.lines().enumerate() {
            if names(line, what) || short.is_some_and(|s| bare(line, s)) {
                found.push((relative.to_owned(), i + 1));
                if found.len() >= max {
                    return false;
                }
            }
        }
        true
    });
    found
}

/// The quirks the port's code branches on: every `keeps("name")`, each with where it first does.
pub fn kept(root: &Path) -> BTreeMap<String, (String, usize)> {
    let mut found = BTreeMap::new();
    each_file(root, root, &mut |relative, text| {
        for (i, line) in text.lines().enumerate() {
            for name in kept_in(line) {
                found.entry(name.to_owned()).or_insert_with(|| (relative.to_owned(), i + 1));
            }
        }
        true
    });
    found
}

/// The names in `line`'s `keeps("name")`s.
fn kept_in(line: &str) -> impl Iterator<Item = &str> {
    line.match_indices("keeps(\"").filter_map(|(at, call)| {
        let rest = &line[at + call.len()..];
        rest.find('"').map(|end| &rest[..end])
    })
}

/// Where the port's code calls something a bug or a quirk of the original without naming a quirk of `names` on that
/// line or the two either side (D148): (path, line number, the line).
pub fn unmarked(root: &Path, names: &[String]) -> Vec<(String, usize, String)> {
    let mut found = Vec::new();
    each_file(root, root, &mut |relative, text| {
        let lines: Vec<&str> = text.lines().collect();
        for at in unmarked_lines(&lines, names) {
            found.push((relative.to_owned(), at + 1, lines[at].trim().to_owned()));
        }
        true
    });
    found
}

/// The words that call something a bug.
const BUG_WORDS: [&str; 7] = ["bug", "bugs", "buggy", "quirk", "quirks", "glitch", "glitches"];
/// What a line says that makes its bug word no bug of the original's left out: it denies one, or it is about the
/// register and its tools.
const NOT_ONE: [&str; 8] = [
    "not the original's",
    "not a bug",
    "no bug",
    "changes nothing",
    "rrc quirk",
    "rrc suspects",
    "quirks.toml",
    "keeps(",
];

/// Whether `text` has one of [`BUG_WORDS`] as a word.
fn says_bug(text: &str) -> bool {
    text.split(|c: char| !c.is_alphanumeric() && c != '_')
        .any(|word| BUG_WORDS.contains(&word.to_ascii_lowercase().as_str()))
}

/// Where the port's notes and knowledge (`notes/`, `knowledge/`) call something a bug or a quirk without naming a
/// quirk of `names` on that line or the two either side, nor saying it isn't one (D160): (path, line number, the
/// line). A bug written down there and nowhere else is one the port can lose.
pub fn unmarked_text(root: &Path, names: &[String]) -> Vec<(String, usize, String)> {
    fn files(dir: &Path, found: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<_> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                files(&path, found);
            } else if path.metadata().is_ok_and(|m| m.len() <= MAX_FILE) {
                found.push(path);
            }
        }
    }
    let mut paths = Vec::new();
    for dir in ["notes", "knowledge"] {
        files(&root.join(dir), &mut paths);
    }
    let mut found = Vec::new();
    for path in paths {
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let relative = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
        let lines: Vec<&str> = text.lines().collect();
        for at in unmarked_text_lines(&lines, names) {
            found.push((relative.clone(), at + 1, lines[at].trim().to_owned()));
        }
    }
    found
}

/// The lines of a note that say a bug word, don't say it isn't one of the original's left out, and have none of
/// `names` near.
fn unmarked_text_lines(lines: &[&str], names: &[String]) -> Vec<usize> {
    let says = |line: &str| says_bug(line) && !NOT_ONE.iter().any(|not| line.contains(not));
    let named = |at: usize| {
        let near = &lines[at.saturating_sub(2)..(at + 3).min(lines.len())];
        near.iter().any(|line| names.iter().any(|name| self::names(line, name)))
    };
    (0..lines.len()).filter(|&at| says(lines[at]) && !named(at)).collect()
}

/// A line's comment, if it has one: what's after its `//`, `/*` or `#`, or all of it when it goes on a block's ` * `.
fn comment(line: &str) -> Option<&str> {
    let at = ["//", "/*", "#"].iter().filter_map(|mark| line.find(mark)).min();
    match at {
        Some(at) => Some(&line[at..]),
        None => line.trim_start().starts_with('*').then_some(line),
    }
}

/// The lines whose comment says `quirk` or `bug` as a word, and doesn't say it's `not the original's` (a port's own) or
/// that it `changes nothing` the game does (D163), with none of `names` near.
fn unmarked_lines(lines: &[&str], names: &[String]) -> Vec<usize> {
    let says = |line: &str| {
        comment(line).is_some_and(|text| {
            !text.contains("not the original's") && !text.contains("changes nothing") && says_bug(text)
        })
    };
    let named = |at: usize| {
        let near = &lines[at.saturating_sub(2)..(at + 3).min(lines.len())];
        near.iter().any(|line| names.iter().any(|name| self::names(line, name)))
    };
    (0..lines.len()).filter(|&at| says(lines[at]) && !named(at)).collect()
}

/// The IDs the port's files name, as IDs are written (`GAME.EXE+0A2F0`, `GAME.EXE#2+00010`), with `module`'s
/// for an offset alone (`+0A2F0`).
pub fn ids(root: &Path, module: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    each_file(root, root, &mut |_, text| {
        for line in text.lines() {
            ids_in(line, module, &mut found);
        }
        true
    });
    found
}

/// The port's code as it is: the hash of its files' paths and contents, in path order. What a comparison found
/// holds for the code it ran, so a comparison is kept with this, and one the code as it is now didn't make is said to
/// be stale (D158).
pub fn fingerprint(root: &Path) -> String {
    let mut all = Vec::new();
    each_file(root, root, &mut |relative, text| {
        all.extend_from_slice(relative.as_bytes());
        all.push(0);
        all.extend_from_slice(text.as_bytes());
        all.push(0);
        true
    });
    crate::cache::sha256(&all)
}

/// The port's code files under `dir`, in path order, each given to `f` with its path from `root` (with `/`)
/// until it says to stop; whether it did not.
fn each_file(root: &Path, dir: &Path, f: &mut dyn FnMut(&str, &str) -> bool) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else { return true };
    let mut entries: Vec<_> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if path.is_dir() {
            if !NOT_CODE.contains(&name.as_str()) && !each_file(root, &path, f) {
                return false;
            }
            continue;
        }
        let extension = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        if NOT_CODE_FILES.contains(&extension.as_str()) || path.metadata().map_or(true, |m| m.len() > MAX_FILE) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let relative = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
        if !f(&relative, &text) {
            return false;
        }
    }
    true
}

/// A character of an ID's module (`GAME.EXE#2`).
fn module_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '#')
}

/// Whether `line` has `short` (`+0A2F0`) as an offset alone: no module before it, no more digits after.
fn bare(line: &str, short: &str) -> bool {
    line.match_indices(short).any(|(at, _)| {
        !line[..at].chars().next_back().is_some_and(module_char)
            && !line[at + short.len()..].chars().next().is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// The IDs in `line`: each `+` and 5 to 8 uppercase hex digits, with the module written before it, else
/// `module`'s.
fn ids_in(line: &str, module: &str, found: &mut BTreeSet<String>) {
    for (at, _) in line.match_indices('+') {
        let rest = &line[at + 1..];
        let digits = rest.chars().take_while(|c| c.is_ascii_digit() || ('A'..='F').contains(c)).count();
        if !(5..=8).contains(&digits)
            || rest[digits..].chars().next().is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            continue;
        }
        let start = line[..at].rfind(|c: char| !module_char(c)).map_or(0, |i| i + 1);
        let before = &line[start..at];
        let id = format!("{}+{}", if before.is_empty() { module } else { before }, &rest[..digits]);
        if let Ok(id) = crate::knowledge::parse_id(&id) {
            found.insert(id.to_string());
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
    fn a_bug_said_in_the_code_names_its_quirk() {
        let names = ["overrun".to_owned()];
        let lines = [
            "// a bug: reads past the table",
            "let a = 1;",
            "// debug output, and a bugle",
            "",
            "// a quirk: quirk overrun",
            "let b = 2;",
            "",
            "",
            "",
            "// Quirk, this one too.",
            "",
            "",
            "",
            "fn bug(which: &str) -> bool {",
            "let a = 1; // a bug of the test's, not the original's",
            "unported(\"a bug\");",
            "",
            "",
            "// The original's bug, but it changes nothing the game does: the value is never read.",
        ];
        assert_eq!(unmarked_lines(&lines, &names), [0, 9], "a comment's word, and only the original's");
        let near = ["// a bug", "x", "if rrc_runtime::keeps(\"overrun\") {"];
        assert_eq!(unmarked_lines(&near, &names), [] as [usize; 0], "two lines off is near");
    }

    #[test]
    fn a_bug_said_in_the_notes_names_its_quirk() {
        let names = ["overrun".to_owned()];
        let lines = [
            "The sixth enemy reads past the table: a bug.",
            "",
            "",
            "The seed is never reset, which is a glitch: quirk overrun",
            "and its neighbour's line is near enough.",
            "",
            "",
            "Debugging showed nothing here, and this is not a bug: the table is longer.",
            "Every bug is registered with rrc quirk as it is found.",
            "The loop is Buggy at the wall.",
            "",
            "",
            "A bug of the original's that changes nothing: what it writes nothing reads.",
        ];
        assert_eq!(unmarked_text_lines(&lines, &names), [0, 9]);
    }

    #[test]
    fn the_quirks_kept_are_found_by_their_calls() {
        let line = r#"if rrc_runtime::keeps("one") && keeps("two") { keeps(x) }"#;
        assert_eq!(kept_in(line).collect::<Vec<_>>(), ["one", "two"]);
    }

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
        assert_eq!(naming(&root, id, None, 10), [("src/game/enemies.rs".to_owned(), 2), ("src/main.rs".to_owned(), 3)]);
        assert_eq!(naming(&root, id, None, 1).len(), 1);
        assert!(naming(&root, "GAME.EXE+00200", None, 10).is_empty());
        write("src/quirks.rs", "if rrc_runtime::keeps(\"overrun\") {}\nkeeps(\"overrun_2\")\n");
        assert_eq!(naming(&root, "overrun", None, 10), [("src/quirks.rs".to_owned(), 1)]);
        // An offset alone names the program's own.
        write("src/draw.rs", "/// `+00200`: draws (and +002001, X+00200 are others)\nfn draw() {}\n");
        assert_eq!(naming(&root, "GAME.EXE+00200", Some("+00200"), 10), [("src/draw.rs".to_owned(), 1)]);
        // The code as it is: another hash when a file's text changes, the same for what isn't code.
        let before = fingerprint(&root);
        write("notes.md", "more notes\n");
        assert_eq!(fingerprint(&root), before);
        write("src/draw.rs", "/// `+00200`: draws (and +002001, X+00200 are others)\nfn draw() { }\n");
        assert_ne!(fingerprint(&root), before);
        let ids = ids(&root, "GAME.EXE");
        assert!(ids.contains("GAME.EXE+00114") && ids.contains("GAME.EXE+00200"), "{ids:?}");
        assert!(!ids.contains("GAME.EXE+002001") && !ids.iter().any(|id| id.contains("00114 ")), "{ids:?}");
        std::fs::remove_dir_all(&root).ok();
    }
}
