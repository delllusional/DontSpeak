public struct HostRelaunchPlan: Equatable, Sendable {
    public let executablePath: String
    public let arguments: [String]

    public init(bundlePath: String, processIdentifier: Int32) {
        executablePath = "/bin/sh"
        arguments = [
            "-c",
            """
            pid="$1"
            app="$2"
            while kill -0 "$pid" 2>/dev/null; do sleep 0.1; done
            exec /usr/bin/open -n "$app"
            """,
            "dontspeak-relaunch",
            String(processIdentifier),
            bundlePath,
        ]
    }
}
