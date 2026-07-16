/// Whether DontSpeak itself opens the mic for this STT engine token — and thus whether
/// the Status tab shows a Microphone permission row (and folds its grant into the Caps Lock
/// header dot).
///
/// macOS raises the mic prompt LAZILY (first real capture), never at launch. Row policy:
///   - `built_in` / `system`: we open the mic → show row
///   - `off`: dictation disabled → hide
///   - `claude_code`: Claude Code owns mic prompt + capture → hide (our grant would mislead)
///   - unknown token: treat as capturing (row shown) — matches Status view's Parakeet fallback
public func dontSpeakUsesMicrophone(sttEngine token: String) -> Bool {
    switch token {
    case "off", "claude_code": return false
    default: return true
    }
}
