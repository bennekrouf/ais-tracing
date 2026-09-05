//! Lightweight update check.
//!
//! Fetches the `latest.json` published with each GitHub release and compares
//! the version field to this build's `CARGO_PKG_VERSION`. Designed to be
//! cheap and side-effect-free so it can run in the background at startup.

use serde::Deserialize;
use std::collections::HashMap;

/// Served from mayorana.ch alongside the builds it describes, so update
/// checks do not depend on the source repository staying publicly readable.
const LATEST_URL: &str = "https://mayorana.ch/downloads/ais-tracing/latest/latest.json";
/// Fallback when `latest.json` has no entry for this OS (e.g. an Intel Mac —
/// only Apple Silicon is built). Sends the user to pick a build by hand
/// instead of at a link that would 404.
const RELEASES_URL: &str = "https://mayorana.ch/en/apps";

/// Sent on the update check so the download logs can tell a new install
/// (a browser hitting the site) from an existing user updating. Also
/// carries the version, which is what makes per-version adoption
/// visible — the number that says how many people are still on a build
/// with a bug that is already fixed.
const USER_AGENT: &str = concat!("ais-tracing/", env!("CARGO_PKG_VERSION"), " (updater)");

#[derive(Debug, Deserialize)]
struct LatestJson {
    version: String,
    tag: String,
    platforms: Platforms,
}

#[derive(Debug, Deserialize)]
struct Platforms {
    macos: HashMap<String, Artifact>,
    windows: HashMap<String, Artifact>,
    linux: HashMap<String, Artifact>,
}

#[derive(Clone, Debug, Deserialize)]
struct Artifact {
    url: String,
    #[allow(dead_code)]
    sha256: String,
}

#[derive(Debug, Clone)]
pub struct UpdateInfo {
    pub latest_version: String,
    #[allow(dead_code)]
    pub latest_tag: String,
    /// Direct link to this OS's build, so the banner's button downloads the
    /// binary itself rather than opening a landing page to pick one from.
    pub release_url: String,
}

/// Returns `Some(UpdateInfo)` if a newer release is available, else `None`.
/// Any network / parse failure → `None`. Never panics.
/// Disabled if DISABLE_UPDATE_CHECK environment variable is set.
pub async fn check() -> Option<UpdateInfo> {
    if std::env::var("DISABLE_UPDATE_CHECK").is_ok() {
        return None;
    }

    let current = env!("CARGO_PKG_VERSION");
    let body = reqwest::Client::new()
        .get(LATEST_URL)
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
        .ok()?
        .text()
        .await
        .ok()?;
    let latest: LatestJson = serde_json::from_str(&body).ok()?;
    if is_newer(&latest.version, current) {
        Some(UpdateInfo {
            latest_version: latest.version,
            latest_tag: latest.tag,
            release_url: platform_url(&latest.platforms),
        })
    } else {
        None
    }
}

/// Architecture spellings that appear in artifact names, and which machine
/// each belongs to.
const ARCH_TOKENS: [(&str, &[&str]); 2] = [
    ("aarch64", &["aarch64", "arm64"]),
    ("x86_64", &["x86_64", "x86-64", "amd64", "x64"]),
];

/// Picks the artifact published for this machine. Anything ambiguous falls
/// back to the landing page, where the user can choose.
///
/// The previous version took `values().next()` off a `HashMap`, which is
/// unordered — correct only by accident, while every OS happened to publish
/// exactly one build. The moment a second architecture ships (an Intel dmg,
/// an arm64 tarball) that hands out a random one, which is the case the
/// landing-page fallback exists for in the first place.
fn platform_url(platforms: &Platforms) -> String {
    let by_os = match std::env::consts::OS {
        "macos" => &platforms.macos,
        "windows" => &platforms.windows,
        "linux" => &platforms.linux,
        _ => return RELEASES_URL.to_string(),
    };

    // Sorted, so the same release always resolves to the same URL.
    let mut entries: Vec<(&str, &str)> = by_os
        .iter()
        .filter(|(_, a)| !a.url.is_empty())
        .map(|(k, a)| (k.as_str(), a.url.as_str()))
        .collect();
    entries.sort_unstable();

    let mine: &[&str] = ARCH_TOKENS
        .iter()
        .find(|(arch, _)| *arch == std::env::consts::ARCH)
        .map(|(_, tokens)| *tokens)
        .unwrap_or(&[]);
    let names_arch = |token_set: &[&str], key: &str, url: &str| {
        let haystack = format!("{key} {url}").to_lowercase();
        token_set.iter().any(|t| haystack.contains(t))
    };

    let chosen = entries
        .iter()
        .find(|(key, url)| names_arch(mine, key, url))
        // Nothing names an architecture at all — this OS ships one build for
        // every machine (Windows), so the single entry is it. But if some
        // entry *does* name one and none of them is ours, we are an Intel Mac
        // looking at an Apple Silicon dmg: say nothing rather than hand over a
        // binary that will not run.
        .or_else(|| {
            let any_named = entries.iter().any(|(key, url)| {
                ARCH_TOKENS
                    .iter()
                    .any(|(_, tokens)| names_arch(tokens, key, url))
            });
            (!any_named && entries.len() == 1).then(|| &entries[0])
        });

    chosen
        // Marks the hit as coming from an existing install. The banner opens
        // this in the user's browser, so the updater's own User-Agent is not
        // what fetches the file — without the marker the request is
        // indistinguishable from a first-time download off the website.
        // nginx serves the file regardless of the query string.
        .map(|(_, url)| format!("{url}?src=updater"))
        .unwrap_or_else(|| RELEASES_URL.to_string())
}

fn is_newer(a: &str, b: &str) -> bool {
    let parse = |s: &str| -> Option<(u32, u32, u32)> {
        let mut parts = s.trim_start_matches('v').split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = parts.next()?.split(['-', '+']).next()?.parse().ok()?;
        Some((major, minor, patch))
    };
    match (parse(a), parse(b)) {
        (Some(av), Some(bv)) => av > bv,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact(url: &str) -> Artifact {
        Artifact {
            url: url.into(),
            sha256: String::new(),
        }
    }

    fn platforms(entries: &[(&str, &str)]) -> Platforms {
        let map: HashMap<String, Artifact> = entries
            .iter()
            .map(|(k, u)| (k.to_string(), artifact(u)))
            .collect();
        Platforms {
            macos: map.clone(),
            windows: map.clone(),
            linux: map,
        }
    }

    /// One build for every machine — Windows publishes a single installer with
    /// no architecture in its name, and that is the one to hand over.
    #[test]
    fn a_single_architecture_less_build_is_used_as_is() {
        let url = platform_url(&platforms(&[(
            "exe_or_msi",
            "https://x/ais-tracing-setup.exe",
        )]));
        assert_eq!(url, "https://x/ais-tracing-setup.exe?src=updater");
    }

    /// The failure this replaces: `values().next()` on a `HashMap` handed out
    /// whichever build hashing happened to put first.
    #[test]
    fn the_build_for_this_machine_is_chosen_not_an_arbitrary_one() {
        let url = platform_url(&platforms(&[
            ("arm", "https://x/ais-tracing-macos-arm64.dmg"),
            ("intel", "https://x/ais-tracing-macos-x86_64.dmg"),
        ]));
        let expected = match std::env::consts::ARCH {
            "aarch64" => "https://x/ais-tracing-macos-arm64.dmg?src=updater",
            _ => "https://x/ais-tracing-macos-x86_64.dmg?src=updater",
        };
        assert_eq!(url, expected);
    }

    /// An Intel Mac looking at an Apple-Silicon-only release. Sending it to
    /// the landing page is the point: the alternative is a download that
    /// cannot run.
    #[test]
    fn a_release_with_no_build_for_this_machine_falls_back_to_the_site() {
        let other = match std::env::consts::ARCH {
            "aarch64" => "https://x/ais-tracing-linux-x86_64.tar.gz",
            _ => "https://x/ais-tracing-macos-arm64.dmg",
        };
        assert_eq!(platform_url(&platforms(&[("only", other)])), RELEASES_URL);
    }

    /// Two calls on the same release must agree — the banner is rendered
    /// repeatedly, and a link that changes under the cursor is its own bug.
    #[test]
    fn the_same_release_always_resolves_to_the_same_url() {
        let p = platforms(&[
            ("a", "https://x/one.tar.gz"),
            ("b", "https://x/two.tar.gz"),
            ("c", "https://x/three.tar.gz"),
        ]);
        assert_eq!(platform_url(&p), platform_url(&p));
    }

    #[test]
    fn versions_compare_numerically_and_tolerate_prereleases() {
        assert!(is_newer("0.1.22", "0.1.21"));
        assert!(is_newer("0.2.0", "0.1.99"));
        assert!(!is_newer("0.1.21", "0.1.21"));
        assert!(!is_newer("0.1.20", "0.1.21"));
        assert!(is_newer("v0.1.22", "0.1.21"));
        assert!(!is_newer("0.1.22-rc1", "0.1.22"));
        // Anything unparseable must never claim an update is available.
        assert!(!is_newer("not-a-version", "0.1.21"));
    }
}
