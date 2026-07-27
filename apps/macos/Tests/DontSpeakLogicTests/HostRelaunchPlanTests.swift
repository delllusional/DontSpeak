import XCTest

@testable import DontSpeakLogic

final class HostRelaunchPlanTests: XCTestCase {
    func testRelaunchWaitsForTheOriginalPidAndPassesTheBundleAsAnArgument() {
        let bundlePath = "/Applications/Dont Speak's Preview.app"
        let plan = HostRelaunchPlan(bundlePath: bundlePath, processIdentifier: 42)

        XCTAssertEqual(plan.executablePath, "/bin/sh")
        XCTAssertEqual(plan.arguments[0], "-c")
        XCTAssertEqual(plan.arguments[2], "dontspeak-relaunch")
        XCTAssertEqual(plan.arguments[3], "42")
        XCTAssertEqual(plan.arguments[4], bundlePath)
        XCTAssertFalse(
            plan.arguments[1].contains(bundlePath),
            "the bundle path must not be interpolated into shell source"
        )
        XCTAssertTrue(plan.arguments[1].contains("while kill -0 \"$pid\""))
        XCTAssertTrue(plan.arguments[1].contains("exec /usr/bin/open -n \"$app\""))
    }
}
