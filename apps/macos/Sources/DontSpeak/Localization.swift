//  Localization.swift
//
//  Thin Swift bridge to the shared ds-i18n catalog via ds-core's C ABI. Every
//  user-facing string flows through `L.t(...)` so macOS and Windows render ONE catalog
//  instead of duplicating literals. English is the fallback; the active locale defaults
//  to the OS language (resolved lazily in Rust on first lookup).

import Foundation
import CDontSpeak

enum L {
    /// Localized string for `key` (English fallback; a missing key returns the key).
    static func t(_ key: String) -> String {
        key.withCString { kp in
            guard let ptr = ds_t(kp) else { return key }
            defer { ds_string_free(ptr) }
            return String(cString: ptr)
        }
    }

    /// Localized string for `key` with `%{name}` placeholders filled from `args`. Numbers
    /// should be formatted by the caller (locale-aware) and passed as strings.
    static func t(_ key: String, _ args: [String: String]) -> String {
        let json = (try? JSONSerialization.data(withJSONObject: args))
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
