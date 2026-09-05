use crate::services::az::{self, AzLoginState};
use crate::services::history;
use dioxus::prelude::*;

/// Sign-in + Cosmos account picker. Same shape as ais-monitor's `Welcome`
/// screen: check `az login`, offer to connect if not signed in, then let
/// the user pick the resource to work with — here a Cosmos DB account
/// instead of a Logic App. Connecting always opens a new window; this one
/// stays put as the launcher.
/// Opens `az login` and waits for it to take effect.
///
/// `az login` is spawned, not awaited — the sign-in happens in a browser, so
/// the app only learns it worked by asking again. Without this poll the screen
/// keeps saying "session expired" after a successful sign-in, with no way
/// forward: the expired path had the button but not the waiting, so it looked
/// broken precisely when the user had done the right thing.
fn start_login<F>(
    mut az_state: Signal<AzLoginState>,
    mut checking: Signal<bool>,
    mut login_error: Signal<Option<String>>,
    mut load_accounts: F,
) where
    F: FnMut() + Copy + 'static,
{
    login_error.set(None);
    match az::open_login() {
        Ok(()) => {
            checking.set(true);
            spawn(async move {
                // Two minutes: long enough for a browser sign-in with MFA,
                // short enough that an abandoned one stops asking.
                for _ in 0..24 {
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    let state = tokio::task::spawn_blocking(az::check_login)
                        .await
                        .unwrap_or(AzLoginState::NotLoggedIn);
                    let done = matches!(state, AzLoginState::LoggedIn { .. });
                    az_state.set(state);
                    checking.set(false);
                    if done {
                        load_accounts();
                        break;
                    }
                }
            });
        }
        Err(e) => login_error.set(Some(e)),
    }
}

#[component]
pub fn Welcome() -> Element {
    let mut az_state = use_signal(|| AzLoginState::AzNotFound);
    let mut checking = use_signal(|| true);
    // The whole scan, kept intact: `blind()` and `any_expired()` are the two
    // questions the screen has to answer, and re-deriving them from the parts
    // is how the two drift.
    let mut scan = use_signal(az::AccountScan::default);
    let mut accounts_error = use_signal(|| Option::<String>::None);
    let mut loading_accounts = use_signal(|| false);
    // Accounts are identified by endpoint, never by display name: two
    // subscriptions can hold accounts called the same thing, and picking the
    // first match by name silently opens the wrong one.
    let mut selected_endpoint = use_signal(String::new);
    let login_error = use_signal(|| Option::<String>::None);
    let mut recent = use_signal(history::load_accounts);

    let mut load_accounts = move || {
        loading_accounts.set(true);
        spawn(async move {
            match tokio::task::spawn_blocking(az::list_cosmos_accounts).await {
                Ok(Ok(found)) => {
                    scan.set(found);
                    accounts_error.set(None);
                }
                Ok(Err(e)) => {
                    scan.set(az::AccountScan::default());
                    accounts_error.set(Some(e));
                }
                Err(e) => {
                    scan.set(az::AccountScan::default());
                    accounts_error.set(Some(e.to_string()));
                }
            }
            loading_accounts.set(false);
        });
    };

    use_effect(move || {
        spawn(async move {
            let state = tokio::task::spawn_blocking(az::check_login)
                .await
                .unwrap_or(AzLoginState::NotLoggedIn);
            let is_logged_in = matches!(state, AzLoginState::LoggedIn { .. });
            az_state.set(state);
            checking.set(false);
            if is_logged_in {
                load_accounts();
            }
        });
    });

    let is_logged_in = matches!(*az_state.read(), AzLoginState::LoggedIn { .. });
    let list = scan.read().accounts.clone();
    let skipped = scan.read().errors.clone();
    // Reported by the scan, which counted the subscriptions it enumerated —
    // including the ones that answered and simply held nothing.
    let subscription_count = scan.read().subscriptions;
    let blind = scan.read().blind();
    let any_expired = scan.read().any_expired();
    let can_connect = !selected_endpoint.read().is_empty();

    rsx! {
        div { class: "welcome",
            div { class: "welcome-card",
                h1 { "AIS Tracing" }
                p { class: "subtitle", "Azure Cosmos DB — correlation-key flow explorer" }

                div { class: "welcome-box",
                    div { class: "welcome-pick",
                        if *checking.read() {
                            div { class: "az-status",
                                span { class: "dot pulse" }
                                span { "Checking Azure login..." }
                            }
                        } else {
                            match &*az_state.read() {
                                AzLoginState::LoggedIn { account, .. } => rsx! {
                                    div { class: "az-status",
                                        span { class: "dot ok" }
                                        span { "Connected: {account}" }
                                    }
                                },
                                AzLoginState::Expired { account, message } => rsx! {
                                    div { class: "az-status",
                                        span { class: "dot error" }
                                        span { "Session expired: {account}" }
                                        button {
                                            class: "btn-primary",
                                            onclick: move |_| start_login(
                                                az_state,
                                                checking,
                                                login_error,
                                                load_accounts,
                                            ),
                                            "Sign in again"
                                        }
                                    }
                                    p { class: "az-error", style: "margin-top:8px;", "{message}" }
                                },
                                AzLoginState::AzNotFound => rsx! {
                                    div { class: "az-status",
                                        span { class: "dot error" }
                                        span { "Azure CLI ('az') not found on PATH." }
                                    }
                                },
                                AzLoginState::NotLoggedIn => {
                                    let err = login_error.read().clone();
                                    rsx! {
                                        div { class: "az-status",
                                            span { class: "dot error" }
                                            span { "Not signed in" }
                                            button {
                                                class: "btn-primary",
                                                onclick: move |_| start_login(
                                                    az_state,
                                                    checking,
                                                    login_error,
                                                    load_accounts,
                                                ),
                                                "Connect to Azure"
                                            }
                                        }
                                        if let Some(e) = err {
                                            p { style: "margin-top:8px; font-size:12px; color:var(--red);", "{e}" }
                                        }
                                    }
                                },
                            }
                        }
                    }

                    if is_logged_in {
                        div { class: "az-form",
                            h3 { style: "font-size:11px; color:var(--text2); text-transform:uppercase; letter-spacing:0.06em; text-align:left;",
                                "Cosmos DB account"
                            }
                            if *loading_accounts.read() {
                                div { class: "az-loading", "Discovering Cosmos DB accounts across your subscriptions..." }
                            } else if let Some(e) = accounts_error.read().clone() {
                                div { class: "az-error", "{e}" }
                            } else if list.is_empty() {
                                // "Nothing found" and "nothing could be read"
                                // are different claims. Only make the first
                                // one when every subscription actually answered.
                                if blind {
                                    div { class: "az-error",
                                        "Could not read {skipped.len()} of "
                                        "{subscription_count} subscriptions, so this is not "
                                        "an answer about whether you have Cosmos DB accounts."
                                    }
                                } else {
                                    div { class: "az-hint",
                                        "No Cosmos DB accounts in any of your "
                                        "{subscription_count} subscriptions."
                                    }
                                }
                            } else {
                                div { class: "az-field",
                                    select {
                                        onchange: move |evt| selected_endpoint.set(evt.value()),
                                        option { value: "", selected: selected_endpoint.read().is_empty(), "-- choose an account --" }
                                        for acc in list.iter() {
                                            option {
                                                value: "{acc.endpoint}",
                                                "{acc.name}  ({acc.resource_group})"
                                            }
                                        }
                                    }
                                }
                            }
                            if !skipped.is_empty() {
                                div { class: "skipped-list",
                                    if any_expired {
                                        div { class: "az-error",
                                            "Your Azure session has expired. Sign in again and rescan — "
                                            "until then these subscriptions cannot be read at all."
                                            button {
                                                class: "btn-primary",
                                                style: "margin-left:10px;",
                                                // Through `start_login`, not a bare
                                                // `open_login`: without the poll the
                                                // screen keeps saying "expired" after a
                                                // successful sign-in, which is the exact
                                                // bug `start_login` was written for.
                                                onclick: move |_| start_login(
                                                    az_state,
                                                    checking,
                                                    login_error,
                                                    load_accounts,
                                                ),
                                                "Sign in again"
                                            }
                                        }
                                    }
                                    for e in skipped.iter() {
                                        div { class: "skipped-row",
                                            span { class: "dot error" }
                                            span { class: "skipped-name", "{e.name}" }
                                            span { class: "skipped-why", "{e.message}" }
                                        }
                                    }
                                }
                            }

                            div { class: "az-form-actions",
                                button {
                                    class: "btn-primary",
                                    disabled: !can_connect,
                                    onclick: {
                                        let list = list.clone();
                                        move |_| {
                                            let endpoint = selected_endpoint.read().clone();
                                            if let Some(acc) = list.iter().find(|a| a.endpoint == endpoint).cloned() {
                                                recent.set(history::record_account(&acc));
                                                crate::open_in_new_window(acc);
                                            }
                                        }
                                    },
                                    "Connect →"
                                }
                            }
                        }
                    }

                    // Recently opened accounts. Shown even when signed out —
                    // knowing what you last worked on is useful before you can
                    // act on it — but only openable once `az` has a session.
                    if !recent.read().is_empty() {
                        div { class: "profile-section",
                            h3 { "Recent" }
                            for acc in recent.read().iter() {
                                div { class: "profile-item",
                                    div { class: "profile-main",
                                        div { class: "profile-label", "{acc.name}" }
                                        div { class: "profile-sub", "{acc.resource_group}" }
                                    }
                                    div { class: "profile-actions",
                                        button {
                                            class: "btn btn-open btn-small",
                                            title: if is_logged_in { "Open this account" } else { "Sign in first" },
                                            disabled: !is_logged_in,
                                            onclick: {
                                                let acc = acc.clone();
                                                move |_| {
                                                    recent.set(history::record_account(&acc));
                                                    crate::open_in_new_window(acc.clone());
                                                }
                                            },
                                            "Open →"
                                        }
                                        button {
                                            class: "btn btn-small",
                                            title: "Forget this account",
                                            onclick: {
                                                let endpoint = acc.endpoint.clone();
                                                move |_| recent.set(history::forget_account(&endpoint))
                                            },
                                            "✕"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
