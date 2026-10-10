import Foundation
import Testing
@testable import AIMemoryMenuCore

struct LaunchdControllerTests {
    @Test func substitutesPlaceholders() {
        let template = """
        <string>__AI_MEMORY_BIN__</string>
        <string>__HOME__/Library/Logs/ai-memory/stderr.log</string>
        """
        let rendered = LaunchdController.renderTemplate(
            template,
            binary: URL(fileURLWithPath: "/Applications/AI Memory.app/Contents/Resources/runtime/ai-memory"),
            home: URL(fileURLWithPath: "/Users/ada"),
            dataDir: nil
        )
        #expect(rendered.contains("/Applications/AI Memory.app/Contents/Resources/runtime/ai-memory"))
        #expect(rendered.contains("/Users/ada/Library/Logs/ai-memory/stderr.log"))
        #expect(!rendered.contains("__AI_MEMORY_BIN__"))
        #expect(!rendered.contains("EnvironmentVariables"))
    }

    @Test func injectsDataDirEnvironment() {
        let template = """
        <dict>
          <key>Label</key>
          <string>com.github.akitaonrails.ai-memory</string>
        </dict>
        """
        let rendered = LaunchdController.renderTemplate(
            template,
            binary: URL(fileURLWithPath: "/bin/ai-memory"),
            home: URL(fileURLWithPath: "/Users/ada"),
            dataDir: URL(fileURLWithPath: "/Users/ada/.ai-memory")
        )
        #expect(rendered.contains("<key>AI_MEMORY_DATA_DIR</key>"))
        #expect(rendered.contains("<string>/Users/ada/.ai-memory</string>"))
        #expect(rendered.contains("<key>EnvironmentVariables</key>"))
    }

    @Test func rendersCheckedInLaunchdTemplate() throws {
        let templateURL = repoRoot()
            .appending(path: "packaging/launchd/com.github.akitaonrails.ai-memory.plist")
        let template = try String(contentsOf: templateURL, encoding: .utf8)
        let binary = URL(fileURLWithPath: "/Applications/AI Memory.app/Contents/Resources/runtime/ai-memory")
        let home = URL(fileURLWithPath: "/Users/ada")
        let rendered = LaunchdController.renderTemplate(template, binary: binary, home: home, dataDir: nil)
        #expect(LaunchdController.programArgumentsBinary(inPlist: rendered) == binary.path)
        #expect(rendered.contains("/Users/ada/Library/Logs/ai-memory/stderr.log"))
        #expect(rendered.contains("serve"))
        #expect(rendered.contains("--enable-web"))
        #expect(!rendered.contains("__HOME__"))
    }

    @Test func xmlEscapesDataDir() {
        let template = "<dict></dict>"
        let rendered = LaunchdController.renderTemplate(
            template,
            binary: URL(fileURLWithPath: "/bin/ai-memory"),
            home: URL(fileURLWithPath: "/Users/ada"),
            dataDir: URL(fileURLWithPath: "/tmp/a&b<c>")
        )
        #expect(rendered.contains("/tmp/a&amp;b&lt;c&gt;"))
    }

    @Test(arguments: ["ordinary", "A&B", "A<B>", "A\"B", "A&amp;B"])
    func renderedPathsRoundTripThroughPropertyList(component: String) throws {
        let template = try String(
            contentsOf: repoRoot().appending(path: "packaging/launchd/com.github.akitaonrails.ai-memory.plist"),
            encoding: .utf8
        )
        let binary = URL(fileURLWithPath: "/tmp/\(component)/ai-memory")
        let home = URL(fileURLWithPath: "/tmp/\(component)/home")
        let dataDir = URL(fileURLWithPath: "/tmp/\(component)/data")
        let rendered = LaunchdController.renderTemplate(template, binary: binary, home: home, dataDir: dataDir)
        let plist = try #require(PropertyListSerialization.propertyList(
            from: Data(rendered.utf8), options: [], format: nil
        ) as? [String: Any])
        let arguments = try #require(plist["ProgramArguments"] as? [String])
        #expect(arguments.first == binary.path)
        #expect(LaunchdController.programArgumentsBinary(inPlist: rendered) == binary.path)
        #expect(plist["StandardOutPath"] as? String == home.path + "/Library/Logs/ai-memory/stdout.log")
        #expect(plist["StandardErrorPath"] as? String == home.path + "/Library/Logs/ai-memory/stderr.log")
        #expect((plist["EnvironmentVariables"] as? [String: String])?["AI_MEMORY_DATA_DIR"] == dataDir.path)
    }

    @Test(arguments: ["not a plist", "<plist><dict></dict></plist>", "<plist><dict><key>ProgramArguments</key><string>wrong type</string></dict></plist>"])
    func malformedArgumentsHaveNoBinary(plist: String) {
        #expect(LaunchdController.programArgumentsBinary(inPlist: plist) == nil)
    }
}

private func repoRoot(file: String = #filePath) -> URL {
    URL(fileURLWithPath: file)
        .deletingLastPathComponent() // Tests/AIMemoryMenuTests
        .deletingLastPathComponent() // Tests
        .deletingLastPathComponent() // companions/ai-memory-macos
        .deletingLastPathComponent() // companions
        .deletingLastPathComponent() // repo
}
