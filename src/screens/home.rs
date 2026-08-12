use crate::screens::trace_view::TraceView;
use crate::services::{az::CosmosAccount, cosmos, discover, history, trace};
use dioxus::prelude::*;
use std::collections::BTreeSet;

/// Top-level sections. Setup answers "what are we tracing and where could it
/// be"; Trace answers "where did this one value actually go".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tab {
    Setup,
    Trace,
}

#[derive(Clone, Debug, PartialEq)]
enum LoadState {
    Idle,
    Loading,
    Done,
    Failed(String),
}

#[derive(Props, Clone, PartialEq)]
pub struct HomeProps {
    pub account: CosmosAccount,
    pub on_back: EventHandler<()>,
}

#[component]
pub fn Home(props: HomeProps) -> Element {
    let account = props.account.clone();
    let mut tab = use_signal(|| Tab::Setup);
    let mut state = use_signal(|| LoadState::Idle);
    let mut schemas = use_signal(Vec::<cosmos::ContainerSchema>::new);
    let mut granting = use_signal(|| false);

    // What the user is tracing on: the key that links steps, the field that
    // orders them, the field that names them.
    let mut key_id = use_signal(String::new);
    let mut time_id = use_signal(String::new);
    let mut label_id = use_signal(String::new);
    // Empty = one lane per Cosmos container, the physical default.
    let mut lane_id = use_signal(String::new);

    let mut traced = use_signal(|| Option::<trace::Trace>::None);
    let mut tracing = use_signal(|| false);
    let mut selected_block = use_signal(|| Option::<String>::None);
    let mut recent = use_signal({
        let endpoint = account.endpoint.clone();
        move || history::load(&endpoint)
    });

    let insights = use_memo(move || discover::analyze(&schemas.read()));

    let run_scan = {
        let account = account.clone();
        move || {
            let account = account.clone();
            state.set(LoadState::Loading);
            schemas.set(Vec::new());
            spawn(async move {
                match scan_account(&account.endpoint).await {
                    Ok(found) => {
                        schemas.set(found);
                        state.set(LoadState::Done);
                    }
                    Err(e) => state.set(LoadState::Failed(e)),
                }
            });
        }
    };

    // Kick off automatically on first mount so there's something to look at
    // right after connecting.
    use_effect({
        let mut run_scan = run_scan.clone();
        move || run_scan()
    });

    // Propose the best-evidenced choice for each role, but only as a default:
    // every one stays a plain selection the user can override. A rescan that
    // no longer turns up the chosen field clears it rather than leaving a
    // selection nothing matches.
    use_effect(move || {
        let insights = insights.read();
        reconcile(&mut key_id, insights.keys.iter().map(|c| c.id.as_str()));
        reconcile(&mut time_id, insights.times.iter().map(|c| c.id.as_str()));
        reconcile(&mut label_id, insights.labels.iter().map(|c| c.id.as_str()));
        // Lanes default to containers, so this one only ever needs clearing.
        let lane = lane_id.peek().clone();
        if !lane.is_empty() && !insights.labels.iter().any(|c| c.id == lane) {
            lane_id.set(String::new());
        }
    });

    let is_forbidden = matches!(&*state.read(), LoadState::Failed(e) if e.contains("403"));

    let on_setup = *tab.read() == Tab::Setup;

    // The lane axis is a view of the documents already fetched, so changing it
    // re-renders rather than re-queries — and takes effect immediately on the
    // trace that's on screen.
    let shown = use_memo(move || {
        let lane_field = lane_id.read().clone();
        let expected = discover::field_values(&schemas.read(), &lane_field);
        traced
            .read()
            .as_ref()
            .map(|base| trace::relane(base, &lane_field, &expected))
    });
    let lane_options: Vec<(String, String)> = insights
        .read()
        .labels
        .iter()
        .map(|c| (c.id.clone(), c.label.clone()))
        .collect();

    // The open document, resolved from the selected card each render so it
    // can never drift out of step with the trace behind it. It belongs to the
    // Trace tab, so it folds away with the cards it came from.
    let open_doc = selected_block.read().clone().filter(|_| !on_setup).and_then(|id| {
        traced
            .read()
            .as_ref()
            // The card's own container, not the lane — once lanes are field
            // values the two are different things.
            .and_then(|t| t.find_block(&id).map(|(_, b)| (b.container.clone(), b.clone())))
    });

    rsx! {
        div { class: "app-shell",
        div { class: "topbar",
            h1 { "ais-tracing" }
            span { class: "account-tag", "{account.name}  ({account.resource_group})" }

            div { class: "topbar-tabs",
                div { class: "topbar-group",
                    button {
                        class: if on_setup { "topbar-tab active" } else { "topbar-tab" },
                        title: "Setup — what we trace on, and the containers we found",
                        onclick: move |_| tab.set(Tab::Setup),
                        "⚙"
                    }
                    button {
                        class: if on_setup { "topbar-tab" } else { "topbar-tab active" },
                        title: "Trace — follow one key value across the containers",
                        onclick: move |_| tab.set(Tab::Trace),
                        "🔎"
                    }
                }
            }

            div { class: "spacer" }
            button {
                class: "btn",
                onclick: {
                    let mut run_scan = run_scan.clone();
                    move |_| run_scan()
                },
                "↻ Rescan"
            }
            button { class: "btn-back", onclick: move |_| props.on_back.call(()), "← Back" }
        }

        div { class: "screen-split",
        div { class: "main-screen",
            match &*state.read() {
                LoadState::Idle => rsx! {},
                LoadState::Loading => rsx! {
                    div { class: "panel", "Sampling containers..." }
                },
                LoadState::Failed(e) => {
                    let e = e.clone();
                    rsx! {
                        div { class: "panel",
                            div { class: "az-error", "Scan failed: {e}" }
                            if is_forbidden {
                                p { style: "font-size:12px; color:var(--text2); margin-top:8px;",
                                    "Cosmos data-plane access is governed by a separate SQL role assignment from ARM RBAC. "
                                    "Grant yourself Built-in Data Reader on this account and retry."
                                }
                                button {
                                    class: "btn",
                                    style: "margin-top:8px;",
                                    disabled: *granting.read(),
                                    onclick: {
                                        let account = account.clone();
                                        let run_scan = run_scan.clone();
                                        move |_| {
                                            let account = account.clone();
                                            let mut run_scan = run_scan.clone();
                                            granting.set(true);
                                            spawn(async move {
                                                let result = tokio::task::spawn_blocking(move || {
                                                    crate::services::az::grant_self_cosmos_data_reader(&account)
                                                }).await;
                                                granting.set(false);
                                                match result {
                                                    Ok(Ok(())) => {
                                                        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                                                        run_scan();
                                                    }
                                                    Ok(Err(e)) => state.set(LoadState::Failed(format!("Grant failed: {e}"))),
                                                    Err(e) => state.set(LoadState::Failed(format!("Grant failed: {e}"))),
                                                }
                                            });
                                        }
                                    },
                                    if *granting.read() { "Granting..." } else { "Grant myself Data Reader" }
                                }
                            }
                        }
                    }
                },
                LoadState::Done => {
                    let insights = insights.read().clone();
                    let key = insights
                        .keys
                        .iter()
                        .find(|c| c.id == *key_id.read())
                        .cloned();
                    rsx! {
                        div {
                            if on_setup {
                                if !schemas.read().is_empty() {
                                    TraceSetup {
                                        insights: insights.clone(),
                                        trace_key: key.clone(),
                                        time_id: time_id.read().clone(),
                                        label_id: label_id.read().clone(),
                                        lane_id: lane_id.read().clone(),
                                        on_key: move |v: String| key_id.set(v),
                                        on_time: move |v: String| time_id.set(v),
                                        on_label: move |v: String| label_id.set(v),
                                        on_lane: move |v: String| lane_id.set(v),
                                    }
                                }

                                ContainerList {
                                    schemas: schemas.read().clone(),
                                    trace_key: key.clone(),
                                }
                            } else if let Some(key) = key.clone() {
                                FollowValue {
                                    disabled: *tracing.read(),
                                    key_label: key.label.clone(),
                                    recent: recent.read().clone(),
                                    on_clear: {
                                        let endpoint = account.endpoint.clone();
                                        move |_| recent.set(history::clear(&endpoint))
                                    },
                                    on_follow: {
                                        let containers = insights.containers.clone();
                                        let facts: Vec<String> = insights
                                            .labels
                                            .iter()
                                            .take(6)
                                            .map(|c| c.id.clone())
                                            .collect();
                                        let endpoint = account.endpoint.clone();
                                        move |value: String| {
                                            recent.set(history::record(
                                                &endpoint,
                                                history::Entry {
                                                    value: value.clone(),
                                                    key: key.label.clone(),
                                                },
                                            ));
                                            let spec = trace::TraceSpec {
                                                key: key.clone(),
                                                value,
                                                time_field: time_id.peek().clone(),
                                                label_field: label_id.peek().clone(),
                                                fact_fields: facts.clone(),
                                            };
                                            let endpoint = endpoint.clone();
                                            let containers = containers.clone();
                                            tracing.set(true);
                                            selected_block.set(None);
                                            spawn(async move {
                                                let result =
                                                    trace::run(&endpoint, &containers, &spec).await;
                                                traced.set(Some(result));
                                                tracing.set(false);
                                            });
                                        }
                                    },
                                }
                            }

                            if !on_setup {
                                if *tracing.read() {
                                    div { class: "panel", "Following the value across containers..." }
                                } else if let Some(t) = shown.read().clone() {
                                    TraceView {
                                        trace: t,
                                        lane_id: lane_id.read().clone(),
                                        lane_options: lane_options.clone(),
                                        on_lane: move |v: String| lane_id.set(v),
                                        selected: selected_block.read().clone(),
                                        // Clicking the open card closes the panel.
                                        on_select: move |id: String| {
                                            let same = selected_block.peek().as_deref() == Some(id.as_str());
                                            selected_block.set(if same { None } else { Some(id) });
                                        },
                                    }
                                } else if key.is_none() {
                                    div { class: "az-hint",
                                        "No correlation key chosen yet — pick one in Setup ⚙ first."
                                    }
                                }
                            }

                        }
                    }
                },
            }
        }

        if let Some((container, block)) = open_doc {
            DocPanel {
                container,
                block,
                on_close: move |_| selected_block.set(None),
            }
        }
        }
        }
    }
}

/// Keeps a role selection valid across rescans: default to the top-ranked
/// option, drop it if it no longer exists.
fn reconcile<'a>(selection: &mut Signal<String>, mut options: impl Iterator<Item = &'a str>) {
    let current = selection.peek().clone();
    if current.is_empty() {
        if let Some(best) = options.next() {
            selection.set(best.to_string());
        }
    } else if !options.any(|id| id == current) {
        selection.set(String::new());
    }
}

#[derive(Props, Clone, PartialEq)]
struct TraceSetupProps {
    insights: discover::Insights,
    trace_key: Option<discover::KeyCandidate>,
    time_id: String,
    label_id: String,
    lane_id: String,
    on_key: EventHandler<String>,
    on_time: EventHandler<String>,
    on_label: EventHandler<String>,
    on_lane: EventHandler<String>,
}

/// Asks — right after the scan, off the sampled data — what to trace on.
/// Every option and every ranking comes from the documents themselves, so
/// this reads the same on any schema.
#[component]
fn TraceSetup(props: TraceSetupProps) -> Element {
    let total = props.insights.containers.len();
    let key = props.trace_key.clone();
    let no_key = key.is_none();

    rsx! {
        div { class: "panel key-panel",
            div { class: "panel-head",
                h3 { "What are we tracing?" }
                span { class: "chip muted", "{total} containers sampled" }
            }

            div { class: "role-grid",
                div { class: "az-field",
                    label { "Correlation key — links the steps" }
                    select {
                        onchange: move |evt| props.on_key.call(evt.value()),
                        option { value: "", selected: no_key, "-- choose a field --" }
                        for c in props.insights.keys.iter() {
                            option {
                                value: "{c.id}",
                                selected: key.as_ref().is_some_and(|k| k.id == c.id),
                                {format!(
                                    "{}  —  {}/{} containers{}",
                                    c.label,
                                    c.bindings.len(),
                                    total,
                                    if c.shared_values > 0 { ", values match" } else { "" },
                                )}
                            }
                        }
                    }
                }

                div { class: "az-field",
                    label { "Order by — sequences the steps" }
                    select {
                        onchange: move |evt| props.on_time.call(evt.value()),
                        option { value: "", selected: props.time_id.is_empty(), "-- none --" }
                        for c in props.insights.times.iter() {
                            option {
                                value: "{c.id}",
                                selected: props.time_id == c.id,
                                {format!("{}  —  {}", c.label, c.note)}
                            }
                        }
                    }
                }

                div { class: "az-field",
                    label { "Rows (left index) — one lane per…" }
                    select {
                        onchange: move |evt| props.on_lane.call(evt.value()),
                        option {
                            value: "",
                            selected: props.lane_id.is_empty(),
                            "Cosmos container (default)"
                        }
                        for c in props.insights.labels.iter() {
                            option {
                                value: "{c.id}",
                                selected: props.lane_id == c.id,
                                {format!("{}  —  {}", c.label, c.note)}
                            }
                        }
                    }
                }

                div { class: "az-field",
                    label { "Step label — names each step" }
                    select {
                        onchange: move |evt| props.on_label.call(evt.value()),
                        option { value: "", selected: props.label_id.is_empty(), "-- none --" }
                        for c in props.insights.labels.iter() {
                            option {
                                value: "{c.id}",
                                selected: props.label_id == c.id,
                                {format!("{}  —  {}", c.label, c.note)}
                            }
                        }
                    }
                }
            }

            match props.trace_key.as_ref() {
                Some(c) => rsx! {
                    div { class: "key-summary",
                        div { class: "evidence",
                            for e in c.evidence.iter() {
                                span {
                                    class: if e.good { "chip ok" } else { "chip warn" },
                                    "{e.text}"
                                }
                            }
                        }
                        p { class: "meta",
                            "Reads as: "
                            for (i, b) in c.bindings.iter().enumerate() {
                                if i > 0 { ", " }
                                code { "{b.container}.{b.field}" }
                            }
                        }
                        if !c.missing.is_empty() {
                            p { class: "meta", "Not sampled in: {c.missing.join(\", \")}" }
                        }
                    }
                },
                None => rsx! {
                    div { class: "az-hint",
                        "Pick the field whose value is the same across the steps of one flow. "
                        "Nothing in the sample linked containers on its own."
                    }
                },
            }
        }
    }
}

#[derive(Props, Clone, PartialEq)]
struct DocPanelProps {
    container: String,
    block: trace::Block,
    on_close: EventHandler<()>,
}

/// The full document behind a card. A card can only carry three facts; this
/// is everything else, unflattened — nesting and arrays intact, so what you
/// read is what Cosmos returned.
#[component]
fn DocPanel(props: DocPanelProps) -> Element {
    let pretty = serde_json::to_string_pretty(&props.block.doc)
        .unwrap_or_else(|e| format!("could not render document: {e}"));
    let lines = pretty.lines().count();
    let bytes = pretty.len();

    rsx! {
        aside { class: "doc-panel",
            div { class: "doc-head",
                div { style: "min-width:0;",
                    h3 { "{props.block.label}" }
                    span { class: "doc-source", "{props.container}" }
                }
                button {
                    class: "btn",
                    onclick: move |_| props.on_close.call(()),
                    "✕"
                }
            }
            if !props.block.at_text.is_empty() {
                p { class: "meta", "{props.block.at_text}" }
            }
            div { class: "doc-body",
                pre { class: "doc-json", "{pretty}" }
            }
            p { class: "meta doc-foot", "{lines} lines · {bytes} bytes" }
        }
    }
}

#[derive(Props, Clone, PartialEq)]
struct FollowValueProps {
    key_label: String,
    disabled: bool,
    recent: Vec<history::Entry>,
    on_follow: EventHandler<String>,
    on_clear: EventHandler<()>,
}

/// The value side of the question: the setup panel chose which field links a
/// flow, this asks which flow.
#[component]
fn FollowValue(props: FollowValueProps) -> Element {
    let mut value = use_signal(String::new);
    let ready = !value.read().trim().is_empty() && !props.disabled;

    let submit = move |_| {
        let v = value.read().trim().to_string();
        if !v.is_empty() {
            props.on_follow.call(v);
        }
    };

    rsx! {
        div { class: "panel follow-panel",
            div { class: "follow-row",
                div { class: "az-field",
                    label { "Follow one value of {props.key_label}" }
                    input {
                        r#type: "text",
                        placeholder: "paste a value...",
                        value: "{value}",
                        oninput: move |e| value.set(e.value()),
                        onkeydown: move |e| {
                            if e.key() == Key::Enter && ready {
                                let v = value.read().trim().to_string();
                                if !v.is_empty() {
                                    props.on_follow.call(v);
                                }
                            }
                        },
                    }
                }
                button {
                    class: "btn-primary",
                    disabled: !ready,
                    onclick: submit,
                    if props.disabled { "Following..." } else { "Follow →" }
                }
            }

            if !props.recent.is_empty() {
                div { class: "recent-row",
                    span { class: "recent-label", "Recent" }
                    for e in props.recent.iter() {
                        button {
                            class: "recent-chip",
                            title: "{e.value}\ntraced on {e.key}",
                            disabled: props.disabled,
                            onclick: {
                                let v = e.value.clone();
                                move |_| {
                                    value.set(v.clone());
                                    props.on_follow.call(v.clone());
                                }
                            },
                            "{shorten(&e.value)}"
                        }
                    }
                    div { class: "spacer" }
                    button {
                        class: "recent-clear",
                        title: "Forget these values",
                        onclick: move |_| props.on_clear.call(()),
                        "clear"
                    }
                }
            }
        }
    }
}

/// Correlation ids are too long for a chip. Keep both ends — the tail is
/// usually what distinguishes two ids at a glance.
fn shorten(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= 20 {
        return value.to_string();
    }
    let head: String = chars[..10].iter().collect();
    let tail: String = chars[chars.len() - 6..].iter().collect();
    format!("{head}…{tail}")
}


#[derive(Props, Clone, PartialEq)]
struct ContainerListProps {
    schemas: Vec<cosmos::ContainerSchema>,
    trace_key: Option<discover::KeyCandidate>,
}

/// The sampled containers. Collapsed by default — with a dozen containers the
/// field tables bury everything else, and the header row already carries what
/// you scan for: whether the key lives there, and how much we saw.
#[component]
fn ContainerList(props: ContainerListProps) -> Element {
    let mut open = use_signal(BTreeSet::<String>::new);
    let paths: Vec<String> = props.schemas.iter().map(cosmos::ContainerSchema::path).collect();
    let all_open = !paths.is_empty() && paths.iter().all(|p| open.read().contains(p));

    if props.schemas.is_empty() {
        return rsx! {
            div { class: "panel", "No databases/containers found in this account." }
        };
    }

    rsx! {
        div { class: "panel",
            div { class: "panel-head",
                h3 { "Containers" }
                span { class: "chip muted", "{props.schemas.len()} sampled" }
                div { class: "spacer" }
                button {
                    class: "btn",
                    onclick: move |_| {
                        if all_open {
                            open.write().clear();
                        } else {
                            open.set(paths.iter().cloned().collect());
                        }
                    },
                    if all_open { "Collapse all" } else { "Expand all" }
                }
            }

            div { class: "container-list",
                for s in props.schemas.iter() {
                    ContainerRow {
                        schema: s.clone(),
                        trace_key: props.trace_key.clone(),
                        open: open.read().contains(&s.path()),
                        on_toggle: move |path: String| {
                            let mut open = open.write();
                            if !open.remove(&path) {
                                open.insert(path);
                            }
                        },
                    }
                }
            }
        }
    }
}

#[derive(Props, Clone, PartialEq)]
struct ContainerRowProps {
    schema: cosmos::ContainerSchema,
    trace_key: Option<discover::KeyCandidate>,
    open: bool,
    on_toggle: EventHandler<String>,
}

#[component]
fn ContainerRow(props: ContainerRowProps) -> Element {
    let s = &props.schema;
    let path = s.path();
    let bound = props
        .trace_key
        .as_ref()
        .and_then(|k| k.binding_for(&path))
        .map(|b| b.field.clone());

    rsx! {
        div { class: if props.open { "container-row open" } else { "container-row" },
            div {
                class: "container-head",
                onclick: {
                    let path = path.clone();
                    move |_| props.on_toggle.call(path.clone())
                },
                span { class: "caret", if props.open { "▾" } else { "▸" } }
                span { class: "container-name", "{s.database} / {s.container}" }
                if props.trace_key.is_some() {
                    match &bound {
                        Some(field) => rsx! { span { class: "chip ok", "key: {field}" } },
                        None => rsx! { span { class: "chip muted", "no key" } },
                    }
                }
                div { class: "spacer" }
                span { class: "container-meta",
                    "{s.fields.len()} fields · {s.sampled_docs} sampled"
                }
            }

            if props.open {
                table { class: "schema-table",
                    thead {
                        tr {
                            th { "field" }
                            th { "type(s)" }
                            th { "seen in" }
                            th { "distinct" }
                        }
                    }
                    tbody {
                        for f in s.fields.iter() {
                            tr {
                                class: if bound.as_deref() == Some(f.name.as_str()) { "key-row" } else { "" },
                                td { "{f.name}" }
                                td { "{f.types.join(\", \")}" }
                                td { "{f.seen_in}/{s.sampled_docs}" }
                                td { "{f.distinct}" }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::shorten;

    #[test]
    fn short_values_are_left_alone() {
        assert_eq!(shorten("order-42"), "order-42");
        assert_eq!(shorten(&"x".repeat(20)), "x".repeat(20));
    }

    #[test]
    fn long_ids_keep_both_ends() {
        let uuid = "9f1c2d3e-4a5b-6c7d-8e9f-0a1b2c3d4e5f";
        assert_eq!(shorten(uuid), "9f1c2d3e-4…3d4e5f");
    }

    /// Slicing by byte would panic here; the chip must survive any value the
    /// user pastes.
    #[test]
    fn multibyte_values_do_not_panic() {
        let value = "→".repeat(40);
        assert_eq!(shorten(&value).chars().count(), 17);
    }
}

async fn scan_account(endpoint: &str) -> Result<Vec<cosmos::ContainerSchema>, String> {
    let databases = cosmos::list_databases(endpoint).await?;
    let mut out = Vec::new();
    for db in databases {
        let containers = cosmos::list_containers(endpoint, &db).await?;
        for container in containers {
            let schema = cosmos::infer_container_schema(endpoint, &db, &container).await?;
            out.push(schema);
        }
    }
    Ok(out)
}
