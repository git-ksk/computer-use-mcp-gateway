import CryptoKit
import Foundation
import Security

private let schemaVersion = 1
private let maxInputBytes = 64 * 1024
private let maxSealedKeyBytes = 4096
private let maxMessageBytes = 16 * 1024

enum ExitCode: Int32 {
    case invalidInput = 20
    case keyUnavailable = 21
    case userPresenceDenied = 22
    case internalFailure = 23
}

struct HelperRequest: Decodable {
    let schema_version: Int
    let sealed_key_base64: String?
    let message_base64: String?
}

struct HelperResponse: Encodable {
    let schema_version: Int
    let sealed_key_base64: String?
    let public_key_base64: String?
    let signature_base64: String?
}

func fail(_ code: ExitCode) -> Never {
    FileHandle.standardError.write(Data("RECOVERY_HELPER_ERROR code=\(code.rawValue)\n".utf8))
    exit(code.rawValue)
}

func readRequest(allowedKeys: Set<String>) -> HelperRequest {
    let data = FileHandle.standardInput.readDataToEndOfFile()
    guard !data.isEmpty, data.count <= maxInputBytes else { fail(.invalidInput) }
    guard let raw = try? JSONSerialization.jsonObject(with: data),
          let dictionary = raw as? [String: Any],
          Set(dictionary.keys).isSubset(of: allowedKeys),
          let request = try? JSONDecoder().decode(HelperRequest.self, from: data),
          request.schema_version == schemaVersion else { fail(.invalidInput) }
    return request
}

func decodeBase64(_ value: String?, maxBytes: Int) -> Data {
    guard let value,
          value.utf8.count <= maxBytes * 2,
          let data = Data(base64Encoded: value),
          !data.isEmpty,
          data.count <= maxBytes else { fail(.invalidInput) }
    return data
}

func writeResponse(_ response: HelperResponse) {
    guard let data = try? JSONEncoder().encode(response), data.count <= maxInputBytes else {
        fail(.internalFailure)
    }
    FileHandle.standardOutput.write(data)
    FileHandle.standardOutput.write(Data([0x0a]))
}

func makeAccessControl() -> SecAccessControl {
    let flags: SecAccessControlCreateFlags = [.userPresence, .privateKeyUsage]
    var error: Unmanaged<CFError>?
    guard let access = SecAccessControlCreateWithFlags(
        nil,
        kSecAttrAccessibleWhenUnlockedThisDeviceOnly,
        flags,
        &error
    ) else {
        _ = error?.takeRetainedValue()
        fail(.keyUnavailable)
    }
    return access
}

func generate() {
    do {
        let key = try SecureEnclave.P256.Signing.PrivateKey(accessControl: makeAccessControl())
        let sealed = key.dataRepresentation
        let publicKey = key.publicKey.x963Representation
        guard !sealed.isEmpty,
              sealed.count <= maxSealedKeyBytes,
              publicKey.count == 65 else { fail(.keyUnavailable) }
        writeResponse(HelperResponse(
            schema_version: schemaVersion,
            sealed_key_base64: sealed.base64EncodedString(),
            public_key_base64: publicKey.base64EncodedString(),
            signature_base64: nil
        ))
    } catch {
        fail(.keyUnavailable)
    }
}

func restoreKey(_ request: HelperRequest) -> SecureEnclave.P256.Signing.PrivateKey {
    let sealed = decodeBase64(request.sealed_key_base64, maxBytes: maxSealedKeyBytes)
    do {
        return try SecureEnclave.P256.Signing.PrivateKey(dataRepresentation: sealed)
    } catch {
        fail(.keyUnavailable)
    }
}

func publicKey() {
    let request = readRequest(allowedKeys: ["schema_version", "sealed_key_base64"])
    guard request.message_base64 == nil else { fail(.invalidInput) }
    let key = restoreKey(request)
    let publicKey = key.publicKey.x963Representation
    guard publicKey.count == 65 else { fail(.keyUnavailable) }
    writeResponse(HelperResponse(
        schema_version: schemaVersion,
        sealed_key_base64: nil,
        public_key_base64: publicKey.base64EncodedString(),
        signature_base64: nil
    ))
}

func sign() {
    let request = readRequest(allowedKeys: ["schema_version", "sealed_key_base64", "message_base64"])
    let key = restoreKey(request)
    let message = decodeBase64(request.message_base64, maxBytes: maxMessageBytes)
    do {
        let signature = try key.signature(for: message).derRepresentation
        guard !signature.isEmpty, signature.count <= 256 else { fail(.internalFailure) }
        writeResponse(HelperResponse(
            schema_version: schemaVersion,
            sealed_key_base64: nil,
            public_key_base64: nil,
            signature_base64: signature.base64EncodedString()
        ))
    } catch {
        // Signing is the only operation that may require interactive user
        // presence. Keep failure output low-cardinality and payload-free.
        fail(.userPresenceDenied)
    }
}

let arguments = CommandLine.arguments
if arguments.count == 2 && arguments[1] == "--version" {
    print("cumg-v2-recovery-enclave-helper 1")
    exit(0)
}
guard arguments.count == 2 else { fail(.invalidInput) }
switch arguments[1] {
case "generate": generate()
case "public": publicKey()
case "sign": sign()
default: fail(.invalidInput)
}
