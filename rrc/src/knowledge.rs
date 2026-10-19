//! The knowledge files (DESIGN.md §9, `docs/formats.md`).

/// Named functions.
pub const FUNCTIONS: &str = "functions.tsv";
pub const FUNCTIONS_HEADER: &str = "# id\tname\tprovenance\tconfidence\tevidence";
/// Named data.
pub const DATA: &str = "data.tsv";
pub const DATA_HEADER: &str = "# id\tname\tsize\ttype\tprovenance\tconfidence\tevidence";
/// Structures and enumerations.
pub const TYPES: &str = "types.toml";
/// Sync points and boundary values.
pub const TICKS: &str = "ticks.toml";
