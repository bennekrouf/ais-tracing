//! Thin wrapper around the local `az` CLI — same auth story as ais-monitor:
//! the user is expected to already be signed in (`az login`), and we shell
//! out for anything that's control-plane (ARM) rather than data-plane.
//! Data-plane Cosmos calls go through `azure_identity::DeveloperToolsCredential`
//! instead (see `cosmos.rs`), which itself reads the same `az` session.

use serde::{Deserialize, Serialize};
use std::process::Command;

fn az_command(args: &[&str]) -> Command {
    let mut cmd = Command::new("az");
    cmd.args(args);
    cmd
}

#[derive(Clone, Debug, PartialEq)]
pub enum AzLoginState {
    LoggedIn {
        account: String,
        subscription_id: String,
    },
    /// A profile exists, but Azure rejects the refresh token. Distinct from
    /// `NotLoggedIn` because the symptom is worse: everything looks signed
    /// in until the first real call quietly returns nothing.
    Expired {
        account: String,
        message: String,
    },
    NotLoggedIn,
    AzNotFound,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
struct AzAccount {
    #[serde(default)]
    name: String,
    #[serde(default)]
    id: String,
}

/// Whether the CLI can actually reach Azure right now.
///
/// `az account show` on its own is not enough, and trusting it is what makes
/// an expired session so confusing: it reads the *local* profile cache and
/// keeps succeeding long after the refresh token has died. The app then says
/// "Connected", finds nothing in any subscription, and reports that as an
/// empty account list. Conditional-access sign-in frequency policies expire
/// tokens on a fixed schedule, so this is a daily event, not an edge case.
///
/// Acquiring a token is the cheapest call that proves the session really
/// works. `--output none` keeps the token off stdout; there is no reason for
/// a secret to pass through this process.
pub fn check_login() -> AzLoginState {
    let out = az_command(&["account", "show", "--output", "json"]).output();
    match out {
        Ok(out) if out.status.success() => {
            let body = String::from_utf8_lossy(&out.stdout);
            match serde_json::from_str::<AzAccount>(&body) {
                Ok(acc) => {
                    match az_command(&["account", "get-access-token", "--output", "none"]).output()
                    {
                        Ok(token) if token.status.success() => AzLoginState::LoggedIn {
                            account: acc.name,
                            subscription_id: acc.id,
                        },
                        Ok(token) => AzLoginState::Expired {
                            account: acc.name,
                            message: azure_error_summary(&String::from_utf8_lossy(&token.stderr)).0,
                        },
                        Err(e) => AzLoginState::Expired {
                            account: acc.name,
                            message: format!("could not acquire a token: {e}"),
                        },
                    }
                }
                Err(_) => AzLoginState::NotLoggedIn,
            }
        }
        Ok(_) => AzLoginState::NotLoggedIn,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => AzLoginState::AzNotFound,
        Err(_) => AzLoginState::NotLoggedIn,
    }
}

/// Opens `az login` (non-blocking) so the desktop app doesn't have to embed
/// its own OAuth flow — same approach as ais-monitor.
pub fn open_login() -> Result<(), String> {
    az_command(&["login"]).spawn().map(|_| ()).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "Azure CLI ('az') not found on PATH.".to_string()
        } else {
            format!("Failed to start 'az login': {e}")
        }
    })
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
struct Subscription {
    id: String,
    #[serde(default)]
    name: String,
}

fn list_subscriptions() -> Result<Vec<Subscription>, String> {
    let output = az_command(&[
        "account",
        "list",
        "--query",
        "[].{id:id,name:name}",
        "--output",
        "json",
    ])
    .output()
    .map_err(|e| format!("az account list failed: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    let body = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&body).map_err(|e| format!("parse: {e}"))
}

/// A subscription that could not be read, and why.
#[derive(Clone, Debug, PartialEq)]
pub struct SubscriptionError {
    pub name: String,
    pub id: String,
    pub message: String,
    /// The session is dead, rather than the permissions being wrong. Worth
    /// separating: one is fixed by `az login`, the other cannot be fixed by
    /// the user at all.
    pub expired: bool,
}

/// What was found, and what could not be looked at.
///
/// The second half is the point. Skipping unreadable subscriptions is right
/// — one PIM-gated subscription should not block discovery in the others —
/// but doing it silently turns "your session expired" into "you have no
/// Cosmos DB accounts", which sends people looking in the wrong place
/// entirely.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AccountScan {
    pub accounts: Vec<CosmosAccount>,
    pub errors: Vec<SubscriptionError>,
}

impl AccountScan {
    /// Nothing could be read anywhere, so an empty result says nothing about
    /// whether any accounts exist.
    pub fn blind(&self) -> bool {
        self.accounts.is_empty() && !self.errors.is_empty()
    }

    pub fn any_expired(&self) -> bool {
        self.errors.iter().any(|e| e.expired)
    }
}

/// Reduces a wall of CLI stderr to one line, and says whether it means the
/// session has expired.
///
/// Azure reports a dead refresh token as `AADSTS70043` (or `700082`) inside
/// a paragraph that also suggests `az logout`. Neither the code nor the
/// paragraph is worth showing anyone; "your session expired" is.
fn azure_error_summary(stderr: &str) -> (String, bool) {
    let flat = stderr.trim();
    let expired = flat.contains("AADSTS70043")
        || flat.contains("AADSTS700082")
        || flat.contains("AADSTS50173")
        || flat.contains("refresh token has expired");
    if expired {
        return (
            "Azure session expired — re-run `az login` (conditional access \
             enforces a sign-in frequency)."
                .to_string(),
            true,
        );
    }
    let first = flat
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("unknown error")
        .trim_start_matches("ERROR:")
        .trim();
    (first.chars().take(220).collect(), false)
}

/// A Cosmos DB account discovered in the signed-in subscription(s).
///
/// `Serialize` is here so recently-opened accounts can be remembered; the
/// field renames round-trip, so what we write back matches what `az` emits.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct CosmosAccount {
    pub name: String,
    #[serde(rename = "resourceGroup")]
    pub resource_group: String,
    /// e.g. `https://myaccount.documents.azure.com:443/`
    #[serde(rename = "documentEndpoint")]
    pub endpoint: String,
    #[serde(default)]
    pub subscription_id: String,
}

/// Lists every Cosmos DB account visible across *all* of the signed-in
/// account's subscriptions — a single account often only has Cosmos
/// resources in one non-default subscription, so scanning just the
/// current default (as `az cosmosdb list` does with no `--subscription`)
/// misses them. This is an ARM (control-plane) call via `az`, kept
/// separate from the data-plane SDK calls in `cosmos.rs`.
pub fn list_cosmos_accounts() -> Result<AccountScan, String> {
    let subscriptions = list_subscriptions()?;
    let mut scan = AccountScan::default();
    for sub in subscriptions {
        let output = az_command(&[
            "cosmosdb",
            "list",
            "--subscription",
            sub.id.as_str(),
            "--query",
            "[].{name:name,resourceGroup:resourceGroup,documentEndpoint:documentEndpoint}",
            "--output",
            "json",
        ])
        .output()
        .map_err(|e| format!("az cosmosdb list failed: {e}"))?;

        // A subscription the caller can't read (PIM not activated, an
        // expired session) must not block discovery in the others — but it
        // is recorded, because an empty list from a subscription nobody
        // could read is not evidence of anything.
        if !output.status.success() {
            let (message, expired) = azure_error_summary(&String::from_utf8_lossy(&output.stderr));
            scan.errors.push(SubscriptionError {
                name: sub.name,
                id: sub.id,
                message,
                expired,
            });
            continue;
        }

        let body = String::from_utf8_lossy(&output.stdout);
        match serde_json::from_str::<Vec<CosmosAccount>>(&body) {
            Ok(mut found) => {
                for acc in &mut found {
                    acc.subscription_id = sub.id.clone();
                }
                scan.accounts.extend(found);
            }
            // One subscription answering in an unexpected shape is the same
            // class of problem as one refusing to answer.
            Err(e) => scan.errors.push(SubscriptionError {
                name: sub.name,
                id: sub.id,
                message: format!("unreadable response: {e}"),
                expired: false,
            }),
        }
    }
    Ok(scan)
}

/// Cosmos DB's built-in "Data Reader" role. Data-plane access is governed
/// by Cosmos SQL role assignments, entirely separate from ARM RBAC — a
/// principal can see an account via `az cosmosdb list` and still get a 403
/// reading its data until this role (or better) is granted on the account.
const DATA_READER_ROLE_ID: &str = "00000000-0000-0000-0000-000000000001";

/// The signed-in user's Entra object id — needed as the `principal-id` for
/// a Cosmos SQL role assignment.
fn signed_in_principal_id() -> Result<String, String> {
    let output = az_command(&[
        "ad",
        "signed-in-user",
        "show",
        "--query",
        "id",
        "--output",
        "tsv",
    ])
    .output()
    .map_err(|e| format!("az ad signed-in-user show failed: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Grants the signed-in user Cosmos DB Built-in Data Reader on `account`,
/// so subsequent data-plane queries (via `cosmos.rs`) stop 403'ing.
pub fn grant_self_cosmos_data_reader(account: &CosmosAccount) -> Result<(), String> {
    let principal_id = signed_in_principal_id()?;
    let scope = format!(
        "/subscriptions/{}/resourceGroups/{}/providers/Microsoft.DocumentDB/databaseAccounts/{}",
        account.subscription_id, account.resource_group, account.name,
    );
    let output = az_command(&[
        "cosmosdb",
        "sql",
        "role",
        "assignment",
        "create",
        "--subscription",
        &account.subscription_id,
        "--resource-group",
        &account.resource_group,
        "--account-name",
        &account.name,
        "--role-definition-id",
        DATA_READER_ROLE_ID,
        "--principal-id",
        &principal_id,
        "--scope",
        &scope,
    ])
    .output()
    .map_err(|e| format!("az cosmosdb sql role assignment create failed: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The failure that started this: a wall of AADSTS text that means one
    /// simple thing, and that the app used to discard entirely.
    #[test]
    fn an_expired_refresh_token_is_recognised_and_summarised() {
        let stderr = "ERROR: AADSTS70043: The refresh token has expired or is invalid \
                      due to sign-in frequency checks by conditional access. The token \
                      was issued on 2026-08-27T15:20:56Z and the maximum allowed \
                      lifetime for this request is 36000.\n\
                      Run the command below to authenticate interactively:\n\
                      az logout\naz login --tenant \"...\"";
        let (message, expired) = azure_error_summary(stderr);
        assert!(expired);
        assert!(message.contains("session expired"), "got: {message}");
        // The trace ids and the az logout advice are noise to the user.
        assert!(!message.contains("AADSTS"), "got: {message}");
        assert!(!message.contains("az logout"), "got: {message}");
    }

    #[test]
    fn other_failures_keep_their_own_first_line() {
        let (message, expired) = azure_error_summary(
            "ERROR: (AuthorizationFailed) The client does not have authorization \
             to perform action over scope.\nmore detail here",
        );
        assert!(!expired, "a permissions problem is not an expired session");
        assert!(
            message.starts_with("(AuthorizationFailed)"),
            "got: {message}"
        );
        assert!(!message.contains("more detail"), "one line only");
    }

    #[test]
    fn an_empty_stderr_still_yields_something_printable() {
        let (message, expired) = azure_error_summary("   \n  ");
        assert!(!expired);
        assert_eq!(message, "unknown error");
    }

    /// The distinction the whole fix rests on: an empty result from
    /// subscriptions that all answered means something, and an empty result
    /// from subscriptions that could not be read means nothing.
    #[test]
    fn an_unreadable_scan_is_not_an_empty_one() {
        let failed = AccountScan {
            accounts: vec![],
            errors: vec![SubscriptionError {
                name: "prod".into(),
                id: "sub-1".into(),
                message: "session expired".into(),
                expired: true,
            }],
        };
        assert!(failed.blind());
        assert!(failed.any_expired());

        let genuinely_empty = AccountScan::default();
        assert!(!genuinely_empty.blind());
        assert!(!genuinely_empty.any_expired());
    }

    /// One subscription failing must not hide the accounts found in others,
    /// but it must still be reported.
    #[test]
    fn a_partial_scan_reports_both_halves() {
        let partial = AccountScan {
            accounts: vec![CosmosAccount {
                name: "cosmos-a".into(),
                resource_group: "rg".into(),
                endpoint: "https://a.documents.azure.com:443/".into(),
                subscription_id: "sub-1".into(),
            }],
            errors: vec![SubscriptionError {
                name: "locked".into(),
                id: "sub-2".into(),
                message: "PIM not activated".into(),
                expired: false,
            }],
        };
        assert!(
            !partial.blind(),
            "something was found, so this is not blind"
        );
        assert!(!partial.any_expired());
        assert_eq!(partial.errors.len(), 1, "the skipped one is still reported");
    }
}
