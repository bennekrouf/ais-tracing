//! Recently traced key values, remembered per Cosmos account.
//!
//! Correlation ids are long, opaque and impossible to retype, so losing the
//! last few on restart makes the app markedly worse to use. Persistence is
//! best-effort throughout: a history that fails to save is a nuisance, never
//! an error worth interrupting a trace for.

use crate::services::az::CosmosAccount;
use crate::services::trace::ErrorRule;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Enough to get back to what you were just looking at, few enough to stay a
/// single row of chips.
pub const MAX_ENTRIES: usize = 5;
/// Recently opened accounts. Discovery needs `az login` and a scan of every
/// subscription, so remembering the last few is the difference between
/// reopening an account instantly and waiting for the list.
pub const MAX_ACCOUNTS: usize = 5;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub value: String,
    /// The field it was traced on, so a chip can say what the value means.
    pub key: String,
}

#[derive(Default, Serialize, Deserialize)]
struct Store {
    /// Keyed by account endpoint — unique, unlike the display name.
    #[serde(default)]
    accounts: BTreeMap<String, Vec<Entry>>,
    /// Most recently opened first.
    #[serde(default)]
    recent_accounts: Vec<CosmosAccount>,
    /// What counts as a failed step, keyed by account endpoint. This is
    /// domain knowledge the user taught the app, so it must outlive the
    /// session that taught it.
    #[serde(default)]
    error_rules: BTreeMap<String, Vec<ErrorRule>>,
}

fn path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ais-tracing")
        .join("history.json")
}

fn read() -> Store {
    std::fs::read_to_string(path())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn write(store: &Store) {
    if let Ok(json) = serde_json::to_string_pretty(store) {
        crate::services::atomic_write(&path(), &json);
    }
}

pub fn load(account: &str) -> Vec<Entry> {
    read().accounts.remove(account).unwrap_or_default()
}

/// Records a value as the most recent, returning the updated list.
pub fn record(account: &str, entry: Entry) -> Vec<Entry> {
    let mut store = read();
    let list = store.accounts.entry(account.to_string()).or_default();
    insert(list, entry);
    let updated = list.clone();
    write(&store);
    updated
}

pub fn clear(account: &str) -> Vec<Entry> {
    let mut store = read();
    store.accounts.remove(account);
    write(&store);
    Vec::new()
}

/// Most recent first, no duplicates, capped. Re-tracing a value moves it back
/// to the front rather than adding a second chip for it.
fn insert(list: &mut Vec<Entry>, entry: Entry) {
    list.retain(|e| e.value != entry.value);
    list.insert(0, entry);
    list.truncate(MAX_ENTRIES);
}

// ── Recently opened accounts ──────────────────────────────────────────────

pub fn load_accounts() -> Vec<CosmosAccount> {
    read().recent_accounts
}

pub fn record_account(account: &CosmosAccount) -> Vec<CosmosAccount> {
    let mut store = read();
    insert_account(&mut store.recent_accounts, account.clone());
    let updated = store.recent_accounts.clone();
    write(&store);
    updated
}

pub fn forget_account(endpoint: &str) -> Vec<CosmosAccount> {
    let mut store = read();
    store.recent_accounts.retain(|a| a.endpoint != endpoint);
    let updated = store.recent_accounts.clone();
    write(&store);
    updated
}

// ── Error rules ───────────────────────────────────────────────────────────

pub fn load_rules(account: &str) -> Vec<ErrorRule> {
    read().error_rules.remove(account).unwrap_or_default()
}

/// Replaces the whole set — the caller owns the list and edits it in place.
pub fn save_rules(account: &str, rules: &[ErrorRule]) {
    let mut store = read();
    if rules.is_empty() {
        store.error_rules.remove(account);
    } else {
        store
            .error_rules
            .insert(account.to_string(), rules.to_vec());
    }
    write(&store);
}

/// Deduped by endpoint rather than name: two subscriptions can hold accounts
/// with the same display name, and they are not the same account.
fn insert_account(list: &mut Vec<CosmosAccount>, account: CosmosAccount) {
    list.retain(|a| a.endpoint != account.endpoint);
    list.insert(0, account);
    list.truncate(MAX_ACCOUNTS);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(value: &str) -> Entry {
        Entry {
            value: value.into(),
            key: "correlationId".into(),
        }
    }

    #[test]
    fn most_recent_comes_first() {
        let mut list = Vec::new();
        insert(&mut list, entry("a"));
        insert(&mut list, entry("b"));
        assert_eq!(values(&list), vec!["b", "a"]);
    }

    #[test]
    fn retracing_a_value_moves_it_up_instead_of_duplicating() {
        let mut list = Vec::new();
        for v in ["a", "b", "c"] {
            insert(&mut list, entry(v));
        }
        insert(&mut list, entry("a"));
        assert_eq!(values(&list), vec!["a", "c", "b"]);
    }

    /// Re-tracing under a different key updates the annotation rather than
    /// leaving two chips that look identical.
    #[test]
    fn a_repeated_value_keeps_only_its_latest_key() {
        let mut list = vec![entry("a")];
        insert(
            &mut list,
            Entry {
                value: "a".into(),
                key: "traceId".into(),
            },
        );
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].key, "traceId");
    }

    #[test]
    fn the_oldest_entry_falls_off_the_end() {
        let mut list = Vec::new();
        for v in ["a", "b", "c", "d", "e", "f"] {
            insert(&mut list, entry(v));
        }
        assert_eq!(list.len(), MAX_ENTRIES);
        assert_eq!(values(&list), vec!["f", "e", "d", "c", "b"]);
    }

    fn values(list: &[Entry]) -> Vec<&str> {
        list.iter().map(|e| e.value.as_str()).collect()
    }

    fn account(name: &str, endpoint: &str) -> CosmosAccount {
        CosmosAccount {
            name: name.into(),
            resource_group: "rg".into(),
            endpoint: endpoint.into(),
            subscription_id: "sub".into(),
        }
    }

    #[test]
    fn reopening_an_account_moves_it_to_the_front() {
        let mut list = Vec::new();
        for (n, e) in [("a", "ea"), ("b", "eb"), ("c", "ec")] {
            insert_account(&mut list, account(n, e));
        }
        insert_account(&mut list, account("a", "ea"));
        let names: Vec<&str> = list.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["a", "c", "b"]);
    }

    /// Two subscriptions can hold accounts with the same display name; only
    /// the endpoint identifies one.
    #[test]
    fn accounts_are_deduped_by_endpoint_not_name() {
        let mut list = Vec::new();
        insert_account(&mut list, account("shared", "endpoint-one"));
        insert_account(&mut list, account("shared", "endpoint-two"));
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn the_account_list_is_capped() {
        let mut list = Vec::new();
        for i in 0..8 {
            insert_account(&mut list, account(&format!("a{i}"), &format!("e{i}")));
        }
        assert_eq!(list.len(), MAX_ACCOUNTS);
        assert_eq!(list[0].name, "a7");
    }
}
