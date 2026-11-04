//! Clockwork's source hash (D76): what decides how it behaves, so that a run and its snapshots are kept
//! by it, not by the program's bytes, which any rebuild changes. It covers the sources and manifests of
//! Clockwork's crates (`cli`, `dos`, `cpu`), of the workspace's manifest what they take from it (its
//! `[workspace.package]`: the edition) and of its lock file the packages they depend on (D127: not the
//! workspace's other crates and theirs, which change nothing here), and the compiler's version; line endings
//! don't count. `clockwork --source` prints it.

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
        println!("cargo:rerun-if-changed={}", workspace.join(file).display());
    }
    println!("cargo:rerun-if-changed=build.rs");

    let mut hasher = Sha256::new();
    hasher.update(workspace_package(&workspace.join("Cargo.toml")).as_bytes());
    hasher.update([0]);
    hasher.update(locked("clockwork", &workspace.join("Cargo.lock")).as_bytes());
    hasher.update([0]);
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

/// The workspace manifest's `[workspace.package]` (what the crates take from it), as text.
fn workspace_package(manifest: &Path) -> String {
    let text = std::fs::read_to_string(manifest).expect("the workspace's manifest reads");
    let table: toml::Table = text.parse().expect("the workspace's manifest parses");
    table.get("workspace").and_then(|w| w.get("package")).map_or_else(String::new, |p| p.to_string())
}

/// The packages of the lock file `root` depends on, itself and its dependencies' through theirs: each as its
/// name, version, source and checksum, a line each, sorted.
fn locked(root: &str, lock: &Path) -> String {
    let text = std::fs::read_to_string(lock).expect("the lock file reads");
    let table: toml::Table = text.parse().expect("the lock file parses");
    let packages: Vec<&toml::Table> =
        table.get("package").and_then(|p| p.as_array()).into_iter().flatten().filter_map(|p| p.as_table()).collect();
    let field = |p: &toml::Table, key: &str| p.get(key).and_then(|v| v.as_str()).unwrap_or_default().to_owned();
    // A dependency is "name", "name version" or "name version (source)".
    let find = |dependency: &str| -> Option<&toml::Table> {
        let mut parts = dependency.split(' ');
        let (name, version) = (parts.next()?, parts.next());
        packages.iter().copied().find(|p| field(p, "name") == name && version.is_none_or(|v| field(p, "version") == v))
    };
    let mut lines = std::collections::BTreeSet::new();
    let mut pending: Vec<&toml::Table> = find(root).into_iter().collect();
    while let Some(p) = pending.pop() {
        let line =
            format!("{} {} {} {}", field(p, "name"), field(p, "version"), field(p, "source"), field(p, "checksum"));
        if !lines.insert(line) {
            continue;
        }
        let dependencies = p.get("dependencies").and_then(|d| d.as_array()).into_iter().flatten();
        pending.extend(dependencies.filter_map(|d| d.as_str()).filter_map(find));
    }
    lines.into_iter().collect::<Vec<_>>().join(
        "
",
    )
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
