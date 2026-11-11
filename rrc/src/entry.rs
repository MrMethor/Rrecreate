//! A port's entry point (D155): what an agent meets first in a port repository, made by `rrc init` and `rrc hooks`.
//! `RULES.md` holds the person's rules for the port, each a question until it is answered; `AGENTS.md` (which
//! `CLAUDE.md` loads) tells an agent to run `rrc rules` and `rrc status` before anything else; `notes/` is where
//! what was learned goes; and this machine's session hook runs the two commands at a session's start and after a
//! summary, so neither depends on the agent remembering.

use std::path::Path;

use crate::say;

/// A port's own rules, from the person who runs it.
pub(crate) const RULES: &str = "RULES.md";
/// What marks a question of `RULES.md` the person hasn't answered yet.
pub(crate) const ASK: &str = "ASK:";
/// What marks a session hook as this tool's.
const HOOK_MARK: &str = "Written by rrc (D155)";
/// This machine's settings for an agent's sessions in the port, where its session hook goes.
const SESSION_SETTINGS: &str = ".claude/settings.local.json";

const RULES_TEXT: &str = "\
## Rules the person set for this port

Ask the person each question below once, and write the answer in its place (docs/porting.md, stage 0). `rrc rules`
prints this file with the playbook's rules; `rrc status` and `rrc doctor` count the questions left.

- **Scope.** ASK: which stretch of the game is this port to reproduce?
- **Sources kept out.** ASK: which outside sources stay out of this port (a source release, another project)?
- **Commits.** ASK: where does this port commit and push, and as whom?
- **Asking first.** ASK: what needs asking first? (Unless said otherwise: downloading, publishing, deleting.)
- **A second agent.** ASK: may the agent start a fresh agent for routes when it is stuck?
- The game's files are never committed, nor anything extracted from them (D33, D82).
";

const NOTES_TEXT: &str = "\
# Notes

What was learned about the game and why: one file a subject (a loop, a format, a subsystem), referred to from the
knowledge's evidence (`rrc name ID NAME --evidence \"...; notes/loops.md\"`). A route found the hard way goes here
too, as a recipe for the next agent (docs/porting.md, \"Recipes, by problem\").

Not the state of the work: `rrc status` says that, and a note that says it goes stale.
";

/// The agent's page of the port `name`.
fn agents_text(name: &str) -> String {
    format!(
        "\
# {name}: a port made with Rrecreate

Before anything else, and again whenever your context is summarised, run in this folder:

- `rrc rules`: the playbook's rules and this port's own (`RULES.md`), as they are written;
- `rrc status`: where the port stands and the next step.

Then work by the playbook, Rrecreate's `docs/porting.md`: one workflow for every port, in stages, each done when the
toolkit says so. Where the work stands is what `rrc status` says, never what a note says; `notes/` holds what was
learned about the game and why. A session of work ends with `rrc check` passing and a commit. The game's files are
never committed, nor anything extracted from them.
"
    )
}

/// Writes `text` to `path` unless the file is there already; whether it wrote it.
pub(crate) fn write_new(path: &Path, text: &str) -> Result<bool, String> {
    if path.exists() {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(true)
}

/// Writes the entry files the port `name` in `root` lacks, and says which.
pub(crate) fn write(root: &Path, name: &str) -> Result<(), String> {
    let files = [
        (RULES, RULES_TEXT.to_owned()),
        ("AGENTS.md", agents_text(name)),
        ("CLAUDE.md", "@AGENTS.md\n".to_owned()),
        ("notes/README.md", NOTES_TEXT.to_owned()),
    ];
    let mut written = Vec::new();
    for (file, text) in files {
        if write_new(&root.join(file), &text)? {
            written.push(file);
        }
    }
    if !written.is_empty() {
        say("entry", format!("{}: what an agent meets first; ask the person RULES.md's questions", written.join(", ")));
    }
    Ok(())
}

/// The questions of the port's `RULES.md` the person hasn't answered; none if the port has no such file.
pub(crate) fn unanswered(root: &Path) -> Option<Vec<String>> {
    let text = std::fs::read_to_string(root.join(RULES)).ok()?;
    Some(questions(&text))
}

/// The lines of a `RULES.md` that still ask.
fn questions(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| line.split_once(ASK).map(|(_, question)| question.trim().to_owned()))
        .filter(|question| !question.is_empty())
        .collect()
}

/// Writes this machine's session hook: `rrc rules` and `rrc status`, as this `rrc`, at a session's start and after a
/// summary; unless settings that aren't this tool's are there.
pub(crate) fn install_session_hook(root: &Path) -> Result<(), String> {
    let path = root.join(SESSION_SETTINGS);
    if std::fs::read_to_string(&path).is_ok_and(|text| !text.contains(HOOK_MARK)) {
        say("agent", format!("{} is someone else's: add rrc rules and rrc status to its SessionStart", path.display()));
        return Ok(());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?.display().to_string().replace('\\', "/");
    let command = format!("cd \"$CLAUDE_PROJECT_DIR\" && \"{exe}\" rules && \"{exe}\" status # {HOOK_MARK}");
    let settings = serde_json::json!({
        "hooks": {
            "SessionStart": [{
                "matcher": "startup|resume|clear|compact",
                "hooks": [{"type": "command", "command": command, "timeout": 60}],
            }],
        },
    });
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let text = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    std::fs::write(&path, text + "\n").map_err(|e| format!("{}: {e}", path.display()))?;
    say("agent", format!("{}: rrc rules and rrc status at a session's start", path.display()));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_port_s_rules_ask_until_the_person_answered() {
        assert_eq!(questions(RULES_TEXT).len(), 5, "the template's questions");
        let answered = "- **Scope.** The first dungeon.\n- **Commits.** ASK: where does this port commit?\n- ASK:\n";
        assert_eq!(questions(answered), ["where does this port commit?"]);
        assert!(questions("## Rules\n\n- Commit locally.\n").is_empty());
    }

    #[test]
    fn the_entry_files_are_written_once() {
        let root = std::env::temp_dir().join(format!("rrc-entry-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        write(&root, "toy").unwrap();
        assert!(std::fs::read_to_string(root.join("AGENTS.md")).unwrap().starts_with("# toy: a port"));
        assert_eq!(std::fs::read_to_string(root.join("CLAUDE.md")).unwrap(), "@AGENTS.md\n");
        assert!(root.join("notes/README.md").is_file());
        assert_eq!(unanswered(&root).map(|q| q.len()), Some(5));
        // The person's answers stay.
        std::fs::write(root.join(RULES), "- Commit locally.\n").unwrap();
        write(&root, "toy").unwrap();
        assert_eq!(unanswered(&root), Some(vec![]));
        // The session hook is this tool's to write again, and nobody else's to overwrite.
        install_session_hook(&root).unwrap();
        let hook = std::fs::read_to_string(root.join(SESSION_SETTINGS)).unwrap();
        assert!(hook.contains("rules && ") && hook.contains("status # Written by rrc"), "{hook}");
        install_session_hook(&root).unwrap();
        std::fs::write(root.join(SESSION_SETTINGS), "{}").unwrap();
        install_session_hook(&root).unwrap();
        assert_eq!(std::fs::read_to_string(root.join(SESSION_SETTINGS)).unwrap(), "{}");
        std::fs::remove_dir_all(&root).ok();
    }
}
