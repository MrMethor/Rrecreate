//! Outside sources in the port's books (D156, D32, D77): `sources.toml` in the port repository lists what others
//! wrote about the game and the program's parts, what each covers and how far it is trusted, the ones the person
//! keeps out, and the sweeps made for them. `rrc sources` keeps the file and says who wrote about a thing, so a
//! source is met at the item it covers; a sweep's gate (docs/porting.md, stage 1) asks that each kind of source
//! was found or looked for. (The port's own code is `sources.rs`'s.)

use std::process::ExitCode;

use crate::args::Args;
use crate::repo::{Repo, read_toml, write_toml};
use crate::say;

pub const SOURCES: &str = "sources.toml";

/// The kinds a sweep looks for, each with what it is: the game's first, then the program's parts as the analysis
/// identified them.
pub const KINDS: [(&str, &str); 11] = [
    ("wiki", "the game's wikis"),
    ("formats", "documentation of its file formats"),
    ("reimplementation", "reimplementations and source ports, of this game or others on its engine"),
    ("tools", "modding tools and editors"),
    ("patches", "patch notes"),
    ("walkthrough", "walkthroughs and maps: they script sessions"),
    ("bugs", "bug lists and speedrunners' glitch lists: each entry a candidate quirk and a session to write"),
    ("library", "the compiler's runtime library"),
    ("extender", "the DOS extender"),
    ("sound", "the sound driver"),
    ("compression", "the compression"),
];
/// A kind for what is none of those.
const OTHER: &str = "other";

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Sources {
    #[serde(default, rename = "sweep", skip_serializing_if = "Vec::is_empty")]
    pub sweeps: Vec<Sweep>,
    #[serde(default, rename = "source", skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<Source>,
}

/// A search for sources: when, what was searched for, and the kinds it looked for and found none of.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Sweep {
    pub date: String,
    pub searched: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub none: Vec<String>,
    pub provenance: String,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Source {
    /// How evidence names it (`prior-art:<name>`).
    pub name: String,
    /// One of [`KINDS`], or `other`.
    pub kind: String,
    /// Where it is: an address, a book, a file the person has.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub at: String,
    /// What it covers: subsystems, formats, words to find it by.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub covers: Vec<String>,
    /// How far it is trusted, and for what.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub trust: String,
    /// Why it stays out of this port, if it does: the person's word. Listed so it isn't found again.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub excluded: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    pub provenance: String,
}

impl Sources {
    pub fn load(repo: &Repo) -> Result<Sources, String> {
        let path = repo.root.join(SOURCES);
        if path.is_file() { read_toml(&path) } else { Ok(Sources::default()) }
    }

    fn save(&self, repo: &Repo) -> Result<(), String> {
        write_toml(&repo.root.join(SOURCES), self)
    }

    /// What is wrong in the file: a kind that isn't one, a name twice, a source neither placed nor excluded.
    pub fn problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        let known = |kind: &str| kind == OTHER || KINDS.iter().any(|(k, _)| *k == kind);
        for (i, s) in self.sources.iter().enumerate() {
            if !known(&s.kind) {
                problems.push(format!("{SOURCES}: {}: no kind {:?} (rrc sources lists the kinds)", s.name, s.kind));
            }
            if self.sources[..i].iter().any(|other| other.name == s.name) {
                problems.push(format!("{SOURCES}: {} is listed twice", s.name));
            }
            if s.at.is_empty() && s.excluded.is_empty() {
                problems.push(format!("{SOURCES}: {} has no place (at) and isn't excluded", s.name));
            }
        }
        for sweep in &self.sweeps {
            for kind in sweep.none.iter().filter(|kind| !known(kind)) {
                problems.push(format!("{SOURCES}: the sweep of {}: no kind {kind:?}", sweep.date));
            }
        }
        problems
    }

    /// The kinds neither found (a source of the kind, kept or excluded) nor looked for in vain (a sweep's `none`).
    pub fn missing(&self) -> Vec<&'static str> {
        let found = |kind: &str| {
            self.sources.iter().any(|s| s.kind == kind) || self.sweeps.iter().any(|w| w.none.iter().any(|n| n == kind))
        };
        KINDS.iter().map(|(kind, _)| *kind).filter(|kind| !found(kind)).collect()
    }

    /// The sources kept that cover any of `words`: by what they cover, their name, kind or note, whatever the case.
    pub fn covering(&self, words: &[String]) -> Vec<&Source> {
        let words: Vec<String> = words.iter().map(|w| w.to_lowercase()).filter(|w| !w.is_empty()).collect();
        let has = |text: &str| {
            let text = text.to_lowercase();
            words.iter().any(|w| text.contains(w.as_str()) || (text.len() > 2 && w.contains(text.as_str())))
        };
        self.sources
            .iter()
            .filter(|s| s.excluded.is_empty())
            .filter(|s| s.covers.iter().any(|c| has(c)) || has(&s.name) || has(&s.kind) || has(&s.note))
            .collect()
    }
}

/// What the survey's gate still asks of the outside sources (docs/porting.md, stage 1): nothing of a port that takes
/// no imports, an honest test bench (D46).
pub(crate) fn open(repo: &Repo) -> Result<Vec<String>, String> {
    if !repo.settings.port.imports {
        return Ok(Vec::new());
    }
    let sources = Sources::load(repo)?;
    if sources.sweeps.is_empty() {
        return Ok(vec![
            "the outside sources aren't swept: search, rrc sources add each, then rrc sources sweep --searched TEXT"
                .to_owned(),
        ]);
    }
    let missing = sources.missing();
    if missing.is_empty() {
        return Ok(Vec::new());
    }
    Ok(vec![format!(
        "sources neither found nor looked for: {} (rrc sources add NAME --kind KIND, or rrc sources sweep --none KIND)",
        missing.join(", ")
    )])
}

/// One line about the sources that cover `words`, for a place or an item they are about; none if there are none.
pub(crate) fn about(repo: &Repo, words: &[String]) -> Option<String> {
    let sources = Sources::load(repo).ok()?;
    let covering = sources.covering(words);
    if covering.is_empty() {
        return None;
    }
    let names: Vec<String> = covering.iter().take(4).map(|s| format!("{} ({})", s.name, s.kind)).collect();
    let more = if covering.len() > 4 { format!(", {} more", covering.len() - 4) } else { String::new() };
    Some(format!("{}{more}: rrc sources {}", names.join(", "), words.first().map_or("", String::as_str)))
}

const USAGE: &str = "sources [WORD...] | sources add NAME --kind KIND [--at WHERE] [--covers A,B] [--trust TEXT] \
                     [--note TEXT] | sources exclude NAME --why TEXT [--kind KIND] | sources sweep --searched \"A; B\" \
                     [--none KIND,...] [--date YYYY-MM-DD] | sources remove NAME";

/// `rrc sources`: the outside sources, those that cover a word, or one added, excluded or removed, or a sweep
/// recorded.
pub(crate) fn sources(repo: &Repo, args: &[String]) -> Result<ExitCode, String> {
    let valued = ["--kind", "--at", "--covers", "--trust", "--note", "--why", "--searched", "--none", "--date", "--by"];
    let a = Args::parse(args, &valued, &[])?;
    let mut sources = Sources::load(repo)?;
    let list = |text: &str| -> Vec<String> {
        text.split([',', ';']).map(|part| part.trim().to_owned()).filter(|part| !part.is_empty()).collect()
    };
    match a.positional.as_slice() {
        [verb, name] if verb == "add" => {
            let kind = a.required("--kind")?.to_owned();
            let source = Source {
                name: name.clone(),
                kind,
                at: a.required("--at")?.to_owned(),
                covers: a.value("--covers").map(list).unwrap_or_default(),
                trust: a.value("--trust").unwrap_or_default().to_owned(),
                excluded: String::new(),
                note: a.value("--note").unwrap_or_default().to_owned(),
                provenance: crate::naming::provenance(&a),
            };
            place(&mut sources, source)?;
            sources.save(repo)?;
            say("source", line(sources.sources.iter().find(|s| s.name == *name).expect("added")));
        }
        [verb, name] if verb == "exclude" => {
            let source = Source {
                name: name.clone(),
                kind: a.value("--kind").unwrap_or(OTHER).to_owned(),
                excluded: a.required("--why")?.to_owned(),
                provenance: crate::naming::provenance(&a),
                ..Source::default()
            };
            place(&mut sources, source)?;
            sources.save(repo)?;
            say("excluded", format!("{name}: {}", a.required("--why")?));
        }
        [verb, name] if verb == "remove" => {
            let before = sources.sources.len();
            sources.sources.retain(|s| s.name != *name);
            if sources.sources.len() == before {
                return Err(format!("no source {name}: rrc sources lists them"));
            }
            sources.save(repo)?;
            say("removed", name);
        }
        [verb] if verb == "sweep" => {
            let sweep = Sweep {
                date: a.value("--date").map_or_else(today, str::to_owned),
                searched: a.value("--searched").map(list).unwrap_or_default(),
                none: a.value("--none").map(list).unwrap_or_default(),
                provenance: crate::naming::provenance(&a),
            };
            if sweep.searched.is_empty() {
                return Err(
                    "--searched says what was searched for (\"A; B\"): a sweep nobody can repeat isn't one".into()
                );
            }
            let text = format!("{}: {} searches", sweep.date, sweep.searched.len());
            sources.sweeps.push(sweep);
            if let Some(problem) = sources.problems().into_iter().next() {
                return Err(problem);
            }
            sources.save(repo)?;
            say("swept", text);
            show_missing(&sources);
        }
        [verb, ..] if ["add", "exclude", "remove", "sweep"].contains(&verb.as_str()) => {
            return Err(format!("usage: rrc {USAGE}"));
        }
        [] => {
            for s in &sources.sources {
                match s.excluded.is_empty() {
                    true => say("source", line(s)),
                    false => say("excluded", format!("{}: {}", s.name, s.excluded)),
                }
            }
            for sweep in &sources.sweeps {
                say("swept", format!("{}: {} searches ({})", sweep.date, sweep.searched.len(), sweep.provenance));
            }
            if sources.sweeps.is_empty() {
                say("swept", "never: search for each kind, rrc sources add what is found, then rrc sources sweep");
                for (kind, what) in KINDS {
                    say("kind", format!("{kind}: {what}"));
                }
            } else {
                show_missing(&sources);
            }
        }
        words => {
            let covering = sources.covering(words);
            for s in &covering {
                say("source", line(s));
            }
            if covering.is_empty() {
                say(
                    "none",
                    format!(
                        "no source listed covers {}: search for it, then rrc sources add NAME --kind KIND --at WHERE \
                         --covers {}",
                        words.join(", "),
                        words.join(",")
                    ),
                );
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Puts `source` in the list, in place of one of its name; an error if the list is wrong with it.
fn place(sources: &mut Sources, source: Source) -> Result<(), String> {
    if !crate::knowledge::is_name(&source.name.replace('-', "_")) {
        return Err(format!(
            "{:?} isn't a name (letters, digits, - and _): evidence names it prior-art:NAME",
            source.name
        ));
    }
    sources.sources.retain(|s| s.name != source.name);
    sources.sources.push(source);
    sources.sources.sort_by(|a, b| a.name.cmp(&b.name));
    match sources.problems().into_iter().next() {
        Some(problem) => Err(problem),
        None => Ok(()),
    }
}

/// A source in a line.
fn line(s: &Source) -> String {
    let covers = if s.covers.is_empty() { String::new() } else { format!(": covers {}", s.covers.join(", ")) };
    let trust = if s.trust.is_empty() { String::new() } else { format!("; trusted for {}", s.trust) };
    let note = if s.note.is_empty() { String::new() } else { format!("; {}", s.note) };
    format!("{} ({}){covers}{trust}{note}; at {}", s.name, s.kind, s.at)
}

fn show_missing(sources: &Sources) {
    let missing = sources.missing();
    if !missing.is_empty() {
        say("missing", format!("neither found nor looked for: {}", missing.join(", ")));
    }
}

/// Today's date as the host has it (UTC), `YYYY-MM-DD`: when a sweep was made.
fn today() -> String {
    let seconds = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_secs());
    date((seconds / 86400) as i64)
}

/// The date `days` after 1970-01-01, in the Gregorian calendar.
fn date(days: i64) -> String {
    // Howard Hinnant's days-to-civil: eras of 400 years from 0000-03-01.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 { month_index + 3 } else { month_index - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(name: &str, kind: &str, covers: &[&str]) -> Source {
        Source {
            name: name.into(),
            kind: kind.into(),
            at: "https://example.org".into(),
            covers: covers.iter().map(|c| c.to_string()).collect(),
            provenance: "agent".into(),
            ..Source::default()
        }
    }

    #[test]
    fn a_source_is_found_by_what_it_covers() {
        let mut sources = Sources::default();
        place(&mut sources, source("the-wiki", "wiki", &["spells", "items"])).unwrap();
        place(&mut sources, source("format-notes", "formats", &["IMG", "archives"])).unwrap();
        let excluded =
            Source { excluded: "the person: D69".into(), at: String::new(), ..source("old", "other", &["spells"]) };
        place(&mut sources, excluded).unwrap();
        let found = |words: &[&str]| -> Vec<String> {
            let words: Vec<String> = words.iter().map(|w| w.to_string()).collect();
            sources.covering(&words).iter().map(|s| s.name.clone()).collect()
        };
        assert_eq!(found(&["Spells"]), ["the-wiki"], "whatever the case, and never an excluded one");
        assert_eq!(found(&["spell_points"]), [] as [&str; 0]);
        assert_eq!(found(&["img_header"]), ["format-notes"], "a name that holds what it covers");
        assert_eq!(found(&["formats"]), ["format-notes"], "by its kind");
        assert!(place(&mut sources, source("x", "novel", &[])).is_err(), "a kind that isn't one");
        assert!(place(&mut sources, source("a name", "wiki", &[])).is_err());
    }

    #[test]
    fn a_sweep_s_gate_is_each_kind_found_or_looked_for() {
        let mut sources = Sources::default();
        assert_eq!(sources.missing().len(), KINDS.len());
        place(&mut sources, source("the-wiki", "wiki", &[])).unwrap();
        let none = KINDS.iter().map(|(kind, _)| kind.to_string()).filter(|kind| kind != "wiki" && kind != "sound");
        sources.sweeps.push(Sweep {
            date: "2026-11-11".into(),
            searched: vec!["the game's name and formats".into()],
            none: none.collect(),
            provenance: "agent".into(),
        });
        assert_eq!(sources.missing(), ["sound"]);
        assert!(sources.problems().is_empty());
        let text = toml::to_string_pretty(&sources).unwrap();
        assert!(text.contains("[[sweep]]") && text.contains("[[source]]"), "{text}");
        assert_eq!(toml::from_str::<Sources>(&text).unwrap(), sources);
    }

    #[test]
    fn days_are_dates() {
        assert_eq!((date(0), date(59), date(11_016)), ("1970-01-01".into(), "1970-03-01".into(), "2000-02-29".into()));
        assert_eq!(date(20_768), "2026-11-11");
    }
}
