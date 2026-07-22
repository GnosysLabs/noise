import Foundation

struct ProfileImage: Codable, Hashable, Sendable {
    let blobId: String
    let keyBase64: String
    let mimeType: String
    let byteLength: UInt32
}

struct IdentitySummary: Codable, Sendable {
    let username: String
    let publicKey: String
    let bio: String
    let avatar: ProfileImage?
}

struct GroupSummary: Codable, Identifiable, Hashable, Sendable {
    let groupId: String
    let name: String
    let description: String
    let avatar: ProfileImage?
    let ownerPublicKey: String
    let isActive: Bool

    var id: String { groupId }
}

struct LocalSummary: Codable, Sendable {
    let identity: IdentitySummary
    let groups: [GroupSummary]
}

struct MakeResult: Codable, Sendable {
    let group: GroupSummary
    let frequency: String
    let displayFrequency: String
}

struct JoinResult: Codable, Sendable {
    let group: GroupSummary
}

struct MemberSummary: Codable, Identifiable, Sendable {
    let publicKey: String
    let username: String
    let bio: String
    let avatar: ProfileImage?

    var id: String { publicKey }
}

struct MessageSummary: Codable, Identifiable, Sendable {
    let eventId: String
    let authorPublicKey: String
    let username: String
    let bio: String
    let avatar: ProfileImage?
    let text: String
    let createdAtMillis: UInt64

    var id: String { eventId }
    var createdAt: Date { Date(timeIntervalSince1970: Double(createdAtMillis) / 1_000) }
}

struct Conversation: Codable, Sendable {
    let group: GroupSummary
    let members: [MemberSummary]
    let messages: [MessageSummary]
    let rejectedEvents: Int
}

struct EmptyPayload: Codable, Sendable {}

struct AvatarData: Codable, Sendable {
    let mimeType: String
    let dataBase64: String
}

struct NoiseRequest: Encodable, Sendable {
    let action: String
    let statePath: String
    var username: String?
    var groupId: String?
    var name: String?
    var frequency: String?
    var text: String?
    var relays: [String]?
    var bio: String?
    var description: String?
    var avatarDataBase64: String?
    var avatarMimeType: String?
    var removeAvatar: Bool?
    var image: ProfileImage?

    init(
        action: String,
        statePath: String,
        username: String? = nil,
        groupId: String? = nil,
        name: String? = nil,
        frequency: String? = nil,
        text: String? = nil,
        relays: [String]? = nil,
        bio: String? = nil,
        description: String? = nil,
        avatarDataBase64: String? = nil,
        avatarMimeType: String? = nil,
        removeAvatar: Bool? = nil,
        image: ProfileImage? = nil
    ) {
        self.action = action
        self.statePath = statePath
        self.username = username
        self.groupId = groupId
        self.name = name
        self.frequency = frequency
        self.text = text
        self.relays = relays
        self.bio = bio
        self.description = description
        self.avatarDataBase64 = avatarDataBase64
        self.avatarMimeType = avatarMimeType
        self.removeAvatar = removeAvatar
        self.image = image
    }
}
