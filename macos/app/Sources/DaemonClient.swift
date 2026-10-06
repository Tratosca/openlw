// Client XPC du daemon lw-daemon (service Mach fr.francois-brille.openlw.daemon, domaine système).
// Protocole : dictionnaire XPC avec une clé "json" (requête et réponse JSON). Voir daemon/lw-daemon/src/control.rs.

import Foundation
import XPC

final class DaemonClient {
    static let serviceName = "fr.francois-brille.openlw.daemon"

    /// Service Mach visé ; `privileged` : LaunchDaemon (domaine système), sinon LaunchAgent (tests).
    let service: String
    let privileged: Bool

    init(service: String = DaemonClient.serviceName, privileged: Bool = true) {
        self.service = service
        self.privileged = privileged
    }

    private let queue = DispatchQueue(label: "fr.francois-brille.openlw.control.xpc")
    private var connection: xpc_connection_t?

    /// Requête JSON asynchrone ; `completion` est appelé sur le fil principal.
    func call(_ request: [String: Any], completion: @escaping (Result<[String: Any], DaemonError>) -> Void) {
        queue.async {
            let result = self.callSync(request)
            DispatchQueue.main.async { completion(result) }
        }
    }

    private func ensureConnection() -> xpc_connection_t {
        if let c = connection {
            return c
        }
        let flags = privileged ? UInt64(XPC_CONNECTION_MACH_SERVICE_PRIVILEGED) : 0
        let c = xpc_connection_create_mach_service(service, nil, flags)
        xpc_connection_set_event_handler(c) { [weak self] event in
            if xpc_get_type(event) == XPC_TYPE_ERROR {
                // Connexion perdue (daemon redémarré ou absent) : nouvelle connexion à la prochaine requête.
                self?.queue.async { self?.connection = nil }
            }
        }
        xpc_connection_resume(c)
        connection = c
        return c
    }

    private func callSync(_ request: [String: Any]) -> Result<[String: Any], DaemonError> {
        guard let data = try? JSONSerialization.data(withJSONObject: request),
              let text = String(data: data, encoding: .utf8) else {
            return .failure(.encoding)
        }
        let message = xpc_dictionary_create(nil, nil, 0)
        xpc_dictionary_set_string(message, "json", text)
        let reply = xpc_connection_send_message_with_reply_sync(ensureConnection(), message)
        guard xpc_get_type(reply) == XPC_TYPE_DICTIONARY, let raw = xpc_dictionary_get_string(reply, "json") else {
            connection = nil
            return .failure(.unreachable)
        }
        guard let obj = try? JSONSerialization.jsonObject(with: Data(String(cString: raw).utf8)),
              let dict = obj as? [String: Any] else {
            return .failure(.decoding)
        }
        if dict["ok"] as? Bool == true {
            return .success(dict)
        }
        return .failure(.refused(dict["error"] as? String ?? "erreur inconnue"))
    }
}

enum DaemonError: Error {
    case unreachable
    case encoding
    case decoding
    case refused(String)

    /// Message affiché : ce qui s'est passé, puis ce que l'utilisateur peut faire.
    var message: String {
        switch self {
        case .unreachable:
            return "Le service OpenLW ne répond pas. Si le problème persiste, réinstallez OpenLW."
        case .encoding, .decoding:
            return "Réponse illisible du service OpenLW. Réinstallez OpenLW pour mettre l'app et le service à la même version."
        case .refused(let reason):
            return "Modification impossible : \(reason)"
        }
    }
}
