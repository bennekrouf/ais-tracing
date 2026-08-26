//! On-disk cache of the container scan.
//!
//! Sampling every container means listing databases, listing containers, then
//! pulling documents from each — seconds of waiting before the app can show
//! anything. Since the shape of a Cosmos account changes rarely and the whole
//! result is derived data, it is cheap to keep the last scan and open on it
//! immediately while a fresh one runs behind.
//!
//! The cache is a convenience, never a source of truth: a corrupt or
//! unreadable file simply means a cold start, and a refresh always follows.

use crate::services::cosmos::ContainerSchema;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CachedScan {
    /// Unix seconds, so the UI can say how stale this is.
    pub scanned_at: i64,
    pub schemas: Vec<ContainerSchema>,
}

/// Endpoints are URLs; flatten to something safe for a filename while staying
/// recognisable when someone looks in the directory.
fn slug(endpoint: &str) -> String {
    endpoint
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

fn path(endpoint: &str) -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ais-tracing")
        .join("scans")
        .join(format!("{}.json", slug(endpoint)))
}

pub fn load(endpoint: &str) -> Option<CachedScan> {
    let text = std::fs::read_to_string(path(endpoint)).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn save(endpoint: &str, schemas: &[ContainerSchema]) {
    // An empty scan is indistinguishable from a failed one at read time, so
    // don't persist it — a cold start is better than a cache that says the
    // account is empty.
    if schemas.is_empty() {
        return;
    }
    let path = path(endpoint);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let scan = CachedScan {
        scanned_at: chrono::Utc::now().timestamp(),
        schemas: schemas.to_vec(),
    };
    if let Ok(json) = serde_json::to_string(&scan) {
        let _ = std::fs::write(path, json);
    }
}

/// "just now", "5m ago", "3h ago" — enough to judge whether to trust it.
pub fn age(scanned_at: i64) -> String {
    let secs = (chrono::Utc::now().timestamp() - scanned_at).max(0);
    match secs {
        0..=59 => "just now".to_string(),
        60..=3599 => format!("{}m ago", secs / 60),
        3600..=86_399 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_become_safe_recognisable_filenames() {
        assert_eq!(
            slug("https://cosmos-tom-dev.documents.azure.com:443/"),
            "https---cosmos-tom-dev-documents-azure-com-443"
        );
        // Two different accounts must never collide on one file.
        assert_ne!(
            slug("https://a.documents.azure.com:443/"),
            slug("https://b.documents.azure.com:443/")
        );
    }

    #[test]
    fn ages_read_in_sensible_units() {
        let now = chrono::Utc::now().timestamp();
        assert_eq!(age(now), "just now");
        assert_eq!(age(now - 300), "5m ago");
        assert_eq!(age(now - 7200), "2h ago");
        assert_eq!(age(now - 172_800), "2d ago");
        // A clock that jumped backwards must not print a negative age.
        assert_eq!(age(now + 60), "just now");
    }
}
