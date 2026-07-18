//! Health panel — `AdwApplicationWindow` of preference rows for live engine state (GTK4
//! analogue of macOS `StatusView` / Windows status tab). Read-only; control is MCP.
//!
//! Strings only from shared `ds-i18n` via [`crate::ffi::t`]. State as colored dots (libadwaita
//! semantic classes), never words — same as the other hosts.

use adw::prelude::*;
use ds_status::{EngineObj, EngineState, ModelStatus};

use crate::status::Snapshot;

fn t(key: &str) -> String {
    crate::ffi::t(key)
}

/// Handles refreshed on each status push. Cloneable (GTK widgets are refcounted).
#[derive(Clone)]
pub struct Widgets {
    pub window: adw::ApplicationWindow,
    /// Engine headline dot (green running / gray idle).
    engine: gtk::Image,
    tts_row: adw::ExpanderRow,
    tts_dot: gtk::Image,
    tts_runtime: gtk::Label,
    tts_realtime: gtk::Label,
    tts_first: gtk::Label,
    tts_spoken: gtk::Label,
    tts_failures: gtk::Label,
    tts_failures_row: adw::ActionRow,
    stt_row: adw::ExpanderRow,
    stt_dot: gtk::Image,
    stt_runtime: gtk::Label,
    stt_realtime: gtk::Label,
    stt_transcribed: gtk::Label,
    stt_failures: gtk::Label,
    stt_failures_row: adw::ActionRow,
    /// Caps Lock: green active / orange enabled-idle / gray off.
    caps_dot: gtk::Image,
    spoken: gtk::Label,
    heard: gtk::Label,
    /// Headline version subtitle — same `GtkLabel` [`make_version_link`] wires for homepage
    /// click. [`apply_update_check`] rewrites text to "current → new" and tints background
    /// in place (one shared pill, not a separate badge).
    version_subtitle: gtk::Label,
}

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
    // Close only: decoration-layout left`:`right (not CSS-hiding buttons). ":close" = nothing
    // left, close right. Window lives in the tray; close just hides it (main.rs).
    header.set_decoration_layout(Some(":close"));

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(18)
        .margin_top(18)
        .margin_bottom(18)
        .margin_start(18)
        .margin_end(18)
        .build();

    // Headline: name + version, expandable lifetime totals; dot is the only state indicator.
    let engine_group = adw::PreferencesGroup::new();
    let version = crate::ffi::version();
    let status_row = adw::ExpanderRow::builder()
        .title(app_name.as_str())
        .subtitle(version.as_str())
        .build();
    let engine = expander_indicator(&status_row);
    let version_subtitle = make_version_link(&status_row, &crate::ffi::homepage_url());

    let tts_life = format!(
        "{} {}",
        t("status.engine.role_tts"),
        t("status.stats.lifetime_all_time")
    );
    let spoken = value_label();
    status_row.add_row(&action_row(&tts_life, &spoken));
    let stt_life = format!(
        "{} {}",
        t("status.engine.role_stt"),
        t("status.stats.lifetime_all_time")
    );
    let heard = value_label();
    status_row.add_row(&action_row(&stt_life, &heard));

    engine_group.add(&status_row);
    content.append(&engine_group);

    // TTS/STT: role + engine-name subtitle + lifecycle dot; expand for shared status_fmt stats.
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
    stt_row.add_row(&action_row(
        &t("status.stats.transcribed"),
        &stt_transcribed,
    ));
    let stt_failures = value_label();
    stt_failures.add_css_class("error");
    let stt_failures_row = action_row(&t("status.stats.failures"), &stt_failures);
    stt_row.add_row(&stt_failures_row);
    voice_group.add(&stt_row);

    content.append(&voice_group);

    // Caps Lock at bottom; expands to tap/hold hint (peer parity).
    let caps_group = adw::PreferencesGroup::new();
    let caps_row = adw::ExpanderRow::builder()
        .title(t("status.caps_lock").as_str())
        .build();
    let caps_dot = expander_indicator(&caps_row);
    let caps_hint = adw::ActionRow::builder()
        .title(t("status.caps_hint").as_str())
        .build();
    caps_hint.set_title_lines(0); // wrap full hint instead of ellipsizing
    caps_row.add_row(&caps_hint);
    caps_group.add(&caps_row);
    content.append(&caps_group);

    // Status / Tools / Log / Credits — AdwViewStack + InlineViewSwitcher (HIG 3–5 views).
    // Order matches macOS/Windows (Log before Credits).
    let stack = adw::ViewStack::new();
    stack.add_titled(&scrolled(&content), Some("status"), &t("common.nav_status"));
    stack.add_titled(
        &scrolled(&build_tools_page()),
        Some("tools"),
        &t("common.nav_tools"),
    );
    let (log_scroll, log_view) = build_log_page();
    // Free-text filter (shared ds_log rules); no dedicated i18n placeholder on other hosts either.
    let log_filter = gtk::SearchEntry::builder().hexpand(true).build();
    let log_clear_button = gtk::Button::builder()
        .icon_name("user-trash-symbolic")
        .tooltip_text(t("logs.clear"))
        .build();
    let log_toolbar = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .margin_top(6)
        .margin_start(6)
        .margin_end(6)
        .build();
    log_toolbar.append(&log_filter);
    log_toolbar.append(&log_clear_button);
    let log_page = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .build();
    log_page.append(&log_toolbar);
    log_page.append(&log_scroll);
    stack.add_titled(&log_page, Some("log"), &t("common.nav_log"));
    stack.add_titled(
        &scrolled(&build_credits_page()),
        Some("credits"),
        &t("common.nav_credits"),
    );
    // Latest raw JSON from push; filter re-applies without another disk read.
    let log_json: std::rc::Rc<std::cell::RefCell<String>> =
        std::rc::Rc::new(std::cell::RefCell::new(String::new()));
    let log_query: std::rc::Rc<std::cell::RefCell<String>> =
        std::rc::Rc::new(std::cell::RefCell::new(String::new()));
    // Live log only while the tab is visible (`log_push`); stop when leaving.
    // `Rc<RefCell>` for GTK closures; the push itself is a real OS thread.
    let log_push_stop: std::rc::Rc<
        std::cell::RefCell<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>>,
    > = std::rc::Rc::new(std::cell::RefCell::new(None));
    {
        let lv = log_view.clone();
        let log_push_stop = log_push_stop.clone();
        let log_json = log_json.clone();
        let log_query = log_query.clone();
        stack.connect_visible_child_name_notify(move |s| {
            if s.visible_child_name().as_deref() == Some("log") {
                if log_push_stop.borrow().is_none() {
                    let (tx, rx) = async_channel::unbounded::<String>();
                    *log_push_stop.borrow_mut() = Some(crate::log_push::spawn_push(tx));
                    let lv = lv.clone();
                    let log_json = log_json.clone();
                    let log_query = log_query.clone();
                    gtk::glib::spawn_future_local(async move {
                        while let Ok(json) = rx.recv().await {
                            *log_json.borrow_mut() = json;
                            let q = log_query.borrow().clone();
                            set_log_from_json(&lv, &log_json.borrow(), &q);
                        }
                    });
                }
            } else if let Some(stop) = log_push_stop.borrow_mut().take() {
                stop.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        });
    }
    {
        let lv = log_view.clone();
        let log_json = log_json.clone();
        let log_query = log_query.clone();
        log_filter.connect_search_changed(move |entry| {
            let q = entry.text().to_string();
            *log_query.borrow_mut() = q.clone();
            set_log_from_json(&lv, &log_json.borrow(), &q);
        });
    }
    {
        let window = window.clone();
        log_clear_button.connect_clicked(move |_| {
            let dialog = adw::AlertDialog::builder()
                .heading(t("logs.clear_confirm_title"))
                .default_response("cancel")
                .close_response("cancel")
                .build();
            dialog.add_response("cancel", &t("common.cancel"));
            dialog.add_response("clear", &t("logs.clear_confirm_action"));
            dialog.set_response_appearance("clear", adw::ResponseAppearance::Destructive);
            // AdwAlertDialog closes itself once a response is activated.
            dialog.connect_response(None, move |_dialog, response| {
                if response == "clear" {
                    // File removal can block — keep it off the GTK main loop; log-push
                    // publishes the resulting empty tail.
                    let _ = std::thread::Builder::new()
                        .name("ds-logs-clear".into())
                        .spawn(crate::ffi::logs_clear);
                }
            });
            dialog.present(Some(&window));
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
        version_subtitle,
    }
}

/// Apply a status push to the panel.
pub fn update(w: &Widgets, snap: &Snapshot) {
    let Some(s) = &snap.status else {
        // Engine down: idle dots, cleared names, dashed stats, failures hidden.
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

    set_dot(&w.engine, "running");

    let (tts_name, tts_state, tts_o) = match tts_engine(s) {
        Some((name, state, obj)) => (name, state, Some(obj)),
        None => (String::new(), "idle", None),
    };
    w.tts_row
        .set_subtitle(&engine_subtitle(&tts_name, tts_state, tts_o, None));
    set_dot(&w.tts_dot, tts_state);
    let tts = &s.stats.tts;
    w.tts_runtime
        .set_text(&runtime_text(s.tts_provider.as_deref()));
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

    let (stt_name, stt_state, stt_o) = match stt_engine(s) {
        Some((name, state, obj)) => (name, state, Some(obj)),
        None => (String::new(), "idle", None),
    };
    w.stt_row.set_subtitle(&engine_subtitle(
        &stt_name,
        stt_state,
        stt_o,
        claude_hint(s),
    ));
    set_dot(&w.stt_dot, stt_state);
    let stt = &s.stats.stt;
    w.stt_runtime
        .set_text(&runtime_text(s.stt_provider.as_deref()));
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

    w.spoken
        .set_text(&crate::ffi::duration_live(s.stats.lifetime.tts_secs as f64));
    w.heard
        .set_text(&crate::ffi::duration_live(s.stats.lifetime.stt_secs as f64));
}

/// Shared `runtime_label` for the provider token, or dash when null (system/off/claude_code).
fn runtime_text(provider: Option<&str>) -> String {
    match provider {
        Some(p) if !p.is_empty() => crate::ffi::runtime_label(p),
        _ => t("common.dash"),
    }
}

/// Failures row only when count > 0 (macOS/Windows parity).
fn set_failures(row: &adw::ActionRow, label: &gtk::Label, failures: u64) {
    if failures > 0 {
        label.set_text(&failures.to_string());
        row.set_visible(true);
    } else {
        row.set_visible(false);
    }
}

/// Active TTS row via [`ds_status::ActiveTtsSlot`].
fn tts_engine(s: &ModelStatus) -> Option<(String, &str, &EngineObj)> {
    use ds_status::ActiveTtsSlot;
    match ActiveTtsSlot::from_engine(&s.tts_engine)? {
        ActiveTtsSlot::Kokoro => Some((
            t("status.engine.kokoro"),
            s.kokoro.state.as_str(),
            &s.kokoro,
        )),
        ActiveTtsSlot::TtsSystem => Some((
            t("status.engine.system"),
            s.tts_system.state.as_str(),
            &s.tts_system,
        )),
    }
}

/// Active STT row via [`ds_status::ActiveSttSlot`].
fn stt_engine(s: &ModelStatus) -> Option<(String, &str, &EngineObj)> {
    use ds_status::ActiveSttSlot;
    match ActiveSttSlot::from_engine(&s.stt_engine)? {
        ActiveSttSlot::Parakeet => Some((
            t("status.engine.parakeet"),
            s.parakeet.state.as_str(),
            &s.parakeet,
        )),
        ActiveSttSlot::ClaudeCode => Some((
            t("status.engine.claude_code"),
            s.claude_code.state.as_str(),
            &s.claude_code,
        )),
        ActiveSttSlot::System => Some((
            t("status.engine.system"),
            s.system.state.as_str(),
            &s.system,
        )),
    }
}

/// Name + lifecycle note when not ready ("Kokoro · Downloading 45%"), or `extra` when ready.
fn engine_subtitle(
    name: &str,
    state: &str,
    obj: Option<&EngineObj>,
    extra: Option<String>,
) -> String {
    // Shared state-word returns "" for ready states — that emptiness IS the note-vs-ready
    // gate (same as macOS/Windows). No local "is trouble" list to drift.
    let (prog, why) = obj
        .map(|o| (o.progress, o.error.as_deref().unwrap_or("")))
        .unwrap_or((0.0, ""));
    let word = crate::ffi::engine_state_word(state, prog, why);
    if !word.is_empty() {
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

/// Claude Code STT has no local transcription — name the key it sends (peer delegation hint).
fn claude_hint(s: &ModelStatus) -> Option<String> {
    if s.stt_engine != "claude_code" {
        return None;
    }
    Some(match s.claude_code_key.as_deref() {
        Some(k) if !k.is_empty() => crate::ffi::t_args("status.stt_claude_code", &[("key", k)]),
        _ => t("status.stt_claude_code_off"),
    })
}

fn value_label() -> gtk::Label {
    let dash = t("common.dash");
    let l = gtk::Label::new(Some(dash.as_str()));
    l.add_css_class("dim-label");
    l.set_halign(gtk::Align::End);
    l
}

/// First descendant with `class` — reaches into AdwExpanderRow/ActionRow template widgets
/// (disclosure arrow, subtitle label) that aren't otherwise exposed.
fn find_by_css_class(w: &gtk::Widget, class: &str) -> Option<gtk::Widget> {
    if w.has_css_class(class) {
        return Some(w.clone());
    }
    let mut child = w.first_child();
    while let Some(c) = child {
        if let Some(found) = find_by_css_class(&c, class) {
            return Some(found);
        }
        child = c.next_sibling();
    }
    None
}

/// Wire homepage click on the headline version subtitle without restyling it. Claims
/// `GestureClick` on *press* so the row's expand gesture (also press) loses; claiming on
/// release is too late. Returns the subtitle `GtkLabel` for [`apply_update_check`].
fn make_version_link(row: &adw::ExpanderRow, url: &str) -> gtk::Label {
    let subtitle = find_by_css_class(row.upcast_ref::<gtk::Widget>(), "subtitle")
        .and_then(|w| w.downcast::<gtk::Label>().ok())
        .unwrap_or_else(|| gtk::Label::new(None));
    if url.is_empty() {
        return subtitle;
    }
    subtitle.set_cursor_from_name(Some("pointer"));
    let click = gtk::GestureClick::new();
    click.connect_pressed(|gesture, _, _, _| {
        gesture.set_state(gtk::EventSequenceState::Claimed);
    });
    let url = url.to_string();
    click.connect_released(move |_, _, _, _| {
        let _ =
            gtk::gio::AppInfo::launch_default_for_uri(&url, None::<&gtk::gio::AppLaunchContext>);
    });
    subtitle.add_controller(click);
    subtitle
}

/// `.ds-update-badge` background only — brand purple from [`crate::icon::brand_colors`]
/// (never a second hardcoded hex). Does NOT set `color`/`font-*` so the version subtitle
/// keeps its text styling. Local color source; safe at startup independent of the network check.
pub fn load_update_badge_css() {
    let (crate::icon::Rgb(r, g, b), _mic_orange) =
        crate::icon::brand_colors(&crate::ffi::brand_colors_json());
    let css = format!(
        ".ds-update-badge {{
            background-color: rgba({r}, {g}, {b}, 0.18);
            border-radius: 999px;
            padding: 2px 10px;
        }}"
    );
    let provider = gtk::CssProvider::new();
    provider.load_from_string(&css);
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

/// One-shot startup update result. `{}` / missing-or-false `update_available` / missing
/// `latest_version` → leave plain version (never show a pill on doubt). Rewrites the existing
/// subtitle in place to "current → new" + badge class; homepage click unchanged.
pub fn apply_update_check(w: &Widgets, json: &str) {
    let v: serde_json::Value = serde_json::from_str(json).unwrap_or(serde_json::Value::Null);
    let available = v
        .get("update_available")
        .and_then(|b| b.as_bool())
        .unwrap_or(false);
    let Some(latest) = available
        .then(|| v.get("latest_version"))
        .flatten()
        .and_then(|s| s.as_str())
    else {
        return;
    };
    let current = w.version_subtitle.text();
    w.version_subtitle
        .set_text(&format!("{current} {} {latest}", t("common.update_arrow")));
    w.version_subtitle.add_css_class("ds-update-badge");
    w.version_subtitle
        .set_tooltip_text(Some(&t("status.update_available")));
    // Subtitle is hexpand/halign-fill by default; shrink-wrap so the badge hugs the text.
    w.version_subtitle.set_hexpand(false);
    w.version_subtitle.set_halign(gtk::Align::Start);
}

fn action_row(title: &str, value: &impl IsA<gtk::Widget>) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(title).build();
    row.add_suffix(value);
    row
}

/// Symbolic filled circle; recolored by libadwaita semantic class via [`set_dot`].
fn status_dot() -> gtk::Image {
    let dot = gtk::Image::from_icon_name("media-record-symbolic");
    dot.set_pixel_size(12);
    dot.set_valign(gtk::Align::Center);
    dot.add_css_class("dim-label");
    dot
}

/// Hide expander-row disclosure arrow with `set_visible(false)` (not CSS opacity) so the ~16px
/// slot frees and trailing suffixes sit flush — dots align with non-expander rows.
fn hide_expander_arrow(row: &adw::ExpanderRow) {
    if let Some(arrow) = find_by_css_class(row.upcast_ref::<gtk::Widget>(), "expander-row-arrow") {
        arrow.set_visible(false);
    }
}

/// Collapsed: status dot; expanded: chevron in the same slot (native arrow hidden).
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

/// Recolor from `EngineObj.state`: running→success, warming/downloading/blocked→warning,
/// failed→error, else dim-label. Shared engine→app contract.
fn set_dot(dot: &gtk::Image, state: &str) {
    for c in ["success", "warning", "error", "dim-label"] {
        dot.remove_css_class(c);
    }
    dot.add_css_class(match EngineState::parse(state) {
        Some(EngineState::Running) => "success",
        Some(EngineState::Warming | EngineState::Downloading | EngineState::Blocked) => "warning",
        Some(EngineState::Failed) => "error",
        _ => "dim-label",
    });
}

fn scrolled(child: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(child)
        .build()
}

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

/// Tools catalog from `ds_tools_json` — expander per tool, params with shared `detail` string.
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
                    // Constraint qualifier pre-built by status_fmt::tool_param_detail.
                    let detail = p["detail"].as_str().unwrap_or("");
                    let pdesc = p["description"].as_str().unwrap_or("");
                    let mut sub = format!("{ptype} · {req}");
                    if !detail.is_empty() {
                        sub.push_str(&format!(" · {detail}"));
                    }
                    if !pdesc.is_empty() {
                        sub.push_str(&format!(" — {pdesc}"));
                    }
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

/// Credits from `ds_libraries_json` — expander per project: homepage, license link, file sizes.
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
            // License row labeled with the license name, opening its license page.
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
                        lbl.set_text(&crate::ffi::human_size(sz));
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

fn link_row(title: &str, url: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(title)
        .activatable(true)
        .build();
    row.add_suffix(&gtk::Image::from_icon_name("adw-external-link-symbolic"));
    let url = url.to_string();
    row.connect_activated(move |_| {
        let _ =
            gtk::gio::AppInfo::launch_default_for_uri(&url, None::<&gtk::gio::AppLaunchContext>);
    });
    row
}

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

/// Filter/flatten via [`ds_log`]; empty or no-match placeholders.
fn set_log_from_json(view: &gtk::TextView, json: &str, query: &str) {
    let (total, shown, flat) = crate::ffi::filter_and_flatten_logs(json, query);
    let text = if total == 0 {
        t("logs.empty")
    } else if shown == 0 {
        t("logs.no_match")
    } else {
        flat
    };
    let buf = view.buffer();
    buf.set_text(&text);
    let view = view.clone();
    gtk::glib::idle_add_local_once(move || {
        let mut end = view.buffer().end_iter();
        view.scroll_to_iter(&mut end, 0.0, false, 0.0, 1.0);
    });
}
