//! Clockwork's source hash (D76): what decides how it behaves, so that a run and its snapshots are kept
//! by it, not by the program's bytes, which any rebuild changes. It covers the sources and manifests of
//! Clockwork's crates (`cli`, `dos`, `cpu`), of the workspace's manifest what they take from it (its
//! `[workspace.package]`: the edition) and of its lock file the packages they depend on (D127: not the
//! workspace's other crates and theirs, which change nothing here), and the compiler's version; line endings
//! don't count. `clockwork --source` prints it. The analysis tools' own files ([`TOOLS`]) are left out (D173):
//! they look at a run and never change what the machine does, so a change to one doesn't make every session's
//! recording stale. A new file counts until it is listed there.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

const CRATES: [&str; 3] = ["cli", "dos", "cpu"];

/// The analysis tools' files, which the hash leaves out (D173), by path in `clockwork/`: what looks at a run (watches,
/// coverage, bounds checks, uninitialized reads, contract inference, service and file tracing, the server's tool
/// operations), shows it (listings, hot spots, summaries, screenshots, the debugger stub) or plays it live (the
/// window, its sound and gamepads, which record new input but replay none). Not the machine, its devices, its records,
/// its snapshots or the operations that run it and read what a recording keeps: those decide a recording.
const TOOLS: &[&str] = &[
    "cli/src/gamepads.rs",
    "cli/src/gdb.rs",
    "cli/src/hotspots.rs",
    "cli/src/listing.rs",
    "cli/src/live_audio.rs",
    "cli/src/play.rs",
    "cli/src/protocol.rs",
    "cli/src/screenshot.rs",
    "cli/src/serve_tools.rs",
    "cli/src/summary.rs",
    "cli/src/unpack.rs",
    "dos/src/bounds.rs",
    "dos/src/contracts.rs",
    "dos/src/coverage.rs",
    "dos/src/trace.rs",
    "dos/src/uninit.rs",
    "dos/src/watch.rs",
];

fn main() {
    // Parents, not `..`: a file's name in the hash is its path in the workspace, which a path through `..` isn't (it
    // stayed the whole path, so the same sources hashed differently in each checkout, found in M6.5's step 2).
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets it"));
    let clockwork = manifest.parent().expect("cli is in clockwork/").to_owned();
    let workspace = clockwork.parent().expect("clockwork is in the workspace").to_owned();
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
        // Its path in the workspace, the same in every checkout.
        let name = file.strip_prefix(&workspace).expect("a file of the workspace").to_string_lossy().replace('\\', "/");
        if TOOLS.iter().any(|tool| name.ends_with(&format!("/{tool}"))) {
            continue;
        }
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
