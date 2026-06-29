//  FFI.swift
//
//  Tiny shared helpers for the `ds_*` C ABI so the "call → copy the owned C string → free it"
//  dance lives in ONE place instead of being re-spelled at every catalog/colors read.

import Foundation
import CDontSpeak

/// Run a `ds_*` call that returns an OWNED `char*`, copy it into a Swift `String`, and free the
/// C allocation (`ds_string_free`). `nil` when the call returns NULL. Every FFI string read
/// (tools / libraries / logs / colors catalogs) funnels through here so none can leak or
/// double-free.
func ffiString(_ call: () -> UnsafeMutablePointer<CChar>?) -> String? {
    guard let ptr = call() else { return nil }
    defer { ds_string_free(ptr) }
    return String(cString: ptr)
}

/// Read a `ds_*` JSON string and decode it into `T`. `nil` on a NULL return, non-UTF-8 bytes,
/// or a decode mismatch — callers substitute an empty catalog, so a bad read degrades to an
/// empty screen rather than a crash.
func ffiDecode<T: Decodable>(_ type: T.Type = T.self, _ call: () -> UnsafeMutablePointer<CChar>?) -> T? {
    guard let json = ffiString(call) else { return nil }
    return try? JSONDecoder().decode(T.self, from: Data(json.utf8))
}
