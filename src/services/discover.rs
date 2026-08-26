//! Domain-agnostic discovery of what makes a set of containers traceable.
//!
//! Nothing here knows about any particular schema. Given the sampled
//! documents, it works out three things a trace view needs:
//!
//!   * **a correlation key** — the field whose value ties the steps of one
//!     flow together,
//!   * **a time field** — what orders those steps,
//!   * **a step label** — what to call each step.
//!
//! The key is the hard one, and it is decided from the data rather than from
//! field names: fields in different containers that carry *the same values*
//! are treated as the same identifier even when they're spelled differently
//! (`orderId` here, `entityId` there). Names are used only as a weak
//! tie-breaker, so an unfamiliar schema ranks on evidence alone.

use crate::services::cosmos::ContainerSchema;
use std::collections::{BTreeMap, BTreeSet};

/// Cosmos-injected metadata. `_ts` is excluded here but offered separately as
/// a time field, where it is genuinely useful.
const SYSTEM_FIELDS: [&str; 6] = ["_rid", "_self", "_etag", "_attachments", "_ts", "_lsn"];

/// How many values two fields must share before they're taken to be the same
/// identifier. Two is enough to be well past coincidence for long values.
const LINK_MIN_SHARED: usize = 2;
/// Values shorter than this are too collision-prone to link on — `"1"` and
/// `"ok"` appear everywhere and mean nothing.
const LINK_MIN_LEN: usize = 8;

/// One container's participation in a key: which field carries it there.
#[derive(Clone, Debug, PartialEq)]
pub struct Binding {
    pub container: String,
    pub field: String,
    pub seen_in: usize,
    pub sampled_docs: usize,
    pub distinct: usize,
}

/// A reason to trust — or distrust — a candidate, shown to the user so the
/// ranking is arguable rather than magic.
#[derive(Clone, Debug, PartialEq)]
pub struct Evidence {
    pub good: bool,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct KeyCandidate {
    /// Stable identity of the group across rescans.
    pub id: String,
    pub label: String,
    pub bindings: Vec<Binding>,
    /// Containers where nothing in this group was sampled.
    pub missing: Vec<String>,
    /// Distinct values seen in more than one container — direct proof the
    /// field links documents rather than just existing in several places.
    pub shared_values: usize,
    /// The group spans more than one spelling, so only the values connect it.
    pub cross_named: bool,
    pub id_shaped: bool,
    pub avg_fill: f32,
    pub avg_distinct_ratio: f32,
    pub score: f32,
    pub evidence: Vec<Evidence>,
}

/// A simpler candidate for the roles that don't need value-linking.
#[derive(Clone, Debug, PartialEq)]
pub struct RoleCandidate {
    /// Lowercased field path; matched case-insensitively per container.
    pub id: String,
    pub label: String,
    pub containers: Vec<String>,
    pub note: String,
    pub score: f32,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Insights {
    pub containers: Vec<String>,
    pub keys: Vec<KeyCandidate>,
    pub times: Vec<RoleCandidate>,
    pub labels: Vec<RoleCandidate>,
}

impl KeyCandidate {
    /// The field path carrying this key in a given container, if any.
    pub fn binding_for(&self, container: &str) -> Option<&Binding> {
        self.bindings.iter().find(|b| b.container == container)
    }
}

pub fn analyze(schemas: &[ContainerSchema]) -> Insights {
    Insights {
        containers: schemas.iter().map(ContainerSchema::path).collect(),
        keys: key_candidates(schemas),
        times: time_candidates(schemas),
        labels: label_candidates(schemas),
    }
}

/// Every distinct value sampled for a field, across all containers.
///
/// When a field drives the lane axis, this is the set of stages we know
/// exist — and therefore the only stages that can be reported as *not*
/// reached. It is exactly as complete as the sample was.
pub fn field_values(schemas: &[ContainerSchema], lower_path: &str) -> Vec<String> {
    if lower_path.is_empty() {
        return Vec::new();
    }
    let mut out: BTreeSet<&str> = BTreeSet::new();
    for schema in schemas {
        for field in &schema.fields {
            if field.name.to_lowercase() == lower_path {
                out.extend(field.values.iter().map(String::as_str));
            }
        }
    }
    out.into_iter().map(str::to_string).collect()
}

/// Every scalar field in the account, for pickers that need the full list
/// rather than a ranked subset — an error flag can live on any field, however
/// unremarkable, so this deliberately does no filtering beyond "has a value
/// you could compare against".
pub fn scalar_fields(schemas: &[ContainerSchema]) -> Vec<RoleCandidate> {
    let mut by_key: BTreeMap<String, (String, Vec<String>)> = BTreeMap::new();

    for schema in schemas {
        let path = schema.path();
        for field in &schema.fields {
            if SYSTEM_FIELDS.contains(&field.name.as_str()) || !field.is_scalar() {
                continue;
            }
            let entry = by_key
                .entry(field.name.to_lowercase())
                .or_insert_with(|| (field.name.clone(), Vec::new()));
            entry.1.push(path.clone());
        }
    }

    by_key
        .into_iter()
        .map(|(id, (label, containers))| RoleCandidate {
            score: containers.len() as f32,
            note: format!("in {} container(s)", containers.len()),
            id,
            label,
            containers,
        })
        .collect()
}

// ── Correlation keys ──────────────────────────────────────────────────────

struct Node<'a> {
    schema: usize,
    container: String,
    field: &'a str,
    name_key: String,
    seen_in: usize,
    sampled_docs: usize,
    distinct: usize,
    fill: f32,
    distinct_ratio: f32,
    /// Values long enough to be worth linking on.
    linkable: BTreeSet<&'a str>,
    id_shaped: bool,
}

fn key_candidates(schemas: &[ContainerSchema]) -> Vec<KeyCandidate> {
    let all_paths: Vec<String> = schemas.iter().map(ContainerSchema::path).collect();
    let mut nodes: Vec<Node> = Vec::new();

    for (idx, schema) in schemas.iter().enumerate() {
        let path = schema.path();
        for field in &schema.fields {
            if SYSTEM_FIELDS.contains(&field.name.as_str()) || !field.is_scalar() {
                continue;
            }
            let linkable: BTreeSet<&str> = field
                .values
                .iter()
                .filter(|v| v.len() >= LINK_MIN_LEN)
                .map(String::as_str)
                .collect();
            let id_like = field.values.iter().filter(|v| id_shaped(v)).count();
            nodes.push(Node {
                schema: idx,
                container: path.clone(),
                field: &field.name,
                name_key: field.name.to_lowercase(),
                seen_in: field.seen_in,
                sampled_docs: schema.sampled_docs,
                distinct: field.distinct,
                fill: field.fill(schema.sampled_docs),
                distinct_ratio: field.distinct_ratio(),
                linkable,
                id_shaped: !field.values.is_empty() && id_like * 2 > field.values.len(),
            });
        }
    }

    // Merge fields that are the same identifier: same name across containers,
    // or — regardless of name — enough shared values to rule out coincidence.
    let mut dsu = Dsu::new(nodes.len());
    for i in 0..nodes.len() {
        for j in (i + 1)..nodes.len() {
            // Only ever link across containers. Two fields of one document
            // sharing values (`orderId` / `parentOrderId`) are related, but
            // they are not one key.
            if nodes[i].schema == nodes[j].schema {
                continue;
            }
            if nodes[i].name_key == nodes[j].name_key
                || shared_count(&nodes[i].linkable, &nodes[j].linkable) >= LINK_MIN_SHARED
            {
                dsu.union(i, j);
            }
        }
    }

    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..nodes.len() {
        groups.entry(dsu.find(i)).or_default().push(i);
    }

    let mut candidates: Vec<KeyCandidate> = groups
        .into_values()
        .map(|members| build_candidate(&nodes, &members, &all_paths))
        .collect();

    candidates.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.label.cmp(&b.label))
    });
    candidates
}

fn build_candidate(nodes: &[Node], members: &[usize], all_paths: &[String]) -> KeyCandidate {
    // One binding per container: if a container contributes several fields to
    // the group, show the most selective one.
    let mut best_per_container: BTreeMap<&str, &Node> = BTreeMap::new();
    for &i in members {
        let node = &nodes[i];
        best_per_container
            .entry(node.container.as_str())
            .and_modify(|current| {
                if (node.distinct, node.seen_in) > (current.distinct, current.seen_in) {
                    *current = node;
                }
            })
            .or_insert(node);
    }

    // Values landing in more than one container are the linking evidence.
    let mut value_containers: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for node in best_per_container.values() {
        for value in &node.linkable {
            value_containers
                .entry(value)
                .or_default()
                .insert(node.container.as_str());
        }
    }
    let shared_values = value_containers.values().filter(|c| c.len() > 1).count();

    let bindings: Vec<Binding> = best_per_container
        .values()
        .map(|n| Binding {
            container: n.container.clone(),
            field: n.field.to_string(),
            seen_in: n.seen_in,
            sampled_docs: n.sampled_docs,
            distinct: n.distinct,
        })
        .collect();

    let names: BTreeSet<&str> = best_per_container.values().map(|n| n.field).collect();
    let cross_named = names.len() > 1;
    let reach = bindings.len();
    let count = best_per_container.len().max(1) as f32;
    let avg_fill = best_per_container.values().map(|n| n.fill).sum::<f32>() / count;
    let avg_distinct_ratio = best_per_container
        .values()
        .map(|n| n.distinct_ratio)
        .sum::<f32>()
        / count;
    let id_shaped = best_per_container.values().filter(|n| n.id_shaped).count() * 2 > reach;
    let name_hint = best_per_container
        .values()
        .any(|n| name_suggests_identifier(&n.name_key));

    // Label: prefer the spelling used by the most containers, then the
    // shortest, so a group reads by its most common name.
    let mut spelling_counts: BTreeMap<&str, usize> = BTreeMap::new();
    for node in best_per_container.values() {
        *spelling_counts.entry(node.field).or_default() += 1;
    }
    let label = spelling_counts
        .iter()
        .max_by_key(|(name, count)| (**count, std::cmp::Reverse(name.len())))
        .map(|(name, _)| name.to_string())
        .unwrap_or_default();

    let id = best_per_container
        .values()
        .map(|n| format!("{}\u{1}{}", n.container, n.field))
        .min()
        .unwrap_or_default();

    let mut score = 0.0f32;
    let mut evidence = Vec::new();

    // Reach: a key that only exists in one place cannot trace anything.
    score += 3.0 * (reach.min(6) as f32 - 1.0);
    if reach > 1 {
        evidence.push(Evidence {
            good: true,
            text: format!("present in {reach} containers"),
        });
    } else {
        evidence.push(Evidence {
            good: false,
            text: "only in one container — nothing to trace across".into(),
        });
    }

    // Shared values: the one piece of hard proof that it links.
    if shared_values > 0 {
        score += 4.0 + (shared_values.min(10) as f32) * 0.3;
        evidence.push(Evidence {
            good: true,
            text: format!("{shared_values} values seen in more than one container"),
        });
    } else if reach > 1 {
        evidence.push(Evidence {
            good: false,
            text: "no shared values in the sample — the link is unproven".into(),
        });
    }

    if cross_named {
        evidence.push(Evidence {
            good: true,
            text: format!("same values under different names: {}", join(&names)),
        });
    }

    if id_shaped {
        score += 1.5;
        evidence.push(Evidence {
            good: true,
            text: "values look like identifiers".into(),
        });
    }

    // Selectivity: a field with a handful of repeated values is a status, not
    // an identity.
    if avg_distinct_ratio < 0.15 {
        score -= 5.0;
        evidence.push(Evidence {
            good: false,
            text: format!(
                "only {:.0}% distinct values — looks like a status, not an id",
                avg_distinct_ratio * 100.0
            ),
        });
    }

    score += avg_fill * 2.0;
    if avg_fill < 0.5 {
        evidence.push(Evidence {
            good: false,
            text: format!("set on only {:.0}% of sampled documents", avg_fill * 100.0),
        });
    }

    // Weakest signal, deliberately: naming conventions are a hint, never the
    // reason a candidate wins.
    if name_hint {
        score += 1.0;
        evidence.push(Evidence {
            good: true,
            text: "name follows a common id convention".into(),
        });
    }

    let containers: BTreeSet<&str> = bindings.iter().map(|b| b.container.as_str()).collect();
    let missing = all_paths
        .iter()
        .filter(|p| !containers.contains(p.as_str()))
        .cloned()
        .collect();

    KeyCandidate {
        id,
        label,
        bindings,
        missing,
        shared_values,
        cross_named,
        id_shaped,
        avg_fill,
        avg_distinct_ratio,
        score,
        evidence,
    }
}

// ── Time and label roles ──────────────────────────────────────────────────

fn time_candidates(schemas: &[ContainerSchema]) -> Vec<RoleCandidate> {
    let mut by_key: BTreeMap<String, (String, Vec<String>, usize)> = BTreeMap::new();

    for schema in schemas {
        let path = schema.path();
        for field in &schema.fields {
            if field.values.is_empty() {
                continue;
            }
            let hits = field.values.iter().filter(|v| time_shaped(v)).count();
            if hits * 5 < field.values.len() * 3 {
                continue; // fewer than 60% of values are time-shaped
            }
            let entry = by_key
                .entry(field.name.to_lowercase())
                .or_insert_with(|| (field.name.clone(), Vec::new(), 0));
            entry.1.push(path.clone());
            entry.2 += hits;
        }
    }

    let mut out: Vec<RoleCandidate> = by_key
        .into_iter()
        .map(|(id, (label, containers, _))| RoleCandidate {
            score: containers.len() as f32,
            note: format!("timestamp-shaped, in {} container(s)", containers.len()),
            id,
            label,
            containers,
        })
        .collect();

    // Every Cosmos document has `_ts`, so it always works — but it records
    // when the document was written, which is not always when the step
    // happened. Offered last, and said plainly.
    out.push(RoleCandidate {
        id: "_ts".into(),
        label: "_ts".into(),
        containers: schemas.iter().map(ContainerSchema::path).collect(),
        note: "Cosmos write time — always present, but not your domain timestamp".into(),
        score: 0.5,
    });

    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.label.cmp(&b.label))
    });
    out
}

fn label_candidates(schemas: &[ContainerSchema]) -> Vec<RoleCandidate> {
    let mut by_key: BTreeMap<String, (String, Vec<String>, usize)> = BTreeMap::new();

    for schema in schemas {
        let path = schema.path();
        for field in &schema.fields {
            if SYSTEM_FIELDS.contains(&field.name.as_str())
                || !field.types.iter().any(|t| t == "string")
                || field.fill(schema.sampled_docs) < 0.5
            {
                continue;
            }
            // A useful step label is a small vocabulary describing what
            // happened, not a value unique to every document.
            if field.distinct < 2 || field.distinct > 25 {
                continue;
            }
            // Only hold repetition against a field once the sample is big
            // enough to expect it — in five documents even a real enum can
            // come out all-distinct.
            if field.seen_in >= 8 && field.distinct_ratio() > 0.8 {
                continue;
            }
            if field.values.iter().any(|v| v.len() > 60) {
                continue; // free text, not a label
            }
            if field.values.iter().filter(|v| id_shaped(v)).count() * 2 > field.values.len() {
                continue; // identifiers name nothing
            }
            let entry = by_key
                .entry(field.name.to_lowercase())
                .or_insert_with(|| (field.name.clone(), Vec::new(), 0));
            entry.1.push(path.clone());
            entry.2 = entry.2.max(field.distinct);
        }
    }

    let mut out: Vec<RoleCandidate> = by_key
        .into_iter()
        .map(|(id, (label, containers, distinct))| RoleCandidate {
            score: containers.len() as f32,
            note: format!(
                "{distinct} distinct values, in {} container(s)",
                containers.len()
            ),
            id,
            label,
            containers,
        })
        .collect();

    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.label.cmp(&b.label))
    });
    out
}

// ── Shape tests ───────────────────────────────────────────────────────────

/// Whether a value looks like a machine-generated identifier: UUID, ULID,
/// hex digest, or similar. Deliberately shape-based, not format-specific.
fn id_shaped(v: &str) -> bool {
    let n = v.len();
    if n < 8 {
        return false;
    }
    let has_digit = v.chars().any(|c| c.is_ascii_digit());
    let hex_dashes = v.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
    let alnum = v.chars().all(|c| c.is_ascii_alphanumeric());
    (hex_dashes && has_digit && n >= 16) || (alnum && has_digit && n >= 12)
}

/// ISO-8601-ish dates, or epoch seconds/milliseconds in a plausible range.
fn time_shaped(v: &str) -> bool {
    let b = v.as_bytes();
    if b.len() >= 10
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[7] == b'-'
        && b[5..7].iter().all(u8::is_ascii_digit)
    {
        return true;
    }
    match v.parse::<i64>() {
        // ~2001-09-09 to ~2096 in seconds, same window in milliseconds.
        Ok(n) => {
            (1_000_000_000..=4_000_000_000).contains(&n)
                || (1_000_000_000_000..=4_000_000_000_000).contains(&n)
        }
        Err(_) => false,
    }
}

/// A weak, convention-based hint — never decisive on its own.
fn name_suggests_identifier(lower: &str) -> bool {
    const HINTS: [&str; 6] = [
        "correlation",
        "trace",
        "requestid",
        "conversation",
        "session",
        "flowid",
    ];
    HINTS.iter().any(|h| lower.contains(h))
}

// ── Helpers ───────────────────────────────────────────────────────────────

fn shared_count(a: &BTreeSet<&str>, b: &BTreeSet<&str>) -> usize {
    if a.len() > b.len() {
        return shared_count(b, a);
    }
    a.iter().filter(|v| b.contains(*v)).count()
}

fn join(names: &BTreeSet<&str>) -> String {
    names.iter().copied().collect::<Vec<_>>().join(", ")
}

struct Dsu {
    parent: Vec<usize>,
}

impl Dsu {
    fn new(n: usize) -> Self {
        Dsu {
            parent: (0..n).collect(),
        }
    }

    fn find(&mut self, mut x: usize) -> usize {
        while self.parent[x] != x {
            self.parent[x] = self.parent[self.parent[x]];
            x = self.parent[x];
        }
        x
    }

    fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            self.parent[rb] = ra;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::cosmos::FieldInfo;

    fn field(name: &str, values: &[&str]) -> FieldInfo {
        let set: BTreeSet<String> = values.iter().map(|v| v.to_string()).collect();
        FieldInfo {
            name: name.into(),
            types: vec!["string".into()],
            seen_in: values.len(),
            distinct: set.len(),
            values: set,
        }
    }

    fn container(db: &str, name: &str, fields: Vec<FieldInfo>) -> ContainerSchema {
        let sampled = fields.iter().map(|f| f.seen_in).max().unwrap_or(0);
        ContainerSchema {
            database: db.into(),
            container: name.into(),
            sampled_docs: sampled,
            fields,
        }
    }

    fn uuids() -> Vec<&'static str> {
        vec![
            "9f1c2d3e-4a5b-6c7d-8e9f-0a1b2c3d4e5f",
            "1a2b3c4d-5e6f-7a8b-9c0d-1e2f3a4b5c6d",
            "2b3c4d5e-6f7a-8b9c-0d1e-2f3a4b5c6d7e",
            "3c4d5e6f-7a8b-9c0d-1e2f-3a4b5c6d7e8f",
        ]
    }

    /// The point of the whole module: two containers naming the same
    /// identifier differently are still recognised as one key, purely from
    /// the values.
    #[test]
    fn links_differently_named_fields_by_shared_values() {
        let ids = uuids();
        let schemas = vec![
            container(
                "shop",
                "orders",
                vec![
                    field("orderRef", &ids),
                    field("status", &["open", "open", "closed", "closed"]),
                ],
            ),
            container(
                "shop",
                "events",
                vec![
                    field("entityId", &ids),
                    field("eventType", &["created", "paid", "shipped", "paid"]),
                ],
            ),
        ];

        let insights = analyze(&schemas);
        let best = &insights.keys[0];

        assert!(best.cross_named, "expected the group to span both names");
        assert_eq!(best.shared_values, 4);
        assert_eq!(best.bindings.len(), 2);
        assert_eq!(
            best.binding_for("shop/orders").map(|b| b.field.as_str()),
            Some("orderRef")
        );
        assert_eq!(
            best.binding_for("shop/events").map(|b| b.field.as_str()),
            Some("entityId")
        );
    }

    /// An enum-ish field repeated across containers must not outrank a real
    /// identifier just because it is everywhere.
    #[test]
    fn low_cardinality_fields_rank_below_identifiers() {
        let ids = uuids();
        let schemas = vec![
            container(
                "shop",
                "orders",
                vec![
                    field("orderRef", &ids),
                    field("status", &["a", "a", "b", "b"]),
                ],
            ),
            container(
                "shop",
                "events",
                vec![
                    field("entityId", &ids),
                    field("status", &["a", "b", "b", "a"]),
                ],
            ),
        ];

        let insights = analyze(&schemas);
        let status_rank = insights.keys.iter().position(|c| c.label == "status");
        assert_eq!(insights.keys[0].label, "orderRef");
        assert!(status_rank.is_some_and(|r| r > 0), "status should not win");
    }

    /// Two fields inside one container that happen to share values are
    /// related, but they are not a single key.
    #[test]
    fn does_not_link_fields_within_one_container() {
        let ids = uuids();
        let schemas = vec![container(
            "shop",
            "orders",
            vec![field("orderId", &ids), field("parentOrderId", &ids)],
        )];

        let insights = analyze(&schemas);
        assert!(
            insights.keys.iter().all(|c| c.bindings.len() == 1),
            "same-container fields must stay separate candidates"
        );
    }

    #[test]
    fn nested_paths_link_to_top_level_fields() {
        let ids = uuids();
        let schemas = vec![
            container("shop", "orders", vec![field("correlationId", &ids)]),
            container(
                "shop",
                "audit",
                vec![field("properties.correlationId", &ids)],
            ),
        ];

        let insights = analyze(&schemas);
        let best = &insights.keys[0];
        assert_eq!(best.bindings.len(), 2);
        assert_eq!(
            best.binding_for("shop/audit").map(|b| b.field.as_str()),
            Some("properties.correlationId")
        );
    }

    /// Lane values come from the sample, so they are only ever as complete as
    /// the sample was — but they must at least be the union across containers.
    #[test]
    fn field_values_unions_across_containers_and_dedupes() {
        let schemas = vec![
            container(
                "shop",
                "events",
                vec![field("workflowName", &["Validate", "Invoice", "Validate"])],
            ),
            container(
                "shop",
                "audit",
                vec![field("WorkflowName", &["Validate", "Archive"])],
            ),
        ];

        assert_eq!(
            field_values(&schemas, "workflowname"),
            vec!["Archive", "Invoice", "Validate"]
        );
        assert!(field_values(&schemas, "").is_empty());
        assert!(field_values(&schemas, "nosuchfield").is_empty());
    }

    /// The error-rule picker must offer numeric flags like `sessionStatus`,
    /// which the ranked role lists filter out as too low-cardinality.
    #[test]
    fn scalar_fields_includes_numeric_flags_the_role_lists_reject() {
        let numeric = FieldInfo {
            name: "sessionStatus".into(),
            types: vec!["number".into()],
            seen_in: 20,
            distinct: 2,
            values: ["2", "3"].iter().map(|s| s.to_string()).collect(),
        };
        let schemas = vec![container(
            "ais",
            "MsgItems",
            vec![numeric, field("correlationId", &uuids())],
        )];

        let offered = scalar_fields(&schemas);
        let ids: Vec<&str> = offered.iter().map(|f| f.id.as_str()).collect();
        assert!(
            ids.contains(&"sessionstatus"),
            "numeric fields must be offerable as error flags, got {ids:?}"
        );
        assert_eq!(field_values(&schemas, "sessionstatus"), vec!["2", "3"]);
    }

    #[test]
    fn time_and_label_roles_are_detected() {
        let schemas = vec![container(
            "shop",
            "events",
            vec![
                field(
                    "occurredAt",
                    &[
                        "2026-01-04T10:00:00Z",
                        "2026-01-04T10:05:00Z",
                        "2026-01-05T09:00:00Z",
                    ],
                ),
                field("eventType", &["created", "paid", "shipped"]),
            ],
        )];

        let insights = analyze(&schemas);
        assert_eq!(insights.times[0].label, "occurredAt");
        // `_ts` is always offered, but never ahead of a real domain timestamp.
        assert!(insights.times.iter().any(|c| c.id == "_ts"));
        assert!(insights.labels.iter().any(|c| c.label == "eventType"));
    }

    #[test]
    fn short_values_do_not_link_containers() {
        let schemas = vec![
            container("shop", "a", vec![field("code", &["1", "2", "3", "4"])]),
            container("shop", "b", vec![field("rank", &["1", "2", "3", "4"])]),
        ];

        let insights = analyze(&schemas);
        assert!(
            insights.keys.iter().all(|c| c.bindings.len() == 1),
            "short values are too collision-prone to link on"
        );
    }
}
