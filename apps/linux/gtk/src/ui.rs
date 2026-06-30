//! The health panel — an `AdwApplicationWindow` of `AdwPreferencesGroup` rows showing the
//! engine's live state, the GTK4/libadwaita analogue of the macOS `StatusView` / Windows
//! `MainWindow` status tab. Control lives in the MCP, so this is read-only; it just renders
//! the pushed [`Snapshot`].
//!
//! Strings come ONLY from the shared `ds-i18n` catalog via [`crate::ffi::t`] (the same
//! keys the Swift/C# hosts use) — no Linux-local text. State is shown as colored dots (the
//! libadwaita semantic style classes), exactly like the macOS/Windows hosts, never as words.

use adw::prelude::*;
use ds_status::{EngineObj, ModelStatus};

use crate::status::Snapshot;

/// Shorthand for a shared-catalog lookup (the same keys macOS/Windows bind).
fn t(key: &str) -> String {
    crate::ffi::t(key)
}

/// Handles refreshed on each status push. Cloneable (GTK widgets are refcounted).
#[derive(Clone)]
pub struct Widgets {
    pub window: adw::ApplicationWindow,
    /// Engine headline dot (green running / gray idle).
    engine: gtk::Image,
    /// TTS expander — subtitle = engine name, suffix dot = lifecycle; expands to the stat rows.
    tts_row: adw::ExpanderRow,
    tts_dot: gtk::Image,
    tts_runtime: gtk::Label,
    tts_realtime: gtk::Label,
    tts_first: gtk::Label,
    tts_spoken: gtk::Label,
    tts_failures: gtk::Label,
    tts_failures_row: adw::ActionRow,
    /// STT expander + its stat rows.
    stt_row: adw::ExpanderRow,
    stt_dot: gtk::Image,
    stt_runtime: gtk::Label,
    stt_realtime: gtk::Label,
    stt_transcribed: gtk::Label,
    stt_failures: gtk::Label,
    stt_failures_row: adw::ActionRow,
    /// Caps Lock state dot (green active / orange enabled-idle / gray off).
    caps_dot: gtk::Image,
    /// Lifetime totals revealed under the headline (TTS / STT all-time durations).
    spoken: gtk::Label,
    heard: gtk::Label,
}

/// Build the health window (hidden until presented). Returns the window + the value handles.
pub fn build_window(app: &adw::Application) -> Widgets {
    let app_name = {
        let n = t("common.app_name");
        if n.is_empty() || n == "common.app_name" {
            "DontSpeak".to_string()
        } else {
            n
        }
    };

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title(app_name.as_str())
        .default_width(460)
        .default_height(640)
        .build();

    let header = adw::HeaderBar::new();
    // Show ONLY the close button. The documented GTK way is the header bar's decoration-layout
    // (button names split into left`:`right; recognized: minimize, maximize, close, icon, menu),
    // not CSS-hiding individual buttons — ":close" = nothing on the left, close on the right.
    // The window lives in the tray, so its close request just hides it (see main.rs).
    header.set_decoration_layout(Some(":close"));

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(18)
        .margin_top(18)
        .margin_bottom(18)
        .margin_start(18)
        .margin_end(18)
        .build();

    // ── Headline: app name + version, expandable to the lifetime totals; the dot is the only
    //    state indicator. Mirrors the macOS expandable engine row (StatusView.swift). ────────
    let engine_group = adw::PreferencesGroup::new();
    let version = crate::ffi::version();
    let status_row = adw::ExpanderRow::builder()
        .title(app_name.as_str())
        .subtitle(version.as_str())
        .build();
    let engine = expander_indicator(&status_row);

    // Lifetime totals — "TTS all-time" / "STT all-time" (role + lifetime keys), revealed on expand.
    let tts_life = format!("{} {}", t("status.engine.role_tts"), t("status.stats.lifetime_all_time"));
    let spoken = value_label();
    status_row.add_row(&action_row(&tts_life, &spoken));
    let stt_life = format!("{} {}", t("status.engine.role_stt"), t("status.stats.lifetime_all_time"));
    let heard = value_label();
    status_row.add_row(&action_row(&stt_life, &heard));

    engine_group.add(&status_row);
    content.append(&engine_group);

    // ── TTS / STT engine rows: role label + engine-name subtitle + lifecycle dot; expand to
    //    the same detail the macOS/Windows hosts show (runtime, realtime, first-audio, count,
    //    failures), built from the shared `status_fmt` formatters + shared stat snapshots. ────
    let voice_group = adw::PreferencesGroup::new();

    let tts_row = adw::ExpanderRow::builder()
        .title(t("status.engine.role_tts").as_str())
        .build();
    let tts_dot = expander_indicator(&tts_row);
    let tts_runtime = value_label();
    tts_row.add_row(&action_row(&t("status.engine.role_runtime"), &tts_runtime));
    let tts_realtime = value_label();
    tts_row.add_row(&action_row(&t("status.stats.realtime"), &tts_realtime));
    let tts_first = value_label();
    tts_row.add_row(&action_row(&t("status.stats.first_audio"), &tts_first));
    let tts_spoken = value_label();
    tts_row.add_row(&action_row(&t("status.stats.spoken"), &tts_spoken));
    let tts_failures = value_label();
    tts_failures.add_css_class("error");
    let tts_failures_row = action_row(&t("status.stats.failures"), &tts_failures);
    tts_row.add_row(&tts_failures_row);
    voice_group.add(&tts_row);

    let stt_row = adw::ExpanderRow::builder()
        .title(t("status.engine.role_stt").as_str())
        .build();
    let stt_dot = expander_indicator(&stt_row);
    let stt_runtime = value_label();
    stt_row.add_row(&action_row(&t("status.engine.role_runtime"), &stt_runtime));
    let stt_realtime = value_label();
    stt_row.add_row(&action_row(&t("status.stats.realtime"), &stt_realtime));
    let stt_transcribed = value_label();
    stt_row.add_row(&action_row(&t("status.stats.transcribed"), &stt_transcribed));
    let stt_failures = value_label();
    stt_failures.add_css_class("error");
    let stt_failures_row = action_row(&t("status.stats.failures"), &stt_failures);
    stt_row.add_row(&stt_failures_row);
    voice_group.add(&stt_row);

    content.append(&voice_group);

    // ── Caps Lock — pinned to the very bottom; a dot, not text ───────────────────────────────
    let caps_group = adw::PreferencesGroup::new();
    let caps_dot = status_dot();
    let caps_row = adw::ActionRow::builder()
        .title(t("status.caps_lock").as_str())
        .build();
    caps_row.add_suffix(&caps_dot);
    caps_group.add(&caps_row);
    content.append(&caps_group);

    // ── Four views: Status (this panel) / Tools / Log / Credits — an AdwViewStack driven by an
    //    AdwInlineViewSwitcher (libadwaita 1.7's segmented switcher), the GNOME idiom for a
    //    handful of top-level pages (HIG: 3–5 views). The switcher sits centered in the header.
    //    Order matches the macOS + Windows tabs: Log sits before Credits. ─
    let stack = adw::ViewStack::new();
    stack.add_titled(&scrolled(&content), Some("status"), &t("common.nav_status"));
    stack.add_titled(&scrolled(&build_tools_page()), Some("tools"), &t("common.nav_tools"));
    // Log view — a read-only tail of the unified activity log (mirrors the Windows Log tab).
    let (log_scroll, log_view) = build_log_page();
    stack.add_titled(&log_scroll, Some("log"), &t("common.nav_log"));
    stack.add_titled(
        &scrolled(&build_credits_page()),
        Some("credits"),
        &t("common.nav_credits"),
    );
    // (Re)load + scroll-to-newest whenever the Log page is selected (no poll), like Windows.
    {
        let lv = log_view.clone();
        stack.connect_visible_child_name_notify(move |s| {
            if s.visible_child_name().as_deref() == Some("log") {
                load_logs(&lv);
            }
        });
    }

    let switcher = adw::InlineViewSwitcher::builder()
        .stack(&stack)
        .display_mode(adw::InlineViewSwitcherDisplayMode::Labels)
        .build();
    header.set_title_widget(Some(&switcher));

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&stack));
    window.set_content(Some(&toolbar));

    Widgets {
        window,
        engine,
        tts_row,
        tts_dot,
        tts_runtime,
        tts_realtime,
        tts_first,
        tts_spoken,
        tts_failures,
        tts_failures_row,
        stt_row,
        stt_dot,
        stt_runtime,
        stt_realtime,
        stt_transcribed,
        stt_failures,
        stt_failures_row,
        caps_dot,
        spoken,
        heard,
    }
}

/// Apply a status push to the panel.
pub fn update(w: &Widgets, snap: &Snapshot) {
    let Some(s) = &snap.status else {
        // Engine down: every dot idle, names cleared, all stats dashed, failures hidden.
        let dash = t("common.dash");
        for dot in [&w.engine, &w.tts_dot, &w.stt_dot, &w.caps_dot] {
            set_dot(dot, "idle");
        }
        w.tts_row.set_subtitle("");
        w.stt_row.set_subtitle("");
        for l in [
            &w.tts_runtime,
            &w.tts_realtime,
            &w.tts_first,
            &w.tts_spoken,
            &w.stt_runtime,
            &w.stt_realtime,
            &w.stt_transcribed,
            &w.spoken,
            &w.heard,
        ] {
            l.set_text(&dash);
        }
        w.tts_failures_row.set_visible(false);
        w.stt_failures_row.set_visible(false);
        return;
    };

    // Headline dot: green while the engine is up.
    set_dot(&w.engine, "running");

    // TTS row: engine name + lifecycle dot; the expanded stat detail (runtime/realtime/
    // first-audio/spoken/failures) from the shared formatters + shared TtsSnapshot.
    let (tts_name, tts_state) = tts_display(s);
    w.tts_row
        .set_subtitle(&engine_subtitle(&tts_name, tts_state, tts_obj(s), None));
    set_dot(&w.tts_dot, tts_state);
    let tts = &s.stats.tts;
    w.tts_runtime.set_text(&runtime_text(s.tts_provider.as_deref()));
    w.tts_realtime.set_text(&crate::ffi::stats_range(
        tts.rtf_min,
        tts.rtf_avg,
        tts.rtf_max,
        2,
        "status.stats.unit.times",
    ));
    w.tts_first.set_text(&crate::ffi::stats_range(
        tts.first_min_ms / 1000.0,
        tts.first_avg_ms / 1000.0,
        tts.first_max_ms / 1000.0,
        1,
        "status.stats.unit.seconds",
    ));
    w.tts_spoken
        .set_text(&crate::ffi::stats_count(tts.utterances, tts.audio_secs));
    set_failures(&w.tts_failures_row, &w.tts_failures, tts.failures);

    // STT row: engine name + lifecycle dot; the expanded stat detail.
    let (stt_name, stt_state) = stt_display(s);
    w.stt_row
        .set_subtitle(&engine_subtitle(&stt_name, stt_state, stt_obj(s), claude_hint(s)));
    set_dot(&w.stt_dot, stt_state);
    let stt = &s.stats.stt;
    w.stt_runtime.set_text(&runtime_text(s.stt_provider.as_deref()));
    w.stt_realtime.set_text(&crate::ffi::stats_range(
        stt.rtf_min,
        stt.rtf_avg,
        stt.rtf_max,
        2,
        "status.stats.unit.times",
    ));
    w.stt_transcribed
        .set_text(&crate::ffi::stats_count(stt.transcriptions, stt.audio_secs));
    set_failures(&w.stt_failures_row, &w.stt_failures, stt.failures);

    // Caps Lock dot: active → green, armed-but-idle → orange, off → gray.
    set_dot(
        &w.caps_dot,
        if s.running.caps {
            "running"
        } else if s.running.caps_wanted {
            "warming"
        } else {
            "idle"
        },
    );

    // Lifetime totals.
    w.spoken
        .set_text(&crate::ffi::duration_live(s.stats.lifetime.tts_secs as f64));
    w.heard
        .set_text(&crate::ffi::duration_live(s.stats.lifetime.stt_secs as f64));
}

/// The runtime row value: the shared `runtime_label` for the resolved provider token, or the
/// dash when the engine has no ORT runtime (system / off / claude_code → `tts_provider` null).
fn runtime_text(provider: Option<&str>) -> String {
    match provider {
        Some(p) if !p.is_empty() => crate::ffi::runtime_label(p),
        _ => t("common.dash"),
    }
}

/// Show the failures row (red count) only when there were failures — matching macOS/Windows.
fn set_failures(row: &adw::ActionRow, label: &gtk::Label, failures: u64) {
    if failures > 0 {
        label.set_text(&failures.to_string());
        row.set_visible(true);
    } else {
        row.set_visible(false);
    }
}

/// The selected TTS engine → (display name from a shared key, lifecycle `state` token).
/// `built_in` is the Kokoro model; `system` is the OS voice; `off` shows nothing + idle dot.
fn tts_display(s: &ModelStatus) -> (String, &str) {
    match s.tts_engine.as_str() {
        "built_in" => (t("status.engine.kokoro"), s.kokoro.state.as_str()),
        "system" => (t("status.engine.system"), s.tts_system.state.as_str()),
        _ => (String::new(), "idle"),
    }
}

/// The selected STT engine → (display name from a shared key, lifecycle `state` token).
/// `built_in` is the Parakeet model; `claude_code` delegates; `system` is the OS recognizer.
fn stt_display(s: &ModelStatus) -> (String, &str) {
    match s.stt_engine.as_str() {
        "built_in" => (t("status.engine.parakeet"), s.parakeet.state.as_str()),
        "claude_code" => (t("status.engine.claude_code"), s.claude_code.state.as_str()),
        "system" => (t("status.engine.system"), s.system.state.as_str()),
        _ => (String::new(), "idle"),
    }
}

/// Append the engine's lifecycle NOTE to its name when not ready ("Kokoro · Downloading 45%"),
/// or an `extra` qualifier (the Claude-Code key hint) when ready — the subtitle analogue of the
/// macOS/Windows "state word / delegation" detail. Empty name → just the note.
fn engine_subtitle(name: &str, state: &str, obj: Option<&EngineObj>, extra: Option<String>) -> String {
    if is_trouble(state) {
        let (prog, why) = obj
            .map(|o| (o.progress, o.error.as_deref().unwrap_or("")))
            .unwrap_or((0.0, ""));
        let word = crate::ffi::engine_state_word(state, prog, why);
        return if name.is_empty() {
            word
        } else {
            format!("{name} · {word}")
        };
    }
    match extra {
        Some(x) if name.is_empty() => x,
        Some(x) => format!("{name} · {x}"),
        None => name.to_string(),
    }
}

/// The not-ready (download / warm / fail) lifecycle tokens that get a status NOTE.
fn is_trouble(state: &str) -> bool {
    matches!(state, "missing" | "downloading" | "warming" | "failed" | "blocked")
}

/// The selected TTS engine's `EngineObj` (for its progress/error), or None for off.
fn tts_obj(s: &ModelStatus) -> Option<&EngineObj> {
    match s.tts_engine.as_str() {
        "built_in" => Some(&s.kokoro),
        "system" => Some(&s.tts_system),
        _ => None,
    }
}

/// The selected STT engine's `EngineObj`, or None for off.
fn stt_obj(s: &ModelStatus) -> Option<&EngineObj> {
    match s.stt_engine.as_str() {
        "built_in" => Some(&s.parakeet),
        "claude_code" => Some(&s.claude_code),
        "system" => Some(&s.system),
        _ => None,
    }
}

/// Claude Code STT delegates (no local transcription), so name the key it sends — mirrors the
/// macOS/Windows delegation hint.
fn claude_hint(s: &ModelStatus) -> Option<String> {
    if s.stt_engine != "claude_code" {
        return None;
    }
    Some(match s.claude_code_key.as_deref() {
        Some(k) if !k.is_empty() => t("status.stt_claude_code").replace("%{key}", k),
        _ => t("status.stt_claude_code_off"),
    })
}

/// A right-aligned, dimmed value label, primed with the shared dash placeholder.
fn value_label() -> gtk::Label {
    let dash = t("common.dash");
    let l = gtk::Label::new(Some(dash.as_str()));
    l.add_css_class("dim-label");
    l.set_halign(gtk::Align::End);
    l
}

/// An `AdwActionRow` with `title` and any widget (a value label or a status dot) as suffix.
fn action_row(title: &str, value: &impl IsA<gtk::Widget>) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(title).build();
    row.add_suffix(value);
    row
}

/// A status dot — a symbolic filled circle recolored by a libadwaita semantic style class.
/// Created idle (dimmed); [`set_dot`] switches the class per state.
fn status_dot() -> gtk::Image {
    let dot = gtk::Image::from_icon_name("media-record-symbolic");
    dot.set_pixel_size(12);
    dot.set_valign(gtk::Align::Center);
    dot.add_css_class("dim-label");
    dot
}

/// Hide an `AdwExpanderRow`'s built-in disclosure arrow (the symbolic `expander-row-arrow`
/// image) by walking the template tree and dropping it from layout with `set_visible(false)` —
/// unlike CSS `opacity`, this also frees the ~16px slot it reserves, so a trailing suffix sits
/// flush at the row's edge (keeping the status dots aligned with the non-expander rows).
fn hide_expander_arrow(row: &adw::ExpanderRow) {
    fn find_arrow(w: &gtk::Widget) -> Option<gtk::Widget> {
        if w.has_css_class("expander-row-arrow") {
            return Some(w.clone());
        }
        let mut child = w.first_child();
        while let Some(c) = child {
            if let Some(found) = find_arrow(&c) {
                return Some(found);
            }
            child = c.next_sibling();
        }
        None
    }
    if let Some(arrow) = find_arrow(row.upcast_ref::<gtk::Widget>()) {
        arrow.set_visible(false);
    }
}

/// The right-side indicator for an expandable status row: the shared [`status_dot`] while the
/// row is collapsed, a chevron once it's expanded (the dot "turns into" the chevron, occupying
/// the same slot). The row's native expander arrow is hidden, so a bare dot is the only thing
/// on the right until you open the row. Returns the dot `Image` so the caller can keep
/// recoloring it with [`set_dot`].
fn expander_indicator(row: &adw::ExpanderRow) -> gtk::Image {
    hide_expander_arrow(row);
    let dot = status_dot();
    let chevron = gtk::Image::from_icon_name("pan-up-symbolic");
    chevron.set_pixel_size(12);
    chevron.set_valign(gtk::Align::Center);
    let stack = gtk::Stack::new();
    stack.add_named(&dot, Some("dot"));
    stack.add_named(&chevron, Some("chevron"));
    stack.set_visible_child_name("dot");
    row.add_suffix(&stack);
    let stack = stack.downgrade();
    row.connect_expanded_notify(move |r| {
        if let Some(stack) = stack.upgrade() {
            stack.set_visible_child_name(if r.is_expanded() { "chevron" } else { "dot" });
        }
    });
    dot
}

/// Recolor a dot from an `EngineObj.state` lifecycle token (the shared engine→app contract):
/// running → green (`.success`), warming/downloading/blocked → orange (`.warning`),
/// failed → red (`.error`), missing/idle/other → gray (`.dim-label`). Mirrors the
/// macOS/Windows engine status dot.
fn set_dot(dot: &gtk::Image, state: &str) {
    for c in ["success", "warning", "error", "dim-label"] {
        dot.remove_css_class(c);
    }
    dot.add_css_class(match state {
        "running" => "success",
        "warming" | "downloading" | "blocked" => "warning",
        "failed" => "error",
        _ => "dim-label",
    });
}

// ── Tools / Libraries pages (the other two views) ────────────────────────────────────────────

/// Wrap a page's content in a vertically-scrolling container (the lists can be long).
fn scrolled(child: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(child)
        .build()
}

/// A page body: the margins the Status page uses + an AdwClamp around one preferences group,
/// so wide windows don't stretch the cards.
fn page_box(group: &adw::PreferencesGroup) -> gtk::Box {
    let b = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .margin_top(18)
        .margin_bottom(18)
        .margin_start(18)
        .margin_end(18)
        .build();
    let clamp = adw::Clamp::builder().maximum_size(600).child(group).build();
    b.append(&clamp);
    b
}

/// The Tools page — the shared `ds_tools_json` catalog, one expander per tool (name +
/// description), revealing its params. Mirrors the macOS/Windows Tools view.
fn build_tools_page() -> gtk::Widget {
    let group = adw::PreferencesGroup::new();
    if let Ok(serde_json::Value::Array(tools)) = serde_json::from_str(&crate::ffi::tools_json()) {
        for tool in &tools {
            let row = adw::ExpanderRow::builder()
                .title(tool["name"].as_str().unwrap_or(""))
                .subtitle(tool["description"].as_str().unwrap_or(""))
                .build();
            if let Some(params) = tool["params"].as_array() {
                for p in params {
                    let ptype = p["type"].as_str().unwrap_or("any");
                    let req = if p["required"].as_bool().unwrap_or(false) {
                        t("tools.param.required")
                    } else {
                        t("tools.param.optional")
                    };
                    let pdesc = p["description"].as_str().unwrap_or("");
                    let sub = if pdesc.is_empty() {
                        format!("{ptype} · {req}")
                    } else {
                        format!("{ptype} · {req} — {pdesc}")
                    };
                    let prow = adw::ActionRow::builder()
                        .title(p["name"].as_str().unwrap_or(""))
                        .subtitle(&sub)
                        .build();
                    row.add_row(&prow);
                }
            }
            group.add(&row);
        }
    }
    page_box(&group).upcast()
}

/// The Credits page — the shared `ds_libraries_json` catalog (downloaded models +
/// runtimes), one expander per project (name + usage), revealing the project page link, the
/// license (its name links to the license page), and the files with sizes. Mirrors the Windows
/// Credits tab.
fn build_credits_page() -> gtk::Widget {
    let group = adw::PreferencesGroup::builder().build();
    if let Ok(serde_json::Value::Array(projects)) =
        serde_json::from_str(&crate::ffi::libraries_json())
    {
        for p in &projects {
            let row = adw::ExpanderRow::builder()
                .title(p["name"].as_str().unwrap_or(""))
                .subtitle(p["usage"].as_str().unwrap_or(""))
                .build();
            if let Some(hp) = p["homepage"].as_str().filter(|s| !s.is_empty()) {
                row.add_row(&link_row(&t("libraries.homepage"), hp));
            }
            // The license is a link row LABELED with the license name itself (e.g. "MIT",
            // "Apache-2.0"), opening its license page — the same external-link affordance as the
            // project page row.
            if let (Some(lic), Some(lu)) = (
                p["license"].as_str().filter(|s| !s.is_empty()),
                p["license_url"].as_str().filter(|s| !s.is_empty()),
            ) {
                row.add_row(&link_row(lic, lu));
            }
            if let Some(files) = p["files"].as_array() {
                for f in files {
                    let frow = adw::ActionRow::builder()
                        .title(f["name"].as_str().unwrap_or(""))
                        .build();
                    if let Some(sz) = f["size_bytes"].as_u64().filter(|&s| s > 0) {
                        let lbl = value_label();
                        lbl.set_text(&human_size(sz));
                        frow.add_suffix(&lbl);
                    }
                    row.add_row(&frow);
                }
            }
            group.add(&row);
        }
    }
    page_box(&group).upcast()
}

/// An activatable row that opens `url` in the default browser (the external-link affordance).
fn link_row(title: &str, url: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(title).activatable(true).build();
    row.add_suffix(&gtk::Image::from_icon_name("adw-external-link-symbolic"));
    let url = url.to_string();
    row.connect_activated(move |_| {
        let _ = gtk::gio::AppInfo::launch_default_for_uri(&url, None::<&gtk::gio::AppLaunchContext>);
    });
    row
}

/// The Log page — a read-only, monospace, scrollable text area. Returns the scroller (the
/// page widget) + the text view to (re)fill from the log tail.
fn build_log_page() -> (gtk::ScrolledWindow, gtk::TextView) {
    let view = gtk::TextView::builder()
        .editable(false)
        .cursor_visible(false)
        .monospace(true)
        .wrap_mode(gtk::WrapMode::WordChar)
        .left_margin(12)
        .right_margin(12)
        .top_margin(12)
        .bottom_margin(12)
        .build();
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&view)
        .build();
    (scroll, view)
}

/// Fill the Logs view from the shared `log_tail` and scroll to the newest line.
fn load_logs(view: &gtk::TextView) {
    let tail = crate::ffi::log_tail(64 * 1024);
    let text = if tail.trim().is_empty() {
        t("logs.empty")
    } else {
        tail
    };
    let buf = view.buffer();
    buf.set_text(&text);
    // Scroll to the end after layout settles (the buffer just changed).
    let view = view.clone();
    gtk::glib::idle_add_local_once(move || {
        let mut end = view.buffer().end_iter();
        view.scroll_to_iter(&mut end, 0.0, false, 0.0, 1.0);
    });
}

/// Bytes → a compact human size ("325 MB", "8 MB", "9 kB"), matching the download labels.
fn human_size(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1_000_000_000.0 {
        format!("{:.1} GB", b / 1_000_000_000.0)
    } else if b >= 1_000_000.0 {
        format!("{:.0} MB", b / 1_000_000.0)
    } else if b >= 1_000.0 {
        format!("{:.0} kB", b / 1_000.0)
    } else {
        format!("{bytes} B")
    }
}
