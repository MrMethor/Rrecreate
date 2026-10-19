//! The outside tools analysis needs: Ghidra and a JDK for it.

use std::path::PathBuf;

use crate::repo::Local;

/// Ghidra's folder: `tools.ghidra` in `rrc.local.toml`, or `GHIDRA_INSTALL_DIR`.
pub fn ghidra(local: &Local) -> Result<PathBuf, String> {
    local
        .tools
        .ghidra
        .clone()
        .or_else(|| std::env::var_os("GHIDRA_INSTALL_DIR").map(PathBuf::from))
        .filter(|dir| headless(dir).is_file())
        .ok_or_else(|| "Ghidra not found: set tools.ghidra in rrc.local.toml or GHIDRA_INSTALL_DIR".into())
}

/// Its headless analyzer.
pub fn headless(ghidra: &std::path::Path) -> PathBuf {
    ghidra.join("support").join(if cfg!(windows) { "analyzeHeadless.bat" } else { "analyzeHeadless" })
}

/// A JDK for Ghidra: `tools.java` in `rrc.local.toml`, or `JAVA_HOME`.
pub fn java(local: &Local) -> Result<PathBuf, String> {
    local
        .tools
        .java
        .clone()
        .or_else(|| std::env::var_os("JAVA_HOME").map(PathBuf::from))
        .filter(|dir| dir.join("bin").join(if cfg!(windows) { "java.exe" } else { "java" }).is_file())
        .ok_or_else(|| "no JDK for Ghidra: set tools.java in rrc.local.toml or JAVA_HOME (JDK 21)".into())
}
