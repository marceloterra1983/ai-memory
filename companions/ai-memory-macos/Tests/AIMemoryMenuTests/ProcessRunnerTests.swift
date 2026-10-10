import Darwin
import Foundation
import Testing
@testable import AIMemoryMenuCore

struct ProcessRunnerTests {
    @Test(arguments: ["stdout", "stderr", "both"])
    func drainsOutputLargerThanEitherPipe(destination: String) throws {
        let emit = "/bin/dd if=/dev/zero bs=65536 count=16 2>/dev/null"
        let script = switch destination {
        case "stdout": emit
        case "stderr": "(\(emit)) >&2"
        default: "\(emit)\n(\(emit)) >&2"
        }
        let result = try ProcessRunner(timeout: 5).run(
            binary: URL(fileURLWithPath: "/bin/sh"),
            arguments: ["-c", script], extraEnv: [:]
        )
        #expect(result.exitCode == 0)
        #expect(result.stdout.utf8.count == (destination == "stderr" ? 0 : 1048576))
        #expect(result.stderr.utf8.count == (destination == "stdout" ? 0 : 1048576))
    }

    @Test func preservesSmallOutputAndExitCode() throws {
        let result = try ProcessRunner(timeout: 5).run(
            binary: URL(fileURLWithPath: "/bin/sh"),
            arguments: ["-c", "printf output; printf error >&2; exit 7"], extraEnv: [:]
        )
        #expect(result == ProcessResult(exitCode: 7, stdout: "output", stderr: "error"))
    }

    @Test(arguments: [false, true])
    func timeoutReapsItsChildEvenWhenTermIsIgnored(ignoreTerm: Bool) throws {
        let directory = FileManager.default.temporaryDirectory.appending(path: "runner-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let pidFile = directory.appending(path: "child.pid")
        let script = (ignoreTerm ? "trap '' TERM; " : "") + "echo $$ > \"$RUNNER_PID_FILE\"; while :; do :; done"
        let started = ProcessInfo.processInfo.systemUptime
        #expect(throws: CliError.timeout) {
            try ProcessRunner(timeout: 0.5).run(
                binary: URL(fileURLWithPath: "/bin/sh"), arguments: ["-c", script],
                extraEnv: ["RUNNER_PID_FILE": pidFile.path]
            )
        }
        #expect(ProcessInfo.processInfo.systemUptime - started < 3)
        let pid = try #require(Int32(String(contentsOf: pidFile, encoding: .utf8).trimmingCharacters(in: .whitespacesAndNewlines)))
        #expect(kill(pid, 0) == -1)
        #expect(errno == ESRCH)
    }
}
