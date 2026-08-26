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
    feed::FeedScope,
    options::{MaxItemCountHint, QueryOptions},
    AccountEndpoint, AccountReference, CosmosClient, Query, RoutingStrategy,
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
/// Ceiling on retained values per field, so a wide sample can't blow up memory.
const VALUE_CAP: usize = 256;

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
pub async fn connect(endpoint: &str) -> Result<CosmosClient, String> {
    let credential = DeveloperToolsCredential::new(None)
        .map_err(|e| format!("failed to build credential: {e}"))?;
    let account_endpoint: AccountEndpoint = endpoint
        .parse()
        .map_err(|e| format!("invalid Cosmos endpoint {endpoint}: {e}"))?;
    let account = AccountReference::with_credential(account_endpoint, credential as Arc<_>);
    // No region preference — let the SDK use the account's default write region.
    CosmosClient::builder()
        .build(account, RoutingStrategy::PreferredRegions(vec![]))
        .await
        .map_err(|e| format!("failed to build Cosmos client: {e}"))
}

pub async fn list_databases(client: &CosmosClient) -> Result<Vec<String>, String> {
    let mut items = client
        .query_databases("SELECT * FROM dbs", None)
        .await
        .map_err(|e| format!("query_databases: {e}"))?;
    let mut names = Vec::new();
    while let Some(db) = items.next().await {
        let db = db.map_err(|e| format!("query_databases item: {e}"))?;
        if let Some(id) = db.id {
            names.push(id);
        }
    }
    Ok(names)
}

pub async fn list_containers(client: &CosmosClient, database: &str) -> Result<Vec<String>, String> {
    let db = client.database_client(database);
    let mut items = db
        .query_containers("SELECT * FROM c", None)
        .await
        .map_err(|e| format!("query_containers: {e}"))?;
    let mut names = Vec::new();
    while let Some(c) = items.next().await {
        let c = c.map_err(|e| format!("query_containers item: {e}"))?;
        names.push(c.id.to_string());
    }
    Ok(names)
}

/// Runs an arbitrary query against one container, returning at most `limit`
/// documents. Used by `trace.rs` to pull the documents carrying one key value.
pub async fn query_documents(
    client: &CosmosClient,
    database: &str,
    container: &str,
    query: Query,
    limit: usize,
) -> Result<Vec<Value>, String> {
    let container_client = client
        .database_client(database)
        .container_client(container)
        .await
        .map_err(|e| format!("container_client: {e}"))?;

    let options = NonZeroU32::new(limit.min(u32::MAX as usize) as u32)
        .map(MaxItemCountHint::Limit)
        .map(|hint| QueryOptions::default().with_max_item_count(hint));

    let mut items = container_client
        .query_items::<Value>(query, FeedScope::full_container(), options)
        .await
        .map_err(|e| format!("query_items: {e}"))?;

    let mut out = Vec::new();
    while out.len() < limit {
        let Some(doc) = items.next().await else { break };
        out.push(doc.map_err(|e| format!("query_items item: {e}"))?);
    }
    Ok(out)
}

/// Samples up to `SAMPLE_SIZE` documents from a container and records the
/// field paths present, the types observed, and the scalar values seen.
pub async fn infer_container_schema(
    client: &CosmosClient,
    database: &str,
    container: &str,
) -> Result<ContainerSchema, String> {
    let container_client = client
        .database_client(database)
        .container_client(container)
        .await
        .map_err(|e| format!("container_client: {e}"))?;

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
        .map_err(|e| format!("query_items: {e}"))?;

    let mut observed: BTreeMap<String, FieldInfo> = BTreeMap::new();
    let mut sampled = 0usize;

    while sampled < SAMPLE_SIZE {
        let Some(doc) = items.next().await else { break };
        let doc = doc.map_err(|e| format!("query_items item: {e}"))?;
        sampled += 1;

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
            let ty = json_type_name(&value);
            if !entry.types.contains(&ty) {
                entry.types.push(ty);
            }
            entry.seen_in += 1;
            if let Some(scalar) = scalar_repr(&value) {
                if entry.values.len() < VALUE_CAP && entry.values.insert(scalar) {
                    entry.distinct += 1;
                }
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
fn flatten(prefix: &str, value: &Value, depth: usize, out: &mut Vec<(String, Value)>) {
    let Value::Object(map) = value else { return };
    for (key, child) in map {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match child {
            Value::Object(_) if depth + 1 < MAX_DEPTH => {
                out.push((path.clone(), Value::Object(serde_json::Map::new())));
                flatten(&path, child, depth + 1, out);
            }
            other => out.push((path, other.clone())),
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
