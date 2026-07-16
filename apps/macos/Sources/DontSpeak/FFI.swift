// Shared owned-C-string helpers for the `ds_*` ABI (copy → free in one place).

import CDontSpeak
import Foundation

/// Call that returns an owned `char*`: copy to Swift String, `ds_string_free`. Nil on NULL.
func ffiString(_ call: () -> UnsafeMutablePointer<CChar>?) -> String? {
    guard let ptr = call() else { return nil }
    defer { ds_string_free(ptr) }
    return String(cString: ptr)
}

/// Decode `ds_*` JSON into `T`. Nil on NULL / bad UTF-8 / decode mismatch — empty UI, no crash.
func ffiDecode<T: Decodable>(_ type: T.Type = T.self, _ call: () -> UnsafeMutablePointer<CChar>?) -> T? {
    guard let json = ffiString(call) else { return nil }
    return try? JSONDecoder().decode(T.self, from: Data(json.utf8))
}
