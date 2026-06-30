//  MicPermission.swift
//
//  Pure, dependency-free policy for the microphone permission row in the Status tab — split into
//  this framework-free target so the visibility rule is unit-testable without linking the Rust
//  FFI staticlib or any system framework.

/// Whether DontSpeak itself opens the microphone for the selected STT engine — and therefore
/// whether the Status tab shows a Microphone permission row (and folds its grant into the Caps
/// Lock header dot).
///
/// macOS raises the microphone prompt LAZILY — the first time the audio input stream is actually
/// opened for dictation — never at launch and never when the Status tab appears. So the row is
/// only meaningful for the engines whose capture WE drive:
///   - `built_in` (local FastConformer) and `system` (the OS recognizer): DontSpeak opens the
///     mic, so the row is shown and its grant folds into the Caps Lock dot.
///   - `off`: dictation is disabled (gray dot) — nothing ever opens the mic, so HIDE the row.
///   - `claude_code`: Claude Code owns its own dictation; it prompts for and captures the mic
///     itself, so DontSpeak surfacing a mic grant would be misleading — HIDE the row.
///
/// An unknown/unrecognized token is treated as a capturing engine (row shown) — the conservative
/// default, matching the Status view's `default:` → Parakeet fallback.
public func dontSpeakUsesMicrophone(sttEngine token: String) -> Bool {
    switch token {
    case "off", "claude_code": return false
    default: return true
    }
}
