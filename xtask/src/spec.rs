//! docs/ui.md's examples, so the spec runs as tests.
//!
//! Every table with an Action column needs an Example column, and every row
//! in it an example unless its Status is Planned: a script in
//! `cargo xtask simulate`'s notation, then what the light should be after,
//! starting from a torch that's just had its battery put in.
//!
//! ```text
//! | Action | Does | Example        | Status |
//! |--------|------|----------------|--------|
//! | 1C     | On   | `1C` → level 50 | Done   |
//! ```
//!
//! `cargo test` runs each against the library (`ts21::torch`), and
//! `cargo xtask size` runs each against the firmware on the simulated chip
//! (`sim.rs`).

use crate::simulate::{self, Action};

/// docs/ui.md, built in so a change to it rebuilds xtask.
pub const UI_MD: &str = include_str!("../../docs/ui.md");

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Example {
    /// The section and action, e.g. `Off: 1C`.
    pub row: String,
    pub actions: Vec<Action>,
    pub expect: Expect,
}

/// How the light should be at the end of an example.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expect {
    Off,
    /// At any level.
    On,
    Level(u8),
}

impl Expect {
    #[cfg(test)]
    pub fn holds(self, level: u8) -> bool {
        match self {
            Expect::Off => level == 0,
            Expect::On => level > 0,
            Expect::Level(want) => level == want,
        }
    }
}

/// Every example in a UI spec written like docs/ui.md.
pub fn examples(markdown: &str) -> Result<Vec<Example>, String> {
    let mut examples = Vec::new();
    let mut section = "";
    // The header of the table being read, if any.
    let mut header: Option<Vec<&str>> = None;
    for (number, line) in markdown.lines().enumerate() {
        let at = |e: String| format!("docs/ui.md:{}: {e}", number + 1);
        let Some(row) = line.trim().strip_prefix('|') else {
            header = None;
            if let Some(heading) = line.strip_prefix("## ") {
                section = heading.trim();
            }
            continue;
        };
        let cells: Vec<&str> = row
            .trim_end_matches('|')
            .split('|')
            .map(str::trim)
            .collect();
        let Some(columns) = &header else {
            if cells.contains(&"Action") && !cells.contains(&"Example") {
                return Err(at("this table of actions has no Example column".into()));
            }
            header = Some(cells);
            continue;
        };
        if cells
            .iter()
            .all(|c| c.starts_with('-') || c.starts_with(':'))
        {
            continue;
        }
        let cell = |name| {
            columns
                .iter()
                .position(|&c| c == name)
                .and_then(|i| cells.get(i).copied())
        };
        let Some(example) = cell("Example") else {
            continue;
        };
        let row = format!("{section}: {}", cell("Action").unwrap_or("?"));
        if example.is_empty() || example == "-" {
            if cell("Status") == Some("Planned") {
                continue;
            }
            return Err(at(format!("{row} has no example")));
        }
        let (actions, expect) = parse(example).map_err(|e| at(format!("{row}: {e}")))?;
        examples.push(Example {
            row,
            actions,
            expect,
        });
    }
    Ok(examples)
}

/// `` `1C 2H` → off ``, `` → on `` or `` → level 50 ``.
fn parse(example: &str) -> Result<(Vec<Action>, Expect), String> {
    let shape = || format!("expected `script` → off, on or level N, not {example:?}");
    let (script, expect) = example.split_once('→').ok_or_else(shape)?;
    let script = script
        .trim()
        .strip_prefix('`')
        .and_then(|s| s.strip_suffix('`'))
        .ok_or_else(shape)?;
    let words: Vec<&str> = script.split_whitespace().collect();
    if words.is_empty() {
        return Err(shape());
    }
    let expect = match expect.trim() {
        "off" => Expect::Off,
        "on" => Expect::On,
        other => {
            let level = other.strip_prefix("level ").ok_or_else(shape)?;
            Expect::Level(level.parse().map_err(|_| shape())?)
        }
    };
    Ok((simulate::parse(&words)?, expect))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_example_in_docs_ui_md_holds_for_the_library() {
        let examples = examples(UI_MD).unwrap();
        assert!(!examples.is_empty());
        for example in examples {
            let level = simulate::level_after(&example.actions);
            assert!(
                example.expect.holds(level),
                "{}: expected {:?}, but the library ends at level {level}",
                example.row,
                example.expect
            );
        }
    }

    const SPEC: &str = "\
## Off

| Action | Does | Example | Status |
|---|---|---|---|
| 1C | On | `1C` → level 50 | Done |
| 2C | Nothing | `2C 1H:2s` → off | Done |
| 3C | Battery | - | Planned |

## Background

| What | Does | Status |
|---|---|---|
| Sleep | Sleeps | Planned |
";

    #[test]
    fn reads_examples_from_tables_with_an_example_column() {
        let examples = examples(SPEC).unwrap();
        assert_eq!(examples.len(), 2);
        assert_eq!(examples[0].row, "Off: 1C");
        assert_eq!(examples[0].actions, [Action::Clicks(1)]);
        assert_eq!(examples[0].expect, Expect::Level(50));
        assert_eq!(examples[1].row, "Off: 2C");
        assert_eq!(examples[1].expect, Expect::Off);
    }

    #[test]
    fn refuses_a_done_row_without_an_example() {
        let spec = SPEC.replace("| 2C | Nothing | `2C 1H:2s` → off |", "| 2C | Nothing | |");
        let error = examples(&spec).unwrap_err();
        assert!(error.contains("Off: 2C has no example"), "{error}");
    }

    #[test]
    fn refuses_a_table_of_actions_without_examples() {
        let spec = "| Action | Does | Status |\n|---|---|---|\n| 1C | On | Done |\n";
        assert!(examples(spec).unwrap_err().contains("no Example column"));
    }

    #[test]
    fn refuses_examples_it_cant_read() {
        for example in [
            "1C → on",
            "`1C` → bright",
            "`1C` on",
            "`` → on",
            "`1X` → on",
            "`1C` → level fifty",
        ] {
            assert!(parse(example).is_err(), "{example:?} parsed");
        }
    }

    #[test]
    fn checks_the_light() {
        assert!(Expect::Off.holds(0) && !Expect::Off.holds(1));
        assert!(Expect::On.holds(150) && !Expect::On.holds(0));
        assert!(Expect::Level(50).holds(50) && !Expect::Level(50).holds(51));
    }
}
