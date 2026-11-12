//! The work list and the card (D158; docs/porting.md, "The work loop"). The list is every difference the censuses of
//! the scope's sessions count (`rrc compare --lockstep`, D124), merged and ranked: the mapped fields first, then the
//! paths noted as not ported, then the frames, then the sound (a layer stands on the one below), the most passes
//! first. `rrc work --run` counts again the sessions whose census the port's code as it is now didn't make, and
//! compares again those nothing compared since. A card is
//! the item taken: its question, the route chosen, one other considered, and after how many comparisons the route
//! changes; the journal counts against it, so the question "is there a better tool" is asked before the work and
//! answered again when the budget is spent.

use std::collections::BTreeMap;
use std::process::ExitCode;

use serde::{Deserialize, Serialize};

use crate::args::Args;
use crate::compare::Seen;
use crate::journal::Entry;
use crate::repo::Repo;
use crate::say;
use crate::stage::Standing;

/// Items listed unless `--lines` says.
const SHOWN: usize = 10;
/// Comparisons a route gets unless the card says.
const BUDGET: usize = 6;
const CARD: &str = "card.json";
const USAGE: &str = "work [--run] [--lines N] | work take ITEM --question TEXT --route TEXT --other TEXT [--budget N] | \
                     work done --bugs none|NAME,... | work drop";
/// What closing an item asks (D160).
const BUGS: &str = "the first rule: every bug of the original is reproduced and has a switch. Which did this item \
                    meet? rrc work done --bugs none, or --bugs NAME,... (each registered: rrc quirk NAME --at ID --what \
                    TEXT --fix TEXT)";

/// A thing the censuses count: its layer (0 fields, 1 notes, 2 frames, 3 sound), what kind of line it is, its name,
/// in how many passes and sessions, and where first.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Item {
    layer: u8,
    kind: &'static str,
    key: String,
    passes: u64,
    sessions: usize,
    first: Seen,
}

impl Item {
    /// What `rrc work take` names it by.
    fn name(&self) -> &str {
        if self.key.is_empty() { self.kind } else { &self.key }
    }

    fn line(&self) -> (String, String) {
        let s = if self.sessions == 1 { String::new() } else { format!(" of {} sessions", self.sessions) };
        let what = if self.first.what.is_empty() { String::new() } else { format!(" ({})", self.first.what) };
        let at = format!("first at {}'s tick {}", self.first.session, self.first.tick);
        let text = match self.kind {
            "screen" => format!("the port's frame differs at {} ticks{s}, {at}{what}", self.passes),
            "sound" => format!("the port's sound differs at {} ticks{s}, {at}{what}", self.passes),
            "unset" => format!("{}: the port leaves it unset, in {} passes{s}, {at}", self.key, self.passes),
            "note" => format!("{}: not ported, taken in {} passes{s}, {at}", self.key, self.passes),
            _ => format!("{}: in {} passes{s}, {at}{what}", self.key, self.passes),
        };
        (self.kind.to_owned(), text)
    }
}

/// The work list: what the scope's censuses count, merged over the sessions and ranked.
pub(crate) fn list(standing: &Standing) -> Vec<Item> {
    let mut items: BTreeMap<(u8, &'static str, String), Item> = BTreeMap::new();
    let mut add = |layer: u8, kind: &'static str, key: &str, seen: &Seen| {
        let item = items.entry((layer, kind, key.to_owned())).or_insert_with(|| Item {
            layer,
            kind,
            key: key.to_owned(),
            passes: 0,
            sessions: 0,
            first: seen.clone(),
        });
        item.passes += seen.ticks;
        item.sessions += 1;
    };
    for h in &standing.held {
        let Some(c) = &h.census else { continue };
        for (key, seen) in &c.fields {
            add(0, "differs", key, seen);
        }
        for (key, seen) in &c.unset {
            add(0, "unset", key, seen);
        }
        for (key, seen) in &c.notes {
            add(1, "note", key, seen);
        }
        if let Some(seen) = &c.screen {
            add(2, "screen", "", seen);
        }
        if let Some(seen) = &c.sound {
            add(3, "sound", "", seen);
        }
    }
    let mut items: Vec<Item> = items.into_values().collect();
    items.sort_by(|a, b| {
        (a.layer, std::cmp::Reverse(a.passes), &a.key).cmp(&(b.layer, std::cmp::Reverse(b.passes), &b.key))
    });
    items
}

/// The item taken: the question, two routes and a budget (docs/porting.md, the work loop's step 2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Card {
    item: String,
    question: String,
    route: String,
    other: String,
    /// Comparisons after which the route changes.
    budget: usize,
    /// The journal's length when it was taken: what was run for it comes after.
    taken: usize,
    /// When, in the host's seconds.
    t: u64,
}

impl Card {
    fn path(repo: &Repo) -> Result<std::path::PathBuf, String> {
        let dir = repo.state()?.join("work");
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        Ok(dir.join(CARD))
    }

    fn load(repo: &Repo) -> Result<Option<Card>, String> {
        let path = Card::path(repo)?;
        let Ok(text) = std::fs::read_to_string(&path) else { return Ok(None) };
        serde_json::from_str(&text).map(Some).map_err(|e| format!("{}: {e}", path.display()))
    }

    fn save(&self, repo: &Repo) -> Result<(), String> {
        let path = Card::path(repo)?;
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))
    }

    fn drop(repo: &Repo) -> Result<(), String> {
        let path = Card::path(repo)?;
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// The commands run for it: the journal's since it was taken.
    fn since<'a>(&self, journal: &'a [Entry]) -> &'a [Entry] {
        &journal[self.taken.min(journal.len())..]
    }

    fn compared(&self, journal: &[Entry]) -> usize {
        self.since(journal).iter().filter(|e| e.cmd == "compare").count()
    }

    fn say(&self, journal: &[Entry]) {
        say("card", format!("{}: {}", self.item, self.question));
        say("route", format!("{}; the other considered: {}", self.route, self.other));
        let spent = self.compared(journal);
        say("spent", format!("{spent} of {} comparisons; {}", self.budget, crate::journal::mix(self.since(journal))));
        if spent >= self.budget {
            self.say_spent();
        }
    }

    fn say_spent(&self) {
        say(
            "budget",
            format!(
                "spent: this route was to change after {} comparisons. The other route: {}. Or fresh eyes \
                 (docs/porting.md, \"When stuck\"); then rrc work take again with the new route",
                self.budget, self.other
            ),
        );
    }
}

/// After a comparison: says when the card's budget is spent with it (the journal `before` it hasn't it yet).
pub(crate) fn compared(repo: &Repo, before: &[Entry]) {
    if let Ok(Some(card)) = Card::load(repo)
        && card.compared(before) + 1 == card.budget
    {
        card.say_spent();
    }
}

/// For `rrc status`: the card that is open, with what was spent on it.
pub(crate) fn standing(repo: &Repo) -> Result<(), String> {
    if let Some(card) = Card::load(repo)? {
        card.say(&crate::journal::load(repo));
    }
    Ok(())
}

/// The first `shown` items of the list, each a line; then how many more.
pub(crate) fn show(items: &[Item], shown: usize) {
    for item in items.iter().take(shown) {
        let (kind, text) = item.line();
        say(&kind, text);
    }
    if items.len() > shown {
        say("more", format!("{} more items (rrc work --lines N)", items.len() - shown));
    }
}

/// `rrc work`: the list, the censuses counted again, or the card taken, closed or dropped.
pub(crate) fn work(repo: &Repo, args: &[String]) -> Result<ExitCode, String> {
    let a = Args::parse(args, &["--lines", "--question", "--route", "--other", "--budget", "--bugs"], &["--run"])?;
    match a.positionals(0, 2, USAGE)? {
        [] => {}
        [verb, item] if verb == "take" => return take(repo, &a, item),
        [verb] if verb == "done" => return done(repo, &a),
        [verb] if verb == "drop" => {
            Card::drop(repo)?;
            say("dropped", "the card: nothing is taken");
            return Ok(ExitCode::SUCCESS);
        }
        _ => return Err(format!("usage: rrc {USAGE}")),
    }
    let shown: usize = a.value("--lines").map_or(Ok(SHOWN), str::parse).map_err(|_| "--lines is a count")?;
    let mut standing = crate::stage::standing(repo)?;
    if a.has("--run") {
        // Each session of the scope whose census isn't of its recording and the port's code as they are now is
        // counted again.
        let port = crate::sources::fingerprint(&repo.root);
        let again: Vec<String> = standing
            .held
            .iter()
            .filter(|h| h.unrecorded.is_none() && h.census.as_ref().is_none_or(|c| c.port != port))
            .map(|h| h.name.clone())
            .collect();
        for name in &again {
            let census = crate::quietly(|| crate::compare::lockstep(repo, name))?;
            census.save(&repo.state()?.join("compare").join(name).join("census.json"))?;
            let things = census.fields.len() + census.unset.len();
            let seen = [census.screen.is_some().then_some("frames"), census.sound.is_some().then_some("sound")];
            let seen: Vec<&str> = seen.into_iter().flatten().collect();
            let more = if seen.is_empty() { String::new() } else { format!(", {} differ", seen.join(" and ")) };
            say(
                "counted",
                format!("{name}: {} passes, {things} fields{more}, {} notes", census.passes, census.notes.len()),
            );
        }
        // And compared again, at the layer the port is at (the fields alone before stage 6: a frame that differs
        // would stop a whole comparison short of the fields after it): each session nothing compared as its
        // recording and the port's code are now, from the port's own start, or from a transplant one that starts at
        // another's tick.
        let fields_only = standing.stage() < 6;
        let compare: Vec<(String, bool)> = standing
            .held
            .iter()
            .filter(|h| h.unrecorded.is_none() && !h.outcomes.values().any(|o| o.port == port))
            .map(|h| (h.name.clone(), h.starts_at_power_on()))
            .collect();
        for (name, from_start) in &compare {
            let mut args = vec![name.clone()];
            if fields_only {
                args.push("--fields".to_owned());
            }
            if !from_start {
                args.extend(["--transplant".to_owned(), "1".to_owned()]);
            }
            crate::quietly(|| crate::compare::compare(repo, &args))?;
            let kept = crate::progress::Outcome::kept(repo, name)?;
            let found = kept.values().find(|o| o.port == port).map(crate::progress::outcome_text);
            say("compared", format!("{name}: {}", found.unwrap_or_else(|| "nothing kept of it".to_owned())));
        }
        if again.is_empty() && compare.is_empty() {
            say("counted", "nothing: every census and comparison is of its recording and the port's code as they are");
        }
        standing = crate::stage::standing(repo)?;
    }
    let items = list(&standing);
    let counted = standing.held.iter().filter(|h| h.census.is_some()).count();
    say(
        "work",
        format!("{} items in the censuses of {counted} of {} sessions in scope", items.len(), standing.held.len()),
    );
    show(&items, shown);
    if counted < standing.held.len() || !standing.stale.is_empty() {
        say(
            "uncounted",
            format!(
                "{} sessions without a census of their recording, {} with a census or a comparison made before the \
                 port's code changed: rrc work --run",
                standing.held.len() - counted,
                standing.stale.len()
            ),
        );
    }
    let open = crate::quirks::open(repo)?;
    if let Some(first) = open.first() {
        say("check", format!("rrc check refuses {} things; the first: {first}", open.len()));
    }
    match (Card::load(repo)?, items.first()) {
        (Some(card), _) => card.say(&crate::journal::load(repo)),
        (None, Some(top)) => say(
            "next",
            format!(
                "rrc work take {:?} --question TEXT --route TEXT --other TEXT: the top item, with the route chosen and \
                 one other considered",
                top.name()
            ),
        ),
        (None, None) => {}
    }
    Ok(ExitCode::SUCCESS)
}

/// `rrc work take ITEM`: the card written.
fn take(repo: &Repo, a: &Args, item: &str) -> Result<ExitCode, String> {
    let needs = "a card says the question, the route chosen and one other considered: --question TEXT --route TEXT \
                 --other TEXT";
    let text = |flag: &str| a.value(flag).map(str::trim).filter(|t| !t.is_empty()).ok_or(needs);
    let (question, route, other) = (text("--question")?, text("--route")?, text("--other")?);
    if route == other {
        return Err("the other route is another one: a second way to answer the question".into());
    }
    let budget: usize = a.value("--budget").map_or(Ok(BUDGET), str::parse).map_err(|_| "--budget is a count")?;
    if budget == 0 {
        return Err("--budget is how many comparisons the route gets: at least 1".into());
    }
    if let Some(open) = Card::load(repo)? {
        say("replaces", format!("the card on {} (rrc work done closes one)", open.item));
    }
    let journal = crate::journal::load(repo);
    let t = journal.last().map_or(0, |e| e.t);
    // The journal gets this command after it ran: what is run for the card comes after that.
    let card = Card {
        item: item.to_owned(),
        question: question.to_owned(),
        route: route.to_owned(),
        other: other.to_owned(),
        budget,
        taken: journal.len() + 1,
        t,
    };
    card.save(repo)?;
    say("card", format!("{}: {}", card.item, card.question));
    say("route", format!("{}; the other considered: {}", card.route, card.other));
    say("budget", format!("{budget} comparisons, then the route changes"));
    // What is known where the item is, so the plan starts from it: the sources that cover it.
    let words: Vec<String> =
        item.split(|c: char| !c.is_alphanumeric()).filter(|w| w.len() >= 4).map(str::to_owned).collect();
    if let Some(sources) = crate::prior::about(repo, &words) {
        say("sources", sources);
    }
    Ok(ExitCode::SUCCESS)
}

/// `rrc work done`: the card closed, with whether its item still stands in the censuses and what it took.
fn done(repo: &Repo, a: &Args) -> Result<ExitCode, String> {
    let Some(card) = Card::load(repo)? else {
        return Err("no card is open: rrc work take ITEM ...".into());
    };
    // The bugs of the original the item met (D160): said at every close, each one registered.
    let bugs = a.value("--bugs").map(str::trim).filter(|bugs| !bugs.is_empty()).ok_or(BUGS)?;
    let bugs: Vec<&str> = match bugs {
        "none" => Vec::new(),
        names => names.split(',').map(str::trim).filter(|name| !name.is_empty()).collect(),
    };
    let register = crate::quirks::Quirks::load(repo)?;
    if let Some(unknown) = bugs.iter().find(|name| !register.quirks.iter().any(|q| q.name == **name)) {
        return Err(format!(
            "no quirk {unknown}: register it first (rrc quirk {unknown} --at ID --what TEXT --fix TEXT), then close"
        ));
    }
    let standing = crate::stage::standing(repo)?;
    let left = list(&standing).into_iter().find(|i| i.name() == card.item);
    match &left {
        Some(item) => {
            say("still", format!("in the censuses: {}", item.line().1));
            say("still", "counted again? rrc work --run; a card closed with its item standing says so in the journal");
        }
        None => say("closed", format!("{}: not in the censuses as they stand", card.item)),
    }
    let journal = crate::journal::load(repo);
    let since = card.since(&journal);
    let minutes = journal.last().map_or(0, |e| e.t.saturating_sub(card.t)) / 60;
    let took = format!(
        "{}: {} commands over {} ({}){}; bugs of the original met: {}",
        card.item,
        since.len(),
        crate::journal::span(minutes),
        crate::journal::mix(since),
        if left.is_some() { ", its item still standing" } else { "" },
        if bugs.is_empty() { "none".to_owned() } else { bugs.join(", ") }
    );
    say("took", &took);
    say("recipe", "a route found the hard way? Write it down before going on (docs/porting.md, \"Recipes\"; notes/)");
    crate::journal::ended(&card.item, took);
    Card::drop(repo)?;
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compare::Census;
    use crate::stage::Held;

    fn seen(session: &str, tick: u64, ticks: u64) -> Seen {
        Seen { ticks, session: session.into(), tick, what: String::new() }
    }

    fn held(name: &str, census: Census) -> Held {
        Held {
            name: name.into(),
            session: crate::session::Session::default(),
            unrecorded: None,
            census: Some(census),
            outcomes: BTreeMap::new(),
        }
    }

    #[test]
    fn the_list_is_the_censuses_merged_fields_first_the_most_passes_first() {
        let mut a = Census::default();
        a.fields.insert("player.x".into(), seen("a", 10, 3));
        a.fields.insert("score".into(), seen("a", 4, 9));
        a.notes.insert("a door not opened (G+00100)".into(), seen("a", 2, 50));
        a.screen = Some(seen("a", 2, 40));
        let mut b = Census::default();
        b.fields.insert("player.x".into(), seen("b", 7, 8));
        b.unset.insert("map".into(), seen("b", 1, 30));
        b.sound = Some(seen("b", 5, 2));
        let standing = Standing { lacks: Vec::new(), held: vec![held("a", a), held("b", b)], stale: Vec::new() };
        let items = list(&standing);
        let names: Vec<&str> = items.iter().map(Item::name).collect();
        assert_eq!(names, ["map", "player.x", "score", "a door not opened (G+00100)", "screen", "sound"]);
        assert_eq!((items[1].passes, items[1].sessions, items[1].first.session.as_str()), (11, 2, "a"));
        assert_eq!(items[1].line().1, "player.x: in 11 passes of 2 sessions, first at a's tick 10");
        assert_eq!(
            items[0].line(),
            ("unset".into(), "map: the port leaves it unset, in 30 passes, first at b's tick 1".into())
        );
        assert_eq!(items[4].line().1, "the port's frame differs at 40 ticks, first at a's tick 2");
    }

    #[test]
    fn a_card_counts_the_comparisons_made_for_it() {
        let ran = |cmd: &str| Entry { cmd: cmd.into(), ok: true, ..Entry::default() };
        let journal = [ran("compare"), ran("work"), ran("compare"), ran("show"), ran("compare")];
        let card = Card {
            item: "score".into(),
            question: "who adds to it?".into(),
            route: "the writer's code".into(),
            other: "a watch over the session".into(),
            budget: 2,
            taken: 2,
            t: 0,
        };
        assert_eq!((card.since(&journal).len(), card.compared(&journal)), (3, 2));
        let late = Card { taken: 9, ..card };
        assert_eq!(late.compared(&journal), 0, "a journal shorter than when it was taken");
    }
}
