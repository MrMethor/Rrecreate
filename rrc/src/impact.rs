//! What a change of the port's code touches (D219), so `rrc work --run` counts again only the sessions it can change.
//!
//! The port's code is cut into items: a function, a type, a constant, each from its doc comment to the next item's.
//! Each item's hash, its text and the original's places it names are kept by the code's fingerprint (`code` in the
//! cache). Two versions compared give the items that changed, and the places named near the lines that changed in
//! them (an edit in a long function reaches what its own block follows, not all the function names); a changed item
//! that names none the
//! sessions run (a helper, a constant of the data's) stands for the items that use it, three deep. A session whose
//! run executed none of those places (`rrc suspects`' coverage, D151) is spared: its census and comparisons hold for
//! the new code. What can't be told (an item nothing names a place for, a session never looked through, code kept
//! before) touches every session.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::cache::Cache;
use crate::repo::Repo;
use crate::stage::Held;

/// How deep a changed item that names none of the places run is followed to the items that use it.
const USERS_DEEP: usize = 3;

/// An item of the port's code: its text's hash, the original's places it names, and its name (what a use says).
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Item {
    pub hash: String,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub ids: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// What holds no code of its own (a file's head: its uses and its docs; an `impl` line): its change is its items'.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub frame: bool,
    /// Its text, as kept with its code's fingerprint: what changed in it is told by its lines.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
}

/// The port's code by item (`path:kind name`, `#2` on for another of the same), with each item's text (not kept).
#[derive(Debug, Default)]
pub(crate) struct Code {
    pub items: BTreeMap<String, Item>,
    pub texts: BTreeMap<String, String>,
}

/// The port's code now, cut into items.
pub(crate) fn code(root: &Path, module: &str) -> Code {
    let mut code = Code::default();
    crate::sources::each_file(root, root, &mut |relative, text| {
        for (key, mut item, text) in items(relative, text, module) {
            item.text.clone_from(&text);
            code.texts.insert(key.clone(), text);
            code.items.insert(key, item);
        }
        true
    });
    code
}

/// The port's code's fingerprint (`sources::fingerprint`), its items kept under it for a later change to be told by.
pub(crate) fn fingerprint(repo: &Repo) -> Result<String, String> {
    let port = crate::sources::fingerprint(&repo.root);
    let cache = Cache::open(&repo.local)?;
    if cache.recall("code", &port).is_none() {
        let items = code(&repo.root, &repo.module()).items;
        let text = serde_json::to_string(&items).map_err(|e| e.to_string())?;
        cache.remember("code", &port, &text)?;
        // The crates it builds on, as they were (D234): a change of theirs reaches every session.
        cache.remember("dependencies", &port, &crate::sources::dependencies_fingerprint(&repo.root))?;
    }
    Ok(port)
}

/// The items kept for the code of fingerprint `port`, if any.
fn kept(cache: &Cache, port: &str) -> Option<BTreeMap<String, Item>> {
    serde_json::from_str(&cache.recall("code", port)?).ok()
}

/// A file's items: what is before its first item (its head), then each item from its doc comment and attributes
/// on; a file's tests (from a `#[cfg(test)]` at its margin) are left out.
fn items(path: &str, text: &str, module: &str) -> Vec<(String, Item, String)> {
    let lines: Vec<&str> = text.lines().collect();
    let end = lines.iter().position(|l| l.trim_end() == "#[cfg(test)]").unwrap_or(lines.len());
    let lines = &lines[..end];
    // Each item's first line (its docs' and attributes' first) and its kind and name.
    let mut starts: Vec<(usize, String)> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let Some(sig) = signature(line) else { continue };
        let floor = starts.last().map_or(0, |(s, _)| s + 1);
        let mut first = i;
        while first > floor && is_lead(lines[first - 1]) {
            first -= 1;
        }
        starts.push((first, sig));
    }
    let mut out = Vec::new();
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let head_end = starts.first().map_or(lines.len(), |(s, _)| *s);
    let mut push = |key: String, name: String, frame: bool, body: &[&str]| {
        let text = body.join("\n");
        let mut ids = BTreeSet::new();
        for line in body {
            crate::sources::ids_in(line, module, &mut ids);
        }
        let n = seen.entry(key.clone()).or_insert(0);
        *n += 1;
        let key = if *n == 1 { key } else { format!("{key}#{n}") };
        let item = Item { hash: crate::cache::sha256(text.as_bytes()), ids, name, frame, text: String::new() };
        out.push((key, item, text));
    };
    push(format!("{path}:"), String::new(), true, &lines[..head_end]);
    for (k, (start, sig)) in starts.iter().enumerate() {
        let next = starts.get(k + 1).map_or(lines.len(), |(s, _)| *s);
        let (kind, name) = sig.split_once(' ').unwrap_or((sig.as_str(), ""));
        // An `impl` line, and a `mod NAME;` (its file's items are its own), hold no code.
        let declared = kind == "mod" && lines[*start..next].iter().any(|l| !is_lead(l) && l.trim_end().ends_with(';'));
        push(format!("{path}:{sig}"), name.to_owned(), kind == "impl" || declared, &lines[*start..next]);
    }
    out
}

/// A doc comment, a comment or an attribute: what goes with the item after it.
fn is_lead(line: &str) -> bool {
    let t = line.trim_start();
    (t.starts_with("//") && !t.starts_with("//!")) || t.starts_with("#[")
}

/// The item a line begins, as `kind name`: a function at any depth; a type, a constant, a static, a trait, a module,
/// a macro or an `impl` at the margin.
fn signature(line: &str) -> Option<String> {
    let indent = line.len() - line.trim_start().len();
    let mut rest = line.trim_start();
    for visibility in ["pub(crate) ", "pub(super) ", "pub(self) ", "pub "] {
        if let Some(r) = rest.strip_prefix(visibility) {
            rest = r;
            break;
        }
    }
    loop {
        let before = rest;
        for qualifier in ["const ", "async ", "unsafe ", "extern \"C\" "] {
            if let Some(r) = rest.strip_prefix(qualifier)
                && (r.starts_with("fn ") || ["async ", "unsafe ", "extern "].iter().any(|q| r.starts_with(q)))
            {
                rest = r;
            }
        }
        if rest == before {
            break;
        }
    }
    let ident = |s: &str| s.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect::<String>();
    if let Some(r) = rest.strip_prefix("fn ") {
        let name = ident(r);
        return (!name.is_empty()).then(|| format!("fn {name}"));
    }
    if indent > 0 {
        return None;
    }
    if let Some(r) = rest.strip_prefix("impl") {
        if r.starts_with([' ', '<']) {
            let head = r.split('{').next().unwrap_or("").trim();
            // `impl<T> Name<T>` and `impl Trait for Name`: the type's name.
            let head = head.trim_start_matches(|c| c != ' ' && head.starts_with('<')).trim();
            let ty = head.rsplit(" for ").next().unwrap_or(head);
            return Some(format!("impl {}", ident(ty.trim())));
        }
        return None;
    }
    if let Some(r) = rest.strip_prefix("macro_rules! ") {
        return Some(format!("macro {}", ident(r)));
    }
    for kind in ["struct", "enum", "type", "trait", "const", "static", "mod", "union"] {
        if let Some(r) = rest.strip_prefix(kind).and_then(|r| r.strip_prefix(' ')) {
            let r = r.strip_prefix("mut ").unwrap_or(r);
            let name = ident(r);
            if !name.is_empty() {
                return Some(format!("{kind} {name}"));
            }
        }
    }
    None
}

/// What a change from the items `old` to the code `now` touches: each changed item, with the places it names and
/// those its users name (three deep, for one that names none a session runs).
pub(crate) struct Change {
    pub items: Vec<Changed>,
}

pub(crate) struct Changed {
    pub key: String,
    /// The places it names (its old version's too).
    pub ids: BTreeSet<String>,
    /// The places the items that use it name, three deep through users that name none.
    pub users: BTreeSet<String>,
    /// Whether anything uses it at all.
    pub used: bool,
    /// Told by its lines and none near them names a place, in an item that names some elsewhere: it reaches nothing
    /// of its own (the core sessions and `--all` stand for it).
    pub nowhere: bool,
}

pub(crate) fn change(old: &BTreeMap<String, Item>, now: &Code, module: &str) -> Change {
    let keys: BTreeSet<&String> = old.keys().chain(now.items.keys()).collect();
    let mut items = Vec::new();
    for key in keys {
        let (a, b) = (old.get(key), now.items.get(key));
        if a.map(|i| &i.hash) == b.map(|i| &i.hash) || a.or(b).is_some_and(|i| i.frame) {
            continue;
        }
        // Both versions' texts: the places near what changed; else, or a changed line near none, the item's own.
        let near = match (a, now.texts.get(key)) {
            (Some(a), Some(text)) if !a.text.is_empty() => near_changes(&a.text, text, module, is_type(key)),
            _ => None,
        };
        let own: BTreeSet<String> = a.into_iter().chain(b).flat_map(|i| i.ids.iter().cloned()).collect();
        let added_or_removed = a.is_none() || b.is_none();
        let nowhere =
            (near.as_ref().is_some_and(BTreeSet::is_empty) && !own.is_empty()) || (added_or_removed && own.is_empty());
        let ids = near.unwrap_or(own);
        let name = b.or(a).map(|i| i.name.clone()).unwrap_or_default();
        let (users, used) = if added_or_removed { (BTreeSet::new(), false) } else { users(now, key, &name) };
        items.push(Changed { key: key.clone(), ids, users, used, nowhere });
    }
    Change { items }
}

/// Whether an item's key is a type's (a struct, an enum, a union): its lines are fields, each its own.
fn is_type(key: &str) -> bool {
    key.rsplit_once(':').is_some_and(|(_, sig)| ["struct ", "enum ", "union "].iter().any(|k| sig.starts_with(k)))
}

/// The places named near what changed between two versions of an item's text: for each line one version has more of
/// than the other (as trimmed), those named on it, else on the nearest line that names some (above before below)
/// within three lines, else above, at most 12 lines up (further is another block's: a long function's or `match`'s). In a type, a field's line
/// reaches only what it and its own doc comment name: a field added or changed does nothing until code uses it, and
/// that code is a change of its own.
fn near_changes(old: &str, new: &str, module: &str, a_type: bool) -> Option<BTreeSet<String>> {
    let (a, b): (Vec<&str>, Vec<&str>) = (old.lines().collect(), new.lines().collect());
    let named = |lines: &[&str]| {
        let mut ids = BTreeSet::new();
        for line in lines {
            crate::sources::ids_in(line, module, &mut ids);
        }
        ids
    };
    let mut ids = BTreeSet::new();
    for (lines, other) in [(&a, &b), (&b, &a)] {
        let mut left: BTreeMap<&str, usize> = BTreeMap::new();
        for line in other.iter() {
            *left.entry(line.trim()).or_default() += 1;
        }
        for (i, line) in lines.iter().enumerate() {
            if let Some(n) = left.get_mut(line.trim()).filter(|n| **n > 0) {
                *n -= 1;
                continue;
            }
            if line.trim().is_empty() {
                continue;
            }
            if a_type {
                let mut first = i;
                while first > 0 && lines[first - 1].trim_start().starts_with("//") {
                    first -= 1;
                }
                ids.extend(named(&lines[first..=i]));
                continue;
            }
            // The line's own; else the nearest line naming some, above before below, within three; else above.
            let near = (0..=3usize).flat_map(|d| [i.checked_sub(d), Some(i + d).filter(|&j| d > 0 && j < lines.len())]);
            let mut found = near.flatten().map(|j| named(&lines[j..=j])).find(|s| !s.is_empty()).unwrap_or_default();
            if found.is_empty() {
                found = (i.saturating_sub(12)..i)
                    .rev()
                    .map(|j| named(&lines[j..=j]))
                    .find(|s| !s.is_empty())
                    .unwrap_or_default();
            }
            ids.extend(found);
        }
    }
    Some(ids)
}

/// The places named by the items that use `name` (a word of their text), and those that use them while they name
/// none, `USERS_DEEP` deep; and whether anything uses it.
fn users(now: &Code, key: &str, name: &str) -> (BTreeSet<String>, bool) {
    let mut seen = BTreeSet::from([key.to_owned()]);
    let mut names = vec![(key.to_owned(), name.to_owned())];
    let (mut found, mut used) = (BTreeSet::new(), false);
    // Its code, its comments left out: a name in a comment uses nothing.
    let code = |text: &str| text.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
    for _ in 0..USERS_DEEP {
        let mut next = Vec::new();
        for (of, n) in names.iter().filter(|(_, n)| !n.is_empty()) {
            // A private item is used in its own file alone.
            let file = of.split_once(':').map_or("", |(f, _)| f);
            let private =
                !now.texts.get(of).is_some_and(|t| t.lines().any(|l| !is_lead(l) && l.trim_start().starts_with("pub")));
            for (k, text) in &now.texts {
                if seen.contains(k) || (private && !k.starts_with(&format!("{file}:"))) {
                    continue;
                }
                if !crate::sources::names_word(&code(text), n) {
                    continue;
                }
                seen.insert(k.clone());
                let item = &now.items[k];
                if item.frame {
                    continue;
                }
                used = true;
                found.extend(item.ids.iter().cloned());
                if item.ids.is_empty() {
                    next.push((k.clone(), item.name.clone()));
                }
            }
        }
        if next.is_empty() {
            break;
        }
        names = next;
    }
    (found, used)
}

impl Change {
    /// Every place a session's run would have to have executed for the change to touch it, as `touched` reads them.
    fn candidates(&self) -> BTreeSet<String> {
        self.items.iter().flat_map(|c| c.ids.iter().chain(&c.users).cloned()).collect()
    }

    /// The places the change touches, given what any session ran (`ran`, of the candidates): each changed item's
    /// own where a session runs one, else its users'. None: one can't be told (nothing it names or uses runs, and
    /// something uses it, or nothing it names was ever looked for), which touches every session; with the item.
    fn touched(&self, ran: &BTreeSet<String>) -> Result<BTreeSet<String>, String> {
        let mut places = BTreeSet::new();
        for c in self.items.iter().filter(|c| !c.nowhere) {
            if c.ids.iter().any(|id| ran.contains(id)) {
                places.extend(c.ids.iter().cloned());
            } else if c.users.iter().any(|id| ran.contains(id)) {
                places.extend(c.users.iter().cloned());
            } else if c.ids.is_empty() && c.users.is_empty() && c.used {
                return Err(c.key.clone());
            }
            // Else what it names, and what uses it names, no session runs: it touches none.
        }
        Ok(places)
    }
}

/// What a change of the port's code spares: the sessions whose kept census and comparisons hold for the new code;
/// why the others can't be spared, when something can't be told; and what each changed item reaches (its places and
/// how many sessions run them), the most first.
#[derive(Default)]
pub(crate) struct Spared {
    pub sessions: BTreeSet<String>,
    pub unsure: Option<String>,
    pub reach: Vec<(String, usize, usize)>,
    /// The sessions no run of `rrc suspects` looked through (no coverage kept): what they run can't be told.
    pub unlooked: Vec<String>,
}

/// The sessions of `held` whose kept census and comparisons, made of other code than `port`'s, hold for it all the
/// same: the change since touches nothing their runs executed.
pub(crate) fn spared(repo: &Repo, held: &[Held], port: &str) -> Result<Spared, String> {
    let cache = Cache::open(&repo.local)?;
    let olds: BTreeSet<String> = held.iter().flat_map(|h| old_ports(h, port)).collect();
    if olds.is_empty() {
        return Ok(Spared::default());
    }
    let now = code(&repo.root, &repo.module());
    let mut changes = BTreeMap::new();
    let mut why = None;
    let dependencies = crate::sources::dependencies_fingerprint(&repo.root);
    for old in &olds {
        // The crates the port builds on changed (D234): what that reaches can't be told by the port's items.
        if cache.recall("dependencies", old).as_deref() != Some(dependencies.as_str()) {
            why = Some("the crates the port builds on changed".to_owned());
            continue;
        }
        match kept(&cache, old) {
            Some(items) => {
                changes.insert(old.clone(), change(&items, &now, &repo.module()));
            }
            None => why = Some("code kept before what changed could be told".to_owned()),
        }
    }
    let candidates: BTreeSet<String> = changes.values().flat_map(Change::candidates).collect();
    // What each session's run executed, of the candidates; none for a session never looked through.
    let mut runs: BTreeMap<String, Option<BTreeSet<String>>> = BTreeMap::new();
    for h in held {
        let run = crate::snapshots::run_identity(repo, &h.session)?;
        let hits = cache
            .recall("coverage", &run)
            .map(|text| text.lines().filter(|l| candidates.contains(*l)).map(str::to_owned).collect());
        runs.insert(h.name.clone(), hits);
    }
    let ran: BTreeSet<String> = runs.values().flatten().flatten().cloned().collect();
    let mut touched = BTreeMap::new();
    for (old, change) in &changes {
        match change.touched(&ran) {
            Ok(places) => {
                touched.insert(old.clone(), places);
            }
            Err(item) => why = Some(format!("{item} changed and names nothing a session runs, nor what uses it")),
        }
    }
    let mut spared = BTreeSet::new();
    for h in held {
        let olds = old_ports(h, port);
        let Some(Some(hits)) = runs.get(&h.name) else { continue };
        if !olds.is_empty() && olds.iter().all(|o| touched.get(o).is_some_and(|t| t.is_disjoint(hits))) {
            spared.insert(h.name.clone());
        }
    }
    // What each changed item reaches: for each session, the items its own kept code changed in, their places its run
    // executed; so an item changed since one session's comparison counts that session alone.
    let mut reach: BTreeMap<String, (BTreeSet<String>, BTreeSet<String>)> = BTreeMap::new();
    for h in held {
        let Some(Some(hits)) = runs.get(&h.name) else { continue };
        for change in old_ports(h, port).iter().filter_map(|old| changes.get(old)) {
            for c in change.items.iter().filter(|c| !c.nowhere) {
                let places = if c.ids.iter().any(|id| ran.contains(id)) { &c.ids } else { &c.users };
                if places.is_disjoint(hits) {
                    continue;
                }
                let entry = reach.entry(c.key.clone()).or_default();
                entry.0.extend(places.intersection(hits).cloned());
                entry.1.insert(h.name.clone());
            }
        }
    }
    let mut reach: Vec<(String, usize, usize)> =
        reach.into_iter().filter(|(_, (_, s))| !s.is_empty()).map(|(k, (p, s))| (k, p.len(), s.len())).collect();
    reach.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));
    let unlooked = runs.iter().filter(|(_, hits)| hits.is_none()).map(|(name, _)| name.clone()).collect();
    Ok(Spared { sessions: spared, unsure: why, reach, unlooked })
}

/// The other code than `port`'s that made what is kept of a session: its census's, and its comparisons' when none
/// is of `port`'s (as `Held::stale` reads them).
fn old_ports(h: &Held, port: &str) -> BTreeSet<String> {
    let mut olds = BTreeSet::new();
    if let Some(c) = h.census.as_ref().filter(|c| c.port != port) {
        olds.insert(c.port.clone());
    }
    if !h.outcomes.is_empty() && h.outcomes.values().all(|o| o.port != port) {
        olds.extend(h.outcomes.values().map(|o| o.port.clone()));
    }
    olds
}

/// Whether `name` is one of the core sessions (`[port] core`, by name, `*` and `?` as wildcards): always counted.
pub(crate) fn core(repo: &Repo, name: &str) -> bool {
    repo.settings.port.core.iter().any(|p| crate::setup::matches(p.as_bytes(), name.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code_of(files: &[(&str, &str)]) -> Code {
        let mut code = Code::default();
        for (path, text) in files {
            for (key, mut item, text) in items(path, text, "GAME.EXE") {
                item.text.clone_from(&text);
                code.texts.insert(key.clone(), text);
                code.items.insert(key, item);
            }
        }
        code
    }

    const V1: &str = "//! The view.\nuse x;\n\n/// `+0D18E`: a flat.\nfn flat() {\n    step(1);\n}\n\n\
                      /// A helper.\nfn step(n: u8) {\n    let _ = n;\n}\n\nconst SIDE: u16 = 16;\n\n\
                      /// `+0C746` draws with the side.\nfn still() -> u16 {\n    SIDE\n}\n\n\
                      #[cfg(test)]\nmod tests {\n    fn t() {}\n}\n";

    #[test]
    fn a_file_is_cut_into_items_from_their_docs_its_tests_left_out() {
        let code = code_of(&[("src/view.rs", V1)]);
        let keys: Vec<&String> = code.items.keys().collect();
        assert_eq!(
            keys,
            [
                "src/view.rs:",
                "src/view.rs:const SIDE",
                "src/view.rs:fn flat",
                "src/view.rs:fn step",
                "src/view.rs:fn still"
            ]
        );
        assert!(code.texts["src/view.rs:fn flat"].starts_with("/// `+0D18E`"), "an item starts at its docs");
        assert_eq!(code.items["src/view.rs:fn flat"].ids, BTreeSet::from(["GAME.EXE+0D18E".to_owned()]));
        assert!(code.items["src/view.rs:"].frame && code.items["src/view.rs:fn step"].ids.is_empty());
    }

    #[test]
    fn the_signatures_of_items() {
        assert_eq!(signature("    pub(crate) fn walk(&mut self) {").as_deref(), Some("fn walk"));
        assert_eq!(signature("pub const fn size() -> u8 {").as_deref(), Some("fn size"));
        assert_eq!(signature("impl Default for Level {").as_deref(), Some("impl Level"));
        assert_eq!(signature("impl<'a> Walk<'a> {").as_deref(), Some("impl Walk"));
        assert_eq!(signature("pub struct Level {").as_deref(), Some("struct Level"));
        assert_eq!(signature("    const LOCAL: u8 = 1;"), None, "a constant in a body is its item's");
        assert_eq!(signature("let fn_name = 1;"), None);
    }

    #[test]
    fn a_change_touches_what_its_items_name_or_what_uses_them() {
        let old = code_of(&[("src/view.rs", V1)]).items;
        let ran = BTreeSet::from(["GAME.EXE+0D18E".to_owned(), "GAME.EXE+0C746".to_owned()]);
        // A function naming a place: that place.
        let now = code_of(&[("src/view.rs", &V1.replace("step(1)", "step(2)"))]);
        let c = change(&old, &now, "GAME.EXE");
        assert_eq!(c.touched(&ran).unwrap(), BTreeSet::from(["GAME.EXE+0D18E".to_owned()]));
        // A helper naming none: its user's place.
        let now = code_of(&[("src/view.rs", &V1.replace("let _ = n;", "let _ = n + 1;"))]);
        assert_eq!(
            change(&old, &now, "GAME.EXE").touched(&ran).unwrap(),
            BTreeSet::from(["GAME.EXE+0D18E".to_owned()])
        );
        // A constant: its users'.
        let now = code_of(&[("src/view.rs", &V1.replace("= 16;", "= 17;"))]);
        assert_eq!(
            change(&old, &now, "GAME.EXE").touched(&ran).unwrap(),
            BTreeSet::from(["GAME.EXE+0C746".to_owned()])
        );
        // A file's head (a use, its docs) and its tests: nothing.
        let now = code_of(&[("src/view.rs", &V1.replace("use x;", "use y;").replace("fn t() {}", "fn t() { }"))]);
        assert!(change(&old, &now, "GAME.EXE").touched(&ran).unwrap().is_empty());
        // A function naming no place, used by nothing that does: every session.
        let lone = format!("{V1}\nfn main() {{\n    run();\n}}\n").replace("#[cfg(test)]", "");
        let older = code_of(&[("src/view.rs", &lone)]).items;
        let now = code_of(&[("src/view.rs", &lone.replace("run();", "run(); run();"))]);
        assert!(
            change(&older, &now, "GAME.EXE").touched(&ran).is_ok(),
            "nothing uses main: what it names is nothing run"
        );
        let used = format!("{lone}\nfn caller() {{\n    main();\n}}\n");
        let older = code_of(&[("src/view.rs", &used)]).items;
        let now = code_of(&[("src/view.rs", &used.replace("run();", "run(); run();"))]);
        assert_eq!(change(&older, &now, "GAME.EXE").touched(&ran).unwrap_err(), "src/view.rs:fn main");
    }

    #[test]
    fn an_edit_in_one_block_of_a_long_function_reaches_that_block_s_places() {
        let long = "/// `+0C0F1`: a record drawn.\nfn listed() {\n    // +0C241: the frame steps.\n    step();\n    \
                    other();\n    one();\n    two();\n    three();\n    four();\n    // +0C29E: it moves.\n    \
                    moved(1);\n}\n";
        let old = code_of(&[("src/view.rs", long)]).items;
        let ran =
            BTreeSet::from(["GAME.EXE+0C0F1".to_owned(), "GAME.EXE+0C241".to_owned(), "GAME.EXE+0C29E".to_owned()]);
        let now = code_of(&[("src/view.rs", &long.replace("moved(1)", "moved(2)"))]);
        assert_eq!(
            change(&old, &now, "GAME.EXE").touched(&ran).unwrap(),
            BTreeSet::from(["GAME.EXE+0C29E".to_owned()])
        );
        // Kept without its text (code kept before): the whole item's places.
        let bare: BTreeMap<String, Item> =
            old.into_iter().map(|(k, i)| (k, Item { text: String::new(), ..i })).collect();
        assert_eq!(change(&bare, &now, "GAME.EXE").touched(&ran).unwrap().len(), 3);
    }

    #[test]
    fn an_edit_far_from_any_place_in_a_long_function_reaches_nothing_of_its_own() {
        let mut long = String::from("/// `+0C0F1`: drawn.\nfn drawn() {\n");
        long.push_str(&"    pad();\n".repeat(20));
        long.push_str("    last(1);\n}\n");
        let old = code_of(&[("src/main.rs", &long)]).items;
        let ran = BTreeSet::from(["GAME.EXE+0C0F1".to_owned()]);
        let now = code_of(&[("src/main.rs", &long.replace("last(1)", "last(2)"))]);
        assert!(change(&old, &now, "GAME.EXE").touched(&ran).unwrap().is_empty());
    }

    #[test]
    fn a_new_file_s_items_reach_what_they_name_and_a_mod_line_nothing() {
        let main = "mod view;\n\n/// `+0C0F1`: the word's user.\nfn drawn() {\n    word(1);\n}\n";
        let old = code_of(&[("src/main.rs", main)]).items;
        let ran = BTreeSet::from(["GAME.EXE+0C0F1".to_owned(), "GAME.EXE+0E41A".to_owned()]);
        let sky = "const DS: u32 = 1;\n\nfn word() -> u32 {\n    DS\n}\n\n/// `+0E41A`: the sky.\npub fn draw() {\n    word();\n}\n";
        let main_now = main.replace("mod view;", "mod view;\nmod sky;");
        let now = code_of(&[("src/main.rs", &main_now), ("src/sky.rs", sky)]);
        assert_eq!(
            change(&old, &now, "GAME.EXE").touched(&ran).unwrap(),
            BTreeSet::from(["GAME.EXE+0E41A".to_owned()])
        );
    }

    #[test]
    fn a_field_added_to_a_type_reaches_only_what_its_own_docs_name() {
        let ty = "/// The view.\nstruct View {\n    /// `+0C0F1`'s records.\n    records: u8,\n    /// The last face's steps.\n    \
                  steps: u16,\n}\n";
        let old = code_of(&[("src/view.rs", ty)]).items;
        let ran = BTreeSet::from(["GAME.EXE+0C0F1".to_owned(), "GAME.EXE+0AFD6".to_owned()]);
        let added = ty.replace("    steps: u16,\n", "    steps: u16,\n    /// Kept between passes.\n    kept: u16,\n");
        assert!(change(&old, &code_of(&[("src/view.rs", &added)]), "GAME.EXE").touched(&ran).unwrap().is_empty());
        let named = ty.replace("/// The last face's steps.", "/// The last face's steps (`+0AFD6`).");
        assert_eq!(
            change(&old, &code_of(&[("src/view.rs", &named)]), "GAME.EXE").touched(&ran).unwrap(),
            BTreeSet::from(["GAME.EXE+0AFD6".to_owned()])
        );
    }
}
