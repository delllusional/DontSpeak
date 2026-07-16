import XCTest

@testable import DontSpeakLogic

final class MicPermissionTests: XCTestCase {
    func testBuiltInUsesMicrophone() {
        XCTAssertTrue(dontSpeakUsesMicrophone(sttEngine: "built_in"))
    }

    func testSystemUsesMicrophone() {
        XCTAssertTrue(dontSpeakUsesMicrophone(sttEngine: "system"))
    }

    func testOffHidesMicrophone() {
        XCTAssertFalse(dontSpeakUsesMicrophone(sttEngine: "off"))
    }

    /// Claude Code owns mic prompt + capture — our row would mislead.
    func testClaudeCodeHidesMicrophone() {
        XCTAssertFalse(dontSpeakUsesMicrophone(sttEngine: "claude_code"))
    }

    /// Unknown token → capturing default (matches Status Parakeet fallback).
    func testUnknownTokenDefaultsToShown() {
        XCTAssertTrue(dontSpeakUsesMicrophone(sttEngine: "some_future_engine"))
        XCTAssertTrue(dontSpeakUsesMicrophone(sttEngine: ""))
    }

    /// Exact match only (config is lowercase snake_case).
    func testTokenMatchIsExact() {
        XCTAssertTrue(dontSpeakUsesMicrophone(sttEngine: "OFF"))
        XCTAssertTrue(dontSpeakUsesMicrophone(sttEngine: "Claude_Code"))
    }
}
