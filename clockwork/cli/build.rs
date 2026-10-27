//! Clockwork's source hash (D76): what decides how it behaves, so that a run and its snapshots are kept
//! by it, not by the program's bytes, which any rebuild changes. It covers the sources and manifests of
//! Clockwork's crates (`cli`, `dos`, `cpu`), the workspace's manifest and lock file (the dependencies'
//! versions) and the compiler's version; line endings don't count. `clockwork --source` prints it.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

const CRATES: [&str; 3] = ["cli", "dos", "cpu"];

fn main() {
    let clockwork = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets it")).join("..");
    let workspace = clockwork.join("..");
    let mut files = Vec::new();
    for name in CRATES {
        let dir = clockwork.join(name);
        files.push(dir.join("Cargo.toml"));
        files.push(dir.join("build.rs"));
        sources(&dir.join("src"), &mut files);
        println!("cargo:rerun-if-changed={}", dir.join("src").display());
        println!("cargo:rerun-if-changed={}", dir.join("Cargo.toml").display());
    }
    for file in ["Cargo.toml", "Cargo.lock"] {
        files.push(workspace.join(file));
        println!("cargo:rerun-if-changed={}", workspace.join(file).display());
    }
    println!("cargo:rerun-if-changed=build.rs");

    let mut hasher = Sha256::new();
    for file in files.iter().filter(|f| f.is_file()) {
        let name = file.strip_prefix(&workspace).unwrap_or(file).to_string_lossy().replace('\\', "/");
        let bytes = std::fs::read(file).expect("a source file reads");
        hasher.update(name.as_bytes());
        hasher.update([0]);
        hasher.update(without_cr(&bytes));
        hasher.update([0]);
    }
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let version = std::process::Command::new(rustc).arg("--version").output().map(|o| o.stdout).unwrap_or_default();
    hasher.update(&version);
    let hash: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
    println!("cargo:rustc-env=CLOCKWORK_SOURCE={hash}");
}

/// The files under `dir`, in path order.
fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut paths: Vec<PathBuf> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            sources(&path, out);
        } else {
            out.push(path);
        }
    }
}

/// The bytes with each CR before an LF left out: a checkout's line endings aren't the source.
fn without_cr(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    for (i, &b) in bytes.iter().enumerate() {
        if !(b == b'\r' && bytes.get(i + 1) == Some(&b'\n')) {
            out.push(b);
        }
    }
    out
}
