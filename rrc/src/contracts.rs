//! Functions' register contracts from a session's run (D81): the registers each function the program
//! called read before writing them (its register parameters) and those its callers used after it
//! returned (its result, the carry flag among them), as Clockwork's contract inference saw them.
//! `rrc analyze --session` gives them to Ghidra, so the decompiled code of hand-written assembly shows
//! its register parameters and results instead of `unaff_SI` and a lost carry. A function whose callers
//! used several results (AX and the carry) returns them all, as a structure of registers (D88).
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
    /// What its callers used after it returned, each a register (`AX`, `EAX`, `SI`), `DX:AX` (a
    /// long) or a flag (`CF`, `ZF`), in [`RESULTS`]' order; none when they used nothing.
    pub outputs: Vec<String>,
}

impl Signature {
    /// As `RecExport.java` reads it: "inputs outputs", inputs joined by commas, outputs by `+` (several
    /// are a structure of registers), each `-` when there is none.
    pub fn text(&self) -> String {
        let list = |items: &[String], by: &str| if items.is_empty() { "-".to_owned() } else { items.join(by) };
        format!("{} {}", list(&self.inputs, ","), list(&self.outputs, "+"))
    }
}

/// What can be a result, in the order a structure of results has them: each with the parts the
/// contract names (a byte seen changing is taken as its word: the other byte may have kept its value
/// by chance; the high half of a 32-bit register, as the whole).
const RESULTS: [(&str, &[&str]); 10] = [
    ("EAX", &["EAX", "EAX:high"]),
    ("AX", &["AX", "AL", "AH"]),
    ("DX", &["DX", "DL", "DH", "EDX", "EDX:high"]),
    ("BX", &["BX", "BL", "BH", "EBX", "EBX:high"]),
    ("CX", &["CX", "CL", "CH", "ECX", "ECX:high"]),
    ("SI", &["SI", "ESI", "ESI:high"]),
    ("DI", &["DI", "EDI", "EDI:high"]),
    ("ES", &["ES"]),
    ("CF", &["CF"]),
    ("ZF", &["ZF"]),
];

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
    let used: Vec<String> = names("outputs").into_iter().chain(names("flags_out")).collect();
    let mut outputs: Vec<String> = RESULTS
        .iter()
        .filter(|(_, parts)| used.iter().any(|r| parts.contains(&r.as_str())))
        .map(|(result, _)| (*result).to_owned())
        .collect();
    // EAX holds AX; AX and DX alone are a long, as C returns one.
    if outputs.iter().any(|r| r == "EAX") {
        outputs.retain(|r| r != "AX");
    }
    if outputs == ["AX", "DX"] {
        outputs = vec!["DX:AX".to_owned()];
    }
    if inputs.is_empty() && outputs.is_empty() {
        return None;
    }
    Some(Signature { at: f["at"].as_str()?.to_owned(), inputs, outputs })
}

/// The signatures of the functions the session's run called: from the cache, or a run of the session
/// with contract inference (kept by the run).
pub(crate) fn signatures(repo: &Repo, session: &Session) -> Result<Vec<Signature>, String> {
    let cache = Cache::open(&repo.local)?;
    let key = crate::snapshots::run_key(repo, session, "contracts 2")?;
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
        assert_eq!(
            (s.inputs.clone(), s.outputs.clone()),
            (vec!["SI".to_owned(), "ES".to_owned()], vec!["CF".to_owned()])
        );
        assert_eq!(s.text(), "SI,ES CF");
        assert_eq!(
            signature(&contract(&["DS"], &["AL"], &[])).unwrap().text(),
            "- AX",
            "a byte seen changing is its word"
        );
        assert_eq!(signature(&contract(&[], &["DX", "AL"], &[])).unwrap().text(), "- DX:AX", "a long");
        assert_eq!(signature(&contract(&["BX"], &["BP", "FS"], &[])).unwrap().text(), "BX -");
        // Arena's test_position: its callers use AX, BX, the carry and the zero flag; none is lost.
        let both = signature(&contract(&["AX", "BX", "SI", "DS"], &["AX", "BX"], &["CF", "ZF"])).unwrap();
        assert_eq!(both.text(), "AX,BX,SI AX+BX+CF+ZF");
        let wide = signature(&contract(&["DS"], &["AL", "EAX:high", "CH"], &["CF", "DF"])).unwrap();
        assert_eq!(wide.text(), "- EAX+CX+CF", "the direction flag isn't a result");
    }

    #[test]
    fn nothing_to_say() {
        assert_eq!(signature(&contract(&["DS", "SS", "BP"], &["BP"], &[])), None);
        let mut never = contract(&["SI"], &[], &[]);
        never["calls"] = json!(0);
        assert_eq!(signature(&never), None, "it never returned");
    }
}
