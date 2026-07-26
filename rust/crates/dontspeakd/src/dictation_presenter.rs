use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
struct Lease {
    id: String,
    session_id: String,
    generation: u64,
    ready: bool,
    expires_at: Instant,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct AcquiredLease {
    pub id: String,
    pub ttl_ms: u64,
}

pub(crate) struct PresenterMutation<T> {
    pub result: Result<T, String>,
    pub changed: bool,
}

pub(crate) struct DictationPresenterRegistry {
    namespace: String,
    lease: Mutex<Option<Lease>>,
}

impl Default for DictationPresenterRegistry {
    fn default() -> Self {
        Self {
            namespace: random_hex::<16>(),
            lease: Mutex::new(None),
        }
    }
}

impl DictationPresenterRegistry {
    pub(crate) fn session_id(&self, presentation_id: u64) -> String {
        format!("{}-{presentation_id:016x}", self.namespace)
    }

    pub(crate) fn acquire(
        &self,
        session_id: String,
        current_session_id: Option<&str>,
        ttl_ms: u64,
        now: Instant,
    ) -> PresenterMutation<AcquiredLease> {
        let ttl = match presenter_ttl(ttl_ms) {
            Ok(ttl) => ttl,
            Err(message) => return mutation_error(&message, false),
        };
        let mut slot = self.lease.lock().unwrap_or_else(|e| e.into_inner());
        let Some(generation) = self.session_generation(&session_id) else {
            return mutation_error("dictation presenter: invalid session", false);
        };
        let mut changed = prune_expired_locked(&mut slot, now);
        if current_session_id != Some(session_id.as_str()) {
            return mutation_error("dictation presenter: session is no longer active", changed);
        }
        changed |= prune_older_generation_locked(&mut slot, generation);
        if slot.is_some() {
            return mutation_error(
                "dictation presenter: a presenter lease is already active",
                changed,
            );
        }
        let lease = Lease {
            id: new_lease_id(),
            session_id,
            generation,
            ready: false,
            expires_at: now + ttl,
        };
        let acquired = AcquiredLease {
            id: lease.id.clone(),
            ttl_ms: ttl.as_millis() as u64,
        };
        *slot = Some(lease);
        PresenterMutation {
            result: Ok(acquired),
            changed,
        }
    }

    pub(crate) fn ready(
        &self,
        lease_id: &str,
        session_id: &str,
        current_session_id: Option<&str>,
        now: Instant,
    ) -> PresenterMutation<()> {
        let mut slot = self.lease.lock().unwrap_or_else(|e| e.into_inner());
        let mut changed = prune_expired_locked(&mut slot, now);
        let lease = match matching_lease_mut(&mut slot, lease_id, session_id) {
            Ok(lease) => lease,
            Err(message) => {
                return PresenterMutation {
                    result: Err(message),
                    changed,
                };
            }
        };
        if current_session_id != Some(lease.session_id.as_str()) {
            if current_session_id.is_none_or(|current| {
                self.session_generation(current)
                    .is_some_and(|generation| generation_is_newer(generation, lease.generation))
            }) {
                changed |= lease.ready;
                *slot = None;
            }
            return mutation_error("dictation presenter: session is no longer active", changed);
        }
        changed |= !lease.ready;
        lease.ready = true;
        PresenterMutation {
            result: Ok(()),
            changed,
        }
    }

    pub(crate) fn renew(
        &self,
        lease_id: &str,
        session_id: &str,
        current_session_id: Option<&str>,
        ttl_ms: u64,
        now: Instant,
    ) -> PresenterMutation<()> {
        let ttl = match presenter_ttl(ttl_ms) {
            Ok(ttl) => ttl,
            Err(message) => return mutation_error(&message, false),
        };
        let mut slot = self.lease.lock().unwrap_or_else(|e| e.into_inner());
        let mut changed = prune_expired_locked(&mut slot, now);
        let lease = match matching_lease_mut(&mut slot, lease_id, session_id) {
            Ok(lease) => lease,
            Err(message) => {
                return PresenterMutation {
                    result: Err(message),
                    changed,
                };
            }
        };
        if current_session_id != Some(lease.session_id.as_str()) {
            if current_session_id.is_none_or(|current| {
                self.session_generation(current)
                    .is_some_and(|generation| generation_is_newer(generation, lease.generation))
            }) {
                changed |= lease.ready;
                *slot = None;
            }
            return mutation_error("dictation presenter: session is no longer active", changed);
        }
        if !lease.ready {
            return mutation_error("dictation presenter: lease is not ready", changed);
        }
        lease.expires_at = now + ttl;
        PresenterMutation {
            result: Ok(()),
            changed,
        }
    }

    pub(crate) fn release(
        &self,
        lease_id: &str,
        session_id: &str,
        now: Instant,
    ) -> PresenterMutation<()> {
        let mut slot = self.lease.lock().unwrap_or_else(|e| e.into_inner());
        let changed = prune_expired_locked(&mut slot, now);
        let Some(lease) = slot.as_ref() else {
            return mutation_error("dictation presenter: lease not found", changed);
        };
        if lease.id != lease_id || lease.session_id != session_id {
            return mutation_error("dictation presenter: lease does not match", changed);
        }
        let changed = changed || lease.ready;
        *slot = None;
        PresenterMutation {
            result: Ok(()),
            changed,
        }
    }

    pub(crate) fn external_ui_active(
        &self,
        current_session_id: Option<&str>,
        now: Instant,
    ) -> (bool, bool) {
        let mut slot = self.lease.lock().unwrap_or_else(|e| e.into_inner());
        let mut changed = prune_expired_locked(&mut slot, now);
        if let Some(current_generation) =
            current_session_id.and_then(|session| self.session_generation(session))
        {
            changed |= prune_older_generation_locked(&mut slot, current_generation);
        }
        let active = slot.as_ref().is_some_and(|lease| {
            lease.ready && current_session_id == Some(lease.session_id.as_str())
        });
        (active, changed)
    }

    #[cfg(test)]
    fn current(&self) -> Option<(String, bool)> {
        self.lease
            .lock()
            .unwrap()
            .as_ref()
            .map(|lease| (lease.session_id.clone(), lease.ready))
    }

    fn session_generation(&self, session_id: &str) -> Option<u64> {
        let generation = session_id
            .strip_prefix(&self.namespace)?
            .strip_prefix('-')?;
        (generation.len() == 16)
            .then(|| u64::from_str_radix(generation, 16).ok())
            .flatten()
    }
}

fn mutation_error<T>(message: &str, changed: bool) -> PresenterMutation<T> {
    PresenterMutation {
        result: Err(message.to_string()),
        changed,
    }
}

fn matching_lease_mut<'a>(
    slot: &'a mut Option<Lease>,
    lease_id: &str,
    session_id: &str,
) -> Result<&'a mut Lease, String> {
    let lease = slot
        .as_mut()
        .ok_or_else(|| "dictation presenter: lease not found".to_string())?;
    if lease.id != lease_id || lease.session_id != session_id {
        return Err("dictation presenter: lease does not match".into());
    }
    Ok(lease)
}

fn prune_expired_locked(slot: &mut Option<Lease>, now: Instant) -> bool {
    let revoke = slot.as_ref().is_some_and(|lease| lease.expires_at <= now);
    let changed = revoke && slot.as_ref().is_some_and(|lease| lease.ready);
    if revoke {
        *slot = None;
    }
    changed
}

fn prune_older_generation_locked(slot: &mut Option<Lease>, current_generation: u64) -> bool {
    let revoke = slot
        .as_ref()
        .is_some_and(|lease| generation_is_newer(current_generation, lease.generation));
    let changed = revoke && slot.as_ref().is_some_and(|lease| lease.ready);
    if revoke {
        *slot = None;
    }
    changed
}

fn generation_is_newer(candidate: u64, current: u64) -> bool {
    let distance = candidate.wrapping_sub(current);
    distance != 0 && distance < (1_u64 << 63)
}

fn presenter_ttl(ttl_ms: u64) -> Result<Duration, String> {
    ds_ipc::validate_presenter_ttl_ms(ttl_ms)
        .map_err(|message| format!("dictation presenter: {message}"))?;
    Ok(Duration::from_millis(ttl_ms))
}

fn new_lease_id() -> String {
    random_hex::<32>()
}

fn random_hex<const N: usize>() -> String {
    let mut bytes = [0_u8; N];
    getrandom::fill(&mut bytes).expect("OS entropy must be available for presenter leases");
    bytes
        .iter()
        .fold(String::with_capacity(N * 2), |mut hex, byte| {
            use std::fmt::Write as _;
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acquire_is_not_active_until_ready() {
        let registry = DictationPresenterRegistry::default();
        let session = registry.session_id(7);
        let now = Instant::now();
        let acquired = registry.acquire(session.clone(), Some(&session), 3_500, now);
        assert!(!acquired.changed);
        let lease = acquired.result.unwrap();
        assert_eq!(registry.current(), Some((session.clone(), false)));
        assert_eq!(
            registry.external_ui_active(Some(&session), now),
            (false, false)
        );

        let ready = registry.ready(&lease.id, &session, Some(&session), now);
        assert!(ready.changed);
        ready.result.unwrap();
        assert_eq!(
            registry.external_ui_active(Some(&session), now),
            (true, false)
        );
    }

    #[test]
    fn lease_is_scoped_to_one_session() {
        let registry = DictationPresenterRegistry::default();
        let session = registry.session_id(7);
        let other_session = registry.session_id(8);
        let now = Instant::now();
        let lease = registry
            .acquire(session.clone(), Some(&session), 3_500, now)
            .result
            .unwrap();
        let wrong_session = registry.ready(&lease.id, &other_session, Some(&other_session), now);
        assert!(wrong_session.result.unwrap_err().contains("does not match"));
        assert!(!wrong_session.changed);
        assert_eq!(registry.current(), Some((session, false)));
    }

    #[test]
    fn no_presenter_can_replace_a_live_lease() {
        let registry = DictationPresenterRegistry::default();
        let session = registry.session_id(7);
        let now = Instant::now();
        registry
            .acquire(session.clone(), Some(&session), 3_500, now)
            .result
            .unwrap();
        assert!(
            registry
                .acquire(session.clone(), Some(&session), 3_500, now)
                .result
                .unwrap_err()
                .contains("already active")
        );
    }

    #[test]
    fn expiry_and_release_restore_native_fallback() {
        let registry = DictationPresenterRegistry::default();
        let session = registry.session_id(7);
        let now = Instant::now();
        let lease = registry
            .acquire(session.clone(), Some(&session), 500, now)
            .result
            .unwrap();
        registry
            .ready(&lease.id, &session, Some(&session), now)
            .result
            .unwrap();
        assert_eq!(
            registry.external_ui_active(Some(&session), now + Duration::from_millis(501)),
            (false, true)
        );

        let lease = registry
            .acquire(session.clone(), Some(&session), 3_500, now)
            .result
            .unwrap();
        registry
            .ready(&lease.id, &session, Some(&session), now)
            .result
            .unwrap();
        let released = registry.release(&lease.id, &session, now);
        assert!(released.changed);
        released.result.unwrap();
        assert_eq!(
            registry.external_ui_active(Some(&session), now),
            (false, false)
        );
    }

    #[test]
    fn renew_preserves_ready_and_extends_the_deadline() {
        let registry = DictationPresenterRegistry::default();
        let session = registry.session_id(7);
        let now = Instant::now();
        let lease = registry
            .acquire(session.clone(), Some(&session), 500, now)
            .result
            .unwrap();
        registry
            .ready(&lease.id, &session, Some(&session), now)
            .result
            .unwrap();
        let renewed = registry.renew(
            &lease.id,
            &session,
            Some(&session),
            2_000,
            now + Duration::from_millis(400),
        );
        assert!(!renewed.changed);
        renewed.result.unwrap();
        assert_eq!(
            registry.external_ui_active(Some(&session), now + Duration::from_millis(1_500)),
            (true, false)
        );
    }

    #[test]
    fn a_reserved_lease_cannot_be_renewed_before_first_render() {
        let registry = DictationPresenterRegistry::default();
        let session = registry.session_id(7);
        let now = Instant::now();
        let lease = registry
            .acquire(session.clone(), Some(&session), 500, now)
            .result
            .unwrap();
        let renewed = registry.renew(&lease.id, &session, Some(&session), 2_000, now);
        assert!(renewed.result.unwrap_err().contains("not ready"));
        assert!(!renewed.changed);
        assert_eq!(
            registry.external_ui_active(Some(&session), now + Duration::from_millis(501)),
            (false, false)
        );
    }

    #[test]
    fn ttl_policy_rejects_out_of_range_values_instead_of_clamping() {
        let registry = DictationPresenterRegistry::default();
        let session = registry.session_id(7);
        let now = Instant::now();

        assert!(
            registry
                .acquire(session.clone(), Some(&session), 499, now)
                .result
                .unwrap_err()
                .contains("between 500 and 60000")
        );
        assert_eq!(registry.current(), None);
    }

    #[test]
    fn stale_session_requests_cannot_revoke_a_newer_live_lease() {
        let registry = DictationPresenterRegistry::default();
        let old_session = registry.session_id(7);
        let new_session = registry.session_id(8);
        let now = Instant::now();
        let old = registry
            .acquire(old_session.clone(), Some(&old_session), 3_500, now)
            .result
            .unwrap();
        registry.release(&old.id, &old_session, now).result.unwrap();

        let current = registry
            .acquire(new_session.clone(), Some(&new_session), 3_500, now)
            .result
            .unwrap();
        registry
            .ready(&current.id, &new_session, Some(&new_session), now)
            .result
            .unwrap();

        let delayed_acquire = registry.acquire(old_session.clone(), Some(&old_session), 3_500, now);
        assert!(
            delayed_acquire
                .result
                .unwrap_err()
                .contains("already active")
        );
        assert!(!delayed_acquire.changed);

        let delayed_ready = registry.ready(&old.id, &old_session, Some(&old_session), now);
        assert!(delayed_ready.result.unwrap_err().contains("does not match"));
        assert!(!delayed_ready.changed);

        let delayed_renew = registry.renew(&old.id, &old_session, Some(&old_session), 3_500, now);
        assert!(delayed_renew.result.unwrap_err().contains("does not match"));
        assert!(!delayed_renew.changed);
        assert_eq!(registry.current(), Some((new_session, true)));
    }

    #[test]
    fn matching_stale_lease_revocation_reports_ready_state_change_on_error() {
        let registry = DictationPresenterRegistry::default();
        let session = registry.session_id(7);
        let next_session = registry.session_id(8);
        let now = Instant::now();
        let lease = registry
            .acquire(session.clone(), Some(&session), 3_500, now)
            .result
            .unwrap();
        registry
            .ready(&lease.id, &session, Some(&session), now)
            .result
            .unwrap();

        let stale = registry.renew(&lease.id, &session, Some(&next_session), 3_500, now);
        assert!(stale.result.unwrap_err().contains("no longer active"));
        assert!(stale.changed);
        assert_eq!(registry.current(), None);
    }

    #[test]
    fn a_new_generation_replaces_only_an_older_session_lease() {
        let registry = DictationPresenterRegistry::default();
        let old_session = registry.session_id(7);
        let new_session = registry.session_id(8);
        let now = Instant::now();
        let old = registry
            .acquire(old_session.clone(), Some(&old_session), 3_500, now)
            .result
            .unwrap();
        registry
            .ready(&old.id, &old_session, Some(&old_session), now)
            .result
            .unwrap();

        let new = registry.acquire(new_session.clone(), Some(&new_session), 3_500, now);
        assert!(new.changed);
        new.result.unwrap();
        assert_eq!(registry.current(), Some((new_session, false)));
    }

    #[test]
    fn generated_session_ids_are_stable_per_registry_and_unique_across_restarts() {
        let first = DictationPresenterRegistry::default();
        let second = DictationPresenterRegistry::default();
        assert_eq!(first.session_id(7), first.session_id(7));
        assert_ne!(first.session_id(7), first.session_id(8));
        assert_ne!(first.session_id(7), second.session_id(7));
    }

    #[test]
    fn presentation_generation_order_survives_counter_wrap() {
        assert!(generation_is_newer(8, 7));
        assert!(!generation_is_newer(7, 8));
        assert!(generation_is_newer(0, u64::MAX));
        assert!(!generation_is_newer(u64::MAX, 0));
        assert!(!generation_is_newer(7, 7));
    }
}
