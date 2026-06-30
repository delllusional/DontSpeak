import XCTest

@testable import DontSpeakLogic

final class MicPermissionTests: XCTestCase {
    /// `built_in` (local FastConformer): DontSpeak opens the mic → row shown.
    func testBuiltInUsesMicrophone() {
        XCTAssertTrue(dontSpeakUsesMicrophone(sttEngine: "built_in"))
    }

    /// `system` (OS recognizer): DontSpeak still drives the capture → row shown.
    func testSystemUsesMicrophone() {
        XCTAssertTrue(dontSpeakUsesMicrophone(sttEngine: "system"))
    }

    /// `off`: dictation disabled (gray dot), the mic is never opened → row HIDDEN.
    func testOffHidesMicrophone() {
        XCTAssertFalse(dontSpeakUsesMicrophone(sttEngine: "off"))
    }

    /// `claude_code`: Claude Code owns its own mic prompt + capture → row HIDDEN.
    func testClaudeCodeHidesMicrophone() {
        XCTAssertFalse(dontSpeakUsesMicrophone(sttEngine: "claude_code"))
    }

    /// An unknown/unrecognized token falls through to the capturing default (row shown),
    /// matching the Status view's `default:` → Parakeet fallback — a forward-compat engine
    /// must not silently drop the mic row.
    func testUnknownTokenDefaultsToShown() {
        XCTAssertTrue(dontSpeakUsesMicrophone(sttEngine: "some_future_engine"))
        XCTAssertTrue(dontSpeakUsesMicrophone(sttEngine: ""))
    }

    /// Tokens are matched exactly — the real config tokens are lowercase snake_case, so a
    /// differently-cased string is NOT one of the two hide cases and stays shown.
    func testTokenMatchIsExact() {
        XCTAssertTrue(dontSpeakUsesMicrophone(sttEngine: "OFF"))
        XCTAssertTrue(dontSpeakUsesMicrophone(sttEngine: "Claude_Code"))
    }
}
