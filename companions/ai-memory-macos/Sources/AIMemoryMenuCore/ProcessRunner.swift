import Darwin
import Foundation

public struct ProcessResult: Equatable, Sendable {
    public var exitCode: Int32
    public var stdout: String
    public var stderr: String

    public init(exitCode: Int32, stdout: String, stderr: String) {
        self.exitCode = exitCode
        self.stdout = stdout
        self.stderr = stderr
    }

    public var combinedOutput: String {
        let out = stdout.trimmingCharacters(in: .whitespacesAndNewlines)
        let err = stderr.trimmingCharacters(in: .whitespacesAndNewlines)
        if err.isEmpty {
            return out
        }
        if out.isEmpty {
            return err
        }
        return out + "\n" + err
    }
}

public protocol CommandRunning: Sendable {
    func run(binary: URL, arguments: [String], extraEnv: [String: String]) throws -> ProcessResult
}

public struct ProcessRunner: CommandRunning {
    public var timeout: TimeInterval

    public init(timeout: TimeInterval = 30) {
        self.timeout = timeout
    }

    public func run(binary: URL, arguments: [String], extraEnv: [String: String]) throws -> ProcessResult {
        let process = Process()
        process.executableURL = binary
        process.arguments = arguments
        var env = ProcessInfo.processInfo.environment
        for (key, value) in extraEnv {
            env[key] = value
        }
        process.environment = env

        let stdout = Pipe()
        let stderr = Pipe()
        process.standardOutput = stdout
        process.standardError = stderr

        let outHandle = stdout.fileHandleForReading
        let errHandle = stderr.fileHandleForReading
        defer {
            try? outHandle.close()
            try? errHandle.close()
            try? stdout.fileHandleForWriting.close()
            try? stderr.fileHandleForWriting.close()
        }
        // Nonblocking reads let both pipes and the deadline make progress,
        // even if a descendant inherits a pipe after the direct child exits.
        for handle in [outHandle, errHandle] {
            let flags = fcntl(handle.fileDescriptor, F_GETFL)
            guard flags >= 0, fcntl(handle.fileDescriptor, F_SETFL, flags | O_NONBLOCK) >= 0 else {
                throw POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO)
            }
        }
        try process.run()
        defer {
            if process.isRunning {
                Self.terminateAndWait(process)
            }
        }
        try? stdout.fileHandleForWriting.close()
        try? stderr.fileHandleForWriting.close()

        var outData = Data()
        var errData = Data()
        let deadline = ProcessInfo.processInfo.systemUptime + timeout
        while true {
            let readOut = try Self.readAvailable(outHandle.fileDescriptor, into: &outData)
            let readErr = try Self.readAvailable(errHandle.fileDescriptor, into: &errData)
            if !process.isRunning, !readOut, !readErr {
                break
            }
            if ProcessInfo.processInfo.systemUptime >= deadline {
                Self.terminateAndWait(process)
                throw CliError.timeout
            }
            if !readOut, !readErr {
                Thread.sleep(forTimeInterval: 0.01)
            }
        }
        process.waitUntilExit()
        return ProcessResult(
            exitCode: process.terminationStatus,
            stdout: String(data: outData, encoding: .utf8) ?? "",
            stderr: String(data: errData, encoding: .utf8) ?? ""
        )
    }

    private static func readAvailable(_ descriptor: Int32, into data: inout Data) throws -> Bool {
        var buffer = [UInt8](repeating: 0, count: 65536)
        let count = Darwin.read(descriptor, &buffer, 65536)
        if count > 0 {
            data.append(contentsOf: buffer.prefix(count))
            return true
        }
        if count < 0, errno != EAGAIN, errno != EINTR {
            throw POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO)
        }
        return false
    }

    private static func terminateAndWait(_ process: Process) {
        guard process.isRunning else { return }
        process.terminate()
        let deadline = ProcessInfo.processInfo.systemUptime + 0.2
        while process.isRunning, ProcessInfo.processInfo.systemUptime < deadline {
            Thread.sleep(forTimeInterval: 0.01)
        }
        if process.isRunning {
            // Only the Process instance's own unreaped child is eligible.
            kill(process.processIdentifier, SIGKILL)
        }
        process.waitUntilExit()
    }
}

public enum CliError: Error, Equatable, Sendable {
    case timeout
    case missingRuntime
    case failed(Int32, String)
}
