mod screens;
mod services;
mod update_check;

use dioxus::desktop::LogicalSize;
use dioxus::prelude::*;
use screens::{home::Home, welcome::Welcome};
use services::az::CosmosAccount;

const MAIN_CSS: &str = include_str!("../assets/main.css");

fn webview_data_dir() -> std::path::PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("ais-tracing")
}

fn window_config(title: &str) -> dioxus::desktop::Config {
    dioxus::desktop::Config::new()
        .with_data_directory(webview_data_dir())
        .with_window(
            dioxus::desktop::WindowBuilder::new()
                .with_title(title)
                .with_inner_size(LogicalSize::new(1100.0, 760.0))
                .with_always_on_top(false)
                .with_window_icon(window_icon()),
        )
}

/// The window icon, decoded from the embedded logo.
///
/// build.rs embeds `assets/icon.ico` into the .exe resource, which covers the
/// Start menu and shortcuts — but the *window* (title bar, alt-tab, taskbar
/// button) shows only what the app sets at runtime, and Windows falls back to
/// a blank default when it sets nothing. That was the whole gap against
/// ais-runner, which sets one here.
///
/// Downscaled to 64px on the way in: tao hands Windows this single bitmap for
/// every size it needs, and letting it stretch a 1024px source down to a 16px
/// title bar is what makes the icon look muddy.
fn window_icon() -> Option<dioxus::desktop::tao::window::Icon> {
    const ICON_PNG: &[u8] = include_bytes!("../assets/icon.png");
    const SIZE: u32 = 64;

    let img = image::load_from_memory(ICON_PNG).ok()?.resize_exact(
        SIZE,
        SIZE,
        image::imageops::FilterType::Lanczos3,
    );
    dioxus::desktop::tao::window::Icon::from_rgba(img.into_rgba8().into_raw(), SIZE, SIZE).ok()
}

/// Opens another window on `account`, in this same process.
///
/// One process, many windows — not many processes. Two copies of the binary
/// would race each other on cached history/config files. Windows inside one
/// process share none of that: each gets its own VirtualDom and its own
/// signals, and the OS sees a single app.
pub fn open_in_new_window(account: CosmosAccount) {
    let name = account.name.clone();
    let dom = VirtualDom::new_with_props(
        WindowRoot,
        WindowRootProps {
            initial: Some(account),
        },
    );
    dioxus::desktop::window().new_window(
        dom,
        window_config(&format!(
            "ais-tracing {} — {}",
            env!("CARGO_PKG_VERSION"),
            name
        )),
    );
}

fn main() {
    // Before anything can shell out: a bundle launched from Finder does not
    // inherit the terminal's PATH, so `az` would read as "not found on PATH".
    services::env::adopt_login_path();

    if std::env::var("RUST_LOG").is_err() {
        // SAFETY: single-threaded, before any other threads (e.g. tokio) start.
        unsafe {
            std::env::set_var("RUST_LOG", "info,hyper_util=warn,hyper=warn,reqwest=warn");
        }
    }

    let cfg = window_config(concat!("ais-tracing ", env!("CARGO_PKG_VERSION")));
    dioxus::LaunchBuilder::desktop().with_cfg(cfg).launch(App);
}

/// The first window's root. Every other window is a `WindowRoot` too — this
/// exists only because the launcher needs a component that takes no props.
#[component]
fn App() -> Element {
    rsx! { WindowRoot { initial: Option::<CosmosAccount>::None } }
}

/// One window. Each has its own VirtualDom, so its own signals, its own theme
/// state and its own open account — and its own welcome screen to go back to.
#[component]
fn WindowRoot(initial: Option<CosmosAccount>) -> Element {
    let mut account = use_signal(|| initial);

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
            None => rsx! { Welcome {} },
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
