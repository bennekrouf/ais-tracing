use crate::screens::trace_view::TraceView;
use crate::services::{az::CosmosAccount, cache, cosmos, discover, history, trace};
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
    /// Nothing could be sampled at all. Individual containers that failed are
    /// in `scan_errors` instead — the scan still succeeded around them.
    Failed(cosmos::DataError),
}

#[derive(Props, Clone, PartialEq)]
pub struct HomeProps {
    pub account: CosmosAccount,
    /// Owned by the root App so the theme also applies to Welcome.
    pub is_light: Signal<bool>,
    pub on_back: EventHandler<()>,
}

#[component]
pub fn Home(props: HomeProps) -> Element {
    let account = props.account.clone();
    let mut is_light = props.is_light;
    // Tracing is the job; setup is the thing you do once. Land on the work.
    let mut tab = use_signal(|| Tab::Trace);
    let mut state = use_signal(|| LoadState::Idle);
    let mut schemas = use_signal(Vec::<cosmos::ContainerSchema>::new);
    let mut granting = use_signal(|| false);
    // A refresh running behind cached data, and how old that data is.
    let mut refreshing = use_signal(|| false);
    let mut refresh_error = use_signal(|| Option::<String>::None);
    let mut scanned_at = use_signal(|| Option::<i64>::None);
    // Containers the scan could not read. Kept apart from `LoadState::Failed`:
    // these are gaps in an otherwise usable account, not a failed scan.
    let mut scan_errors = use_signal(Vec::<cosmos::ContainerError>::new);

    // What the user is tracing on: the key that links steps, the field that
    // orders them, the field that names them.
    let mut key_id = use_signal(String::new);
    let mut time_id = use_signal(String::new);
    let mut label_id = use_signal(String::new);
    // Empty = one lane per Cosmos container, the physical default.
    let mut lane_id = use_signal(String::new);

    let traced = use_signal(|| Option::<trace::Trace>::None);
    let tracing = use_signal(|| false);
    let mut selected_block = use_signal(|| Option::<String>::None);
    let mut recent = use_signal({
        let endpoint = account.endpoint.clone();
        move || history::load(&endpoint)
    });
    let mut rules = use_signal({
        let endpoint = account.endpoint.clone();
        move || history::load_rules(&endpoint)
    });

    let insights = use_memo(move || discover::analyze(&schemas.read()));
    // Every scalar field in the account, for the error-rule picker. Recomputed
    // with the scan rather than on every render — the picker has a filter box,
    // and rebuilding the whole list per keystroke is the one thing it must not
    // do.
    let scalar_fields = use_memo(move || discover::scalar_fields(&schemas.read()));

    // `background` means we already have cached schemas on screen: refresh
    // without blanking them, and keep them if the refresh fails.
    let run_scan = {
        let account = account.clone();
        move |background: bool| {
            let account = account.clone();
            if !background {
                state.set(LoadState::Loading);
                schemas.set(Vec::new());
            }
            refreshing.set(true);
            refresh_error.set(None);
            spawn(async move {
                match cosmos::scan_account(&account.endpoint).await {
                    Ok(found) => {
                        cache::save(&account.endpoint, &found.schemas);
                        scanned_at.set(Some(chrono::Utc::now().timestamp()));
                        scan_errors.set(found.errors);
                        schemas.set(found.schemas);
                        state.set(LoadState::Done);
                    }
                    Err(e) => {
                        if background {
                            // Cached data is still on screen and still useful;
                            // say the refresh failed rather than discarding it.
                            refresh_error.set(Some(e.message));
                        } else {
                            state.set(LoadState::Failed(e));
                        }
                    }
                }
                refreshing.set(false);
            });
        }
    };

    // Granting Data Reader, shared by the two places that offer it: the
    // screen shown when nothing could be read at all, and the panel listing
    // individual containers that were refused.
    let grant_reader = {
        let account = account.clone();
        let run_scan = run_scan.clone();
        move |_| {
            let account = account.clone();
            let mut run_scan = run_scan.clone();
            granting.set(true);
            spawn(async move {
                let result = tokio::task::spawn_blocking(move || {
                    crate::services::az::grant_self_cosmos_data_reader(&account)
                })
                .await;
                granting.set(false);
                match result {
                    Ok(Ok(())) => {
                        // The assignment takes a moment to propagate before a
                        // fresh query sees it.
                        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                        run_scan(true);
                    }
                    Ok(Err(e)) => refresh_error.set(Some(format!("Grant failed: {e}"))),
                    Err(e) => refresh_error.set(Some(format!("Grant failed: {e}"))),
                }
            });
        }
    };

    // Open on the last scan if we have one, so the window is usable
    // immediately, then refresh behind it. Otherwise this is a cold start.
    use_effect({
        let endpoint = account.endpoint.clone();
        let mut run_scan = run_scan.clone();
        move || {
            let cached = cache::load(&endpoint);
            let warm = cached.is_some();
            if let Some(cached) = cached {
                scanned_at.set(Some(cached.scanned_at));
                schemas.set(cached.schemas);
                state.set(LoadState::Done);
            }
            run_scan(warm);
        }
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

    let follow = Follow {
        insights,
        key_id,
        time_id,
        label_id,
        recent,
        traced,
        tracing,
        selected: selected_block,
    };

    // Type-ahead over correlation values. Each lookup is a cross-container
    // scan, so it is debounced and only runs past a minimum length — firing
    // one per keystroke would be genuinely expensive in RUs.
    let mut typed = use_signal(String::new);
    let mut suggestions = use_signal(Vec::<String>::new);
    let mut suggesting = use_signal(|| false);
    use_effect({
        let endpoint = account.endpoint.clone();
        move || {
            let text = typed.read().trim().to_string();
            if text.len() < trace::MIN_FRAGMENT {
                suggestions.set(Vec::new());
                suggesting.set(false);
                return;
            }
            let Some(key) = insights
                .peek()
                .keys
                .iter()
                .find(|c| c.id == *key_id.peek())
                .cloned()
            else {
                return;
            };
            let containers = insights.peek().containers.clone();
            let endpoint = endpoint.clone();
            suggesting.set(true);
            spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(350)).await;
                // Superseded while waiting — drop it without querying.
                if *typed.peek() != text {
                    return;
                }
                let found = trace::suggest(&endpoint, &containers, &key, &text).await;
                // And again on return: a slow query must not overwrite the
                // results of a newer, faster one.
                if *typed.peek() != text {
                    return;
                }
                suggestions.set(found);
                suggesting.set(false);
            });
        }
    });

    // Land on something rather than an empty box: once the scan has settled on
    // a key, replay the value traced last. Guarded so it happens once per
    // session — re-running it on every rescan would fight the user.
    let mut autoloaded = use_signal(|| false);
    use_effect({
        let endpoint = account.endpoint.clone();
        move || {
            let ready = matches!(&*state.read(), LoadState::Done) && !key_id.read().is_empty();
            if !ready || *autoloaded.peek() {
                return;
            }
            // `peek` deliberately: `run` writes to `recent`, and subscribing
            // here would loop.
            let Some(last) = recent.peek().first().cloned() else {
                autoloaded.set(true);
                return;
            };
            autoloaded.set(true);
            follow.run(&endpoint, last.value);
        }
    });

    // Whether to offer the role grant: either the whole scan was refused, or
    // individual containers were. The status code decides it — matching on
    // the rendered message caught any error whose text mentioned the number.
    let is_forbidden = matches!(&*state.read(), LoadState::Failed(e) if e.forbidden())
        || scan_errors.read().iter().any(|e| e.forbidden);

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
    let open_doc = selected_block
        .read()
        .clone()
        .filter(|_| !on_setup)
        .and_then(|id| {
            traced
                .read()
                .as_ref()
                // The card's own container, not the lane — once lanes are field
                // values the two are different things.
                .and_then(|t| {
                    t.find_block(&id)
                        .map(|(_, b)| (b.container.clone(), b.clone()))
                })
        });

    rsx! {
        div { class: "app-shell",
        div { class: "topbar",
            // Leftmost, same as ais-monitor: back is navigation, not an action
            // on the current view.
            button {
                class: "btn btn-back",
                onclick: move |_| props.on_back.call(()),
                "‹ Back"
            }
            h1 { "AIS Tracing" }
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

            // Say where the data came from and how old it is, so cached
            // content is never mistaken for a fresh read.
            if *refreshing.read() {
                span { class: "scan-state", span { class: "dot pulse" } "refreshing…" }
            } else if let Some(e) = refresh_error.read().clone() {
                span { class: "scan-state stale", title: "{e}",
                    span { class: "dot error" }
                    "refresh failed — showing cached"
                }
            } else if let Some(at) = *scanned_at.read() {
                span { class: "scan-state", "sampled {cache::age(at)}" }
            }

            button {
                class: "btn",
                disabled: *refreshing.read(),
                onclick: {
                    let mut run_scan = run_scan.clone();
                    // Explicit rescan keeps the cached view up while it runs —
                    // blanking the screen on a manual refresh is a regression
                    // from just leaving it there.
                    move |_| run_scan(true)
                },
                "↻ Rescan"
            }

            // Far right, after Rescan. The glyph shows what you'd switch *to*,
            // not what you're in.
            button {
                class: "btn-theme",
                title: if *is_light.read() { "Switch to dark mode" } else { "Switch to light mode" },
                onclick: move |_| {
                    let next = !*is_light.peek();
                    is_light.set(next);
                },
                if *is_light.read() { "🌙" } else { "☀️" }
            }
        }

        div { class: "screen-split",
        div { class: "main-screen",
            match &*state.read() {
                LoadState::Idle => rsx! {},
                LoadState::Loading => rsx! {
                    div { class: "panel", "Sampling containers..." }
                },
                LoadState::Failed(e) => {
                    let message = e.message.clone();
                    rsx! {
                        div { class: "panel",
                            div { class: "az-error", "Scan failed: {message}" }
                            if is_forbidden {
                                p { style: "font-size:12px; color:var(--text2); margin-top:8px;",
                                    "Cosmos data-plane access is governed by a separate SQL role assignment from ARM RBAC. "
                                    "Grant yourself Built-in Data Reader on this account and retry."
                                }
                                button {
                                    class: "btn",
                                    style: "margin-top:8px;",
                                    disabled: *granting.read(),
                                    onclick: grant_reader,
                                    if *granting.read() { "Granting..." } else { "Grant myself Data Reader" }
                                }
                            }
                        }
                    }
                },
                LoadState::Done => {
                    let key = insights
                        .read()
                        .keys
                        .iter()
                        .find(|c| c.id == *key_id.read())
                        .cloned();
                    rsx! {
                        div {
                            // Containers the scan could not read. Shown on both
                            // tabs: a gap here changes what every lane means.
                            if !scan_errors.read().is_empty() {
                                SkippedContainers {
                                    errors: scan_errors,
                                    granting: *granting.read(),
                                    on_grant: grant_reader,
                                    offer_grant: is_forbidden,
                                }
                            }

                            if on_setup {
                                if !schemas.read().is_empty() {
                                    TraceSetup {
                                        insights,
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

                                ErrorRules {
                                    fields: scalar_fields,
                                    schemas,
                                    rules: rules.read().clone(),
                                    on_change: {
                                        let endpoint = account.endpoint.clone();
                                        move |next: Vec<trace::ErrorRule>| {
                                            history::save_rules(&endpoint, &next);
                                            rules.set(next);
                                        }
                                    },
                                }

                                ContainerList {
                                    schemas,
                                    trace_key: key.clone(),
                                }
                            } else if let Some(key) = key.clone() {
                                FollowValue {
                                    disabled: *tracing.read(),
                                    key_label: key.label.clone(),
                                    recent: recent.read().clone(),
                                    suggestions: suggestions.read().clone(),
                                    suggesting: *suggesting.read(),
                                    on_typed: move |v: String| typed.set(v),
                                    on_clear: {
                                        let endpoint = account.endpoint.clone();
                                        move |_| recent.set(history::clear(&endpoint))
                                    },
                                    on_follow: {
                                        let endpoint = account.endpoint.clone();
                                        move |value: String| follow.run(&endpoint, value)
                                    },
                                }
                            }

                            if !on_setup {
                                if *tracing.read() {
                                    div { class: "panel", "Following the value across containers..." }
                                } else if let Some(t) = shown.read().clone() {
                                    TraceView {
                                        trace: t,
                                        rules: rules.read().clone(),
                                        on_pick_value: {
                                            let endpoint = account.endpoint.clone();
                                            move |v: String| follow.run(&endpoint, v)
                                        },
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

/// Everything needed to start a trace, bundled so the button and the
/// auto-load on startup share one code path instead of drifting apart.
/// Signals and memos are `Copy`, so this is too.
#[derive(Clone, Copy)]
struct Follow {
    insights: Memo<discover::Insights>,
    key_id: Signal<String>,
    time_id: Signal<String>,
    label_id: Signal<String>,
    recent: Signal<Vec<history::Entry>>,
    traced: Signal<Option<trace::Trace>>,
    tracing: Signal<bool>,
    selected: Signal<Option<String>>,
}

impl Follow {
    fn run(mut self, endpoint: &str, value: String) {
        let (key, containers, facts) = {
            let insights = self.insights.peek();
            let Some(key) = insights
                .keys
                .iter()
                .find(|c| c.id == *self.key_id.peek())
                .cloned()
            else {
                return;
            };
            let facts = insights
                .labels
                .iter()
                .take(6)
                .map(|c| c.id.clone())
                .collect();
            (key, insights.containers.clone(), facts)
        };

        self.recent.set(history::record(
            endpoint,
            history::Entry {
                value: value.clone(),
                key: key.label.clone(),
            },
        ));

        let spec = trace::TraceSpec {
            key,
            value,
            time_field: self.time_id.peek().clone(),
            label_field: self.label_id.peek().clone(),
            fact_fields: facts,
        };

        let endpoint = endpoint.to_string();
        self.tracing.set(true);
        self.selected.set(None);
        spawn(async move {
            let result = trace::run(&endpoint, &containers, &spec).await;
            self.traced.set(Some(result));
            self.tracing.set(false);
        });
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
    /// The memo itself, not a copy of it: `Insights` carries every candidate,
    /// binding and piece of evidence in the account, and cloning that on every
    /// render was pure waste.
    insights: Memo<discover::Insights>,
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
    let insights = props.insights.read();
    let total = insights.containers.len();
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
                        for c in insights.keys.iter() {
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
                        for c in insights.times.iter() {
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
                        for c in insights.labels.iter() {
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
                        for c in insights.labels.iter() {
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
struct SkippedContainersProps {
    errors: Signal<Vec<cosmos::ContainerError>>,
    granting: bool,
    offer_grant: bool,
    on_grant: EventHandler<MouseEvent>,
}

/// Containers the scan could not read.
///
/// This has to be on screen, not in a log. Every lane state downstream is a
/// claim about the account — `awaiting` says the data has not arrived,
/// `off-path` says the key does not live there — and both are only true of
/// containers that were actually sampled. A container that was skipped
/// silently turns every one of those claims into a guess.
#[component]
fn SkippedContainers(props: SkippedContainersProps) -> Element {
    let errors = props.errors.read();
    rsx! {
        div { class: "panel",
            div { class: "panel-head",
                h3 { "Not sampled" }
                span { class: "chip warn", "{errors.len()} container(s)" }
            }
            p { class: "meta",
                "These could not be read, so nothing below says anything about them."
            }
            div { class: "skipped-list",
                for e in errors.iter() {
                    div { class: "skipped-row",
                        span { class: "dot error" }
                        span { class: "skipped-name", "{e.path}" }
                        span { class: "skipped-why", "{e.message}" }
                    }
                }
            }
            if props.offer_grant {
                p { style: "font-size:12px; color:var(--text2); margin-top:8px;",
                    "Cosmos data-plane access is a separate SQL role assignment from ARM RBAC."
                }
                button {
                    class: "btn",
                    style: "margin-top:8px;",
                    disabled: props.granting,
                    onclick: move |e| props.on_grant.call(e),
                    if props.granting { "Granting..." } else { "Grant myself Data Reader" }
                }
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
    let mut copied = use_signal(|| false);

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
            // The button is a sibling of the scroller, not inside it, so it
            // stays pinned to the corner as the document scrolls.
            div { class: "doc-body",
                pre { class: "doc-json", "{pretty}" }
                button {
                    class: if *copied.read() { "doc-copy done" } else { "doc-copy" },
                    title: "Copy the document as JSON",
                    onclick: {
                        let pretty = pretty.clone();
                        move |_| {
                            // Best-effort: a clipboard we can't reach is not
                            // worth interrupting the user over.
                            if let Ok(mut clipboard) = arboard::Clipboard::new()
                                && clipboard.set_text(pretty.clone()).is_ok()
                            {
                                copied.set(true);
                                spawn(async move {
                                    tokio::time::sleep(std::time::Duration::from_millis(1400))
                                        .await;
                                    copied.set(false);
                                });
                            }
                        }
                    },
                    if *copied.read() { "copied" } else { "copy" }
                }
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
    /// Correlation values containing what's been typed so far.
    suggestions: Vec<String>,
    suggesting: bool,
    on_follow: EventHandler<String>,
    on_typed: EventHandler<String>,
    on_clear: EventHandler<()>,
}

/// The value side of the question: the setup panel chose which field links a
/// flow, this asks which flow.
#[component]
fn FollowValue(props: FollowValueProps) -> Element {
    // Prefilled with the most recent value, which is also the one auto-traced
    // on launch — an empty box above a populated timeline reads as a mismatch.
    let mut value = use_signal(|| {
        props
            .recent
            .first()
            .map(|e| e.value.clone())
            .unwrap_or_default()
    });
    let ready = !value.read().trim().is_empty() && !props.disabled;
    // Suppress the list once the box already holds one of its own suggestions
    // — otherwise picking one leaves a dropdown offering the thing you picked.
    let exact_already = props.suggestions.iter().any(|s| *s == *value.read().trim());

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
                        placeholder: "paste a value, or type part of one…",
                        value: "{value}",
                        oninput: move |e| {
                            value.set(e.value());
                            props.on_typed.call(e.value());
                        },
                        onkeydown: move |e| {
                            if e.key() == Key::Enter && ready {
                                let v = value.read().trim().to_string();
                                if !v.is_empty() {
                                    props.on_follow.call(v);
                                }
                            }
                        },
                    }

                    // Only worth showing while the box holds a fragment: once
                    // it holds a full id, the list is just the id again.
                    if !exact_already && (props.suggesting || !props.suggestions.is_empty()) {
                        div { class: "suggest-list",
                            if props.suggesting && props.suggestions.is_empty() {
                                div { class: "suggest-empty", "searching…" }
                            }
                            for s in props.suggestions.iter() {
                                button {
                                    class: "suggest-row",
                                    onclick: {
                                        let s = s.clone();
                                        move |_| {
                                            value.set(s.clone());
                                            props.on_follow.call(s.clone());
                                        }
                                    },
                                    "{s}"
                                }
                            }
                        }
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
struct ErrorRulesProps {
    fields: Memo<Vec<discover::RoleCandidate>>,
    schemas: Signal<Vec<cosmos::ContainerSchema>>,
    rules: Vec<trace::ErrorRule>,
    on_change: EventHandler<Vec<trace::ErrorRule>>,
}

/// Teaches the app which field values mean failure.
///
/// Nothing here is inferred: `sessionStatus = 3` is meaningful only because
/// someone says it is, and guessing would be worse than not colouring cards
/// at all.
#[component]
fn ErrorRules(props: ErrorRulesProps) -> Element {
    let mut field = use_signal(String::new);
    let mut value = use_signal(String::new);
    let mut filter = use_signal(String::new);
    // Why the last Add did nothing, when it did nothing.
    let mut note = use_signal(String::new);

    let chosen = field.read().clone();

    // A busy account has a hundred-odd scalar fields, which makes an
    // alphabetical dropdown useless for finding one you can already name.
    // Ids are stored lowercased, so the needle matches directly.
    let needle = filter.read().trim().to_lowercase();
    let fields = props.fields.read();
    let matches: Vec<&discover::RoleCandidate> = fields
        .iter()
        // The chosen field always stays in the list: if filtering could drop
        // it, the select would silently fall back to its first option and
        // change the selection out from under the user.
        .filter(|f| needle.is_empty() || f.id.contains(&needle) || f.id == chosen)
        .collect();
    // The values actually sampled for the chosen field, so the common case is
    // clicking one rather than remembering what the codes are.
    let observed = discover::field_values(&props.schemas.read(), &chosen);
    let label = fields
        .iter()
        .find(|f| f.id == chosen)
        .map(|f| f.label.clone())
        .unwrap_or_default();
    let ready = !chosen.is_empty() && !value.read().trim().is_empty();

    // EventHandler is Copy and the rule list is not, so each handler takes its
    // own clone rather than sharing one closure across two call sites.
    let on_change = props.on_change;
    let current = props.rules.clone();

    rsx! {
        div { class: "panel",
            div { class: "panel-head",
                h3 { "Error rules" }
                if !props.rules.is_empty() {
                    span { class: "chip warn", "{props.rules.len()} active" }
                }
            }
            p { class: "meta", "Cards matching any of these are drawn in red." }

            if !props.rules.is_empty() {
                div { class: "rule-list",
                    for rule in props.rules.iter() {
                        span { class: "rule-chip",
                            "{rule.label()}"
                            button {
                                class: "rule-drop",
                                title: "Remove this rule",
                                onclick: {
                                    let (rule, current) = (rule.clone(), current.clone());
                                    move |_| {
                                        let next: Vec<_> = current
                                            .iter()
                                            .filter(|r| **r != rule)
                                            .cloned()
                                            .collect();
                                        on_change.call(next);
                                    }
                                },
                                "✕"
                            }
                        }
                    }
                }
            }

            div { class: "rule-form",
                div { class: "az-field",
                    label { "Field" }
                    input {
                        class: "field-filter",
                        r#type: "text",
                        placeholder: "filter… e.g. status",
                        value: "{filter}",
                        oninput: move |e| filter.set(e.value()),
                    }
                    // An inline list rather than a dropdown: a native select
                    // stays shut while you type in a separate box, so the
                    // filtering would be invisible at the moment it matters.
                    div { class: "field-list",
                        if matches.is_empty() {
                            div { class: "field-empty", "no field matches “{needle}”" }
                        }
                        for f in matches.iter() {
                            button {
                                class: if chosen == f.id { "field-option on" } else { "field-option" },
                                title: "{f.id}",
                                onclick: {
                                    let id = f.id.clone();
                                    move |_| {
                                        field.set(id.clone());
                                        value.set(String::new());
                                        note.set(String::new());
                                    }
                                },
                                span { class: "field-option-name", "{f.label}" }
                                span { class: "field-option-note", "{f.note}" }
                            }
                        }
                    }
                    span { class: "meta",
                        if needle.is_empty() {
                            "{fields.len()} fields"
                        } else {
                            "{matches.len()} of {fields.len()} fields"
                        }
                    }
                }
                div { class: "az-field",
                    label {
                        if label.is_empty() {
                            "Means failure when it equals"
                        } else {
                            "Means failure when {label} equals"
                        }
                    }
                    input {
                        r#type: "text",
                        placeholder: "e.g. 3",
                        value: "{value}",
                        oninput: move |e| value.set(e.value()),
                        onkeydown: {
                            let (current, label) = (current.clone(), label.clone());
                            move |e: KeyboardEvent| {
                                if e.key() == Key::Enter && ready {
                                    commit(
                                        &current,
                                        &mut field,
                                        &mut value,
                                        &label,
                                        on_change,
                                        &mut note,
                                    );
                                }
                            }
                        },
                    }
                }
                button {
                    class: "btn",
                    disabled: !ready,
                    onclick: {
                        let (current, label) = (current.clone(), label.clone());
                        move |_| {
                            commit(&current, &mut field, &mut value, &label, on_change, &mut note)
                        }
                    },
                    "Add rule"
                }
                if !note.read().is_empty() {
                    span { class: "meta", "{note}" }
                }
            }

            if !chosen.is_empty() && !observed.is_empty() {
                div { class: "rule-observed",
                    span { class: "meta", "sampled values:" }
                    for v in observed.iter().take(12) {
                        button {
                            class: "value-chip",
                            onclick: {
                                let v = v.clone();
                                move |_| value.set(v.clone())
                            },
                            "{v}"
                        }
                    }
                }
            }
        }
    }
}

/// Appends the pending field/value as a rule, ignoring duplicates, and clears
/// the value box ready for the next one.
fn commit(
    current: &[trace::ErrorRule],
    field: &mut Signal<String>,
    value: &mut Signal<String>,
    display: &str,
    on_change: EventHandler<Vec<trace::ErrorRule>>,
    note: &mut Signal<String>,
) {
    let field_id = field.peek().clone();
    let text = value.peek().trim().to_string();
    if field_id.is_empty() || text.is_empty() {
        return;
    }
    let rule = trace::ErrorRule {
        field: field_id,
        display: display.to_string(),
        value: text,
    };
    // A duplicate used to be dropped while the box was cleared anyway, which
    // looks exactly like the rule being added. Say what happened instead.
    if current.contains(&rule) {
        note.set(format!("{} is already a rule.", rule.label()));
        return;
    }
    note.set(String::new());
    let mut next = current.to_vec();
    next.push(rule);
    on_change.call(next);
    value.set(String::new());
}

#[derive(Props, Clone, PartialEq)]
struct ContainerListProps {
    schemas: Signal<Vec<cosmos::ContainerSchema>>,
    trace_key: Option<discover::KeyCandidate>,
}

/// The sampled containers. Collapsed by default — with a dozen containers the
/// field tables bury everything else, and the header row already carries what
/// you scan for: whether the key lives there, and how much we saw.
#[component]
fn ContainerList(props: ContainerListProps) -> Element {
    let mut open = use_signal(BTreeSet::<String>::new);
    let schemas = props.schemas.read();
    let paths: Vec<String> = schemas.iter().map(cosmos::ContainerSchema::path).collect();
    let all_open = !paths.is_empty() && paths.iter().all(|p| open.read().contains(p));

    if schemas.is_empty() {
        return rsx! {
            div { class: "panel", "No databases/containers found in this account." }
        };
    }

    rsx! {
        div { class: "panel",
            div { class: "panel-head",
                h3 { "Containers" }
                span { class: "chip muted", "{schemas.len()} sampled" }
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
                for s in schemas.iter() {
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
