import Foundation
import Testing
@testable import AIMemoryMenuCore

struct HealthRedirectTests {
    @Test func refusesRedirectAndAcceptsDirectStatus() async throws {
        let fixture = try #require(Bundle.module.url(forResource: "status", withExtension: "json", subdirectory: "fixtures"))
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/env")
        process.arguments = ["python3", "-u", "-c", """
        import http.server, sys
        body = open(sys.argv[1], 'rb').read()
        class Handler(http.server.BaseHTTPRequestHandler):
            leaks = 0
            def log_message(self, *args): pass
            def do_GET(self):
                if self.path == '/redirect/admin/status':
                    self.send_response(302)
                    self.send_header('Location', '/receiver')
                    self.end_headers()
                else:
                    if self.path == '/receiver': Handler.leaks += 1
                    self.send_response(200)
                    self.send_header('Content-Type', 'application/json')
                    self.end_headers()
                    self.wfile.write(str(Handler.leaks).encode() if self.path == '/count' else body)
        server = http.server.HTTPServer(('127.0.0.1', 0), Handler)
        print(server.server_port, flush=True)
        server.serve_forever()
        """, fixture.path]
        let output = Pipe()
        process.standardOutput = output
        try process.run()
        defer {
            process.terminate()
            process.waitUntilExit()
        }
        var port = Data()
        while let byte = try output.fileHandleForReading.read(upToCount: 1), !byte.isEmpty {
            if byte == Data([10]) { break }
            port.append(byte)
        }
        let portText = try #require(String(data: port, encoding: .utf8))
        let origin = try #require(URL(string: "http://127.0.0.1:\(portText)"))
        let client = HealthClient()
        do {
            _ = try await client.fetch(baseURL: origin.appending(path: "redirect"), token: "fixture-bearer")
            Issue.record("redirect must be an HTTP error, even if its destination returns valid status JSON")
        } catch let error as HealthError {
            #expect(error == .http(302))
        }
        let (count, _) = try await URLSession.shared.data(from: origin.appending(path: "count"))
        #expect(String(data: count, encoding: .utf8) == "0")
        let report = try await client.fetch(baseURL: origin.appending(path: "control"), token: "fixture-bearer")
        #expect(report.version == "2.3.2")
    }
}
