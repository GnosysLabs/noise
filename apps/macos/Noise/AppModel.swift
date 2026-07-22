import AppKit
import Foundation

enum NoiseSheet: Identifiable {
    case make
    case join
    case profile
    case group(GroupSummary)
    case frequency(String, String)

    var id: String {
        switch self {
        case .make: "make"
        case .join: "join"
        case .profile: "profile"
        case .group(let group): "group-\(group.groupId)"
        case .frequency(let group, _): "frequency-\(group)"
        }
    }
}

@MainActor
final class AppModel: ObservableObject {
    @Published var summary: LocalSummary?
    @Published var conversation: Conversation?
    @Published var presentedSheet: NoiseSheet?
    @Published var isBootstrapping = true
    @Published var isWorking = false
    @Published var errorMessage: String?
    @Published private(set) var avatarImages: [String: NSImage] = [:]
    private var loadingAvatars = Set<String>()

    let relays = [
        "http://127.0.0.1:4301",
        "http://127.0.0.1:4302",
        "http://127.0.0.1:4303",
    ]

    private let statePath: String

    init() {
        let directory = FileManager.default.urls(
            for: .applicationSupportDirectory,
            in: .userDomainMask
        )[0].appending(path: "noise", directoryHint: .isDirectory)
        statePath = directory.appending(path: "profile.json").path
        Task { await bootstrap() }
    }

    func bootstrap() async {
        isBootstrapping = true
        defer { isBootstrapping = false }
        do {
            summary = try await invoke(
                NoiseRequest(action: "status", statePath: statePath),
                as: LocalSummary.self
            )
            if summary != nil { await refreshConversation() }
        } catch {
            show(error)
        }
    }

    func initialize(username: String) async -> Bool {
        await work {
            self.summary = try await self.required(
                NoiseRequest(action: "initialize", statePath: self.statePath, username: username),
                as: LocalSummary.self
            )
        }
    }

    func updateProfile(bio: String, avatarData: Data?, removeAvatar: Bool) async -> Bool {
        await work {
            self.summary = try await self.required(
                NoiseRequest(
                    action: "update_profile",
                    statePath: self.statePath,
                    relays: self.relays,
                    bio: bio,
                    avatarDataBase64: avatarData?.base64EncodedString(),
                    avatarMimeType: avatarData == nil ? nil : "image/jpeg",
                    removeAvatar: removeAvatar
                ),
                as: LocalSummary.self
            )
            if removeAvatar {
                self.avatarImages.removeAll()
            }
            await self.refreshConversation()
        }
    }

    func updateGroup(
        name: String,
        description: String,
        avatarData: Data?,
        removeAvatar: Bool
    ) async -> Bool {
        await work {
            self.summary = try await self.required(
                NoiseRequest(
                    action: "update_group_profile",
                    statePath: self.statePath,
                    name: name,
                    relays: self.relays,
                    description: description,
                    avatarDataBase64: avatarData?.base64EncodedString(),
                    avatarMimeType: avatarData == nil ? nil : "image/jpeg",
                    removeAvatar: removeAvatar
                ),
                as: LocalSummary.self
            )
            await self.refreshConversation()
        }
    }

    func loadAvatar(_ image: ProfileImage?) async {
        guard let image,
              avatarImages[image.blobId] == nil,
              !loadingAvatars.contains(image.blobId)
        else { return }

        loadingAvatars.insert(image.blobId)
        defer { loadingAvatars.remove(image.blobId) }
        do {
            let result = try await required(
                NoiseRequest(
                    action: "fetch_avatar",
                    statePath: statePath,
                    relays: relays,
                    image: image
                ),
                as: AvatarData.self
            )
            guard let data = Data(base64Encoded: result.dataBase64),
                  let avatar = NSImage(data: data)
            else {
                throw NoiseBridgeError.core("avatar image could not be decoded")
            }
            avatarImages[image.blobId] = avatar
        } catch {
            // An unavailable avatar should never block the conversation itself.
        }
    }

    func makeGroup(name: String) async -> Bool {
        var result: MakeResult?
        let succeeded = await work {
            result = try await self.required(
                NoiseRequest(
                    action: "make",
                    statePath: self.statePath,
                    name: name,
                    relays: self.relays
                ),
                as: MakeResult.self
            )
            self.summary = try await self.required(
                NoiseRequest(action: "status", statePath: self.statePath),
                as: LocalSummary.self
            )
            await self.refreshConversation()
        }
        if succeeded, let result {
            presentedSheet = .frequency(result.group.name, result.displayFrequency)
        }
        return succeeded
    }

    func join(frequency: String) async -> Bool {
        await work {
            _ = try await self.required(
                NoiseRequest(
                    action: "join",
                    statePath: self.statePath,
                    frequency: frequency,
                    relays: self.relays
                ),
                as: JoinResult.self
            )
            self.summary = try await self.required(
                NoiseRequest(action: "status", statePath: self.statePath),
                as: LocalSummary.self
            )
            await self.refreshConversation()
        }
    }

    func select(_ group: GroupSummary) async {
        guard !group.isActive else { return }
        _ = await work {
            self.summary = try await self.required(
                NoiseRequest(
                    action: "select_group",
                    statePath: self.statePath,
                    groupId: group.groupId
                ),
                as: LocalSummary.self
            )
            await self.refreshConversation()
        }
    }

    func send(_ text: String) async -> Bool {
        await work {
            _ = try await self.invoke(
                NoiseRequest(
                    action: "say",
                    statePath: self.statePath,
                    text: text,
                    relays: self.relays
                ),
                as: EmptyPayload.self
            )
            await self.refreshConversation()
        }
    }

    func refreshConversation() async {
        guard summary?.groups.contains(where: \.isActive) == true else {
            conversation = nil
            return
        }
        do {
            conversation = try await required(
                NoiseRequest(
                    action: "conversation",
                    statePath: statePath,
                    relays: relays
                ),
                as: Conversation.self
            )
            summary = try await required(
                NoiseRequest(action: "status", statePath: statePath),
                as: LocalSummary.self
            )
        } catch {
            show(error)
        }
    }

    private func work(_ operation: @escaping @MainActor () async throws -> Void) async -> Bool {
        guard !isWorking else { return false }
        isWorking = true
        errorMessage = nil
        defer { isWorking = false }
        do {
            try await operation()
            return true
        } catch {
            show(error)
            return false
        }
    }

    private func required<Value: Decodable & Sendable>(
        _ request: NoiseRequest,
        as type: Value.Type
    ) async throws -> Value {
        guard let value = try await invoke(request, as: type) else {
            throw NoiseBridgeError.missingData
        }
        return value
    }

    private func invoke<Value: Decodable & Sendable>(
        _ request: NoiseRequest,
        as type: Value.Type
    ) async throws -> Value? {
        try await Task.detached(priority: .userInitiated) {
            try NoiseBridge.invoke(request, as: type)
        }.value
    }

    private func show(_ error: Error) {
        errorMessage = error.localizedDescription
    }
}
