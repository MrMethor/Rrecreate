//! What the outside sources say, where the work is (D185). A source in `sources.toml` is a name and a place; what it
//! says is in its digest, `notes/sources/NAME.md`: the agent's own words on what it read there, a heading a subject and
//! a line a fact, with where in the source. The digests are searched like the code is, word by word, and the lines
//! that match are printed where the agent chooses what to do next: at a comparison's difference, on `rrc show`, when
//! an item is taken, when a route is stuck. Where none matches, the same place says to search before reading on.

use std::cell::Cell;
use std::path::{Path, PathBuf};

use crate::prior::Sources;
use crate::repo::Repo;
use crate::say;

/// Where the digests are, in the port repository.
pub const DIGESTS: &str = "notes/sources";
/// A digest has at least this many lines of its own (not headings, not comments) to count as written.
pub const WRITTEN: usize = 3;
/// A line printed at most this long.
const WIDTH: usize = 170;
/// Words too common to find anything by.
const COMMON: [&str; 40] = [
    "the", "and", "for", "with", "from", "that", "this", "its", "are", "was", "not", "but", "has", "have", "into",
    "when", "what", "which", "then", "than", "each", "one", "two", "all", "any", "port", "original", "tick", "ticks",
    "pass", "passes", "session", "field", "fields", "rrc", "who", "does", "how", "why", "where",
];

thread_local! {
    /// Digest lines this command printed at a choice (for the journal: how often a source was put in front of the
    /// agent).
    static SHOWN: Cell<u32> = const { Cell::new(0) };
}

/// The digest lines this command printed, and the count set back.
pub(crate) fn take_shown() -> u32 {
    SHOWN.take()
}

/// Where source `name`'s digest is.
pub fn path(root: &Path, name: &str) -> PathBuf {
    root.join(DIGESTS).join(format!("{name}.md"))
}

/// Whether a digest line is one of its own: not a heading, a comment or empty.
fn own(line: &str) -> bool {
    let line = line.trim();
    !line.is_empty() && !line.starts_with('#') && !line.starts_with("<!--")
}

/// How many lines of its own source `name`'s digest has (0: none written).
pub fn written(root: &Path, name: &str) -> usize {
    std::fs::read_to_string(path(root, name)).map_or(0, |text| text.lines().filter(|l| own(l)).count())
}

/// Writes source `name`'s digest's head, unless it is there: what goes in it.
pub(crate) fn start(root: &Path, name: &str) -> Result<bool, String> {
    let text = format!(
        "# {name}\n\n<!-- What this source says, in your own words: a heading a subject (## Flats), a line a fact, with \
         where in the source it is (a page, a section, a file and line). rrc searches these lines and prints those that \
         match where the work is (D185). -->\n"
    );
    crate::entry::write_new(&path(root, name), &text)
}

/// The words of a question: lowercase, split at what isn't a letter or a digit, three letters or more, not too
/// common, not an address (hex digits with a digit among them).
pub fn words<S: AsRef<str>>(texts: &[S]) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for text in texts {
        for word in text.as_ref().split(|c: char| !c.is_alphanumeric()) {
            let word = word.to_lowercase();
            let address = word.chars().all(|c| c.is_ascii_hexdigit()) && word.chars().any(|c| c.is_ascii_digit());
            let numeral = word.chars().all(|c| c.is_ascii_digit());
            if word.len() >= 3 && !address && !numeral && !COMMON.contains(&word.as_str()) && !found.contains(&word) {
                found.push(word);
            }
        }
    }
    found
}

/// A word without its ending, so that `flats` finds `flat`, `drawing` finds `draw` and `enemies` finds `enemy`.
fn stem(word: &str) -> String {
    let n = word.len();
    if n > 4
        && let Some(s) = word.strip_suffix("ies")
    {
        return format!("{s}y");
    }
    if n > 5
        && let Some(s) = word.strip_suffix("ing")
    {
        return s.to_owned();
    }
    if n > 4
        && let Some(s) = word.strip_suffix("ed")
    {
        return s.to_owned();
    }
    if n > 4
        && let Some(s) = word.strip_suffix("es")
        && (s.ends_with('s') || s.ends_with('x') || s.ends_with("ch") || s.ends_with("sh"))
    {
        return s.to_owned();
    }
    if n > 3
        && let Some(s) = word.strip_suffix('s')
        && !s.ends_with('s')
    {
        return s.to_owned();
    }
    word.to_owned()
}

/// A digest line that matches a question.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub source: String,
    pub line: usize,
    pub text: String,
    /// How many of the question's words it has, its heading's with it.
    pub score: usize,
}

/// The lines of `text` (source `source`'s digest) that have any of `words`, each with how many; a line under a
/// heading has the heading's words too, so that a fact under `## Flats` is found by `flat`.
fn search(source: &str, text: &str, words: &[String]) -> Vec<Found> {
    let stems: Vec<String> = words.iter().map(|w| stem(w)).collect();
    let mut heading = String::new();
    let mut found = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim_start().starts_with('#') {
            heading = line.trim_start_matches('#').trim().to_owned();
            continue;
        }
        if !own(line) {
            continue;
        }
        let tokens = self::words(&[line, heading.as_str()]);
        let score = stems.iter().filter(|s| tokens.iter().any(|t| stem(t) == **s)).count();
        if score > 0 {
            found.push(Found { source: source.to_owned(), line: i + 1, text: line.trim().to_owned(), score });
        }
    }
    found
}

/// The digests' lines that match `words`, best first, at most `max`: where two or more words are asked, those with
/// two or more of them while there are any.
pub fn lines(root: &Path, sources: &Sources, words: &[String], max: usize) -> Vec<Found> {
    if words.is_empty() {
        return Vec::new();
    }
    let mut found: Vec<Found> = Vec::new();
    for s in sources.sources.iter().filter(|s| s.excluded.is_empty()) {
        if let Ok(text) = std::fs::read_to_string(path(root, &s.name)) {
            found.extend(search(&s.name, &text, words));
        }
    }
    if words.len() >= 2 && found.iter().any(|f| f.score >= 2) {
        found.retain(|f| f.score >= 2);
    }
    found.sort_by(|a, b| b.score.cmp(&a.score).then(a.source.cmp(&b.source)).then(a.line.cmp(&b.line)));
    found.truncate(max);
    found
}

/// A found line as it is printed.
fn text(f: &Found) -> String {
    let mut text: String = f.text.chars().take(WIDTH).collect();
    if f.text.chars().count() > WIDTH {
        text.push('…');
    }
    format!("{}: {text} ({DIGESTS}/{}.md:{})", f.source, f.source, f.line)
}

/// Says, where the agent chooses what to do next, what the outside sources say about `about` (the words of a field,
/// a function, an item): their digests' lines that match, at most `max`; else, the sources whose tags cover it and
/// have no digest line yet, or that none does and a search comes before reading on (where `ask`: a choice of route,
/// not a look at a name). Nothing for a port that reads no outside source (D46), or that has no sweep yet (`rrc
/// status` asks for one). How many lines it printed.
pub(crate) fn say_about<S: AsRef<str>>(repo: &Repo, about: &[S], max: usize, ask: bool) -> usize {
    let Ok(sources) = Sources::load(repo) else { return 0 };
    if !sources.none.is_empty() || sources.sweeps.is_empty() {
        return 0;
    }
    let words = words(about);
    if words.is_empty() {
        return 0;
    }
    let found = lines(&repo.root, &sources, &words, max);
    for f in &found {
        say("source", text(f));
    }
    if !found.is_empty() {
        SHOWN.set(SHOWN.get() + found.len() as u32);
        return found.len();
    }
    if !ask {
        return 0;
    }
    let tagged: Vec<String> = sources.covering(&words).iter().map(|s| s.name.clone()).collect();
    if !tagged.is_empty() {
        say(
            "sources",
            format!(
                "{} cover {} by their tags, and no digest line says anything of it: read them on it first, and write \
                 what they say in {DIGESTS}/NAME.md",
                tagged.join(", "),
                words.join(" ")
            ),
        );
    } else {
        let title =
            if repo.settings.game.title.is_empty() { &repo.settings.game.name } else { &repo.settings.game.title };
        say(
            "search",
            format!(
                "no source says anything of {}: search for it (\"{title} {}\") before reading on; write what is found \
                 in {DIGESTS}/NAME.md (rrc sources add NAME, if it is a new source)",
                words.join(" "),
                words.iter().take(3).cloned().collect::<Vec<_>>().join(" ")
            ),
        );
    }
    0
}

/// The kept sources whose digest isn't written (fewer than [`WRITTEN`] lines of its own).
pub fn unwritten(root: &Path, sources: &Sources) -> Vec<String> {
    sources
        .sources
        .iter()
        .filter(|s| s.excluded.is_empty() && written(root, &s.name) < WRITTEN)
        .map(|s| s.name.clone())
        .collect()
}

/// How many lines of the port's knowledge, quirks and notes (not the digests) cite an outside source: `prior-art:`, or
/// a kept source's name.
pub fn citations(root: &Path, sources: &Sources) -> usize {
    let names: Vec<&str> = sources.sources.iter().filter(|s| s.excluded.is_empty()).map(|s| s.name.as_str()).collect();
    let cites = |line: &str| line.contains("prior-art:") || names.iter().any(|n| line.contains(n));
    let mut files = Vec::new();
    for dir in ["knowledge", "notes"] {
        let Ok(entries) = std::fs::read_dir(root.join(dir)) else { continue };
        files.extend(entries.filter_map(Result::ok).map(|e| e.path()).filter(|p| p.is_file()));
    }
    files.push(root.join(crate::quirks::QUIRKS));
    files
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .map(|text| text.lines().filter(|l| cites(l)).count())
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_question_s_words_leave_out_the_common_and_the_addresses() {
        assert_eq!(words(&["a flat in the floor's walk (+0CFBF)"]), ["flat", "floor", "walk"]);
        assert_eq!(words(&["enemies[1].hits", "update"]), ["enemies", "hits", "update"]);
        assert_eq!(words(&["the port's field at tick 194"]), [] as [&str; 0]);
        let stems: Vec<String> = ["flats", "drawing", "enemies", "bus", "notes", "classes", "class"].map(stem).to_vec();
        assert_eq!(stems, ["flat", "draw", "enemy", "bus", "note", "class", "class"]);
    }

    #[test]
    fn a_digest_s_lines_are_found_by_their_words_and_their_heading_s() {
        let digest = "# OpenTESArena\n\n<!-- what it says -->\n## Flats\nThey animate by their own frame count (src/Entities/EntityAnimation.cpp).\n\
                      A flat in the floor is drawn after the walls.\n\n## Doors\nA door opens in 64 steps.\n";
        let found = search("OpenTESArena", digest, &words(&["flat", "floor"]));
        assert_eq!(found.iter().map(|f| (f.line, f.score)).collect::<Vec<_>>(), [(5, 1), (6, 2)]);
        assert!(search("x", digest, &words(&["comment"])).is_empty(), "comments aren't searched");
        let line = Found { source: "OpenTESArena".into(), line: 6, text: "A flat".into(), score: 2 };
        assert_eq!(text(&line), "OpenTESArena: A flat (notes/sources/OpenTESArena.md:6)");
        assert_eq!(digest.lines().filter(|l| own(l)).count(), 3);
    }
}
