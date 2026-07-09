# Plan: Split tts.rs — reader/demux vs supervision (issue #7)

## Problem

`tts.rs` is ~2656 lines mixing two distinct concerns: the helper stdio-protocol reader/demux thread (slot types + `reader_loop` + `describe_exit`) and the child supervision logic (`TtsManager` + spawn/env). The audit finding calls for splitting the reader/demux into its own module.

## Design decision: submodule vs sibling module

**Chosen: submodule `tts::reader`** (directory module `tts/mod.rs` + `tts/reader.rs`).

Reasons:
- All slot types are currently private to `tts` and used only by `TtsManager`'s fields and methods. A sibling module (`crate::tts_reader`) would force `pub(crate)` visibility on types that don't need to be crate-visible. A submodule keeps them within the `tts` boundary via `pub(super)`.
- `lib.rs` is unchanged — no new `mod` declaration, no module-doc update. The split is a pure internal implementation detail of the (already private) `tts` module.
- The crate already uses this pattern: `codex_stream/` is a directory module (`mod.rs` + submodules `client.rs`, `proto.rs`, `tests.rs`). Following the same convention for `tts/` is consistent.

No external crate or design research is needed — this is a mechanical module extraction. `reader_loop` already takes all dependencies as plain-value arguments and never touches `&self`, so the split is a pure code-move with visibility annotations.

## Step 1: Convert `tts.rs` → `tts/mod.rs`

Move the existing `tts.rs` to `tts/mod.rs` (`git mv`). This makes `tts/` a directory module, matching `codex_stream`'s convention. The `mod tts;` declaration in `lib.rs` resolves unchanged.

## Step 2: Create `tts/reader.rs`

Move these items from `tts/mod.rs` into `tts/reader.rs`:

**Types** (all become `pub(super)` with `pub(super)` fields):
- `struct SpeakSlot` — fields `done`, `err`, `fatal`
- `enum ListenEvt` — variants Partial/Final/Err/Done (keep the existing `#[cfg_attr(test, derive(Debug, PartialEq))]`)
- `struct ListenSlot` — fields `events`, `dead`
- `struct DiarizeSlot` — fields `result`, `done`, `dead`
- `struct EnrollSlot` — fields `result`, `done`, `dead`
- `struct ReaderSlots` — fields `speak`, `listen`, `diarize`, `enroll`
- `struct ReaderStats` — fields `tts`, `stt`, `lifetime`
- `struct ReaderModelState` — fields `tts_model`, `stt_model`, `stt_realized`, `gate`, `child`

**Functions** (`pub(super)`):
- `fn describe_exit(status: Option<std::process::ExitStatus>) -> String` — shared between reader and supervision. Placed in reader.rs because it makes reader.rs fully self-contained (zero dependencies on `mod.rs` code).
- `fn reader_loop(stdout: impl BufRead, slots: ReaderSlots, stats: ReaderStats, model: ReaderModelState)` — converted from an associated fn `TtsManager::reader_loop` to a free function. It never touches `&self`, so this is a signature-only change.

**Imports** the new file needs at its top:
```
use std::io::BufRead;
use std::sync::{Arc, Condvar, Mutex};

use ds_helper_proto as proto;

use crate::child_slot::ChildSlot;
use crate::model_slot::{ModelSlot, ModelState};
use crate::status::StatusGate;
```
Plus `crate::log` and `crate::logging::debug` for the two log calls inside `reader_loop`. `std::collections::VecDeque` is used inline in `ListenSlot`'s field type — either import it or keep it fully qualified (as-is).

**Tests that move** into `tts/reader.rs`:
- `mod dl_lifecycle_tests` (tests `describe_exit`) — `use super::describe_exit;` still resolves (super is now `reader`).
- `mod reader_eof_tests` (tests `reader_loop` EOF/demux + the `BlockThenClose` Read impl + all the demux tests) — these use `use super::*` which now imports from `reader.rs`. The test helper functions (`run_reader`, `run_reader_init`, `run_reader_init_errs`, `run_reader_slots`) call `TtsManager::reader_loop(...)` today — change each call to bare `reader_loop(...)` (same module now). The test module also needs its own imports for items it uses that were previously brought in by tts.rs's top-level `use` block: `std::sync::{Arc, Condvar, Mutex}`, `std::io::BufReader`, `std::sync::atomic::{AtomicBool, Ordering}`, `crate::model_slot::{ModelSlot, ModelState}`, `crate::child_slot::ChildSlot`, `tempfile` (already a dev-dep).

## Step 3: Edit `tts/mod.rs`

1. **Add submodule declaration** near the top: `mod reader;`
2. **Add re-import** so the slot types resolve in field types and method bodies without qualifying every reference: `use reader::*;` — this brings `SpeakSlot`, `ListenSlot`, `ListenEvt`, `DiarizeSlot`, `EnrollSlot`, `ReaderSlots`, `ReaderStats`, `ReaderModelState`, `describe_exit`, `reader_loop` into scope. With this glob import, most existing references in the supervision code need no change (field types like `Arc<(Mutex<SpeakSlot>, Condvar)>` resolve, and `ListenEvt::Partial(...)` in the `listen()` method resolves).
3. **Change the spawn-site call** (line ~928): `Self::reader_loop(...)` → `reader::reader_loop(...)` (or `reader_loop(...)` since it's glob-imported — but `reader::reader_loop` is clearer for the implementer and avoids any ambiguity; either compiles).
4. **Change `describe_exit` call** in `mark_dead_locked` (line ~1057): `describe_exit(status)` → works unchanged via the glob import. (Or `reader::describe_exit(status)` if preferred for clarity.)
5. **Delete the moved items**: the struct/enum definitions (lines 108–193), the `reader_loop` associated fn (lines 1096–1287), and the two moved test modules (`dl_lifecycle_tests`, `reader_eof_tests`).
6. **Items that stay** in `tts/mod.rs` unchanged:
   - `fn helper_stderr()` — stderr routing for spawn
   - `fn child_env(prefs: &SpawnPrefs)` — daemon→helper env contract
   - `struct SpawnPrefs` — spawn preferences (field of `TtsManager`)
   - `pub struct TtsManager` + all fields + `const HEAL_COOLDOWN`
   - `impl TtsManager { ... }` — all methods except `reader_loop`, including `fn resolve_provider()` (associated fn, used by `start_locked` + `set_provider`)
   - Test modules: `mod coexist_it`, `mod child_env_tests`, `mod status_gate_tests`

## What does NOT change

- **`lib.rs`**: nothing. The `mod tts;` declaration resolves to `tts/mod.rs` identically. The crate-doc module list doesn't mention `tts`'s internals.
- **External consumers** of `crate::tts::TtsManager` (boot.rs, engine.rs, downloads.rs, status.rs, helper_stt.rs, stt_test.rs, ttsq.rs, config_gate.rs): no change — `TtsManager`'s public API is untouched.
- **`Cargo.toml`**: no new dependencies.
- **FFI/`ds-core`**: untouched.
- **Config, i18n, deploy routes**: untouched.

## Visibility summary

| Item | Visibility | Why |
|------|-----------|-----|
| `SpeakSlot`, fields | `pub(super)` | TtsManager field type; supervision accesses `.done`/`.err`/`.fatal` |
| `ListenEvt` | `pub(super)` | Matched in `listen()` method (lines 1529–1532) |
| `ListenSlot`, fields | `pub(super)` | TtsManager field type; supervision accesses `.events`/`.dead` |
| `DiarizeSlot`, fields | `pub(super)` | TtsManager field type; supervision accesses `.result`/`.done`/`.dead` |
| `EnrollSlot`, fields | `pub(super)` | Same pattern as DiarizeSlot |
| `ReaderSlots`, fields | `pub(super)` | Constructed at spawn-site call |
| `ReaderStats`, fields | `pub(super)` | Constructed at spawn-site call |
| `ReaderModelState`, fields | `pub(super)` | Constructed at spawn-site call |
| `describe_exit` | `pub(super)` | Called from `mark_dead_locked` in mod.rs |
| `reader_loop` | `pub(super)` | Called from `start_locked` spawn closure in mod.rs |

All `pub(super)` = visible to `tts` module (the parent). Not visible to any other crate module.

## Verification

Run from `rust/` (the workspace root):
1. `cargo clippy -p dontspeakd --all-targets --locked -- -D warnings` — no new warnings (moved code + visibility changes should be clean).
2. `cargo test -p dontspeakd --locked` — all existing tests pass. The reader tests now live in `tts::reader::reader_eof_tests` and `tts::reader::dl_lifecycle_tests`; the supervision tests stay in `tts::status_gate_tests`, `tts::child_env_tests`, `tts::coexist_it`. The test binary names change but the test function names are identical, so `cargo test` filtering still works.
3. Optionally `cargo clippy --workspace --all-targets --locked -- -D warnings` to confirm no downstream breakage from the module restructure (there shouldn't be — the public API is untouched).

No `cargo fmt` needed as a blocking step (release-only gate per AGENTS.md).

## Risk

**Risk: no**

This is a pure internal module extraction within a single private module of one crate:
- Does not touch the FFI boundary (`ds-core`).
- Does not touch the `ds-ipc` socket protocol.
- Does not touch model download/checksum pinning (`ds-model`).
- Does not touch OS permission/entitlement handling.
- Does not add any dependency (no licensing concern).
- Does not touch the release/signing pipeline.
- No public API changes — `TtsManager`'s signature and all its pub/pub(crate) methods are identical. No consumer outside `tts` needs editing.
