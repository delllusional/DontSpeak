/* Empty translation unit so SwiftPM treats CDontSpeak as a buildable C target
 * whose public header (include/dontspeak.h) is importable from Swift. The actual
 * symbols are provided by the linked Rust staticlib libds_core.a. */
