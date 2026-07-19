//! Agents tab: one card per installed agent. [`skeleton`] offline; [`refresh_card`] blocking.
//! Credentials read-only; install via `ClientSpec::present`.

mod providers;

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ds_client::ClientSource;
use serde::{Deserialize, Serialize};

const CACHE_TTL: Duration = Duration::from_secs(60);
const CACHE_FILE: &str = "agent-usage-cache.json";
const MAX_CACHE_BYTES: u64 = 64 * 1024;

/// Wire tokens match `ds-i18n` `usage.<period>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Period {
    Session,
    Week,
    Month,
}

/// One quota gauge (period + percent + reset).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageRow {
    pub period: Period,
    pub used_percent: f64,
    pub resets_at_unix: i64,
}

impl UsageRow {
    pub(crate) fn checked(period: Period, used_percent: f64, resets_at_unix: i64) -> Option<Self> {
        if !used_percent.is_finite() || resets_at_unix <= 0 {
            return None;
        }
        Some(Self {
            period,
            used_percent: used_percent.clamp(0.0, 100.0),
            resets_at_unix,
        })
    }
}

/// One Agents tab card.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageCard {
    /// `claude_code` | `codex` | `qwen_code` | `grok` | `kimi_code`.
    pub agent: ClientSource,
    /// Local login label when present (absent for API-key-only / missing identity).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// Session → week → month. Empty until loaded / unavailable.
    pub rows: Vec<UsageRow>,
}

impl UsageCard {
    fn empty(agent: ClientSource) -> Self {
        Self {
            agent,
            account: None,
            rows: Vec::new(),
        }
    }

    fn from_result(
        agent: ClientSource,
        account: Option<String>,
        result: std::io::Result<Vec<UsageRow>>,
    ) -> Self {
        let mut card = Self {
            agent,
            account,
            rows: result.unwrap_or_default(),
        };
        card.normalize();
        card
    }

    fn normalize(&mut self) {
        if let Some(account) = self.account.take() {
            self.account = normalize_account(&account);
        }
        self.rows
            .retain(|row| row.used_percent.is_finite() && row.resets_at_unix > 0);
        for row in &mut self.rows {
            row.used_percent = row.used_percent.clamp(0.0, 100.0);
        }
        self.rows.sort_by_key(|row| row.period);
        self.rows.dedup_by_key(|row| row.period);
    }

    pub fn has_data(&self) -> bool {
        !self.rows.is_empty()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            serde_json::json!({ "agent": self.agent.as_str(), "rows": [] }).to_string()
        })
    }
}

/// Trim + cap (hostile credential files). Login label may be non-email.
fn normalize_account(raw: &str) -> Option<String> {
    const MAX_LEN: usize = 128;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut out = trimmed.chars().take(MAX_LEN).collect::<String>();
    if out.chars().count() == MAX_LEN && trimmed.chars().count() > MAX_LEN {
        out.push('…');
    }
    Some(out)
}

/// Ordered cards for installed agents.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageDeck {
    pub cards: Vec<UsageCard>,
}

impl UsageDeck {
    pub fn empty() -> Self {
        Self { cards: Vec::new() }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| r#"{"cards":[]}"#.to_string())
    }
}

// ── Cache ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedCard {
    fetched_at_unix: i64,
    card: UsageCard,
}

#[derive(Debug, Serialize, Deserialize)]
struct CacheFile {
    cards: Vec<CachedCard>,
}

#[derive(Default)]
struct UsageCache {
    loaded_from: Option<PathBuf>,
    cards: HashMap<ClientSource, CachedCard>,
}

impl UsageCache {
    fn ensure_loaded(&mut self, paths: &ds_config::Paths) {
        let path = cache_path(paths);
        if self.loaded_from.as_ref() == Some(&path) {
            return;
        }
        self.loaded_from = Some(path.clone());
        self.cards.clear();

        let Some(snapshot) = read_cache_file(&path) else {
            return;
        };
        for mut cached in snapshot.cards {
            cached.card.normalize();
            if cached.card.agent.is_client() && cached.card.has_data() {
                self.cards.insert(cached.card.agent, cached);
            }
        }
    }

    fn get(&mut self, paths: &ds_config::Paths, agent: ClientSource) -> Option<CachedCard> {
        self.ensure_loaded(paths);
        self.cards.get(&agent).cloned()
    }

    fn store(&mut self, paths: &ds_config::Paths, card: UsageCard) {
        if !card.has_data() {
            return;
        }
        self.ensure_loaded(paths);
        self.cards.insert(
            card.agent,
            CachedCard {
                fetched_at_unix: now_unix(),
                card,
            },
        );
        self.persist(paths);
    }

    fn persist(&self, paths: &ds_config::Paths) {
        let cards = ClientSource::CLIENTS
            .iter()
            .filter_map(|agent| self.cards.get(agent).cloned())
            .collect();
        let snapshot = CacheFile { cards };
        if let Ok(value) = serde_json::to_value(snapshot) {
            let _ = ds_config::atomic_write_json(&cache_path(paths), &value);
        }
    }
}

#[derive(Default)]
struct RefreshSlot {
    last_finished_at: Option<Instant>,
}

static CACHE: OnceLock<Mutex<UsageCache>> = OnceLock::new();
static REFRESH_SLOTS: OnceLock<HashMap<ClientSource, Mutex<RefreshSlot>>> = OnceLock::new();

fn cache() -> &'static Mutex<UsageCache> {
    CACHE.get_or_init(|| Mutex::new(UsageCache::default()))
}

fn refresh_slots() -> &'static HashMap<ClientSource, Mutex<RefreshSlot>> {
    REFRESH_SLOTS.get_or_init(|| {
        ClientSource::CLIENTS
            .iter()
            .map(|&agent| (agent, Mutex::new(RefreshSlot::default())))
            .collect()
    })
}

fn cache_path(paths: &ds_config::Paths) -> PathBuf {
    paths.cache_dir.join(CACHE_FILE)
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .try_into()
        .unwrap_or(i64::MAX)
}

fn read_cache_file(path: &Path) -> Option<CacheFile> {
    let file = std::fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() > MAX_CACHE_BYTES {
        return None;
    }
    let mut json = String::new();
    file.take(MAX_CACHE_BYTES.saturating_add(1))
        .read_to_string(&mut json)
        .ok()?;
    if json.len() as u64 > MAX_CACHE_BYTES {
        return None;
    }
    serde_json::from_str(&json).ok()
}

fn cached_card(
    cache: &Mutex<UsageCache>,
    paths: &ds_config::Paths,
    agent: ClientSource,
) -> Option<CachedCard> {
    cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(paths, agent)
}

// ── Install gate (wire registry) ────────────────────────────────────────────

fn client_installed(paths: &ds_config::Paths, client: ClientSource) -> bool {
    ds_config::client_spec(client).is_some_and(|spec| (spec.present)(paths))
}

fn installed_agents(paths: &ds_config::Paths) -> Vec<ClientSource> {
    ClientSource::CLIENTS
        .iter()
        .copied()
        .filter(|&client| client_installed(paths, client))
        .collect()
}

fn fetch_rows(paths: &ds_config::Paths, agent: ClientSource) -> std::io::Result<Vec<UsageRow>> {
    match agent {
        ClientSource::ClaudeCode => providers::claude::fetch(paths),
        ClientSource::Codex => providers::codex::fetch(paths),
        ClientSource::QwenCode => providers::qwen::fetch(paths),
        ClientSource::Grok => providers::grok::fetch(paths),
        ClientSource::KimiCode => providers::kimi::fetch(paths),
        ClientSource::DontSpeak | ClientSource::Unknown => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a usage agent",
        )),
    }
}

/// Local identity only (offline).
fn fetch_account(paths: &ds_config::Paths, agent: ClientSource) -> Option<String> {
    match agent {
        ClientSource::ClaudeCode => providers::claude::account(paths),
        ClientSource::Codex => providers::codex::account(paths),
        ClientSource::Grok => providers::grok::account(paths),
        // Qwen Coding Plan is API-key only; Kimi Code has no documented email source.
        ClientSource::QwenCode
        | ClientSource::KimiCode
        | ClientSource::DontSpeak
        | ClientSource::Unknown => None,
    }
}

// ── Public API ──────────────────────────────────────────────────────────────

pub fn parse_agent(token: &str) -> ClientSource {
    ClientSource::parse(token).unwrap_or(ClientSource::Unknown)
}

/// Installed agents + cached rows. No network.
pub fn skeleton() -> UsageDeck {
    let Some(paths) = ds_config::Paths::resolve() else {
        return UsageDeck::empty();
    };
    let mut cache = cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache.ensure_loaded(&paths);
    let cards = installed_agents(&paths)
        .into_iter()
        .map(|agent| {
            cache
                .cards
                .get(&agent)
                .map(|entry| entry.card.clone())
                .unwrap_or_else(|| UsageCard::empty(agent))
        })
        .collect();
    UsageDeck { cards }
}

/// Blocking one-card refresh. Soft = 60s cache; keep last good card on empty (rate limits).
pub fn refresh_card(agent: ClientSource, force: bool) -> UsageCard {
    if !agent.is_client() {
        return UsageCard::empty(agent);
    }
    let Some(paths) = ds_config::Paths::resolve() else {
        return UsageCard::empty(agent);
    };
    if !client_installed(&paths, agent) {
        return UsageCard::empty(agent);
    }
    let Some(slot) = refresh_slots().get(&agent) else {
        return UsageCard::empty(agent);
    };
    refresh_card_with(
        cache(),
        slot,
        &paths,
        agent,
        force,
        || fetch_account(&paths, agent),
        || fetch_rows(&paths, agent),
    )
}

fn refresh_card_with<A, F>(
    cache: &Mutex<UsageCache>,
    slot: &Mutex<RefreshSlot>,
    paths: &ds_config::Paths,
    agent: ClientSource,
    force: bool,
    account: A,
    fetch: F,
) -> UsageCard
where
    A: FnOnce() -> Option<String>,
    F: FnOnce() -> std::io::Result<Vec<UsageRow>>,
{
    let requested_at = Instant::now();
    let mut slot = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    // Overlapping wait: reuse the result that finished after we started (even if force).
    if slot
        .last_finished_at
        .is_some_and(|finished_at| finished_at >= requested_at)
    {
        return cached_card(cache, paths, agent)
            .map_or_else(|| UsageCard::empty(agent), |entry| entry.card);
    }

    let now = now_unix();
    if !force
        && let Some(entry) = cached_card(cache, paths, agent)
        && entry.fetched_at_unix <= now
        && now.saturating_sub(entry.fetched_at_unix) < CACHE_TTL.as_secs() as i64
    {
        return entry.card;
    }

    let card = UsageCard::from_result(agent, account(), fetch());
    let returned = if card.has_data() {
        cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .store(paths, card.clone());
        card
    } else {
        cached_card(cache, paths, agent).map_or(card, |entry| entry.card)
    };
    slot.last_finished_at = Some(Instant::now());
    returned
}

/// Aggregate refresh (tests/tooling). UI prefers skeleton + per-card.
pub fn snapshot(refresh: bool) -> UsageDeck {
    let Some(paths) = ds_config::Paths::resolve() else {
        return UsageDeck::empty();
    };
    let agents = installed_agents(&paths);
    if agents.is_empty() {
        return UsageDeck::empty();
    }
    std::thread::scope(|scope| {
        let handles: Vec<_> = agents
            .iter()
            .map(|&agent| scope.spawn(move || refresh_card(agent, refresh)))
            .collect();
        let cards = handles
            .into_iter()
            .filter_map(|handle| handle.join().ok())
            .collect();
        UsageDeck { cards }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_clamps_and_rejects_invalid() {
        assert_eq!(
            UsageRow::checked(Period::Week, 125.0, 1)
                .unwrap()
                .used_percent,
            100.0
        );
        assert!(UsageRow::checked(Period::Week, f64::NAN, 1).is_none());
        assert!(UsageRow::checked(Period::Week, 20.0, 0).is_none());
    }

    #[test]
    fn card_orders_and_dedups_periods() {
        let card = UsageCard::from_result(
            ClientSource::QwenCode,
            Some("  user@example.com  ".into()),
            Ok(vec![
                UsageRow::checked(Period::Month, 40.0, 4).unwrap(),
                UsageRow::checked(Period::Week, 20.0, 2).unwrap(),
                UsageRow::checked(Period::Session, 10.0, 1).unwrap(),
                UsageRow::checked(Period::Week, 30.0, 3).unwrap(),
            ]),
        );
        assert_eq!(
            card.rows.iter().map(|r| r.period).collect::<Vec<_>>(),
            vec![Period::Session, Period::Week, Period::Month]
        );
        assert_eq!(card.rows[1].used_percent, 20.0);
        assert_eq!(card.account.as_deref(), Some("user@example.com"));
        assert!(card.has_data());
    }

    #[test]
    fn normalize_account_trims_and_caps() {
        assert_eq!(normalize_account("  a@b.co  ").as_deref(), Some("a@b.co"));
        assert!(normalize_account("   ").is_none());
        let long = "x".repeat(200);
        let capped = normalize_account(&long).unwrap();
        assert!(capped.chars().count() <= 129);
        assert!(capped.ends_with('…'));
    }

    #[test]
    fn install_gate_matches_wire_registry() {
        let root = tempfile::tempdir().unwrap();
        let paths = ds_config::Paths::rooted_at(root.path());
        assert!(!client_installed(&paths, ClientSource::ClaudeCode));
        std::fs::create_dir_all(&paths.claude_dir).unwrap();
        assert_eq!(installed_agents(&paths), vec![ClientSource::ClaudeCode]);
    }

    #[test]
    fn installed_agents_follow_the_canonical_client_enum_order() {
        let root = tempfile::tempdir().unwrap();
        let paths = ds_config::Paths::rooted_at(root.path());
        for &client in ClientSource::CLIENTS {
            let spec = ds_config::client_spec(client).unwrap();
            std::fs::create_dir_all((spec.detect_dir)(&paths)).unwrap();
        }

        assert_eq!(installed_agents(&paths), ClientSource::CLIENTS);
    }

    #[test]
    fn cache_keeps_last_good_card() {
        let root = tempfile::tempdir().unwrap();
        let paths = ds_config::Paths::rooted_at(root.path());
        let agent = ClientSource::Grok;
        let good = UsageCard {
            agent,
            account: Some("user@x.ai".into()),
            rows: vec![UsageRow::checked(Period::Week, 8.0, 1_800_000_000).unwrap()],
        };
        let cache = Mutex::new(UsageCache::default());
        cache.lock().unwrap().store(&paths, good.clone());
        let returned = refresh_card_with(
            &cache,
            &Mutex::new(RefreshSlot::default()),
            &paths,
            agent,
            true,
            || Some("ignored@x.ai".into()),
            || Ok(Vec::new()),
        );
        assert_eq!(returned, good);
    }

    #[test]
    fn cache_roundtrips_last_good_cards_across_instances() {
        let root = tempfile::tempdir().unwrap();
        let paths = ds_config::Paths::rooted_at(root.path());
        let good = UsageCard {
            agent: ClientSource::Codex,
            account: Some("dev@openai.com".into()),
            rows: vec![UsageRow::checked(Period::Session, 42.0, 1_900_000_000).unwrap()],
        };

        let mut first = UsageCache::default();
        first.store(&paths, good.clone());
        let mut reloaded = UsageCache::default();

        assert_eq!(
            reloaded
                .get(&paths, ClientSource::Codex)
                .map(|entry| entry.card),
            Some(good)
        );
    }

    #[test]
    fn fresh_persisted_cache_skips_a_soft_fetch() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let root = tempfile::tempdir().unwrap();
        let paths = ds_config::Paths::rooted_at(root.path());
        let good = UsageCard {
            agent: ClientSource::QwenCode,
            account: None,
            rows: vec![UsageRow::checked(Period::Month, 23.0, 1_900_000_000).unwrap()],
        };
        let mut initial = UsageCache::default();
        initial.store(&paths, good.clone());
        let fetches = AtomicUsize::new(0);

        let returned = refresh_card_with(
            &Mutex::new(UsageCache::default()),
            &Mutex::new(RefreshSlot::default()),
            &paths,
            ClientSource::QwenCode,
            false,
            || None,
            || {
                fetches.fetch_add(1, Ordering::SeqCst);
                Ok(Vec::new())
            },
        );

        assert_eq!(returned, good);
        assert_eq!(fetches.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn cache_load_normalizes_rows_and_drops_non_clients() {
        let root = tempfile::tempdir().unwrap();
        let paths = ds_config::Paths::rooted_at(root.path());
        let snapshot = CacheFile {
            cards: vec![
                CachedCard {
                    fetched_at_unix: now_unix(),
                    card: UsageCard {
                        agent: ClientSource::Codex,
                        account: None,
                        rows: vec![
                            UsageRow {
                                period: Period::Week,
                                used_percent: 150.0,
                                resets_at_unix: 1_900_000_000,
                            },
                            UsageRow {
                                period: Period::Month,
                                used_percent: 10.0,
                                resets_at_unix: 0,
                            },
                        ],
                    },
                },
                CachedCard {
                    fetched_at_unix: now_unix(),
                    card: UsageCard {
                        agent: ClientSource::DontSpeak,
                        account: None,
                        rows: vec![UsageRow::checked(Period::Week, 1.0, 1_900_000_000).unwrap()],
                    },
                },
            ],
        };
        ds_config::atomic_write_json(
            &cache_path(&paths),
            &serde_json::to_value(snapshot).unwrap(),
        )
        .unwrap();

        let mut cache = UsageCache::default();
        let card = cache.get(&paths, ClientSource::Codex).unwrap().card;
        assert_eq!(card.rows.len(), 1);
        assert_eq!(card.rows[0].used_percent, 100.0);
        assert!(cache.get(&paths, ClientSource::DontSpeak).is_none());
    }

    #[test]
    fn overlapping_forced_refreshes_reuse_one_fetch() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Barrier};

        let root = tempfile::tempdir().unwrap();
        let paths = ds_config::Paths::rooted_at(root.path());
        let cache = Arc::new(Mutex::new(UsageCache::default()));
        let slot = Arc::new(Mutex::new(RefreshSlot::default()));
        let starts = Arc::new(Barrier::new(3));
        let fetches = Arc::new(AtomicUsize::new(0));

        let handles: Vec<_> = (0..2)
            .map(|_| {
                let cache = Arc::clone(&cache);
                let slot = Arc::clone(&slot);
                let starts = Arc::clone(&starts);
                let fetches = Arc::clone(&fetches);
                let paths = paths.clone();
                std::thread::spawn(move || {
                    starts.wait();
                    refresh_card_with(
                        &cache,
                        &slot,
                        &paths,
                        ClientSource::ClaudeCode,
                        true,
                        || Some("claude@example.com".into()),
                        || {
                            fetches.fetch_add(1, Ordering::SeqCst);
                            std::thread::sleep(Duration::from_millis(20));
                            Ok(vec![
                                UsageRow::checked(Period::Week, 11.0, 1_900_000_000).unwrap(),
                            ])
                        },
                    )
                })
            })
            .collect();
        starts.wait();
        let cards: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();

        assert_eq!(fetches.load(Ordering::SeqCst), 1);
        assert_eq!(cards[0], cards[1]);
        assert!(cards[0].has_data());
    }

    #[test]
    fn deck_json_uses_cards_and_agent() {
        let deck = UsageDeck {
            cards: vec![UsageCard {
                agent: ClientSource::ClaudeCode,
                account: Some("me@anthropic.test".into()),
                rows: vec![UsageRow::checked(Period::Session, 10.0, 1).unwrap()],
            }],
        };
        let json = deck.to_json();
        assert!(json.contains("\"cards\""));
        assert!(json.contains("\"agent\":\"claude_code\""));
        assert!(json.contains("\"account\":\"me@anthropic.test\""));
        assert!(json.contains("\"rows\""));
        assert!(!json.contains("\"schema_version\""));
        assert!(!json.contains("\"providers\""));
        assert!(!json.contains("\"client\""));
    }
}
