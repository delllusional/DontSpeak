/// Whether DontSpeak opens the mic for this STT engine token (Status Microphone row + Caps
/// Lock header dot). Mic prompt is lazy (first capture), never at launch. Row policy:
///   - `built_in` / `system`: we open mic → show
///   - `off`: dictation disabled → hide
///   - `claude_code`: Claude Code owns prompt + capture → hide (our grant would mislead)
///   - unknown: treat as capturing (show) — matches Status Parakeet fallback
public func dontSpeakUsesMicrophone(sttEngine token: String) -> Bool {
    switch token {
    case "off", "claude_code": return false
    default: return true
    }
}
