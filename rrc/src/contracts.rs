//! Functions' register contracts from a session's run (D81): the registers each function the program
//! called read before writing them (its register parameters) and those its callers used after it
//! returned (its result, the carry flag among them), as Clockwork's contract inference saw them.
//! `rrc analyze --session` gives them to Ghidra, so the decompiled code of hand-written assembly shows
//! its register parameters and results instead of `unaff_SI` and a lost carry.
//!
//! A contract is what the session's calls did: a register only some paths read is a parameter if the
//! run took one of them. The run is kept by the session's run identity, so analysing again runs nothing.

use serde_json::{Value, json};

use crate::cache::Cache;
use crate::clockwork::Clockwork;
use crate::repo::Repo;
use crate::session::Session;

/// What a contract gives Ghidra: a function's register parameters and where its result is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Signature {
    /// Its address as Clockwork names it (`+OFF`).
    pub at: String,
    /// Registers read before written (DS, SS, CS, SP and BP left out: the frame and the data segment).
    pub inputs: Vec<String>,
    /// `AX`, `DX:AX`, `EAX`, `CF` or another register; none when the callers used no result.
    pub output: Option<String>,
}

impl Signature {
    /// As `RecExport.java` reads it: "inputs output", each `-` when there is none.
    pub fn text(&self) -> String {
        let inputs = if self.inputs.is_empty() { "-".to_owned() } else { self.inputs.join(",") };
        format!("{inputs} {}", self.output.as_deref().unwrap_or("-"))
    }
}

/// Registers that can be parameters.
const PARAMETERS: [&str; 23] = [
    "AX", "AL", "AH", "EAX", "BX", "BL", "BH", "EBX", "CX", "CL", "CH", "ECX", "DX", "DL", "DH", "EDX", "SI", "ESI",
    "DI", "EDI", "ES", "FS", "GS",
];

/// The signature of a function's contract (one of `contracts_end`'s), if it has register parameters
/// or a result; none for one that never returned.
pub(crate) fn signature(f: &Value) -> Option<Signature> {
    if f["calls"].as_u64().unwrap_or(0) == 0 {
        return None;
    }
    let names = |field: &str| -> Vec<String> {
        f[field].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_owned)).collect()
    };
    let inputs: Vec<String> = names("inputs").into_iter().filter(|r| PARAMETERS.contains(&r.as_str())).collect();
    let (outputs, flags) = (names("outputs"), names("flags_out"));
    let has = |parts: &[&str]| outputs.iter().any(|r| parts.contains(&r.as_str()));
    // A byte the run saw change is taken as its word: the other byte may have kept its value by chance.
    let output = if has(&["EAX"]) {
        Some("EAX".to_owned())
    } else if has(&["AX", "AL", "AH"]) && has(&["DX", "DL", "DH"]) {
        Some("DX:AX".to_owned())
    } else if has(&["AX", "AL", "AH"]) {
        Some("AX".to_owned())
    } else if flags.iter().any(|f| f == "CF") {
        Some("CF".to_owned())
    } else {
        let words = [("BX", ["BX", "BL", "BH"]), ("CX", ["CX", "CL", "CH"]), ("DX", ["DX", "DL", "DH"])];
        words
            .iter()
            .find(|(_, parts)| has(parts))
            .map(|(word, _)| (*word).to_owned())
            .or_else(|| ["SI", "DI", "ES"].iter().find(|r| has(&[r])).map(|r| (*r).to_owned()))
    };
    if inputs.is_empty() && output.is_none() {
        return None;
    }
    Some(Signature { at: f["at"].as_str()?.to_owned(), inputs, output })
}

/// The signatures of the functions the session's run called: from the cache, or a run of the session
/// with contract inference (kept by the run and the build of `rrc`).
pub(crate) fn signatures(repo: &Repo, session: &Session) -> Result<Vec<Signature>, String> {
    let cache = Cache::open(&repo.local)?;
    let key = crate::snapshots::result_key(repo, session, &["contracts 1".to_owned()])?;
    let functions = match cache.recall("results", &key).and_then(|text| serde_json::from_str::<Value>(&text).ok()) {
        Some(kept) => kept,
        None => {
            crate::say("contracts", "the session's run with contract inference, once (minutes)");
            let (mut clockwork, _) = Clockwork::session(repo, session)?;
            clockwork.ask("contracts", json!({}))?;
            clockwork.ask("run", json!({ "ms": (session.seconds * 1000.0) as u64 }))?;
            let found = clockwork.ask("contracts_end", Value::Null)?;
            // Only what a signature needs: the memory each function touched isn't.
            let functions: Vec<Value> = found["functions"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|f| {
                    json!({"at": f["at"], "calls": f["calls"], "inputs": f["inputs"], "outputs": f["outputs"],
                           "flags_out": f["flags_out"]})
                })
                .collect();
            let functions = Value::Array(functions);
            cache.remember("results", &key, &functions.to_string())?;
            functions
        }
    };
    Ok(functions.as_array().into_iter().flatten().filter_map(signature).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contract(inputs: &[&str], outputs: &[&str], flags: &[&str]) -> Value {
        json!({"at": "+01CEE", "calls": 3, "inputs": inputs, "outputs": outputs, "flags_out": flags})
    }

    #[test]
    fn register_parameters_and_results() {
        let s = signature(&contract(&["SI", "ES", "DS"], &[], &["CF"])).unwrap();
        assert_eq!((s.inputs.clone(), s.output.as_deref()), (vec!["SI".to_owned(), "ES".to_owned()], Some("CF")));
        assert_eq!(s.text(), "SI,ES CF");
        let word = signature(&contract(&["DS"], &["AL"], &["CF"])).unwrap();
        assert_eq!(word.output.as_deref(), Some("AX"), "a byte seen changing is its word; AX over the carry");
        assert_eq!(signature(&contract(&[], &["DX", "AL"], &[])).unwrap().output.as_deref(), Some("DX:AX"));
        assert_eq!(signature(&contract(&["BX"], &["BP", "FS"], &[])).unwrap().text(), "BX -");
    }

    #[test]
    fn nothing_to_say() {
        assert_eq!(signature(&contract(&["DS", "SS", "BP"], &["BP"], &[])), None);
        let mut never = contract(&["SI"], &[], &[]);
        never["calls"] = json!(0);
        assert_eq!(signature(&never), None, "it never returned");
    }
}
