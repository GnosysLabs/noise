import Foundation

enum NoiseBridgeError: LocalizedError {
    case noResponse
    case core(String)
    case missingData

    var errorDescription: String? {
        switch self {
        case .noResponse:
            "the Noise core did not respond"
        case .core(let message):
            message
        case .missingData:
            "the Noise core returned no data"
        }
    }
}

private struct NoiseEnvelope<Value: Decodable & Sendable>: Decodable, Sendable {
    let ok: Bool
    let data: Value?
    let error: String?
}

enum NoiseBridge {
    static func invoke<Value: Decodable & Sendable>(
        _ request: NoiseRequest,
        as type: Value.Type
    ) throws -> Value? {
        let encoder = JSONEncoder()
        encoder.keyEncodingStrategy = .convertToSnakeCase
        let requestData = try encoder.encode(request)
        guard let requestString = String(data: requestData, encoding: .utf8) else {
            throw NoiseBridgeError.noResponse
        }

        let output = requestString.withCString { noise_invoke($0) }
        guard let output else { throw NoiseBridgeError.noResponse }
        defer { noise_free_string(output) }

        let responseData = Data(String(cString: output).utf8)
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let envelope = try decoder.decode(NoiseEnvelope<Value>.self, from: responseData)
        if !envelope.ok {
            throw NoiseBridgeError.core(envelope.error ?? "unknown Noise core error")
        }
        return envelope.data
    }
}
