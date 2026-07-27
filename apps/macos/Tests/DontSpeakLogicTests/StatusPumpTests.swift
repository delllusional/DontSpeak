import Foundation
import XCTest

@testable import DontSpeakLogic

final class StatusPumpTests: XCTestCase {
    func testConstructionDoesNotWaitForTheInitialProbe() {
        let waitEntered = expectation(description: "background wait entered")
        let releaseWait = DispatchSemaphore(value: 0)
        let delivered = DispatchSemaphore(value: 0)
        let rescue = DispatchWorkItem { releaseWait.signal() }
        DispatchQueue.global().asyncAfter(deadline: .now() + 1, execute: rescue)

        let started = Date()
        let pump = StatusPump<Int>(
            name: "status-pump-test",
            wait: { _ in
                waitEntered.fulfill()
                releaseWait.wait()
                return StatusPoll(snapshot: 7, seq: 1, running: false)
            },
            deliver: { value in
                XCTAssertEqual(value, 7)
                delivered.signal()
            }
        )
        XCTAssertLessThan(
            Date().timeIntervalSince(started),
            0.5,
            "starting the pump must not run the initial status wait on the caller"
        )

        wait(for: [waitEntered], timeout: 1)
        XCTAssertEqual(
            delivered.wait(timeout: .now() + 0.05),
            .timedOut,
            "delivery must remain pending while the background probe is blocked"
        )
        releaseWait.signal()
        rescue.cancel()
        XCTAssertEqual(delivered.wait(timeout: .now() + 1), .success)
        pump.cancel()
    }
}
