//! A command's arguments: positionals, `--flag VALUE` options and `--switch`es.

use std::collections::{BTreeMap, BTreeSet};

pub struct Args {
    pub positional: Vec<String>,
    values: BTreeMap<String, String>,
    switches: BTreeSet<String>,
}

impl Args {
    /// `valued` names the options that take a value, `switches` those that don't; anything else
    /// starting with `--` is an error. After `--`, everything is positional.
    pub fn parse(args: &[String], valued: &[&str], switches: &[&str]) -> Result<Args, String> {
        let mut parsed = Args { positional: Vec::new(), values: BTreeMap::new(), switches: BTreeSet::new() };
        let mut iter = args.iter();
        while let Some(arg) = iter.next() {
            if arg == "--" {
                parsed.positional.extend(iter.by_ref().cloned());
            } else if valued.contains(&arg.as_str()) {
                let value = iter.next().ok_or_else(|| format!("{arg} needs a value"))?;
                parsed.values.insert(arg.clone(), value.clone());
            } else if switches.contains(&arg.as_str()) {
                parsed.switches.insert(arg.clone());
            } else if arg.starts_with("--") {
                return Err(format!("unknown option {arg}"));
            } else {
                parsed.positional.push(arg.clone());
            }
        }
        Ok(parsed)
    }

    pub fn value(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(String::as_str)
    }

    pub fn required(&self, name: &str) -> Result<&str, String> {
        self.value(name).ok_or_else(|| format!("{name} is needed"))
    }

    #[allow(dead_code, reason = "no command has a switch yet")]
    pub fn has(&self, name: &str) -> bool {
        self.switches.contains(name)
    }

    /// The positional arguments, which must number between `min` and `max`.
    pub fn positionals(&self, min: usize, max: usize, usage: &str) -> Result<&[String], String> {
        if (min..=max).contains(&self.positional.len()) {
            Ok(&self.positional)
        } else {
            Err(format!("usage: rrc {usage}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_switches_and_positionals() {
        let args: Vec<String> =
            ["a", "--name", "x", "--json", "b", "--", "--c"].iter().map(|s| s.to_string()).collect();
        let parsed = Args::parse(&args, &["--name"], &["--json"]).unwrap();
        assert_eq!(parsed.positional, ["a", "b", "--c"]);
        assert_eq!((parsed.value("--name"), parsed.has("--json"), parsed.has("--x")), (Some("x"), true, false));
        assert!(Args::parse(&["--what".to_string()], &[], &[]).is_err());
        assert!(Args::parse(&["--name".to_string()], &["--name"], &[]).is_err());
    }
}
