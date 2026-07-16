// Bridge to shared ds-i18n via ds-core. All UI strings go through `L.t(...)`.

import CDontSpeak
import Foundation

enum L {
    /// Localized string for `key` (English fallback; missing key returns the key).
    static func t(_ key: String) -> String {
        key.withCString { kp in
            guard let ptr = ds_t(kp) else { return key }
            defer { ds_string_free(ptr) }
            return String(cString: ptr)
        }
    }

    /// `key` with `%{name}` placeholders from `args`. Caller formats numbers as strings.
    static func t(_ key: String, _ args: [String: String]) -> String {
        let json =
            (try? JSONSerialization.data(withJSONObject: args))
            .flatMap { String(data: $0, encoding: .utf8) } ?? "{}"
        return key.withCString { kp in
            json.withCString { jp in
                guard let ptr = ds_t_args(kp, jp) else { return key }
                defer { ds_string_free(ptr) }
                return String(cString: ptr)
            }
        }
    }
}
