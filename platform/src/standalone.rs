//! A port played on its own (D226, the person's): the program alone, started with nothing beside it, finds the
//! game's folder, checks it is the version the port is of, and plays from power-on by its book
//! (`rrc_runtime::standalone`). Its folder: `--game DIR` or `RRC_GAME` if given; else the one it played last (kept
//! in `PORT.toml` beside the program); else the program's own folder or the current one if the game is there; else
//! where Steam keeps its games (each library's `steamapps/common`, two levels down); else asked once, on the console.
//! A folder is the game's when it holds the book's program file and that file is a version the book lists. The games
//! the port saves are kept in `saves` beside the program, never in the game's folder.

use std::path::{Path, PathBuf};

use rrc_runtime::standalone::Book;
use sha2::{Digest, Sha256};

/// Where the program is, and its name without its extension (`arena`).
fn beside() -> Option<(PathBuf, String)> {
    let exe = std::env::current_exe().ok()?;
    Some((exe.parent()?.to_path_buf(), exe.file_stem()?.to_string_lossy().into_owned()))
}

/// What a folder is to the port: the game's, or why not.
fn holds(dir: &Path, book: &Book) -> Result<(), String> {
    let bytes = rrc_runtime::original::file_in(dir, &book.program).map_err(|_| format!("no {} in it", book.program))?;
    let hash: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    match book.versions.is_empty() || book.versions.contains(&hash) {
        true => Ok(()),
        false => Err(format!("its {} isn't of the version the port is of", book.program)),
    }
}

/// The settings file beside the program, and the folder it keeps.
fn remembered() -> Option<(PathBuf, Option<PathBuf>)> {
    let (dir, name) = beside()?;
    let file = dir.join(format!("{name}.toml"));
    let kept = std::fs::read_to_string(&file).ok().and_then(|text| {
        let table: toml::Table = text.parse().ok()?;
        Some(PathBuf::from(table.get("game")?.as_str()?))
    });
    Some((file, kept))
}

/// Keeps `dir` as the game's folder for the next start (where the program's folder can be written).
fn remember(dir: &Path) {
    if let Some((file, _)) = remembered() {
        let mut table = toml::Table::new();
        table.insert("game".into(), toml::Value::String(dir.to_string_lossy().into_owned()));
        let text = format!("# The game's folder this port plays with: delete this file to choose again.\n{table}");
        let _ = std::fs::write(file, text);
    }
}

/// Steam's own folder and its libraries'.
fn steam_libraries() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    #[cfg(windows)]
    {
        for key in [r"HKCU\Software\Valve\Steam", r"HKLM\SOFTWARE\WOW6432Node\Valve\Steam"] {
            for value in ["SteamPath", "InstallPath"] {
                let out = std::process::Command::new("reg").args(["query", key, "/v", value]).output();
                if let Ok(out) = out {
                    let text = String::from_utf8_lossy(&out.stdout).into_owned();
                    if let Some(line) = text.lines().find(|l| l.contains("REG_SZ")) {
                        let path = line.split("REG_SZ").nth(1).unwrap_or("").trim().replace('/', "\\");
                        roots.push(PathBuf::from(path));
                    }
                }
            }
        }
        for var in ["ProgramFiles(x86)", "ProgramFiles"] {
            if let Some(dir) = std::env::var_os(var) {
                roots.push(PathBuf::from(dir).join("Steam"));
            }
        }
    }
    #[cfg(not(windows))]
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        roots.extend([home.join(".steam/steam"), home.join(".local/share/Steam")]);
    }
    let mut libraries = Vec::new();
    for root in roots {
        let vdf = std::fs::read_to_string(root.join("steamapps").join("libraryfolders.vdf")).unwrap_or_default();
        for line in vdf.lines().filter(|l| l.trim_start().starts_with("\"path\"")) {
            if let Some(path) = line.split('"').nth(3) {
                libraries.push(PathBuf::from(path.replace("\\\\", "\\")));
            }
        }
        libraries.push(root);
    }
    libraries.dedup();
    libraries
}

/// The folders under `dir`, `depth` levels down.
fn folders(dir: &Path, depth: usize) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut found = Vec::new();
    for entry in entries.flatten().filter(|e| e.path().is_dir()) {
        found.push(entry.path());
        if depth > 1 {
            found.extend(folders(&entry.path(), depth - 1));
        }
    }
    found
}

/// The game's folder, found or asked for (see the module's documentation), and kept for the next start.
pub fn game(book: &Book) -> Result<PathBuf, String> {
    if let Some(dir) = rrc_runtime::original::game_dir() {
        return holds(&dir, book).map(|()| dir.clone()).map_err(|e| format!("{}: {e}", dir.display()));
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some((_, Some(kept))) = remembered() {
        candidates.push(kept);
    }
    if let Some((dir, _)) = beside() {
        candidates.push(dir);
    }
    candidates.extend(std::env::current_dir().ok());
    for library in steam_libraries() {
        candidates.extend(folders(&library.join("steamapps").join("common"), 2));
    }
    if let Some(dir) = candidates.into_iter().find(|dir| holds(dir, book).is_ok()) {
        remember(&dir);
        return Ok(dir);
    }
    loop {
        println!("Where is the game: the folder that holds {}?", book.program);
        print!("Folder: ");
        let _ = std::io::Write::flush(&mut std::io::stdout());
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).map_err(|e| e.to_string())? == 0 {
            return Err(format!("no game folder: give it with --game DIR (the folder with {})", book.program));
        }
        let dir = PathBuf::from(line.trim().trim_matches('"'));
        match holds(&dir, book) {
            Ok(()) => {
                remember(&dir);
                return Ok(dir);
            }
            Err(e) => println!("{}: {e}", dir.display()),
        }
    }
}

/// The saves folder of a port played on its own: `saves` beside the program.
pub fn saves() -> Option<PathBuf> {
    beside().map(|(dir, _)| dir.join("saves"))
}

/// On Windows a port started from its icon has a console of its own, which closes with it: what went wrong is
/// kept on the screen until Enter is pressed.
pub fn hold(message: &str) {
    if !message.is_empty() {
        eprintln!("{message}");
    }
    #[cfg(windows)]
    {
        eprintln!("Press Enter to close.");
        let _ = std::io::stdin().read_line(&mut String::new());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_is_the_game_s_when_its_program_is_a_version_the_book_lists() {
        let dir = std::env::temp_dir().join(format!("rrc-standalone-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Game.exe"), b"MZ program").unwrap();
        let hash: String = Sha256::digest(b"MZ program").iter().map(|b| format!("{b:02x}")).collect();
        let book = Book { program: "GAME.EXE".into(), versions: vec![hash], ..Book::default() };
        assert_eq!(holds(&dir, &book), Ok(()));
        let other = Book { versions: vec!["00".into()], ..book.clone() };
        assert!(holds(&dir, &other).unwrap_err().contains("isn't of the version"));
        let elsewhere = Book { program: "OTHER.EXE".into(), ..book };
        assert!(holds(&dir, &elsewhere).unwrap_err().contains("no OTHER.EXE"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
