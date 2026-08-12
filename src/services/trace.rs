//! Following one key value through the containers that carry it.
//!
//! The setup phase decided *what* to trace on; this decides *what happened*
//! to one particular value. The result is a set of lanes, each in one of four
//! states — which is the whole point of the view: an empty lane is as
//! informative as a full one, as long as you can tell "hasn't arrived" apart
//! from "was never on this path".
//!
//! A lane is a Cosmos container by default, but it doesn't have to be.
//! Querying is always per container — that is the only axis Cosmos offers —
//! and the results are regrouped afterwards, so a schema that names its own
//! stages (a workflow name, say) can drive the axis instead of the physical
//! storage layout.

use crate::services::cosmos;
use crate::services::discover::KeyCandidate;
use azure_data_cosmos::Query;
use chrono::{DateTime, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Per container. A single flow producing more than this is pathological.
const MAX_BLOCKS: usize = 200;
/// Facts shown on a block card before it stops being "essential data".
const MAX_FACTS: usize = 3;

const SYSTEM_FIELDS: [&str; 6] = ["_rid", "_self", "_etag", "_attachments", "_ts", "_lsn"];

/// What to trace, and how to read each document once found.
#[derive(Clone, Debug, PartialEq)]
pub struct TraceSpec {
    pub key: KeyCandidate,
    pub value: String,
    /// Lowercased field paths; empty string means "not chosen".
    pub time_field: String,
    pub label_field: String,
    /// Fields worth showing on a card, best first.
    pub fact_fields: Vec<String>,
}

/// A field value that means "this step failed".
///
/// What counts as an error is domain knowledge the data doesn't carry —
/// `sessionStatus: 3` means nothing without someone saying so. Rules are
/// therefore user-supplied and persisted per account, never guessed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ErrorRule {
    /// Lowercased dotted field path — what matching uses.
    pub field: String,
    /// The field's original spelling. Matching is case-insensitive, but the
    /// UI shows fields as the documents spell them everywhere else.
    #[serde(default)]
    pub display: String,
    /// Compared against the document's value as text, case-insensitively.
    pub value: String,
}

impl ErrorRule {
    /// `sessionStatus = 3`, for display.
    pub fn label(&self) -> String {
        let name = if self.display.is_empty() {
            leaf(&self.field)
        } else {
            self.display.clone()
        };
        format!("{name} = {}", self.value)
    }
}

/// Whether any rule matches — rules are OR'd, so several values of the same
/// field (or several fields) can all mean failure.
pub fn is_error(doc: &Value, rules: &[ErrorRule]) -> bool {
    rules.iter().any(|rule| {
        lookup(doc, &rule.field)
            .and_then(scalar_text)
            .is_some_and(|actual| actual.eq_ignore_ascii_case(&rule.value))
    })
}

#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    /// Stable across the sort, so a selected card stays selected.
    pub id: String,
    pub label: String,
    /// Epoch milliseconds, when a usable time field was found.
    pub at: Option<i64>,
    pub at_text: String,
    pub facts: Vec<(String, String)>,
    /// Where the document actually lives. Once lanes can be something other
    /// than containers, the card is the only thing that still knows.
    pub container: String,
    /// The document as returned, for the detail panel. A card can only ever
    /// show a summary; this is what it summarises.
    pub doc: Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaneState {
    /// The key value was found here.
    Reached,
    /// This container carries the key, but not this value — the data has not
    /// arrived (or never will).
    Awaiting,
    /// The key doesn't exist in this container at all: it isn't on this path,
    /// so its emptiness means nothing.
    OffPath,
    Failed(u8),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Lane {
    /// A container path, or a value of the chosen lane field.
    pub name: String,
    /// Secondary line: the key's field path for container lanes, the source
    /// containers for field lanes.
    pub detail: Option<String>,
    pub blocks: Vec<Block>,
    pub state: LaneState,
    pub error: Option<String>,
}

impl Lane {
    pub fn first_at(&self) -> Option<i64> {
        self.blocks.iter().filter_map(|b| b.at).min()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Trace {
    pub value: String,
    pub key_label: String,
    pub lanes: Vec<Lane>,
    /// Earliest and latest observed times, when any were found.
    pub span: Option<(i64, i64)>,
    pub blocks_found: usize,
}

impl Trace {
    pub fn reached(&self) -> usize {
        self.lanes
            .iter()
            .filter(|l| l.state == LaneState::Reached)
            .count()
    }

    pub fn awaiting(&self) -> usize {
        self.lanes
            .iter()
            .filter(|l| l.state == LaneState::Awaiting)
            .count()
    }

    /// Resolves a card selection back to its lane and document.
    pub fn find_block(&self, id: &str) -> Option<(&Lane, &Block)> {
        self.lanes.iter().find_map(|lane| {
            lane.blocks
                .iter()
                .find(|b| b.id == id)
                .map(|block| (lane, block))
        })
    }
}

/// Queries every container bound to the key, plus records the ones that
/// aren't bound at all so the view can show the difference.
pub async fn run(endpoint: &str, containers: &[String], spec: &TraceSpec) -> Trace {
    let mut lanes = Vec::new();

    // One client for every container — rebuilding it per query re-resolved
    // credentials each time and dominated the wait.
    let client = match cosmos::connect(endpoint).await {
        Ok(client) => client,
        Err(e) => {
            return Trace {
                value: spec.value.clone(),
                key_label: spec.key.label.clone(),
                blocks_found: 0,
                span: None,
                lanes: containers
                    .iter()
                    .map(|path| Lane {
                        name: path.clone(),
                        detail: None,
                        blocks: Vec::new(),
                        state: LaneState::Failed(0),
                        error: Some(e.clone()),
                    })
                    .collect(),
            }
        }
    };

    for path in containers {
        let Some((database, container)) = path.split_once('/') else {
            continue;
        };
        let Some(binding) = spec.key.binding_for(path) else {
            lanes.push(Lane {
                name: path.clone(),
                detail: None,
                blocks: Vec::new(),
                state: LaneState::OffPath,
                error: None,
            });
            continue;
        };

        let query = build_query(&binding.field, &spec.value);
        match cosmos::query_documents(&client, database, container, query, MAX_BLOCKS).await {
            Ok(docs) => {
                let mut blocks: Vec<Block> = docs
                    .iter()
                    .enumerate()
                    .map(|(i, doc)| {
                        build_block(doc, spec, path, &binding.field, &format!("{path}#{i}"))
                    })
                    .collect();
                blocks.sort_by_key(|b| b.at.unwrap_or(i64::MAX));
                let state = if blocks.is_empty() {
                    LaneState::Awaiting
                } else {
                    LaneState::Reached
                };
                lanes.push(Lane {
                    name: path.clone(),
                    detail: Some(binding.field.clone()),
                    blocks,
                    state,
                    error: None,
                });
            }
            Err(e) => lanes.push(Lane {
                name: path.clone(),
                detail: Some(binding.field.clone()),
                blocks: Vec::new(),
                state: LaneState::Failed(0),
                error: Some(e),
            }),
        }
    }

    // Always container lanes here. Choosing a different axis is a display
    // decision, applied by `relane` — it must not require re-querying.
    sort_lanes(&mut lanes);

    let times: Vec<i64> = lanes
        .iter()
        .flat_map(|l| l.blocks.iter().filter_map(|b| b.at))
        .collect();
    let span = match (times.iter().min(), times.iter().max()) {
        (Some(&lo), Some(&hi)) => Some((lo, hi)),
        _ => None,
    };

    Trace {
        value: spec.value.clone(),
        key_label: spec.key.label.clone(),
        blocks_found: lanes.iter().map(|l| l.blocks.len()).sum(),
        lanes,
        span,
    }
}

/// Chronological where we know the time, so the lanes read as the path the
/// data actually took; unreached lanes sink to the bottom.
fn sort_lanes(lanes: &mut [Lane]) {
    lanes.sort_by(|a, b| {
        let rank = |l: &Lane| match l.state {
            LaneState::Reached => 0,
            LaneState::Awaiting => 1,
            LaneState::Failed(_) => 2,
            LaneState::OffPath => 3,
        };
        rank(a)
            .cmp(&rank(b))
            .then(
                a.first_at()
                    .unwrap_or(i64::MAX)
                    .cmp(&b.first_at().unwrap_or(i64::MAX)),
            )
            .then(a.name.cmp(&b.name))
    });
}

/// Re-expresses a trace on a different axis, without touching Cosmos.
///
/// The documents are already in hand, so switching between "one lane per
/// container" and "one lane per workflow name" is a pure transformation of
/// what's on screen. `lane_field` empty gives the container lanes back
/// unchanged, so the choice is reversible.
pub fn relane(base: &Trace, lane_field: &str, expected_lanes: &[String]) -> Trace {
    if lane_field.is_empty() {
        return base.clone();
    }
    let mut lanes = regroup(base.lanes.clone(), lane_field, expected_lanes);
    sort_lanes(&mut lanes);
    Trace {
        lanes,
        ..base.clone()
    }
}

/// Rebuilds container lanes into lanes of `lane_field`'s values.
///
/// Containers whose query failed stay as their own lanes — an error must not
/// be silently folded into "nothing here".
fn regroup(container_lanes: Vec<Lane>, lane_field: &str, expected_lanes: &[String]) -> Vec<Lane> {
    let unlabelled = format!("(no {})", leaf(lane_field));
    let mut grouped: BTreeMap<String, Vec<Block>> = BTreeMap::new();
    let mut failed = Vec::new();

    for lane in container_lanes {
        if lane.state == LaneState::Failed(0) {
            failed.push(lane);
            continue;
        }
        for block in lane.blocks {
            let name = lookup(&block.doc, lane_field)
                .and_then(scalar_text)
                .unwrap_or_else(|| unlabelled.clone());
            grouped.entry(name).or_default().push(block);
        }
    }

    // Every stage we know about, whether or not this value reached it. A
    // stage the sampling never saw cannot appear here — which is exactly why
    // `expected_lanes` is worth carrying.
    let mut names: BTreeSet<&str> = expected_lanes.iter().map(String::as_str).collect();
    names.extend(grouped.keys().map(String::as_str));

    let mut lanes: Vec<Lane> = names
        .into_iter()
        .map(|name| {
            let mut blocks = grouped.get(name).cloned().unwrap_or_default();
            blocks.sort_by_key(|b| b.at.unwrap_or(i64::MAX));
            let sources: BTreeSet<&str> = blocks.iter().map(|b| b.container.as_str()).collect();
            Lane {
                name: name.to_string(),
                detail: (!sources.is_empty())
                    .then(|| sources.into_iter().collect::<Vec<_>>().join(", ")),
                state: if blocks.is_empty() {
                    LaneState::Awaiting
                } else {
                    LaneState::Reached
                },
                blocks,
                error: None,
            }
        })
        .collect();

    lanes.append(&mut failed);
    lanes
}

fn build_query(field: &str, value: &str) -> Query {
    let path = sql_path(field);
    // Cosmos is typed: a numeric id stored as a number won't match a string
    // parameter, and the user just typed characters. Try both when the input
    // could be either.
    let numeric = value.parse::<f64>().ok().filter(|n| n.is_finite());
    let text = match numeric {
        Some(_) => format!("SELECT * FROM c WHERE {path} = @v OR {path} = @n"),
        None => format!("SELECT * FROM c WHERE {path} = @v"),
    };

    let query = Query::from(text);
    let query = query.with_parameter("@v", value).unwrap_or_else(|_| {
        Query::from(format!("SELECT * FROM c WHERE {path} = null"))
    });
    match numeric {
        Some(n) => query.with_parameter("@n", n).unwrap_or_else(|_| {
            Query::from(format!("SELECT * FROM c WHERE {path} = null"))
        }),
        None => query,
    }
}

/// Renders a dotted field path as bracketed Cosmos SQL, so field names with
/// spaces, dashes or reserved words survive.
fn sql_path(path: &str) -> String {
    let mut out = String::from("c");
    for part in path.split('.') {
        out.push_str("[\"");
        out.push_str(&part.replace('\\', "\\\\").replace('"', "\\\""));
        out.push_str("\"]");
    }
    out
}

fn build_block(
    doc: &Value,
    spec: &TraceSpec,
    container_path: &str,
    key_field: &str,
    id: &str,
) -> Block {
    // The bare container name reads better on a card than `db/container`.
    let container = container_path
        .rsplit('/')
        .next()
        .unwrap_or(container_path);

    let at = if spec.time_field.is_empty() {
        None
    } else {
        lookup(doc, &spec.time_field).and_then(parse_time)
    };

    let label = lookup(doc, &spec.label_field)
        .and_then(scalar_text)
        .unwrap_or_else(|| container.to_string());

    let mut facts: Vec<(String, String)> = Vec::new();
    let mut taken: Vec<String> = vec![
        key_field.to_lowercase(),
        spec.time_field.clone(),
        spec.label_field.clone(),
    ];

    // Preferred fields first — the ones discovery already judged descriptive.
    for path in &spec.fact_fields {
        if facts.len() >= MAX_FACTS || taken.contains(path) {
            continue;
        }
        if let Some(text) = lookup(doc, path).and_then(scalar_text) {
            facts.push((leaf(path), truncate(&text, 28)));
            taken.push(path.clone());
        }
    }

    // Then whatever short scalars the document has, so a card is never blank.
    if facts.len() < MAX_FACTS {
        if let Value::Object(map) = doc {
            for (name, value) in map {
                if facts.len() >= MAX_FACTS
                    || SYSTEM_FIELDS.contains(&name.as_str())
                    || taken.contains(&name.to_lowercase())
                {
                    continue;
                }
                if let Some(text) = scalar_text(value).filter(|t| t.len() <= 28) {
                    facts.push((name.clone(), text));
                }
            }
        }
    }

    Block {
        id: id.to_string(),
        label,
        at,
        at_text: at.map(format_time).unwrap_or_default(),
        facts,
        container: container_path.to_string(),
        doc: doc.clone(),
    }
}

/// Case-insensitive lookup down a dotted path.
fn lookup<'a>(doc: &'a Value, lower_path: &str) -> Option<&'a Value> {
    if lower_path.is_empty() {
        return None;
    }
    let mut current = doc;
    for segment in lower_path.split('.') {
        let Value::Object(map) = current else {
            return None;
        };
        current = map
            .iter()
            .find(|(k, _)| k.to_lowercase() == segment)
            .map(|(_, v)| v)?;
    }
    Some(current)
}

fn scalar_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Epoch milliseconds from whatever shape the timestamp is stored in.
pub fn parse_time(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_f64().and_then(|n| epoch_to_millis(n as i64)),
        Value::String(s) => {
            if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
                return Some(dt.timestamp_millis());
            }
            for format in [
                "%Y-%m-%dT%H:%M:%S%.f",
                "%Y-%m-%d %H:%M:%S%.f",
                "%Y-%m-%dT%H:%M:%S",
                "%Y-%m-%d %H:%M:%S",
                "%Y-%m-%d",
            ] {
                if let Ok(dt) = NaiveDateTime::parse_from_str(s, format) {
                    return Some(dt.and_utc().timestamp_millis());
                }
            }
            s.parse::<i64>().ok().and_then(epoch_to_millis)
        }
        _ => None,
    }
}

/// Accepts epoch seconds or milliseconds, rejecting values outside a
/// plausible range so a random integer isn't read as a date.
fn epoch_to_millis(n: i64) -> Option<i64> {
    match n {
        1_000_000_000..=4_000_000_000 => Some(n * 1000),
        1_000_000_000_000..=4_000_000_000_000 => Some(n),
        _ => None,
    }
}

pub fn format_time(millis: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(millis)
        .map(|dt| dt.format("%m-%d %H:%M:%S%.3f").to_string())
        .unwrap_or_default()
}

/// Human-readable gap between two instants.
pub fn format_gap(millis: i64) -> String {
    let s = millis as f64 / 1000.0;
    if millis < 1000 {
        format!("+{millis}ms")
    } else if s < 60.0 {
        format!("+{s:.1}s")
    } else if s < 3600.0 {
        format!("+{:.1}min", s / 60.0)
    } else if s < 86_400.0 {
        format!("+{:.1}h", s / 3600.0)
    } else {
        format!("+{:.1}d", s / 86_400.0)
    }
}

fn leaf(path: &str) -> String {
    path.rsplit('.').next().unwrap_or(path).to_string()
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let kept: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{kept}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn field_paths_are_bracketed_for_sql() {
        assert_eq!(sql_path("correlationId"), r#"c["correlationId"]"#);
        assert_eq!(
            sql_path("properties.correlation-id"),
            r#"c["properties"]["correlation-id"]"#
        );
        assert_eq!(sql_path(r#"od"d"#), r#"c["od\"d"]"#);
    }

    #[test]
    fn nested_lookup_ignores_case() {
        let doc = json!({ "Properties": { "CorrelationId": "abc" } });
        assert_eq!(
            lookup(&doc, "properties.correlationid").and_then(scalar_text),
            Some("abc".into())
        );
        assert!(lookup(&doc, "properties.missing").is_none());
    }

    #[test]
    fn timestamps_parse_from_every_common_shape() {
        let expect = 1_767_178_800_000; // 2025-12-31T11:00:00Z
        assert_eq!(parse_time(&json!("2025-12-31T11:00:00Z")), Some(expect));
        assert_eq!(parse_time(&json!("2025-12-31T11:00:00")), Some(expect));
        assert_eq!(parse_time(&json!("2025-12-31 11:00:00")), Some(expect));
        assert_eq!(parse_time(&json!(1_767_178_800i64)), Some(expect));
        assert_eq!(parse_time(&json!(1_767_178_800_000i64)), Some(expect));
        // An ordinary small integer is not a date.
        assert_eq!(parse_time(&json!(42)), None);
        assert_eq!(parse_time(&json!("not a date")), None);
    }

    #[test]
    fn rfc3339_offsets_are_normalised_to_utc() {
        let utc = parse_time(&json!("2025-12-31T12:00:00+01:00"));
        assert_eq!(utc, parse_time(&json!("2025-12-31T11:00:00Z")));
    }

    #[test]
    fn blocks_prefer_chosen_fields_then_fill_from_the_document() {
        let spec = TraceSpec {
            key: KeyCandidate {
                id: "x".into(),
                label: "correlationId".into(),
                bindings: vec![],
                missing: vec![],
                shared_values: 0,
                cross_named: false,
                id_shaped: true,
                avg_fill: 1.0,
                avg_distinct_ratio: 1.0,
                score: 0.0,
                evidence: vec![],
            },
            value: "abc".into(),
            time_field: "occurredat".into(),
            label_field: "eventtype".into(),
            fact_fields: vec!["status".into()],
        };
        let doc = json!({
            "correlationId": "abc",
            "occurredAt": "2025-12-31T11:00:00Z",
            "eventType": "paid",
            "status": "ok",
            "amount": 42,
        });

        let block = build_block(&doc, &spec, "events", "correlationId", "events#0");
        assert_eq!(block.label, "paid");
        assert_eq!(block.at, Some(1_767_178_800_000));
        assert_eq!(block.facts[0], ("status".into(), "ok".into()));
        // The key, time and label fields are never repeated as facts.
        assert!(block
            .facts
            .iter()
            .all(|(k, _)| !["correlationId", "occurredAt", "eventType"].contains(&k.as_str())));
    }

    #[test]
    fn a_document_with_no_label_field_falls_back_to_the_container() {
        let spec = TraceSpec {
            key: KeyCandidate {
                id: "x".into(),
                label: "k".into(),
                bindings: vec![],
                missing: vec![],
                shared_values: 0,
                cross_named: false,
                id_shaped: false,
                avg_fill: 1.0,
                avg_distinct_ratio: 1.0,
                score: 0.0,
                evidence: vec![],
            },
            value: "abc".into(),
            time_field: String::new(),
            label_field: String::new(),
            fact_fields: vec![],
        };
        let block = build_block(&json!({ "k": "abc" }), &spec, "orders", "k", "orders#0");
        assert_eq!(block.label, "orders");
        assert_eq!(block.at, None);
    }

    /// Card ids are handed out before the chronological sort, so the panel
    /// must still resolve them afterwards.
    #[test]
    fn blocks_are_found_by_id_after_sorting() {
        let mut blocks: Vec<Block> = [(2, 200i64), (0, 0), (1, 100)]
            .iter()
            .map(|(i, at)| Block {
                id: format!("db/c#{i}"),
                label: format!("step {i}"),
                at: Some(*at),
                at_text: String::new(),
                facts: vec![],
                container: "db/c".into(),
                doc: json!({ "n": i }),
            })
            .collect();
        blocks.sort_by_key(|b| b.at.unwrap_or(i64::MAX));

        let trace = Trace {
            value: "abc".into(),
            key_label: "k".into(),
            blocks_found: blocks.len(),
            span: Some((0, 200)),
            lanes: vec![Lane {
                name: "db/c".into(),
                detail: Some("k".into()),
                blocks,
                state: LaneState::Reached,
                error: None,
            }],
        };

        let (lane, block) = trace.find_block("db/c#2").expect("id should resolve");
        assert_eq!(lane.name, "db/c");
        assert_eq!(block.label, "step 2");
        assert_eq!(block.doc, json!({ "n": 2 }));
        assert!(trace.find_block("db/c#9").is_none());
    }

    fn expected(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    fn doc_block(id: &str, container: &str, workflow: Option<&str>, at: i64) -> Block {
        Block {
            id: id.into(),
            label: "step".into(),
            at: Some(at),
            at_text: String::new(),
            facts: vec![],
            container: container.into(),
            doc: match workflow {
                Some(w) => json!({ "workflowName": w }),
                None => json!({}),
            },
        }
    }

    fn container_lane(name: &str, blocks: Vec<Block>) -> Lane {
        Lane {
            name: name.into(),
            detail: Some("correlationId".into()),
            state: if blocks.is_empty() {
                LaneState::Awaiting
            } else {
                LaneState::Reached
            },
            blocks,
            error: None,
        }
    }

    /// The point of the lane field: documents spread across containers
    /// collapse onto the stage that produced them.
    #[test]
    fn regrouping_replaces_containers_with_field_values() {
        let lanes = vec![
            container_lane(
                "db/events",
                vec![
                    doc_block("1", "db/events", Some("Validate"), 0),
                    doc_block("2", "db/events", Some("Invoice"), 200),
                ],
            ),
            container_lane(
                "db/audit",
                vec![doc_block("3", "db/audit", Some("Validate"), 100)],
            ),
        ];

        let out = regroup(lanes, "workflowname", &[]);
        let names: Vec<&str> = out.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, vec!["Invoice", "Validate"]);

        let validate = out.iter().find(|l| l.name == "Validate").unwrap();
        assert_eq!(validate.blocks.len(), 2);
        // Blocks from two containers, ordered by time, and the lane says where
        // they came from.
        assert_eq!(validate.blocks[0].id, "1");
        assert_eq!(validate.blocks[1].id, "3");
        assert_eq!(validate.detail.as_deref(), Some("db/audit, db/events"));
    }

    /// A stage seen while sampling but absent for this value is the whole
    /// reason `expected_lanes` is carried through.
    #[test]
    fn expected_stages_with_no_documents_show_as_awaiting() {
        let lanes = vec![container_lane(
            "db/events",
            vec![doc_block("1", "db/events", Some("Validate"), 0)],
        )];

        let out = regroup(lanes, "workflowname", &expected(&["Validate", "Invoice", "Archive"]));
        let awaiting: Vec<&str> = out
            .iter()
            .filter(|l| l.state == LaneState::Awaiting)
            .map(|l| l.name.as_str())
            .collect();
        assert_eq!(awaiting, vec!["Archive", "Invoice"]);
    }

    #[test]
    fn documents_missing_the_lane_field_get_their_own_lane() {
        let lanes = vec![container_lane(
            "db/events",
            vec![doc_block("1", "db/events", None, 0)],
        )];

        let out = regroup(lanes, "workflowname", &[]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "(no workflowname)");
        assert_eq!(out[0].state, LaneState::Reached);
    }

    /// A failed query must not be regrouped into silence — "we could not look"
    /// is not "nothing was there".
    #[test]
    fn failed_containers_survive_regrouping() {
        let lanes = vec![
            container_lane(
                "db/events",
                vec![doc_block("1", "db/events", Some("Validate"), 0)],
            ),
            Lane {
                name: "db/broken".into(),
                detail: Some("correlationId".into()),
                blocks: vec![],
                state: LaneState::Failed(0),
                error: Some("403".into()),
            },
        ];

        let out = regroup(lanes, "workflowname", &[]);
        let failed = out.iter().find(|l| l.state == LaneState::Failed(0));
        assert_eq!(failed.map(|l| l.name.as_str()), Some("db/broken"));
        assert_eq!(failed.and_then(|l| l.error.as_deref()), Some("403"));
    }

    /// The regression this pair guards: switching the axis has to change what
    /// is on screen, using only documents already fetched, and has to be
    /// reversible.
    #[test]
    fn relaning_switches_the_axis_and_back_without_requerying() {
        let base = Trace {
            value: "abc".into(),
            key_label: "correlationId".into(),
            blocks_found: 2,
            span: Some((0, 200)),
            lanes: vec![
                container_lane(
                    "db/events",
                    vec![doc_block("1", "db/events", Some("Validate"), 0)],
                ),
                container_lane(
                    "db/audit",
                    vec![doc_block("2", "db/audit", Some("Invoice"), 200)],
                ),
            ],
        };

        let by_container = relane(&base, "", &[]);
        assert_eq!(names(&by_container), vec!["db/events", "db/audit"]);

        let by_workflow = relane(&base, "workflowname", &[]);
        assert_eq!(names(&by_workflow), vec!["Validate", "Invoice"]);
        // Same documents either way — nothing was refetched or dropped.
        assert_eq!(by_workflow.blocks_found, base.blocks_found);
        assert_eq!(by_workflow.span, base.span);

        // Clearing the selection restores the containers.
        assert_eq!(relane(&by_container, "", &[]), by_container);
    }

    fn names(trace: &Trace) -> Vec<&str> {
        trace.lanes.iter().map(|l| l.name.as_str()).collect()
    }

    /// Mirrors a real account: three containers carrying the key, one off the
    /// path, and a `schema` field present in only some of the documents.
    #[test]
    fn a_real_shaped_account_regroups_onto_its_schema_field() {
        // `schema`, not the `workflowName` the other tests use — the point is
        // that the axis follows whatever field was chosen.
        let schema_block = |id: &str, container: &str, schema: Option<&str>, at: i64| Block {
            id: id.into(),
            label: "step".into(),
            at: Some(at),
            at_text: String::new(),
            facts: vec![],
            container: container.into(),
            doc: match schema {
                Some(s) => json!({ "correlationId": "c50f1a6a", "schema": s }),
                None => json!({ "correlationId": "c50f1a6a" }),
            },
        };

        let base = Trace {
            value: "c50f1a6a".into(),
            key_label: "correlationId".into(),
            blocks_found: 4,
            span: Some((0, 11_000)),
            lanes: vec![
                // Sessions documents have no `schema` field at all.
                container_lane(
                    "ais/Sessions",
                    vec![schema_block("1", "ais/Sessions", None, 0)],
                ),
                container_lane(
                    "ais/MsgItems",
                    vec![
                        schema_block("2", "ais/MsgItems", Some("ais.pivot.event"), 11_000),
                        schema_block("3", "ais/MsgItems", Some("ais.jde.invoice"), 11_000),
                    ],
                ),
                container_lane(
                    "ais/MsgTracking",
                    vec![schema_block("4", "ais/MsgTracking", Some("ais.pivot.event"), 11_000)],
                ),
                Lane {
                    name: "ais/leases".into(),
                    detail: None,
                    blocks: vec![],
                    state: LaneState::OffPath,
                    error: None,
                },
            ],
        };

        let out = relane(&base, "schema", &[]);
        let mut got = names(&out);
        got.sort_unstable();
        assert_eq!(got, vec!["(no schema)", "ais.jde.invoice", "ais.pivot.event"]);

        // The same schema value seen in two containers becomes one lane.
        let pivot = out.lanes.iter().find(|l| l.name == "ais.pivot.event").unwrap();
        assert_eq!(pivot.blocks.len(), 2);
        assert_eq!(
            pivot.detail.as_deref(),
            Some("ais/MsgItems, ais/MsgTracking")
        );

        // No container name survives as a lane.
        assert!(!names(&out).iter().any(|n| n.starts_with("ais/")));
    }

    /// The label field is read the same way, so a working `schema` label
    /// proves `schema` is also usable as the axis.
    #[test]
    fn the_lane_field_is_looked_up_exactly_like_the_label_field() {
        let doc = json!({ "correlationId": "abc", "schema": "ais.pivot.event" });
        assert_eq!(
            lookup(&doc, "schema").and_then(scalar_text),
            Some("ais.pivot.event".into())
        );
    }

    /// Containers that never carry the key are a statement about containers.
    /// Once lanes are stages, that statement has nowhere to live.
    #[test]
    fn off_path_containers_do_not_become_stages() {
        let lanes = vec![
            container_lane(
                "db/events",
                vec![doc_block("1", "db/events", Some("Validate"), 0)],
            ),
            Lane {
                name: "db/unrelated".into(),
                detail: None,
                blocks: vec![],
                state: LaneState::OffPath,
                error: None,
            },
        ];

        let out = regroup(lanes, "workflowname", &[]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "Validate");
    }

    #[test]
    fn error_rules_match_numbers_typed_as_text() {
        // The user types "3"; Cosmos stored the number 3.
        let doc = json!({ "sessionStatus": 3, "state": "Failed" });
        let rule = |f: &str, v: &str| ErrorRule {
            field: f.into(),
            display: f.into(),
            value: v.into(),
        };

        assert!(is_error(&doc, &[rule("sessionstatus", "3")]));
        assert!(!is_error(&doc, &[rule("sessionstatus", "2")]));
        // Substrings must not match — status 3 is not status 30.
        assert!(!is_error(&doc, &[rule("sessionstatus", "30")]));
        assert!(is_error(&doc, &[rule("state", "failed")]));
        assert!(!is_error(&doc, &[rule("missing", "3")]));
        assert!(!is_error(&doc, &[]));
    }

    #[test]
    fn error_rules_are_ord_and_read_nested_paths() {
        let doc = json!({ "properties": { "sessionStatus": 3 } });
        let rules = vec![
            ErrorRule {
                field: "state".into(),
                display: "state".into(),
                value: "Failed".into(),
            },
            ErrorRule {
                field: "properties.sessionstatus".into(),
                display: "sessionStatus".into(),
                value: "3".into(),
            },
        ];
        assert!(is_error(&doc, &rules));
        assert_eq!(rules[1].label(), "sessionStatus = 3");
    }

    #[test]
    fn gaps_read_in_sensible_units() {
        assert_eq!(format_gap(250), "+250ms");
        assert_eq!(format_gap(2_500), "+2.5s");
        assert_eq!(format_gap(90_000), "+1.5min");
        assert_eq!(format_gap(5_400_000), "+1.5h");
    }
}
