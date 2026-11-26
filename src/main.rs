mod kit;
mod mcp;
mod screens;
mod services;
mod telemetry;

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
/// One process, many windows — not many processes: each window gets its own
/// VirtualDom and its own signals, while the OS sees a single app with one
/// icon and one dock entry.
///
/// It is not what keeps the on-disk state safe, and it was a mistake to write
/// that it was. Nothing stops a second copy of the binary being launched, so
/// the config and cache files have to survive concurrent writers on their own
/// — see `services::atomic_write`.
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
            "AIS Tracing {} — {}",
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

    kit::init();
    // `ais-tracing mcp`: the tools over stdio for an MCP client, and no window.
    if std::env::args().nth(1).as_deref() == Some("mcp") {
        return mcp::serve();
    }

    let cfg = window_config(concat!("AIS Tracing ", env!("CARGO_PKG_VERSION")));
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

    // The update and notice banners are desktop-kit's (see `Banners` below):
    // "Install & restart" where this copy can replace itself.
    let mut connect_open = use_signal(|| false);
    use_context_provider(|| ConnectOpen(connect_open));

    use_effect(move || {
        // Serialised as a JSON string rather than pasted into a template
        // literal. The hand-rolled version escaped backticks and `${` but not
        // backslashes, so the first CSS escape sequence anyone wrote —
        // `content: "\201C"` — would have been eaten by the JS parser instead.
        // JSON string syntax is a subset of JS string syntax, so this is
        // correct for any input.
        let css = serde_json::to_string(MAIN_CSS).unwrap_or_else(|_| "\"\"".to_string());
        document::eval(&format!(
            "if(!document.getElementById('ais-css')){{var s=document.createElement('style');s.id='ais-css';s.textContent={css};document.head.appendChild(s);}}"
        ));
    });

    use_effect(move || {
        let cls = if *is_light.read() { "light" } else { "" };
        document::eval(&format!("document.body.className = '{}';", cls));
    });

    let current = account.read().clone();

    // ── Anonymous usage statistics ─────────────────────────────────────────
    // On by default, but only once the person has been told: `start` reads the
    // opt-outs (including a shell profile's), records the launch, and says
    // whether the notice is still owed. See telemetry.rs.
    let mut ask_consent = use_signal(|| false);
    use_coroutine(
        move |_rx: dioxus::prelude::UnboundedReceiver<()>| async move {
            if telemetry::start().await {
                ask_consent.set(true);
            }
            telemetry::flush_forever().await;
        },
    );
    // Recorded while the notice is on screen, so collection starts from the
    // next event — never from one the person had no chance to read about.
    use_effect(move || {
        if *ask_consent.read() {
            telemetry::mark_informed();
        }
    });

    rsx! {
        // A newer release and the notice from mayorana.ch, at the top.
        // AIS Tracing counts the update events in its own statistics.
        desktop_kit::ui::Banners {
            on_update_offered: move |to| telemetry::record(telemetry::Event::UpdateOffered { to }),
            on_update_clicked: move |to| telemetry::record(telemetry::Event::UpdateClicked { to }),
        }

        if connect_open() {
            desktop_kit::ui::ConnectWindow {
                lead: "Claude can list your Cosmos DB accounts, sample their containers, find the \
                       fields that link documents together and follow a value through them, \
                       the way this window does. It only reads.",
                try_it: "\u{201c}Where did order 12345 get to in my Cosmos DB account?\u{201d}",
                caution: "Claude reads Azure as your signed-in az account, and the documents it \
                          traces go to the AI model you use with it.".to_string(),
                on_close: move |_| connect_open.set(false),
            }
        }

        // Usage-statistics notice: once, at the bottom so it never sits under the
        // update or notice banners. Either button is remembered.
        if *ask_consent.read() {
            div { class: "consent-banner",
                span { class: "update-banner-text",
                    strong { "AIS Tracing shares anonymous usage statistics. " }
                    "Whether it is installed and opened, and its version and operating system \
                     — never your files, data, accounts or anything you type."
                }
                button {
                    class: "update-banner-link",
                    onclick: move |_| {
                        telemetry::set_consent(true);
                        ask_consent.set(false);
                    },
                    "OK"
                }
                button {
                    class: "update-banner-link",
                    onclick: move |_| {
                        telemetry::set_consent(false);
                        ask_consent.set(false);
                    },
                    "Turn off"
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

/// Whether the "Use AIS Tracing from Claude" window is open, shared with the
/// screens so each can put the MCP button in its own bar.
#[derive(Clone, Copy)]
pub struct ConnectOpen(pub Signal<bool>);

/// The MCP button: opens the window that adds AIS Tracing to Claude Desktop,
/// Claude Code, Codex or VS Code.
#[component]
pub fn McpButton(class: String) -> Element {
    let ConnectOpen(mut open) = use_context::<ConnectOpen>();
    rsx! {
        button {
            class: "{class}",
            title: "Use AIS Tracing from Claude Desktop, Claude Code, Codex or VS Code",
            onclick: move |_| open.set(true),
            "MCP"
        }
    }
}
