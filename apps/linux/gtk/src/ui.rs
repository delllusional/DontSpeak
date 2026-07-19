//! Health panel — AdwApplicationWindow of preference rows (macOS StatusView / Windows status).
//! Read-only; control is MCP. Strings from ds-i18n; state via colored dots.

use std::collections::HashMap;

use adw::prelude::*;
use ds_status::{EngineState, EngineStatus, ModelStatus, StatusSttEngine, StatusTtsEngine};

use crate::ffi::{UsageCard, UsageDeck, UsageRow};
use crate::status::Snapshot;

fn t(key: &str) -> String {
    crate::ffi::t(key)
}

/// Handles refreshed on each status push. Cloneable (GTK widgets are refcounted).
#[derive(Clone)]
pub struct Widgets {
    pub window: adw::ApplicationWindow,
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
    caps_dot: gtk::Image,
    spoken: gtk::Label,
    heard: gtk::Label,
    /// Version subtitle: homepage click via make_version_link; apply_update_check rewrites
    /// in place to "current → new" + badge class on the same label.
    version_subtitle: gtk::Label,
    usage_page: UsagePage,
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
    // Close only (":close"); tray-resident — close hides (main.rs).
    header.set_decoration_layout(Some(":close"));

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(18)
        .margin_top(18)
        .margin_bottom(18)
        .margin_start(18)
        .margin_end(18)
        .build();

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

    let caps_group = adw::PreferencesGroup::new();
    let caps_row = adw::ExpanderRow::builder()
        .title(t("status.caps_lock").as_str())
        .build();
    let caps_dot = expander_indicator(&caps_row);
    let caps_hint = adw::ActionRow::builder()
        .title(t("status.caps_hint").as_str())
        .build();
    caps_hint.set_title_lines(0); // wrap full hint
    caps_row.add_row(&caps_hint);
    caps_group.add(&caps_row);
    content.append(&caps_group);

    // Five-view HIG limit; Agents is launch default (macOS/Windows parity).
    let stack = adw::ViewStack::new();
    let usage_page = UsagePage::new();
    {
        let page = usage_page.clone();
        stack.connect_visible_child_name_notify(move |s| {
            if s.visible_child_name().as_deref() == Some("agents") {
                page.on_tab_selected();
            } else {
                page.cancel_visible_request();
            }
        });
    }
    stack.add_titled(&usage_page.root, Some("agents"), &t("common.nav_agents"));
    stack.add_titled(&scrolled(&content), Some("status"), &t("common.nav_status"));
    stack.add_titled(
        &scrolled(&build_tools_page()),
        Some("tools"),
        &t("common.nav_tools"),
    );
    let (log_scroll, log_view) = build_log_page();
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
    // Cached JSON from push; filter re-applies without another disk read.
    let log_json: std::rc::Rc<std::cell::RefCell<String>> =
        std::rc::Rc::new(std::cell::RefCell::new(String::new()));
    let log_query: std::rc::Rc<std::cell::RefCell<String>> =
        std::rc::Rc::new(std::cell::RefCell::new(String::new()));
    // Live log only while tab visible; stop on leave.
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
            dialog.connect_response(None, move |_dialog, response| {
                if response == "clear" {
                    // File removal can block — off GTK main loop; log-push republishes empty tail.
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
        usage_page,
    }
}

pub fn update(w: &Widgets, snap: &Snapshot) {
    let Some(s) = &snap.status else {
        let dash = t("common.dash");
        for dot in [&w.engine, &w.tts_dot, &w.stt_dot, &w.caps_dot] {
            set_dot(dot, EngineState::Idle);
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
        w.usage_page.set_speaking_agent(None);
        return;
    };

    let speaking = if s.activity.speaking {
        s.activity.speaker.map(|c| c.as_str())
    } else {
        None
    };
    w.usage_page.set_speaking_agent(speaking);

    set_dot(&w.engine, EngineState::Running);

    let (tts_name, tts_state, tts_o) = match tts_engine(s) {
        Some((name, state, obj)) => (name, state, Some(obj)),
        None => (String::new(), EngineState::Idle, None),
    };
    w.tts_row
        .set_subtitle(&engine_subtitle(&tts_name, tts_state, tts_o, None));
    set_dot(&w.tts_dot, tts_state);
    let tts = &s.stats.tts;
    w.tts_runtime
        .set_text(&runtime_text(s.tts.provider.as_deref()));
    w.tts_realtime.set_text(&crate::ffi::stats_range(
        tts.rtf_min,
        tts.rtf_avg,
        tts.rtf_max,
        2,
        "status.stats.unit.times",
    ));
    w.tts_first.set_text(&crate::ffi::stats_range(
        tts.ttfa_min_ms / 1000.0,
        tts.ttfa_avg_ms / 1000.0,
        tts.ttfa_max_ms / 1000.0,
        1,
        "status.stats.unit.seconds",
    ));
    w.tts_spoken
        .set_text(&crate::ffi::stats_count(tts.utterances, tts.audio_secs));
    set_failures(&w.tts_failures_row, &w.tts_failures, tts.failures);

    let (stt_name, stt_state, stt_o) = match stt_engine(s) {
        Some((name, state, obj)) => (name, state, Some(obj)),
        None => (String::new(), EngineState::Idle, None),
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
        .set_text(&runtime_text(s.stt.provider.as_deref()));
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
        if s.activity.caps_active {
            EngineState::Running
        } else if s.activity.caps {
            EngineState::Warming
        } else {
            EngineState::Idle
        },
    );

    w.spoken
        .set_text(&crate::ffi::duration_live(s.stats.lifetime.tts_secs as f64));
    w.heard
        .set_text(&crate::ffi::duration_live(s.stats.lifetime.stt_secs as f64));
}

fn runtime_text(provider: Option<&str>) -> String {
    match provider {
        Some(p) if !p.is_empty() => crate::ffi::runtime_label(p),
        _ => t("common.dash"),
    }
}

/// Failures row only when count > 0 (peer parity).
fn set_failures(row: &adw::ActionRow, label: &gtk::Label, failures: u64) {
    if failures > 0 {
        label.set_text(&failures.to_string());
        row.set_visible(true);
    } else {
        row.set_visible(false);
    }
}

fn tts_engine(s: &ModelStatus) -> Option<(String, EngineState, &EngineStatus)> {
    let status = s.tts.status.as_ref()?;
    let name = match s.tts.engine {
        StatusTtsEngine::BuiltIn => t("status.engine.kokoro"),
        StatusTtsEngine::System => t("status.engine.system"),
        StatusTtsEngine::Off => return None,
    };
    Some((name, status.state, status))
}

fn stt_engine(s: &ModelStatus) -> Option<(String, EngineState, &EngineStatus)> {
    let status = s.stt.status.as_ref()?;
    let name = match s.stt.engine {
        StatusSttEngine::BuiltIn => t("status.engine.parakeet"),
        StatusSttEngine::ClaudeCode => t("status.engine.claude_code"),
        StatusSttEngine::System => t("status.engine.system"),
        StatusSttEngine::Off => return None,
    };
    Some((name, status.state, status))
}

/// Name + lifecycle note when not ready, or `extra` when ready.
fn engine_subtitle(
    name: &str,
    state: EngineState,
    obj: Option<&EngineStatus>,
    extra: Option<String>,
) -> String {
    // Shared state-word returns "" for ready — that emptiness is the note-vs-ready gate
    // (macOS/Windows); host must not invent a local trouble list.
    let (prog, why) = obj
        .map(|o| (o.progress, o.error.as_deref().unwrap_or("")))
        .unwrap_or((0.0, ""));
    let word = crate::ffi::engine_state_word(state.as_str(), prog, why);
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

/// Claude Code STT: delegated voice-key name in the subtitle.
fn claude_hint(s: &ModelStatus) -> Option<String> {
    if s.stt.engine != StatusSttEngine::ClaudeCode {
        return None;
    }
    Some(match s.stt.voice_key.as_deref() {
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

/// First descendant with `class` (reaches into Adw template widgets not otherwise exposed).
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

/// Homepage click on version subtitle. Claim GestureClick on *press* so the row expand
/// gesture loses (release is too late). Returns the subtitle for apply_update_check.
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

/// Brand purple for update badge + usage progress. Speaking wash is dynamic (set_speaking_agent).
pub fn load_update_badge_css() {
    let (crate::icon::Rgb(r, g, b), _mic_orange) =
        crate::icon::brand_colors(&crate::ffi::brand_colors_json());
    let css = format!(
        ".ds-update-badge {{
            background-color: rgba({r}, {g}, {b}, 0.18);
            border-radius: 999px;
            padding: 2px 10px;
        }}
        progressbar.ds-usage-progress > trough > progress {{
            background-color: rgb({r}, {g}, {b});
        }}
        .ds-usage-speaking {{
            border-radius: 12px;
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

/// One roll from ds_random_pastel_wash_json; None if FFI/`{}`.
fn random_pastel_wash() -> Option<(u8, u8, u8, f64)> {
    let v: serde_json::Value = serde_json::from_str(&crate::ffi::random_pastel_wash_json()).ok()?;
    let r = v.get("r")?.as_u64()? as u8;
    let g = v.get("g")?.as_u64()? as u8;
    let b = v.get("b")?.as_u64()? as u8;
    let a = v.get("a").and_then(|x| x.as_f64()).unwrap_or(0.30);
    Some((r, g, b, a.clamp(0.0, 1.0)))
}

/// Startup update pill. Missing/false/malformed → leave plain version.
/// Rewrites subtitle in place; homepage click unchanged.
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
    // Shrink-wrap so the badge hugs the text.
    w.version_subtitle.set_hexpand(false);
    w.version_subtitle.set_halign(gtk::Align::Start);
}

fn action_row(title: &str, value: &impl IsA<gtk::Widget>) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(title).build();
    row.add_suffix(value);
    row
}

fn status_dot() -> gtk::Image {
    let dot = gtk::Image::from_icon_name("media-record-symbolic");
    dot.set_pixel_size(12);
    dot.set_valign(gtk::Align::Center);
    dot.add_css_class("dim-label");
    dot
}

/// Hide native arrow with `set_visible(false)` (opacity still occupies the slot).
fn hide_expander_arrow(row: &adw::ExpanderRow) {
    if let Some(arrow) = find_by_css_class(row.upcast_ref::<gtk::Widget>(), "expander-row-arrow") {
        arrow.set_visible(false);
    }
}

/// Collapsed: status dot; expanded: chevron in the same slot.
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

/// running→success, warming/downloading/blocked→warning, failed→error, else dim-label.
fn set_dot(dot: &gtk::Image, state: EngineState) {
    for c in ["success", "warning", "error", "dim-label"] {
        dot.remove_css_class(c);
    }
    dot.add_css_class(match state {
        EngineState::Running => "success",
        EngineState::Warming | EngineState::Downloading | EngineState::Blocked => "warning",
        EngineState::Failed => "error",
        EngineState::Missing | EngineState::Idle => "dim-label",
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

/// Period row widgets for in-place refresh; remount only when period shape changes.
#[derive(Clone)]
struct UsageRowWidgets {
    period: String,
    progress: gtk::ProgressBar,
    remaining: gtk::Label,
}

/// Per-agent shell: stack for crossfade remounts; rows for in-place updates.
/// `needs_auth` = mounted auth state; a toggle forces a remount (row set changes).
struct MountedUsageCard {
    stack: gtk::Stack,
    account: gtk::Label,
    rows: Vec<UsageRowWidgets>,
    needs_auth: bool,
}

#[derive(Clone)]
struct UsagePage {
    root: gtk::Box,
    list: gtk::Box,
    /// ClientSource::CLIENTS order from the skeleton deck.
    canonical_agents: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
    latest: std::rc::Rc<std::cell::RefCell<Vec<UsageCard>>>,
    rendered: std::rc::Rc<std::cell::RefCell<HashMap<String, MountedUsageCard>>>,
    empty_label: gtk::Label,
    generation: std::rc::Rc<std::cell::Cell<u64>>,
    /// `activity.speaker`; drives `.ds-usage-speaking` wash.
    speaking_agent: std::rc::Rc<std::cell::RefCell<Option<String>>>,
    /// Frozen while the same agent speaks; re-rolled on agent change.
    speaking_wash: std::rc::Rc<std::cell::RefCell<Option<(u8, u8, u8, f64)>>>,
    speaking_css: gtk::CssProvider,
}

impl UsagePage {
    fn new() -> Self {
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .build();
        let list = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(10)
            .margin_bottom(18)
            .margin_start(18)
            .margin_end(18)
            .build();
        let empty_label = gtk::Label::new(Some(&t("usage.unavailable")));
        root.append(&scrolled(&list));
        let speaking_css = gtk::CssProvider::new();
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &speaking_css,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
        Self {
            root,
            list,
            canonical_agents: std::rc::Rc::new(std::cell::RefCell::new(Vec::new())),
            latest: std::rc::Rc::new(std::cell::RefCell::new(Vec::new())),
            rendered: std::rc::Rc::new(std::cell::RefCell::new(HashMap::new())),
            empty_label,
            generation: std::rc::Rc::new(std::cell::Cell::new(0)),
            speaking_agent: std::rc::Rc::new(std::cell::RefCell::new(None)),
            speaking_wash: std::rc::Rc::new(std::cell::RefCell::new(None)),
            speaking_css,
        }
    }

    fn set_speaking_agent(&self, agent: Option<&str>) {
        let next = agent.map(str::to_string);
        if *self.speaking_agent.borrow() == next {
            return;
        }
        *self.speaking_agent.borrow_mut() = next;
        *self.speaking_wash.borrow_mut() = if self.speaking_agent.borrow().is_some() {
            random_pastel_wash()
        } else {
            None
        };
        self.reload_speaking_css();
        self.refresh_speaking_wash();
    }

    fn reload_speaking_css(&self) {
        let css = if let Some((r, g, b, a)) = *self.speaking_wash.borrow() {
            format!(
                ".ds-usage-speaking {{
                    background-color: rgba({r}, {g}, {b}, {a});
                    border-radius: 12px;
                }}"
            )
        } else {
            // Radius only while idle (class may briefly remain).
            ".ds-usage-speaking {
                border-radius: 12px;
            }"
            .to_string()
        };
        self.speaking_css.load_from_string(&css);
    }

    fn refresh_speaking_wash(&self) {
        let speaking = self.speaking_agent.borrow().clone();
        for (agent, mounted) in self.rendered.borrow().iter() {
            let on = speaking.as_ref().is_some_and(|s| s == agent);
            if on {
                mounted.stack.add_css_class("ds-usage-speaking");
            } else {
                mounted.stack.remove_css_class("ds-usage-speaking");
            }
        }
    }

    fn cancel_visible_request(&self) {
        self.generation.set(self.generation.get().saturating_add(1));
    }

    fn update_empty_state(&self, settled: bool) {
        let empty = self.latest.borrow().is_empty();
        if empty && settled {
            if self.empty_label.parent().is_none() {
                self.list.append(&self.empty_label);
            }
        } else if self.empty_label.parent().is_some() {
            self.list.remove(&self.empty_label);
        }
    }

    fn reconcile_agents(&self, installed: &[String]) {
        self.latest
            .borrow_mut()
            .retain(|card| installed.contains(&card.agent));
        let stale: Vec<String> = self
            .rendered
            .borrow()
            .keys()
            .filter(|agent| !installed.contains(agent))
            .cloned()
            .collect();
        for agent in stale {
            if let Some(mounted) = self.rendered.borrow_mut().remove(&agent) {
                self.list.remove(&mounted.stack);
            }
        }
    }

    fn apply_card(&self, card: UsageCard) {
        let changed = {
            let mut latest = self.latest.borrow_mut();
            if let Some(slot) = latest
                .iter_mut()
                .find(|current| current.agent == card.agent)
            {
                if *slot == card {
                    false
                } else {
                    *slot = card.clone();
                    true
                }
            } else {
                latest.push(card.clone());
                let order = self.canonical_agents.borrow();
                latest.sort_by_key(|current| {
                    order
                        .iter()
                        .position(|agent| agent == &current.agent)
                        .unwrap_or(usize::MAX)
                });
                true
            }
        };
        if !changed {
            return;
        }

        // Matching period shape + unchanged auth state → in-place update; else
        // crossfade remount (auth row appears/disappears with the remount).
        if let Some(mounted) = self.rendered.borrow().get(&card.agent)
            && mounted.needs_auth == card.needs_auth
            && try_update_usage_rows(&mounted.rows, &card)
        {
            set_usage_account_label(&mounted.account, card.account.as_deref());
            self.update_empty_state(false);
            return;
        }

        let (group, account, rows) = self.paint_usage_card(&card);
        if let Some(mounted) = self.rendered.borrow_mut().get_mut(&card.agent) {
            let stack = mounted.stack.clone();
            let previous = stack.visible_child();
            stack.add_child(&group);
            stack.set_visible_child(&group);
            mounted.account = account;
            mounted.rows = rows;
            mounted.needs_auth = card.needs_auth;
            if let Some(previous) = previous {
                let stack = stack.clone();
                gtk::glib::timeout_add_local_once(
                    std::time::Duration::from_millis(220),
                    move || {
                        if previous.parent().is_some() {
                            stack.remove(&previous);
                        }
                    },
                );
            }
        } else {
            let stack = gtk::Stack::builder()
                .transition_type(gtk::StackTransitionType::Crossfade)
                .transition_duration(180)
                .hexpand(true)
                .build();
            stack.add_child(&group);
            let previous = {
                let order = self.canonical_agents.borrow();
                let rendered = self.rendered.borrow();
                order
                    .iter()
                    .position(|agent| agent == &card.agent)
                    .and_then(|rank| {
                        order[..rank].iter().rev().find_map(|agent| {
                            rendered.get(agent).map(|mounted| mounted.stack.clone())
                        })
                    })
            };
            self.list.insert_child_after(&stack, previous.as_ref());
            self.rendered.borrow_mut().insert(
                card.agent,
                MountedUsageCard {
                    stack,
                    account,
                    rows,
                    needs_auth: card.needs_auth,
                },
            );
        }
        self.refresh_speaking_wash();
        self.update_empty_state(false);
    }

    fn on_tab_selected(&self) {
        let generation = self.generation.get().saturating_add(1);
        self.generation.set(generation);

        let (tx, rx) = async_channel::bounded::<Option<UsageDeck>>(1);
        if std::thread::Builder::new()
            .name("ds-agent-usage-skel".into())
            .spawn(move || {
                let _ = tx.send_blocking(crate::ffi::agent_usage_skeleton());
            })
            .is_err()
        {
            if self.generation.get() == generation {
                self.update_empty_state(true);
            }
            return;
        }
        let page = self.clone();
        gtk::glib::spawn_future_local(async move {
            let Ok(Some(deck)) = rx.recv().await else {
                if page.generation.get() == generation {
                    page.update_empty_state(true);
                }
                return;
            };
            if page.generation.get() != generation {
                return;
            }

            let agents: Vec<String> = deck.cards.iter().map(|c| c.agent.clone()).collect();
            *page.canonical_agents.borrow_mut() = agents.clone();
            page.reconcile_agents(&agents);
            for cached in deck.cards.into_iter().filter(|card| !card.rows.is_empty()) {
                page.apply_card(cached);
            }
            page.update_empty_state(agents.is_empty());

            if agents.is_empty() {
                return;
            }

            let remaining = std::rc::Rc::new(std::cell::Cell::new(agents.len()));
            for agent in agents {
                let (tx, rx) = async_channel::bounded::<Option<UsageCard>>(1);
                let agent_thread = agent.clone();
                let _ = std::thread::Builder::new()
                    .name("ds-agent-usage-one".into())
                    .spawn(move || {
                        let _ = tx.send_blocking(crate::ffi::agent_usage_card(&agent_thread, true));
                    });
                let page = page.clone();
                let remaining = remaining.clone();
                gtk::glib::spawn_future_local(async move {
                    let Ok(updated) = rx.recv().await else {
                        finish_one(&page, &remaining, generation);
                        return;
                    };
                    if page.generation.get() != generation {
                        return;
                    }
                    if let Some(updated) = updated
                        && (!updated.rows.is_empty() || updated.needs_auth)
                    {
                        page.apply_card(updated);
                    }
                    finish_one(&page, &remaining, generation);
                });
            }
        });
    }
}

fn finish_one(page: &UsagePage, remaining: &std::rc::Rc<std::cell::Cell<usize>>, generation: u64) {
    if page.generation.get() != generation {
        return;
    }
    let left = remaining.get().saturating_sub(1);
    remaining.set(left);
    if left == 0 {
        page.update_empty_state(true);
    }
}

fn try_update_usage_rows(mounted: &[UsageRowWidgets], card: &UsageCard) -> bool {
    if mounted.len() != card.rows.len() {
        return false;
    }
    for (view, row) in mounted.iter().zip(&card.rows) {
        if view.period != row.period {
            return false;
        }
    }
    for (view, row) in mounted.iter().zip(&card.rows) {
        update_usage_row(view, row);
    }
    true
}

fn update_usage_row(view: &UsageRowWidgets, row: &UsageRow) {
    // Bar = percent; remaining label = resets-in string from status_fmt.
    view.progress
        .set_fraction((row.used_percent / 100.0).clamp(0.0, 1.0));
    let remaining = crate::ffi::usage_resets_in(row.resets_at_unix);
    view.remaining.set_label(&remaining);
    view.remaining.set_visible(!remaining.is_empty());
}

/// Localized agent title; unknown tokens → prettified fallback.
fn agent_display_name(agent: &str) -> String {
    let key = format!("usage.provider.{agent}");
    let label = t(&key);
    if label == key {
        prettify_agent_token(agent)
    } else {
        label
    }
}

fn prettify_agent_token(agent: &str) -> String {
    agent
        .split('_')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => {
                    let mut out = first.to_uppercase().collect::<String>();
                    out.extend(chars.flat_map(|c| c.to_lowercase()));
                    out
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

impl UsagePage {
    fn paint_usage_card(
        &self,
        card: &UsageCard,
    ) -> (adw::PreferencesGroup, gtk::Label, Vec<UsageRowWidgets>) {
        let group = adw::PreferencesGroup::builder()
            .title(agent_display_name(&card.agent))
            .build();
        // Account top-right; session-only opacity toggle (starts transparent).
        let account = gtk::Label::builder()
            .halign(gtk::Align::End)
            .valign(gtk::Align::End)
            .xalign(1.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .max_width_chars(28)
            .build();
        account.add_css_class("dim-label");
        account.add_css_class("caption");
        account.set_opacity(0.0);
        account.set_can_target(true);
        {
            let label = account.clone();
            let click = gtk::GestureClick::new();
            click.connect_released(move |_, _, _, _| {
                if !label.is_visible() || label.text().is_empty() {
                    return;
                }
                let next = if label.opacity() < 0.5 { 1.0 } else { 0.0 };
                label.set_opacity(next);
            });
            account.add_controller(click);
        }
        set_usage_account_label(&account, card.account.as_deref());
        group.set_header_suffix(Some(&account));
        let mut rows = Vec::with_capacity(card.rows.len());
        for row in &card.rows {
            let (widget, view) = usage_row_widget(row);
            group.add(&widget);
            rows.push(view);
        }
        if card.needs_auth {
            group.add(&self.usage_auth_row(&card.agent));
        }
        (group, account, rows)
    }

    /// Guarded-credentials row: caption + the only UI path that may prompt.
    /// Click: disable, run the blocking authorize FFI on a named thread, apply
    /// the result through the generation-checked path.
    fn usage_auth_row(&self, agent: &str) -> gtk::Widget {
        let row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .margin_top(8)
            .margin_bottom(8)
            .margin_start(12)
            .margin_end(12)
            .build();
        let label = gtk::Label::builder()
            .label(t("usage.needs_auth"))
            .halign(gtk::Align::Start)
            .hexpand(true)
            .xalign(0.0)
            .wrap(true)
            .build();
        label.add_css_class("dim-label");
        let authorize = gtk::Button::with_label(&t("usage.authorize"));
        authorize.set_valign(gtk::Align::Center);
        row.append(&label);
        row.append(&authorize);

        let page = self.clone();
        let agent = agent.to_string();
        authorize.connect_clicked(move |button| {
            button.set_sensitive(false);
            let (tx, rx) = async_channel::bounded::<Option<UsageCard>>(1);
            let agent_thread = agent.clone();
            if std::thread::Builder::new()
                .name("ds-agent-usage-auth".into())
                .spawn(move || {
                    let _ = tx.send_blocking(crate::ffi::agent_usage_card_authorize(&agent_thread));
                })
                .is_err()
            {
                button.set_sensitive(true);
                return;
            }
            let generation = page.generation.get();
            let page = page.clone();
            let button = button.clone();
            gtk::glib::spawn_future_local(async move {
                let updated = rx.recv().await.ok().flatten();
                // Re-enable regardless: on deny the identical card skips repaint.
                button.set_sensitive(true);
                if page.generation.get() != generation {
                    return;
                }
                if let Some(updated) = updated
                    && (!updated.rows.is_empty() || updated.needs_auth)
                {
                    page.apply_card(updated);
                }
            });
        });
        row.upcast()
    }
}

fn set_usage_account_label(label: &gtk::Label, account: Option<&str>) {
    let text = account.map(str::trim).filter(|s| !s.is_empty());
    match text {
        Some(value) => {
            label.set_text(value);
            label.set_visible(true);
            // Keep current reveal; new widgets start at 0.
        }
        None => {
            label.set_text("");
            label.set_visible(false);
            label.set_opacity(0.0);
        }
    }
}

fn usage_row_widget(row: &UsageRow) -> (gtk::Widget, UsageRowWidgets) {
    let outer = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .margin_top(8)
        .margin_bottom(8)
        .margin_start(12)
        .margin_end(12)
        .build();

    let header = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .valign(gtk::Align::End)
        .build();
    let period = gtk::Label::builder()
        .label(t(&format!("usage.{}", row.period)))
        .halign(gtk::Align::Start)
        .valign(gtk::Align::End)
        .hexpand(true)
        .xalign(0.0)
        .yalign(1.0)
        .build();
    period.add_css_class("heading");
    let remaining_label = gtk::Label::builder()
        .halign(gtk::Align::End)
        .valign(gtk::Align::End)
        .xalign(1.0)
        .yalign(1.0)
        .build();
    remaining_label.add_css_class("dim-label");
    remaining_label.add_css_class("numeric");
    remaining_label.add_css_class("caption");
    header.append(&period);
    header.append(&remaining_label);

    let progress = gtk::ProgressBar::builder().hexpand(true).build();
    progress.add_css_class("ds-usage-progress");

    outer.append(&header);
    outer.append(&progress);

    let view = UsageRowWidgets {
        period: row.period.clone(),
        progress,
        remaining: remaining_label,
    };
    update_usage_row(&view, row);
    (outer.upcast(), view)
}

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
                    // Pre-built by status_fmt::tool_param_detail (host must not re-derive).
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
