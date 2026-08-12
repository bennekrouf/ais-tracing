mod screens;
mod services;

use dioxus::desktop::LogicalSize;
use dioxus::prelude::*;
use screens::{home::Home, welcome::Welcome};
use services::az::CosmosAccount;

const MAIN_CSS: &str = include_str!("../assets/main.css");

fn main() {
    if std::env::var("RUST_LOG").is_err() {
        // SAFETY: single-threaded, before any other threads (e.g. tokio) start.
        unsafe { std::env::set_var("RUST_LOG", "info,hyper_util=warn,hyper=warn,reqwest=warn"); }
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

    let system_light = dark_light::detect().unwrap_or(dark_light::Mode::Dark) != dark_light::Mode::Dark;
    let is_light = use_signal(|| system_light);

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
        match current {
            None => rsx! {
                Welcome {
                    on_connect: move |acc: CosmosAccount| account.set(Some(acc)),
                }
            },
            Some(acc) => rsx! {
                Home {
                    account: acc,
                    on_back: move |_| account.set(None),
                }
            },
        }
    }
}
