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

pub fn check_login() -> AzLoginState {
    let out = az_command(&["account", "show", "--output", "json"]).output();
    match out {
        Ok(out) if out.status.success() => {
            let body = String::from_utf8_lossy(&out.stdout);
            match serde_json::from_str::<AzAccount>(&body) {
                Ok(acc) => AzLoginState::LoggedIn {
                    account: acc.name,
                    subscription_id: acc.id,
                },
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

fn list_subscription_ids() -> Result<Vec<String>, String> {
    let output = az_command(&["account", "list", "--query", "[].id", "--output", "json"])
        .output()
        .map_err(|e| format!("az account list failed: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    let body = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&body).map_err(|e| format!("parse: {e}"))
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
pub fn list_cosmos_accounts() -> Result<Vec<CosmosAccount>, String> {
    let sub_ids = list_subscription_ids()?;
    let mut accounts = Vec::new();
    for sub_id in sub_ids {
        let output = az_command(&[
            "cosmosdb",
            "list",
            "--subscription",
            sub_id.as_str(),
            "--query",
            "[].{name:name,resourceGroup:resourceGroup,documentEndpoint:documentEndpoint}",
            "--output",
            "json",
        ])
        .output()
        .map_err(|e| format!("az cosmosdb list failed: {e}"))?;
        if !output.status.success() {
            // A subscription the caller can't read (PIM not activated, etc.)
            // shouldn't block discovery in the others.
            continue;
        }
        let body = String::from_utf8_lossy(&output.stdout);
        let mut found: Vec<CosmosAccount> =
            serde_json::from_str(&body).map_err(|e| format!("parse: {e}"))?;
        for acc in &mut found {
            acc.subscription_id = sub_id.clone();
        }
        accounts.extend(found);
    }
    Ok(accounts)
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
