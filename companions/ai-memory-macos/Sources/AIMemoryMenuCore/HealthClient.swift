import Foundation

public enum HealthError: Error, Equatable, Sendable {
    case badURL
    case http(Int)
    case decode
    case transport(String)
}

public struct HealthClient: Sendable {
    public var timeout: TimeInterval
    private let session: URLSession

    public init(timeout: TimeInterval = 3) {
        self.timeout = timeout
        let config = URLSessionConfiguration.ephemeral
        config.timeoutIntervalForRequest = timeout
        config.timeoutIntervalForResource = timeout
        config.requestCachePolicy = .reloadIgnoringLocalCacheData
        config.waitsForConnectivity = false
        session = URLSession(configuration: config, delegate: RefuseRedirects(), delegateQueue: nil)
    }

    public func fetch(baseURL: URL, token: String?) async throws -> StatusReport {
        let url = baseURL.appending(path: "admin/status")
        var request = URLRequest(url: url)
        request.timeoutInterval = timeout
        request.cachePolicy = .reloadIgnoringLocalCacheData
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        if let token, !token.isEmpty {
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }

        let data: Data
        let response: URLResponse
        do {
            (data, response) = try await session.data(for: request)
        } catch {
            throw HealthError.transport(error.localizedDescription)
        }

        guard let http = response as? HTTPURLResponse else {
            throw HealthError.transport("non-HTTP response")
        }
        guard (200 ..< 300).contains(http.statusCode) else {
            throw HealthError.http(http.statusCode)
        }
        do {
            return try JSONDecoder().decode(StatusReport.self, from: data)
        } catch {
            throw HealthError.decode
        }
    }
}

private final class RefuseRedirects: NSObject, URLSessionTaskDelegate {
    func urlSession(
        _ session: URLSession,
        task: URLSessionTask,
        willPerformHTTPRedirection response: HTTPURLResponse,
        newRequest request: URLRequest,
        completionHandler: @escaping @Sendable (URLRequest?) -> Void
    ) {
        completionHandler(nil)
    }
}

public enum HealthURL {
    public static func webUI(from baseURL: URL) -> URL {
        baseURL.appending(path: "web")
    }
}
