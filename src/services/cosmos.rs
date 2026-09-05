//! Data-plane Cosmos DB access. Auth goes through `DeveloperToolsCredential`,
//! which picks up the same `az login` session used for the ARM calls in
//! `az.rs` — no keys, no separate sign-in step.
//!
//! Cosmos is schemaless, so there's no real "schema" to query. Instead we
//! sample a handful of documents per container and record what's actually in
//! them: the field paths, the types seen, and a capped sample of the scalar
//! values. The values matter as much as the names — `discover.rs` uses them
//! to work out which fields tie documents together, without knowing anything
//! about the domain.

use azure_data_cosmos::{
    AccountEndpoint, AccountReference, CosmosClient, Query, RoutingStrategy,
    feed::FeedScope,
    options::{MaxItemCountHint, QueryOptions},
};
use azure_identity::DeveloperToolsCredential;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU32;
use std::sync::Arc;

const SAMPLE_SIZE: usize = 20;
/// How far to descend into nested objects. Documents in the wild bury their
/// identifiers under `properties.` / `body.` wrappers, and a tracer that only
/// looks at top-level fields would miss them.
const MAX_DEPTH: usize = 3;

/// Cosmos-injected metadata, excluded wherever fields are offered as something
/// to trace or label with. `_ts` is the exception the callers make for
/// themselves: it is useless as an identifier and genuinely useful as a clock.
///
/// Defined here, next to the sampling that produces the field names, because
/// two copies of this list in two modules drift.
pub const SYSTEM_FIELDS: [&str; 6] = ["_rid", "_self", "_etag", "_attachments", "_ts", "_lsn"];

/// A data-plane failure, with the status code kept as a number.
///
/// The status is the part callers branch on — 403 means "your principal has
/// ARM access but no Cosmos SQL role", which the UI can offer to fix. Reading
/// that back out of a formatted message with `contains("403")` matches any
/// error whose text happens to mention the number, so it is carried instead.
///
/// Named `DataError` rather than `CosmosError` because the SDK already owns
/// that name for the type this wraps.
#[derive(Clone, Debug, PartialEq)]
pub struct DataError {
    pub message: String,
    pub status: Option<u16>,
}

impl DataError {
    /// The signed-in principal can see the account but not read its data —
    /// Cosmos SQL role assignments are separate from ARM RBAC, and this is
    /// the one failure the app can offer to fix for the user.
    pub fn forbidden(&self) -> bool {
        self.status == Some(403)
    }
}

impl std::fmt::Display for DataError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// Wraps a data-plane error. `status()` is always populated by the driver,
/// with a synthetic code for transport failures.
fn fail(context: &str, e: azure_data_cosmos::CosmosError) -> DataError {
    DataError {
        status: Some(u16::from(e.status().status_code())),
        message: format!("{context}: {e}"),
    }
}

/// Wraps a control-plane / credential error, which carries a status only when
/// it came back off the wire.
fn fail_core(context: &str, e: azure_core::Error) -> DataError {
    DataError {
        status: e.http_status().map(u16::from),
        message: format!("{context}: {e}"),
    }
}

/// A plain message with no status behind it.
fn plain(message: String) -> DataError {
    DataError {
        message,
        status: None,
    }
}

// ── Field paths ───────────────────────────────────────────────────────────
//
// A field path is dotted (`properties.correlationId`) because that is what
// reads well in a picker. Cosmos property names may themselves contain a dot,
// so the separator is escaped on the way in and honoured on the way out —
// otherwise `{"a.b": 1}` and `{"a": {"b": 1}}` produce the same path and one
// of them gets queried as the other.

/// Appends `key` to `prefix`, escaping any separator inside the key.
pub fn join_path(prefix: &str, key: &str) -> String {
    let escaped = key.replace('\\', "\\\\").replace('.', "\\.");
    if prefix.is_empty() {
        escaped
    } else {
        format!("{prefix}.{escaped}")
    }
}

/// Splits a dotted path back into its original property names.
pub fn split_path(path: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut chars = path.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => current.push(chars.next().unwrap_or('\\')),
            '.' => out.push(std::mem::take(&mut current)),
            other => current.push(other),
        }
    }
    out.push(current);
    out
}

/// The last segment of a path, with its escaping removed.
pub fn leaf(path: &str) -> String {
    split_path(path).pop().unwrap_or_default()
}

/// A field observed across the sampled documents of a container.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FieldInfo {
    /// Dotted path — `correlationId`, or `properties.correlationId` when nested.
    pub name: String,
    /// JSON value types it was seen with (a field can be inconsistent across
    /// documents, which is itself useful to know).
    pub types: Vec<String>,
    pub seen_in: usize,
    /// Distinct scalar values among the sampled documents.
    pub distinct: usize,
    /// The scalar values themselves, capped at `VALUE_CAP`.
    pub values: BTreeSet<String>,
}

impl FieldInfo {
    /// Fraction of sampled documents carrying this field.
    pub fn fill(&self, sampled_docs: usize) -> f32 {
        if sampled_docs == 0 {
            0.0
        } else {
            self.seen_in as f32 / sampled_docs as f32
        }
    }

    /// How selective the field is: 1.0 means a different value in every
    /// document it appears in, near 0.0 means a handful of repeated values.
    pub fn distinct_ratio(&self) -> f32 {
        if self.seen_in == 0 {
            0.0
        } else {
            self.distinct as f32 / self.seen_in as f32
        }
    }

    pub fn is_scalar(&self) -> bool {
        self.types.iter().any(|t| t == "string" || t == "number")
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContainerSchema {
    pub database: String,
    pub container: String,
    pub sampled_docs: usize,
    pub fields: Vec<FieldInfo>,
}

impl ContainerSchema {
    pub fn path(&self) -> String {
        format!("{}/{}", self.database, self.container)
    }
}

/// Builds one client for a whole run of work.
///
/// This resolves credentials (which can shell out to `az`) and negotiates the
/// account's regions, so it is far too expensive to do per query — building it
/// once and threading it through is the difference between a scan that stalls
/// the window and one that doesn't.
/// The client for `endpoint`, built once and shared.
///
/// [`connect`] is expensive for a reason that does not go away on the second
/// call: it resolves credentials (which can shell out to `az`) and negotiates
/// the account's regions. The type-ahead runs one lookup per debounced
/// keystroke, so rebuilding per call put a credential resolution in front of
/// every search. Only successes are remembered — a failure is usually "not
/// signed in yet", which stops being true.
pub async fn client_for(endpoint: &str) -> Result<CosmosClient, DataError> {
    static CLIENTS: std::sync::OnceLock<tokio::sync::Mutex<BTreeMap<String, CosmosClient>>> =
        std::sync::OnceLock::new();

    // Held across the build so two windows opening the same account at once
    // negotiate regions once between them rather than once each.
    let mut cache = CLIENTS.get_or_init(Default::default).lock().await;
    if let Some(client) = cache.get(endpoint) {
        return Ok(client.clone());
    }
    let client = connect(endpoint).await?;
    cache.insert(endpoint.to_string(), client.clone());
    Ok(client)
}

pub async fn connect(endpoint: &str) -> Result<CosmosClient, DataError> {
    let credential = DeveloperToolsCredential::new(None)
        .map_err(|e| fail_core("failed to build credential", e))?;
    let account_endpoint: AccountEndpoint = endpoint
        .parse()
        .map_err(|e| plain(format!("invalid Cosmos endpoint {endpoint}: {e}")))?;
    let account = AccountReference::with_credential(account_endpoint, credential as Arc<_>);
    // No region preference — let the SDK use the account's default write region.
    CosmosClient::builder()
        .build(account, RoutingStrategy::PreferredRegions(vec![]))
        .await
        .map_err(|e| fail("failed to build Cosmos client", e))
}

pub async fn list_databases(client: &CosmosClient) -> Result<Vec<String>, DataError> {
    let mut items = client
        .query_databases("SELECT * FROM dbs", None)
        .await
        .map_err(|e| fail("query_databases", e))?;
    let mut names = Vec::new();
    while let Some(db) = items.next().await {
        let db = db.map_err(|e| fail("query_databases item", e))?;
        if let Some(id) = db.id {
            names.push(id);
        }
    }
    Ok(names)
}

pub async fn list_containers(
    client: &CosmosClient,
    database: &str,
) -> Result<Vec<String>, DataError> {
    let db = client.database_client(database);
    let mut items = db
        .query_containers("SELECT * FROM c", None)
        .await
        .map_err(|e| fail("query_containers", e))?;
    let mut names = Vec::new();
    while let Some(c) = items.next().await {
        let c = c.map_err(|e| fail("query_containers item", e))?;
        names.push(c.id.to_string());
    }
    Ok(names)
}

/// What a bounded query returned, and whether the bound was the reason it
/// stopped.
///
/// The distinction is the whole point: a lane holding the first 200 of 500
/// documents is not the same claim as a lane holding all 200 there are, and
/// silently presenting one as the other is how a tracing tool lies.
pub struct Page {
    pub docs: Vec<Value>,
    pub truncated: bool,
}

/// Runs an arbitrary query against one container, returning at most `limit`
/// documents. Used by `trace.rs` to pull the documents carrying one key value.
///
/// One extra document is asked for beyond `limit`: its presence is what says
/// the answer was cut short, and it is discarded rather than returned.
pub async fn query_documents(
    client: &CosmosClient,
    database: &str,
    container: &str,
    query: Query,
    limit: usize,
) -> Result<Page, DataError> {
    let container_client = client
        .database_client(database)
        .container_client(container)
        .await
        .map_err(|e| fail("container_client", e))?;

    let probe = limit.saturating_add(1);
    let options = NonZeroU32::new(probe.min(u32::MAX as usize) as u32)
        .map(MaxItemCountHint::Limit)
        .map(|hint| QueryOptions::default().with_max_item_count(hint));

    let mut items = container_client
        .query_items::<Value>(query, FeedScope::full_container(), options)
        .await
        .map_err(|e| fail("query_items", e))?;

    let mut out = Vec::new();
    let mut truncated = false;
    while let Some(doc) = items.next().await {
        let doc = doc.map_err(|e| fail("query_items item", e))?;
        if out.len() == limit {
            truncated = true;
            break;
        }
        out.push(doc);
    }
    Ok(Page {
        docs: out,
        truncated,
    })
}

/// How many containers to sample at once.
///
/// The scan was one round trip after another: on a thirty-container account
/// that is thirty serialized latencies before the window shows anything. The
/// bound keeps it from opening thirty connections and provoking a 429.
const SCAN_CONCURRENCY: usize = 8;

/// A container that could not be sampled, and why.
#[derive(Clone, Debug, PartialEq)]
pub struct ContainerError {
    /// `database/container`, or just the database when the listing failed.
    pub path: String,
    pub message: String,
    /// Data-plane access is missing, which the app can offer to grant.
    pub forbidden: bool,
}

/// What a scan found, and what it could not look at.
///
/// Both halves, for the same reason `az::AccountScan` carries both: one
/// container the caller cannot read must not blank the whole account, and it
/// must not vanish either — a schema list missing a container silently is a
/// trace that will report that container as `OffPath` forever.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AccountSchemas {
    pub schemas: Vec<ContainerSchema>,
    pub errors: Vec<ContainerError>,
}

impl ContainerError {
    fn new(path: String, e: DataError) -> Self {
        ContainerError {
            path,
            forbidden: e.forbidden(),
            message: e.message,
        }
    }
}

/// Samples every container in the account.
///
/// Only the two failures that leave nothing to work with — no client, no
/// database listing — abort. Everything below that is recorded and skipped.
pub async fn scan_account(endpoint: &str) -> Result<AccountSchemas, DataError> {
    let client = client_for(endpoint).await?;
    let databases = list_databases(&client).await?;

    let mut scan = AccountSchemas::default();
    let mut paths: Vec<(String, String)> = Vec::new();
    for db in databases {
        match list_containers(&client, &db).await {
            Ok(containers) => paths.extend(containers.into_iter().map(|c| (db.clone(), c))),
            Err(e) => scan.errors.push(ContainerError::new(db, e)),
        }
    }

    let sampled = futures::stream::iter(paths.into_iter().map(|(db, container)| {
        let client = client.clone();
        async move {
            let path = format!("{db}/{container}");
            (path, infer_container_schema(&client, &db, &container).await)
        }
    }))
    .buffer_unordered(SCAN_CONCURRENCY)
    .collect::<Vec<_>>()
    .await;

    for (path, result) in sampled {
        match result {
            Ok(schema) => scan.schemas.push(schema),
            Err(e) => scan.errors.push(ContainerError::new(path, e)),
        }
    }

    // `buffer_unordered` returns in completion order; the UI lists containers
    // and must not reshuffle them between two scans of the same account.
    scan.schemas
        .sort_by(|a, b| (&a.database, &a.container).cmp(&(&b.database, &b.container)));
    scan.errors.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(scan)
}

/// Samples up to `SAMPLE_SIZE` documents from a container and records the
/// field paths present, the types observed, and the scalar values seen.
pub async fn infer_container_schema(
    client: &CosmosClient,
    database: &str,
    container: &str,
) -> Result<ContainerSchema, DataError> {
    let container_client = client
        .database_client(database)
        .container_client(container)
        .await
        .map_err(|e| fail("container_client", e))?;

    let max_item_count = NonZeroU32::new(SAMPLE_SIZE as u32).map(MaxItemCountHint::Limit);
    let options = max_item_count
        .map(|hint| QueryOptions::default().with_max_item_count(hint))
        .unwrap_or_default();

    let mut items = container_client
        .query_items::<Value>(
            "SELECT * FROM c",
            FeedScope::full_container(),
            Some(options),
        )
        .await
        .map_err(|e| fail("query_items", e))?;

    let mut observed: BTreeMap<String, FieldInfo> = BTreeMap::new();
    let mut sampled = 0usize;

    while sampled < SAMPLE_SIZE {
        let Some(doc) = items.next().await else { break };
        let doc = doc.map_err(|e| fail("query_items item", e))?;
        sampled += 1;

        // Borrowed, not cloned: most of what `flatten` walks past is arrays
        // and nested objects that `scalar_repr` then discards, and cloning a
        // document's whole subtree to throw it away is pure churn.
        let mut leaves = Vec::new();
        flatten("", &doc, 0, &mut leaves);
        for (path, value) in leaves {
            let entry = observed.entry(path.clone()).or_insert_with(|| FieldInfo {
                name: path,
                types: Vec::new(),
                seen_in: 0,
                distinct: 0,
                values: BTreeSet::new(),
            });
            let ty = json_type_name(value);
            if !entry.types.contains(&ty) {
                entry.types.push(ty);
            }
            entry.seen_in += 1;
            // No cap is needed here: one document contributes at most one
            // value per path, so `values` cannot exceed `SAMPLE_SIZE`.
            if let Some(scalar) = scalar_repr(value)
                && entry.values.insert(scalar)
            {
                entry.distinct += 1;
            }
        }
    }

    let mut fields: Vec<FieldInfo> = observed.into_values().collect();
    fields.sort_by(|a, b| b.seen_in.cmp(&a.seen_in).then(a.name.cmp(&b.name)));

    Ok(ContainerSchema {
        database: database.to_string(),
        container: container.to_string(),
        sampled_docs: sampled,
        fields,
    })
}

/// Walks a document into `(dotted path, value)` pairs. Nested objects are
/// recorded in their own right *and* descended into, up to `MAX_DEPTH`.
/// Arrays are recorded but not descended into — an element index is not a
/// stable field path.
fn flatten<'a>(prefix: &str, value: &'a Value, depth: usize, out: &mut Vec<(String, &'a Value)>) {
    /// A stand-in recording "there was an object here", so a wrapper field is
    /// listed in its own right as well as descended into. Borrowed like every
    /// other value, so nothing is allocated to say it.
    static EMPTY_OBJECT: std::sync::LazyLock<Value> =
        std::sync::LazyLock::new(|| Value::Object(serde_json::Map::new()));

    let Value::Object(map) = value else { return };
    for (key, child) in map {
        let path = join_path(prefix, key);
        match child {
            Value::Object(_) if depth + 1 < MAX_DEPTH => {
                out.push((path.clone(), &EMPTY_OBJECT));
                flatten(&path, child, depth + 1, out);
            }
            other => out.push((path, other)),
        }
    }
}

/// The comparable form of a scalar value, or `None` for shapes that can't
/// serve as an identifier or a label.
fn scalar_repr(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn json_type_name(v: &Value) -> String {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The separator has to survive being part of a name. Without this,
    /// `{"a.b": 1}` and `{"a": {"b": 1}}` collapse onto one path and the
    /// tracer queries whichever it happens to have recorded.
    #[test]
    fn a_dotted_property_name_round_trips() {
        let literal = join_path("", "order.id");
        let nested = join_path(&join_path("", "order"), "id");

        assert_ne!(literal, nested);
        assert_eq!(split_path(&literal), vec!["order.id"]);
        assert_eq!(split_path(&nested), vec!["order", "id"]);
        assert_eq!(leaf(&literal), "order.id");
        assert_eq!(leaf(&nested), "id");
    }

    #[test]
    fn a_backslash_in_a_property_name_survives_too() {
        let path = join_path("", r"we\ird");
        assert_eq!(split_path(&path), vec![r"we\ird"]);
    }

    #[test]
    fn ordinary_paths_are_unchanged() {
        assert_eq!(join_path("", "correlationId"), "correlationId");
        assert_eq!(
            join_path("properties", "correlationId"),
            "properties.correlationId"
        );
        assert_eq!(
            split_path("properties.correlationId"),
            vec!["properties", "correlationId"]
        );
    }

    /// Nested objects are recorded in their own right and descended into, and
    /// nothing below is copied out of the document to do it.
    #[test]
    fn flatten_records_wrappers_and_their_leaves() {
        let doc = serde_json::json!({
            "id": "1",
            "properties": {
                "correlationId": "abc",
                "deep": { "x": 1, "deeper": { "y": 2 } },
            },
            "tags": ["a", "b"],
        });
        let mut out = Vec::new();
        flatten("", &doc, 0, &mut out);
        let paths: Vec<&str> = out.iter().map(|(p, _)| p.as_str()).collect();

        assert!(paths.contains(&"properties"));
        assert!(paths.contains(&"properties.correlationId"));
        assert!(paths.contains(&"properties.deep"));
        assert!(paths.contains(&"properties.deep.x"));
        // MAX_DEPTH stops here: the wrapper is listed, its contents are not.
        assert!(paths.contains(&"properties.deep.deeper"));
        assert!(!paths.contains(&"properties.deep.deeper.y"));
        // Arrays are recorded, never descended into.
        assert!(paths.contains(&"tags"));
        assert!(!paths.iter().any(|p| p.starts_with("tags.")));
    }
}
