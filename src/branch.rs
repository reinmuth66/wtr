//! Keeps only the current branch of a conversation that was rewound (Issue #7).

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use crate::history::{History, is_prompt_record};
use crate::records::{Class, classify, record_kind};

/// The session history of the current branch. A rewind leaves the abandoned
/// records in the file and appends a new prompt whose parent is the record it
/// went back to; when a record gets a second prompt child, the earlier prompt
/// and everything under it are hidden. Other forks (parallel tool calls) are
/// left alone.
#[derive(Debug, Default)]
pub struct Conversation {
    pub history: History,
    /// Records `History` reads, in file order, kept to rebuild after a rewind.
    records: Vec<Value>,
    /// Every main-chain uuid in file order, with its parent.
    order: Vec<(String, Option<String>)>,
    /// Per parent uuid, its latest prompt child.
    last_prompt: HashMap<String, String>,
    /// Prompts a rewind abandoned.
    abandoned: HashSet<String>,
    /// Abandoned prompts and their descendants.
    hidden: HashSet<String>,
}

impl Conversation {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds one record. Returns `Some(kept)` when a rewind rebuilt the rows;
    /// the first `kept` rows are the same as before.
    pub fn push(&mut self, rec: &Value) -> Option<usize> {
        if rec.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            return None;
        }
        let uuid = rec.get("uuid").and_then(Value::as_str).map(str::to_string);
        let mut rewound = false;
        if let Some(u) = &uuid {
            // A compact boundary has no parent; it names the one before it.
            let parent = ["parentUuid", "logicalParentUuid"]
                .iter()
                .find_map(|k| rec.get(*k).and_then(Value::as_str))
                .map(str::to_string);
            if let Some(p) = &parent
                && is_prompt_record(rec)
                && let Some(old) = self.last_prompt.insert(p.clone(), u.clone())
                && &old != u
            {
                self.abandoned.insert(old);
                rewound = true;
            }
            if self.abandoned.contains(u)
                || parent.as_ref().is_some_and(|p| self.hidden.contains(p))
            {
                self.hidden.insert(u.clone());
            }
            self.order.push((u.clone(), parent));
        }
        if classify(&record_kind(rec)) == Class::Use {
            self.records.push(rec.clone());
        }
        if rewound {
            return Some(self.rebuild());
        }
        if !uuid.as_ref().is_some_and(|u| self.hidden.contains(u)) {
            self.history.push(rec);
        }
        None
    }

    /// Recomputes what is hidden and rebuilds the rows; returns how many
    /// leading rows did not change.
    fn rebuild(&mut self) -> usize {
        self.hidden.clear();
        for (u, parent) in &self.order {
            if self.abandoned.contains(u)
                || parent.as_ref().is_some_and(|p| self.hidden.contains(p))
            {
                self.hidden.insert(u.clone());
            }
        }
        let mut history = History::new();
        for rec in &self.records {
            let hidden = rec
                .get("uuid")
                .and_then(Value::as_str)
                .is_some_and(|u| self.hidden.contains(u));
            if !hidden {
                history.push(rec);
            }
        }
        let old = std::mem::replace(&mut self.history, history);
        old.rows
            .iter()
            .zip(&self.history.rows)
            .take_while(|(a, b)| a == b)
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::{Prompt, Row};
    use serde_json::json;

    fn prompt(uuid: &str, parent: Option<&str>, text: &str) -> Value {
        json!({"type": "user", "uuid": uuid, "parentUuid": parent,
               "message": {"role": "user", "content": text}})
    }

    fn tool_use(uuid: &str, parent: &str, id: &str) -> Value {
        json!({"type": "assistant", "uuid": uuid, "parentUuid": parent,
               "message": {"role": "assistant", "content": [
                   {"type": "tool_use", "id": id, "name": "Bash", "input": {"command": id}}]}})
    }

    fn tool_result(uuid: &str, parent: &str, id: &str) -> Value {
        json!({"type": "user", "uuid": uuid, "parentUuid": parent,
               "message": {"role": "user", "content": [
                   {"type": "tool_result", "tool_use_id": id, "content": "ok"}]}})
    }

    fn prompts(c: &Conversation) -> Vec<String> {
        c.history
            .rows
            .iter()
            .filter_map(|r| match r {
                Row::Prompt(p) => match &p.prompt {
                    Prompt::Text(t) => Some(t.clone()),
                    Prompt::Command { name, .. } => Some(name.clone()),
                },
                _ => None,
            })
            .collect()
    }

    fn tools(c: &Conversation) -> Vec<String> {
        c.history
            .rows
            .iter()
            .filter_map(|r| match r {
                Row::Tool(t) => Some(t.id.clone()),
                _ => None,
            })
            .collect()
    }

    fn push_all(c: &mut Conversation, recs: &[Value]) -> Option<usize> {
        let mut kept = None;
        for r in recs {
            if let Some(k) = c.push(r) {
                kept = Some(kept.map_or(k, |old: usize| old.min(k)));
            }
        }
        kept
    }

    #[test]
    fn a_rewind_hides_the_abandoned_prompt_and_what_followed() {
        let mut c = Conversation::new();
        push_all(
            &mut c,
            &[
                prompt("p1", None, "first"),
                tool_use("a1", "p1", "t1"),
                tool_result("r1", "a1", "t1"),
                prompt("p2", Some("r1"), "second"),
                tool_use("a2", "p2", "t2"),
                tool_result("r2", "a2", "t2"),
            ],
        );
        assert_eq!(prompts(&c), ["first", "second"]);
        // Rewound to before "second" and asked again.
        let kept = c.push(&prompt("p3", Some("r1"), "second, again"));
        assert_eq!(prompts(&c), ["first", "second, again"]);
        assert_eq!(tools(&c), ["t1"]);
        assert_eq!(kept, Some(2));
    }

    #[test]
    fn parallel_tool_calls_are_not_a_rewind() {
        let mut c = Conversation::new();
        let kept = push_all(
            &mut c,
            &[
                prompt("p1", None, "go"),
                tool_use("a1", "p1", "t1"),
                tool_use("a2", "a1", "t2"),
                tool_result("r1", "a1", "t1"),
                tool_result("r2", "a2", "t2"),
                prompt("p2", Some("r2"), "next"),
            ],
        );
        assert_eq!(kept, None);
        assert_eq!(tools(&c), ["t1", "t2"]);
        assert_eq!(prompts(&c), ["go", "next"]);
    }

    #[test]
    fn records_of_the_abandoned_branch_written_later_stay_hidden() {
        let mut c = Conversation::new();
        push_all(
            &mut c,
            &[
                prompt("p1", None, "first"),
                prompt("p2", Some("p1"), "old"),
                tool_use("a2", "p2", "t2"),
                prompt("p3", Some("p1"), "new"),
            ],
        );
        // The abandoned tool call finishes after the rewind.
        assert_eq!(c.push(&tool_result("r2", "a2", "t2")), None);
        assert_eq!(prompts(&c), ["first", "new"]);
        assert!(tools(&c).is_empty());
    }

    #[test]
    fn a_compaction_keeps_the_rows_before_it() {
        let mut c = Conversation::new();
        push_all(
            &mut c,
            &[
                prompt("p1", None, "before"),
                json!({"type": "system", "subtype": "compact_boundary", "uuid": "cb",
                       "parentUuid": null, "logicalParentUuid": "p1",
                       "compactMetadata": {"trigger": "manual"}}),
                json!({"type": "user", "uuid": "cs", "parentUuid": "cb", "isCompactSummary": true,
                       "message": {"role": "user", "content": "summary"}}),
                prompt("p2", Some("cs"), "after"),
                prompt("p3", Some("cs"), "after, again"),
            ],
        );
        assert_eq!(prompts(&c), ["before", "after, again"]);
        assert!(c.history.rows.iter().any(|r| matches!(r, Row::Compact(_))));
    }

    #[test]
    fn a_rewind_across_a_compaction_hides_the_compaction() {
        let mut c = Conversation::new();
        push_all(
            &mut c,
            &[
                prompt("p1", None, "first"),
                prompt("p2", Some("p1"), "old"),
                json!({"type": "system", "subtype": "compact_boundary", "uuid": "cb",
                       "parentUuid": null, "logicalParentUuid": "p2",
                       "compactMetadata": {"trigger": "auto"}}),
                prompt("p3", Some("cb"), "after"),
                prompt("p4", Some("p1"), "new"),
            ],
        );
        assert_eq!(prompts(&c), ["first", "new"]);
        assert!(!c.history.rows.iter().any(|r| matches!(r, Row::Compact(_))));
    }

    #[test]
    fn meta_and_tool_result_records_are_not_prompts() {
        let mut c = Conversation::new();
        let kept = push_all(
            &mut c,
            &[
                prompt("p1", None, "go"),
                json!({"type": "user", "uuid": "m1", "parentUuid": "p1", "isMeta": true,
                       "message": {"role": "user", "content": "reminder"}}),
                prompt("p2", Some("p1"), "real"),
            ],
        );
        assert_eq!(kept, None);
        assert_eq!(prompts(&c), ["go", "real"]);
    }
}
