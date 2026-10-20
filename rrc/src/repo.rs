//! The port repository (DESIGN.md §9): `rrc.toml`, the port's settings, committed; `rrc.local.toml`,
//! this machine's (the game's folder, the cache, the tools), not committed; `.rrc/`, this machine's
//! state (file hashes, the analysis index, the Ghidra project), not committed either.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const SETTINGS: &str = "rrc.toml";
pub const LOCAL: &str = "rrc.local.toml";
/// This machine's state for the port.
pub const STATE: &str = ".rrc";

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Settings {
    pub game: Game,
    #[serde(default, skip_serializing_if = "Port::is_default")]
    pub port: Port,
    pub original: Original,
    /// The versions of the game the port supports.
    #[serde(default, rename = "version", skip_serializing_if = "Vec::is_empty")]
    pub versions: Vec<Version>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Game {
    /// A short name, for file names and reports.
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
}

/// The port's own rules.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Port {
    /// Whether `rrc import` may bring in knowledge from elsewhere: not for an honest pilot (D46).
    #[serde(default = "yes")]
    pub imports: bool,
}

impl Default for Port {
    fn default() -> Self {
        Port { imports: true }
    }
}

impl Port {
    fn is_default(&self) -> bool {
        *self == Port::default()
    }
}

fn yes() -> bool {
    true
}

/// How Clockwork runs the original.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Original {
    /// The program, as `clockwork run` takes it (in the game's folder).
    pub program: String,
    /// `clockwork run` options: the CD drive, the current folder, the environment, memory…
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
}

/// A version of the game: its files (`/`-separated paths in the game's folder) by SHA-256.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Version {
    pub name: String,
    pub files: BTreeMap<String, String>,
}

/// This machine's settings.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Local {
    /// The person's copy of the game.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game: Option<PathBuf>,
    /// The cache, if not the user's default (`RRC_CACHE` overrides both).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Tools::is_empty")]
    pub tools: Tools,
}

/// Where the tools are, when they aren't where `rrc` looks by default.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Tools {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clockwork: Option<PathBuf>,
    /// Ghidra's folder (the one with `support/analyzeHeadless`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ghidra: Option<PathBuf>,
    /// A JDK for Ghidra (its folder: `JAVA_HOME`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub java: Option<PathBuf>,
}

impl Tools {
    fn is_empty(&self) -> bool {
        *self == Tools::default()
    }
}

pub struct Repo {
    pub root: PathBuf,
    pub settings: Settings,
    pub local: Local,
}

impl Repo {
    /// The repository `from` is in (it or a folder above it has `rrc.toml`).
    pub fn find(from: &Path) -> Result<Repo, String> {
        let mut dir = from;
        loop {
            if dir.join(SETTINGS).is_file() {
                return Repo::load(dir);
            }
            dir = dir.parent().ok_or_else(|| {
                format!("not in a port repository: no {SETTINGS} in {} or above (rrc init makes one)", from.display())
            })?;
        }
    }

    pub fn load(root: &Path) -> Result<Repo, String> {
        let settings = read_toml(&root.join(SETTINGS))?;
        let local_path = root.join(LOCAL);
        let local = if local_path.is_file() { read_toml(&local_path)? } else { Local::default() };
        Ok(Repo { root: root.to_path_buf(), settings, local })
    }

    pub fn save_settings(&self) -> Result<(), String> {
        write_toml(&self.root.join(SETTINGS), &self.settings)
    }

    pub fn save_local(&self) -> Result<(), String> {
        write_toml(&self.root.join(LOCAL), &self.local)
    }

    /// The module part of the program's IDs: its file name, uppercase.
    pub fn module(&self) -> String {
        let program = &self.settings.original.program;
        program.rsplit(['/', '\\']).next().unwrap_or(program).to_ascii_uppercase()
    }

    /// The person's copy of the game.
    pub fn game_dir(&self) -> Result<&Path, String> {
        let dir = self.local.game.as_deref().ok_or("no game folder: rrc game <folder>")?;
        if !dir.is_dir() {
            return Err(format!("the game folder {} isn't there (rrc game <folder>)", dir.display()));
        }
        Ok(dir)
    }

    pub fn knowledge(&self) -> PathBuf {
        self.root.join("knowledge")
    }

    pub fn sessions(&self) -> PathBuf {
        self.root.join("sessions")
    }

    /// This machine's state for the port, created on first use.
    pub fn state(&self) -> Result<PathBuf, String> {
        let dir = self.root.join(STATE);
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        Ok(dir)
    }
}

pub fn read_toml<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn write_toml<T: serde::Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let text = toml::to_string_pretty(value).map_err(|e| format!("{}: {e}", path.display()))?;
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_read_back_as_written() {
        let settings = Settings {
            game: Game { name: "wolf3d".into(), title: "Wolfenstein 3-D".into() },
            port: Port { imports: false },
            original: Original { program: "WOLF3D.EXE".into(), options: vec!["--ems".into(), "0".into()] },
            versions: vec![Version {
                name: "shareware 1.4".into(),
                files: BTreeMap::from([("WOLF3D.EXE".into(), "ab".repeat(32))]),
            }],
        };
        let text = toml::to_string_pretty(&settings).unwrap();
        assert!(text.contains("[[version]]"), "{text}");
        assert_eq!(toml::from_str::<Settings>(&text).unwrap(), settings);
    }

    #[test]
    fn the_module_is_the_program_s_file_name() {
        let dir = std::env::temp_dir().join(format!("rrc-repo-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let settings = Settings {
            game: Game { name: "dagger".into(), title: String::new() },
            port: Port::default(),
            original: Original { program: "dagger\\dagger.exe".into(), options: vec![] },
            versions: vec![],
        };
        write_toml(&dir.join(SETTINGS), &settings).unwrap();
        let repo = Repo::find(&dir.join("sub")).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(repo.module(), "DAGGER.EXE");
        assert!(repo.game_dir().is_err());
    }
}
