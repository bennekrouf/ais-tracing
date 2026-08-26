mod screens;
mod services;
mod update_check;

use dioxus::desktop::LogicalSize;
use dioxus::prelude::*;
use screens::{home::Home, welcome::Welcome};
use services::az::CosmosAccount;

const MAIN_CSS: &str = include_str!("../assets/main.css");

fn main() {
    if std::env::var("RUST_LOG").is_err() {
        // SAFETY: single-threaded, before any other threads (e.g. tokio) start.
        unsafe {
            std::env::set_var("RUST_LOG", "info,hyper_util=warn,hyper=warn,reqwest=warn");
        }
    }

    let webview_data_dir = dirs::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("ais-tracing");

    let cfg = dioxus::desktop::Config::new()
        .with_data_directory(webview_data_dir)
        .with_window(
            dioxus::desktop::WindowBuilder::new()
                .with_title(concat!("ais-tracing ", env!("CARGO_PKG_VERSION")))
                .with_inner_size(LogicalSize::new(1100.0, 760.0))
                .with_always_on_top(false),
        );
    dioxus::LaunchBuilder::desktop().with_cfg(cfg).launch(App);
}

#[component]
fn App() -> Element {
    let mut account = use_signal(|| Option::<CosmosAccount>::None);

    let system_light =
        dark_light::detect().unwrap_or(dark_light::Mode::Dark) != dark_light::Mode::Dark;
    let is_light = use_signal(|| system_light);

    // ── Auto-update check ──────────────────────────────────────────────────
    // Deliberately after a delay and entirely best-effort: a release check is
    // never worth slowing a cold start, and a failed one is not worth saying
    // anything about.
    let mut update_info = use_signal(|| Option::<update_check::UpdateInfo>::None);
    let mut update_dismissed = use_signal(|| false);
    use_coroutine(
        move |_rx: dioxus::prelude::UnboundedReceiver<()>| async move {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            if let Some(info) = update_check::check().await {
                update_info.set(Some(info));
            }
        },
    );

    use_effect(move || {
        let css = MAIN_CSS.replace('`', "\\`").replace("${", "\\${");
        document::eval(&format!(
            "if(!document.getElementById('ais-css')){{var s=document.createElement('style');s.id='ais-css';s.textContent=`{}`;document.head.appendChild(s);}}",
            css
        ));
    });

    use_effect(move || {
        let cls = if *is_light.read() { "light" } else { "" };
        document::eval(&format!("document.body.className = '{}';", cls));
    });

    let current = account.read().clone();

    rsx! {
        // Update banner — fixed top, dismissable per session.
        if let (Some(info), false) = (update_info.read().clone(), *update_dismissed.read()) {
            div { class: "update-banner",
                span { class: "update-banner-text",
                    "ais-tracing "
                    strong { "{info.latest_version}" }
                    " is available (you have {env!(\"CARGO_PKG_VERSION\")})."
                }
                a {
                    class: "update-banner-link",
                    href: "{info.release_url}",
                    target: "_blank",
                    "Download"
                }
                button {
                    class: "update-banner-dismiss",
                    onclick: move |_| update_dismissed.set(true),
                    "×"
                }
            }
        }

        match current {
            None => rsx! {
                Welcome {
                    on_connect: move |acc: CosmosAccount| account.set(Some(acc)),
                }
            },
            Some(acc) => rsx! {
                // The theme signal is owned here so it also covers Welcome;
                // Home only needs it to render the toggle.
                Home {
                    account: acc,
                    is_light: is_light,
                    on_back: move |_| account.set(None),
                }
            },
        }
    }
}
