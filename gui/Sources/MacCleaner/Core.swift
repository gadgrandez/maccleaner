import CMacCleaner
import Foundation

struct CoreError: LocalizedError {
    let message: String
    var errorDescription: String? { locCore(message) }
}

private struct Envelope<T: Decodable>: Decodable {
    let ok: Bool
    let data: T?
    let error: String?
}

/// Transport to the Rust core: the C API exchanges JSON (`snake_case`) inside an
/// `{ok, data, error}` envelope. Calls are blocking, so they run off the main actor.
/// The concrete operations are in `CoreAPI.swift`.
enum Core {
    typealias Response = UnsafeMutablePointer<CChar>?

    static func encode(_ value: some Encodable) throws -> String {
        let encoder = JSONEncoder()
        encoder.keyEncodingStrategy = .convertToSnakeCase
        return String(decoding: try encoder.encode(value), as: UTF8.self)
    }

    static func decode<T: Decodable>(_ response: Response) throws -> T {
        guard let response else { throw CoreError(message: loc("Empty response from the core")) }
        defer { mcl_string_free(response) }
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let envelope = try decoder.decode(Envelope<T>.self, from: Data(bytes: response, count: strlen(response)))
        if envelope.ok, let data = envelope.data { return data }
        throw CoreError(message: envelope.error ?? loc("Unknown error"))
    }

    /// Call without text arguments.
    static func call<T: Decodable & Sendable>(_ function: @escaping @Sendable () -> Response) async throws -> T {
        try await Task.detached(priority: .utility) { try decode(function()) }.value
    }

    /// Call with one text argument (a path, a label…).
    static func call<T: Decodable & Sendable>(
        argument: String, _ function: @escaping @Sendable (UnsafePointer<CChar>) -> Response
    ) async throws -> T {
        try await call { argument.withCString(function) }
    }

    /// Call with a JSON request.
    static func call<T: Decodable & Sendable>(
        request: some Encodable, _ function: @escaping @Sendable (UnsafePointer<CChar>) -> Response
    ) async throws -> T {
        try await call(argument: try encode(request), function)
    }
}
