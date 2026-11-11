//! The hub, `docs/README.md` (D153), stays the entry point: every document has its row there, its links lead
//! somewhere, its milestones are the plan's, its state names the latest decision, the state of the work is written
//! nowhere else, and `CLAUDE.md` loads it.

use std::fs;
use std::path::{Path, PathBuf};

const HUB: &str = "docs/README.md";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("the workspace's folder").to_path_buf()
}

fn text(path: &str) -> String {
    fs::read_to_string(root().join(path)).unwrap_or_else(|error| panic!("{path}: {error}"))
}

/// Where the hub's links lead inside the repository, as paths from its root (the hub's are from `docs/`).
fn links() -> Vec<String> {
    let hub = text(HUB);
    hub.split("](")
        .skip(1)
        .filter_map(|rest| rest.split(')').next())
        .map(|target| target.split('#').next().unwrap_or(target))
        .filter(|target| !target.is_empty() && !target.contains("://"))
        .map(|target| {
            let mut parts = vec!["docs"];
            for part in target.split('/') {
                match part {
                    "" | "." => {}
                    ".." => drop(parts.pop()),
                    part => parts.push(part),
                }
            }
            parts.join("/")
        })
        .collect()
}

/// The repository's documents (`*.md`), as paths from its root: not what builds, tools and other emulators' sources
/// leave (`target`, hidden folders, `reference`).
fn documents(folder: &Path, from_root: &str, found: &mut Vec<String>) {
    let mut entries: Vec<_> = fs::read_dir(folder).expect("a folder").map(|entry| entry.expect("an entry")).collect();
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = if from_root.is_empty() { name.clone() } else { format!("{from_root}/{name}") };
        if entry.path().is_dir() {
            if !name.starts_with('.') && name != "target" && path != "reference" {
                documents(&entry.path(), &path, found);
            }
        } else if name.ends_with(".md") {
            found.push(path);
        }
    }
}

/// The milestones a text's tables name (`| M4 A real game's logic | ...`), each with its row's next cell.
fn milestones(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let mut cells = line.strip_prefix("| M")?.split('|').map(str::trim);
            let name = cells.next()?;
            let numbered = name.starts_with(|c: char| c.is_ascii_digit());
            numbered.then(|| (format!("M{name}"), cells.next().unwrap_or_default().to_owned()))
        })
        .collect()
}

#[test]
fn every_document_is_listed_in_the_hub() {
    let (links, mut found) = (links(), Vec::new());
    documents(&root(), "", &mut found);
    assert!(found.iter().any(|path| path == HUB), "{HUB} is missing");
    let unlisted: Vec<_> = found.iter().filter(|path| *path != HUB && !links.contains(path)).collect();
    assert!(unlisted.is_empty(), "{HUB} doesn't list {unlisted:?}: give each a row in its section 7");
}

#[test]
fn the_hubs_links_lead_somewhere() {
    let dead: Vec<_> = links().into_iter().filter(|path| !root().join(path).exists()).collect();
    assert!(dead.is_empty(), "{HUB} links to what isn't there: {dead:?}");
}

#[test]
fn the_hubs_milestones_are_the_plans() {
    let (hub, plan) = (milestones(&text(HUB)), milestones(&text("docs/design.md")));
    assert!(!plan.is_empty(), "docs/design.md §12 has no milestones");
    let names = |rows: &[(String, String)]| rows.iter().map(|(name, _)| name.clone()).collect::<Vec<_>>();
    assert_eq!(names(&hub), names(&plan), "{HUB} section 5 and docs/design.md §12 name different milestones");
    for (name, state) in &hub {
        assert!(["done", "under way", "ahead"].contains(&state.as_str()), "{HUB}: {name} is `{state}`");
    }
}

#[test]
fn the_hubs_state_names_the_latest_decision() {
    let decisions = text("docs/decisions.md");
    let latest = decisions
        .lines()
        .filter_map(|line| line.strip_prefix("### D")?.split('.').next()?.parse::<u32>().ok())
        .max()
        .expect("a decision");
    let stated = format!("decisions to D{latest})");
    assert!(
        text(HUB).contains(&stated),
        "{HUB} section 5 doesn't say `{stated}`: bring its state up to D{latest}, then its date and that number"
    );
}

#[test]
fn the_state_of_the_work_is_in_the_hub_alone() {
    for path in ["CLAUDE.md", "README.md", "docs/design.md"] {
        assert!(!text(path).contains("**Status"), "{path} states where the work stands: that is {HUB} section 5's");
    }
}

#[test]
fn agents_are_given_the_hub() {
    assert!(text("CLAUDE.md").lines().any(|line| line.trim() == "@docs/README.md"), "CLAUDE.md doesn't load {HUB}");
    assert!(text("AGENTS.md").contains(HUB), "AGENTS.md doesn't name {HUB}");
}
