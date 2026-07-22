import AppKit
import SwiftUI
import UniformTypeIdentifiers

private let noisePurple = Color(red: 0.52, green: 0.36, blue: 1.0)

struct ContentView: View {
    @EnvironmentObject private var model: AppModel

    var body: some View {
        ZStack {
            Color(nsColor: .windowBackgroundColor).ignoresSafeArea()

            if model.isBootstrapping {
                ProgressView()
                    .controlSize(.small)
            } else if model.summary == nil {
                OnboardingView()
            } else {
                NoiseWorkspace()
            }
        }
        .preferredColorScheme(.dark)
        .sheet(item: $model.presentedSheet) { sheet in
            switch sheet {
            case .make:
                MakeNoiseSheet()
            case .join:
                TuneInSheet()
            case .profile:
                if let identity = model.summary?.identity {
                    EditProfileSheet(profile: identity)
                }
            case .group(let group):
                GroupIdentitySheet(group: group)
            case .frequency(let group, let frequency):
                FrequencySheet(group: group, frequency: frequency)
            }
        }
        .alert("signal lost", isPresented: Binding(
            get: { model.errorMessage != nil },
            set: { if !$0 { model.errorMessage = nil } }
        )) {
            Button("okay", role: .cancel) { model.errorMessage = nil }
        } message: {
            Text(model.errorMessage ?? "unknown error")
        }
    }
}

private struct OnboardingView: View {
    @EnvironmentObject private var model: AppModel
    @State private var username = ""
    @FocusState private var focused: Bool

    var body: some View {
        VStack(spacing: 0) {
            Spacer()

            Image(systemName: "waveform")
                .font(.system(size: 38, weight: .light))
                .foregroundStyle(noisePurple)
                .padding(.bottom, 24)

            Text("noise")
                .font(.system(size: 44, weight: .semibold, design: .rounded))
                .tracking(-2)

            Text("no phone number. no email. just a name and a key.")
                .font(.system(size: 15))
                .foregroundStyle(.secondary)
                .padding(.top, 8)
                .padding(.bottom, 32)

            TextField("choose a username", text: $username)
                .textFieldStyle(.plain)
                .font(.system(size: 17, weight: .medium))
                .padding(.horizontal, 16)
                .frame(width: 300, height: 46)
                .background(.white.opacity(0.075), in: RoundedRectangle(cornerRadius: 12))
                .focused($focused)
                .onSubmit { createIdentity() }

            Button(action: createIdentity) {
                HStack(spacing: 8) {
                    if model.isWorking { ProgressView().controlSize(.small) }
                    Text("enter noise")
                }
                .frame(width: 300, height: 44)
            }
            .buttonStyle(.borderedProminent)
            .tint(noisePurple)
            .controlSize(.large)
            .disabled(cleanUsername.isEmpty || model.isWorking)
            .padding(.top, 12)

            Spacer()
            Text("your identity is generated on this device")
                .font(.caption)
                .foregroundStyle(.tertiary)
                .padding(.bottom, 24)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .onAppear { focused = true }
    }

    private var cleanUsername: String {
        username.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private func createIdentity() {
        guard !cleanUsername.isEmpty else { return }
        Task { _ = await model.initialize(username: cleanUsername) }
    }
}

private struct NoiseWorkspace: View {
    @EnvironmentObject private var model: AppModel

    var body: some View {
        NavigationSplitView {
            Sidebar()
                .navigationSplitViewColumnWidth(min: 230, ideal: 265, max: 330)
        } detail: {
            if let conversation = model.conversation {
                ConversationView(conversation: conversation)
            } else {
                EmptyFrequencyView()
            }
        }
        .navigationSplitViewStyle(.balanced)
        .toolbar(removing: .sidebarToggle)
    }
}

private struct Sidebar: View {
    @EnvironmentObject private var model: AppModel

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 10) {
                Image(systemName: "waveform")
                    .font(.system(size: 18, weight: .medium))
                    .foregroundStyle(noisePurple)
                Text("noise")
                    .font(.system(size: 21, weight: .semibold, design: .rounded))
                Spacer()
            }
            .padding(.horizontal, 18)
            .padding(.top, 10)
            .padding(.bottom, 14)

            HStack(spacing: 8) {
                Button {
                    model.presentedSheet = .make
                } label: {
                    Label("make noise", systemImage: "plus")
                        .frame(maxWidth: .infinity)
                }
                Button {
                    model.presentedSheet = .join
                } label: {
                    Image(systemName: "dial.medium")
                }
                .help("tune in")
            }
            .buttonStyle(.bordered)
            .controlSize(.large)
            .padding(.horizontal, 12)
            .padding(.bottom, 16)

            ScrollView {
                LazyVStack(spacing: 4) {
                    ForEach(model.summary?.groups ?? []) { group in
                        Button {
                            Task { await model.select(group) }
                        } label: {
                            HStack(spacing: 11) {
                                GroupAvatar(name: group.name, image: group.avatar, size: 26)
                                Text(group.name)
                                    .lineLimit(1)
                                Spacer()
                                if group.isActive {
                                    Circle()
                                        .fill(noisePurple)
                                        .frame(width: 6, height: 6)
                                }
                            }
                            .contentShape(Rectangle())
                            .padding(.horizontal, 12)
                            .frame(height: 42)
                            .background(
                                group.isActive ? .white.opacity(0.08) : .clear,
                                in: RoundedRectangle(cornerRadius: 8)
                            )
                        }
                        .buttonStyle(.plain)
                    }
                }
                .padding(.horizontal, 8)
            }

            Spacer(minLength: 8)

            Button {
                model.presentedSheet = .profile
            } label: {
                HStack(spacing: 10) {
                    ProfileAvatar(
                        username: model.summary?.identity.username ?? "?",
                        image: model.summary?.identity.avatar,
                        size: 30
                    )
                    VStack(alignment: .leading, spacing: 2) {
                        Text("@\(model.summary?.identity.username ?? "")")
                            .font(.system(size: 13, weight: .medium))
                            .lineLimit(1)
                        if let bio = model.summary?.identity.bio, !bio.isEmpty {
                            Text(bio)
                                .font(.caption2)
                                .foregroundStyle(.secondary)
                                .lineLimit(1)
                        }
                    }
                    Spacer()
                    Image(systemName: "slider.horizontal.3")
                        .font(.caption)
                        .foregroundStyle(.tertiary)
                }
                .contentShape(Rectangle())
                .padding(14)
            }
            .buttonStyle(.plain)
            .background(.black.opacity(0.16))
        }
        .background(.black.opacity(0.12))
    }
}

private struct EmptyFrequencyView: View {
    @EnvironmentObject private var model: AppModel

    var body: some View {
        VStack(spacing: 14) {
            Image(systemName: "dot.radiowaves.left.and.right")
                .font(.system(size: 34, weight: .light))
                .foregroundStyle(noisePurple)
            Text("nothing but noise")
                .font(.system(size: 24, weight: .semibold))
            Text("make a frequency or tune into one someone gave you")
                .foregroundStyle(.secondary)
            HStack {
                Button("make noise") { model.presentedSheet = .make }
                Button("tune in") { model.presentedSheet = .join }
            }
            .buttonStyle(.bordered)
            .controlSize(.large)
            .padding(.top, 6)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

private struct ConversationView: View {
    @EnvironmentObject private var model: AppModel
    let conversation: Conversation
    @State private var draft = ""
    @State private var showingMembers = false

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Button {
                    model.presentedSheet = .group(conversation.group)
                } label: {
                    HStack(spacing: 11) {
                        GroupAvatar(
                            name: conversation.group.name,
                            image: conversation.group.avatar,
                            size: 36
                        )
                        VStack(alignment: .leading, spacing: 0) {
                            Text(conversation.group.name)
                                .font(.system(size: 15, weight: .semibold))
                            Text(
                                conversation.group.description.isEmpty
                                    ? "view frequency profile"
                                    : conversation.group.description
                            )
                            .font(.system(size: 10.5))
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                        }
                    }
                }
                .buttonStyle(.plain)
                Spacer()
                Button(memberCount) { showingMembers.toggle() }
                    .buttonStyle(.plain)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .popover(isPresented: $showingMembers, arrowEdge: .bottom) {
                        MembersPopover(members: conversation.members)
                    }
                if model.isWorking {
                    ProgressView().controlSize(.small)
                }
                Button {
                    Task { await model.refreshConversation() }
                } label: {
                    Image(systemName: "arrow.clockwise")
                }
                .buttonStyle(.plain)
                .help("refresh")
            }
            .padding(.horizontal, 16)
            .frame(height: 48)
            .background(.ultraThinMaterial)

            Divider().opacity(0.4)

            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 17) {
                        if conversation.messages.isEmpty {
                            Text("the frequency is quiet")
                                .font(.system(size: 14))
                                .foregroundStyle(.tertiary)
                                .frame(maxWidth: .infinity)
                                .padding(.top, 70)
                        }
                        ForEach(conversation.messages) { message in
                            MessageRow(message: message)
                                .id(message.id)
                        }
                    }
                    .padding(.horizontal, 24)
                    .padding(.vertical, 22)
                }
                .onAppear { scrollToBottom(proxy) }
                .onChange(of: conversation.messages.count) { _, _ in scrollToBottom(proxy) }
            }

            HStack(alignment: .bottom, spacing: 10) {
                TextField("send noise", text: $draft, axis: .vertical)
                    .textFieldStyle(.plain)
                    .lineLimit(1...6)
                    .padding(.horizontal, 14)
                    .padding(.vertical, 11)
                    .background(.white.opacity(0.075), in: RoundedRectangle(cornerRadius: 12))
                    .onSubmit { submit() }

                Button(action: submit) {
                    Image(systemName: "arrow.up")
                        .font(.system(size: 14, weight: .bold))
                        .frame(width: 28, height: 28)
                }
                .buttonStyle(.borderedProminent)
                .buttonBorderShape(.circle)
                .tint(noisePurple)
                .disabled(cleanDraft.isEmpty || model.isWorking)
            }
            .padding(16)
        }
        .ignoresSafeArea(.container, edges: .top)
    }

    private var cleanDraft: String {
        draft.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private var memberCount: String {
        conversation.members.count == 1 ? "1 signal" : "\(conversation.members.count) signals"
    }

    private func submit() {
        let text = cleanDraft
        guard !text.isEmpty else { return }
        draft = ""
        Task {
            if !(await model.send(text)) { draft = text }
        }
    }

    private func scrollToBottom(_ proxy: ScrollViewProxy) {
        guard let last = conversation.messages.last else { return }
        proxy.scrollTo(last.id, anchor: .bottom)
    }
}

private struct MessageRow: View {
    let message: MessageSummary
    @State private var showingProfile = false

    var body: some View {
        HStack(alignment: .top, spacing: 11) {
            Button { showingProfile.toggle() } label: {
                ProfileAvatar(username: message.username, image: message.avatar, size: 32)
            }
            .buttonStyle(.plain)
            .popover(isPresented: $showingProfile, arrowEdge: .leading) {
                ProfileCard(
                    username: message.username,
                    bio: message.bio,
                    image: message.avatar
                )
            }
            VStack(alignment: .leading, spacing: 4) {
                HStack(spacing: 7) {
                    Button("@\(message.username)") { showingProfile.toggle() }
                        .buttonStyle(.plain)
                        .font(.system(size: 13, weight: .semibold))
                    Text(message.createdAt, style: .time)
                        .font(.caption2)
                        .foregroundStyle(.tertiary)
                }
                Text(message.text)
                    .font(.system(size: 14.5))
                    .textSelection(.enabled)
            }
            Spacer(minLength: 0)
        }
    }
}

private struct ProfileAvatar: View {
    @EnvironmentObject private var model: AppModel
    let username: String
    let image: ProfileImage?
    let size: CGFloat

    var body: some View {
        Group {
            if let image,
               let avatar = model.avatarImages[image.blobId] {
                Image(nsImage: avatar)
                    .resizable()
                    .scaledToFill()
            } else {
                ZStack {
                    noisePurple.opacity(0.19)
                    Text(String(username.prefix(1)).uppercased())
                        .font(.system(size: size * 0.38, weight: .bold))
                        .foregroundStyle(noisePurple)
                }
            }
        }
        .frame(width: size, height: size)
        .clipShape(Circle())
        .overlay(Circle().stroke(.white.opacity(0.08), lineWidth: 1))
        .task(id: image?.blobId) {
            await model.loadAvatar(image)
        }
    }
}

private struct GroupAvatar: View {
    @EnvironmentObject private var model: AppModel
    let name: String
    let image: ProfileImage?
    let size: CGFloat

    var body: some View {
        Group {
            if let image,
               let avatar = model.avatarImages[image.blobId] {
                Image(nsImage: avatar)
                    .resizable()
                    .scaledToFill()
            } else {
                ZStack {
                    noisePurple.opacity(0.19)
                    Text(String(name.prefix(1)).uppercased())
                        .font(.system(size: size * 0.38, weight: .bold))
                        .foregroundStyle(noisePurple)
                }
            }
        }
        .frame(width: size, height: size)
        .clipShape(RoundedRectangle(cornerRadius: size * 0.28, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: size * 0.28, style: .continuous)
                .stroke(.white.opacity(0.08), lineWidth: 1)
        )
        .task(id: image?.blobId) {
            await model.loadAvatar(image)
        }
    }
}

private struct ProfileCard: View {
    let username: String
    let bio: String
    let image: ProfileImage?

    var body: some View {
        VStack(spacing: 13) {
            ProfileAvatar(username: username, image: image, size: 66)
            VStack(spacing: 5) {
                Text("@\(username)")
                    .font(.system(size: 17, weight: .semibold))
                Text(bio.isEmpty ? "no bio yet" : bio)
                    .font(.system(size: 13))
                    .foregroundStyle(bio.isEmpty ? .tertiary : .secondary)
                    .multilineTextAlignment(.center)
                    .lineLimit(4)
            }
        }
        .padding(22)
        .frame(width: 250)
    }
}

private struct MembersPopover: View {
    let members: [MemberSummary]

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text("signals")
                .font(.system(size: 15, weight: .semibold))
                .padding(16)
            Divider()
            ScrollView {
                LazyVStack(spacing: 2) {
                    ForEach(members) { member in
                        HStack(spacing: 11) {
                            ProfileAvatar(
                                username: member.username,
                                image: member.avatar,
                                size: 36
                            )
                            VStack(alignment: .leading, spacing: 2) {
                                Text("@\(member.username)")
                                    .font(.system(size: 13, weight: .semibold))
                                if !member.bio.isEmpty {
                                    Text(member.bio)
                                        .font(.caption)
                                        .foregroundStyle(.secondary)
                                        .lineLimit(1)
                                }
                            }
                            Spacer()
                        }
                        .padding(.horizontal, 14)
                        .frame(height: 52)
                    }
                }
                .padding(.vertical, 6)
            }
        }
        .frame(width: 300, height: min(CGFloat(members.count * 52 + 56), 390))
    }
}

private struct GroupIdentitySheet: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.dismiss) private var dismiss
    let group: GroupSummary

    @State private var name: String
    @State private var description: String
    @State private var avatarData: Data?
    @State private var avatarPreview: NSImage?
    @State private var removeAvatar = false
    @State private var choosingAvatar = false
    @State private var imageError: String?

    init(group: GroupSummary) {
        self.group = group
        _name = State(initialValue: group.name)
        _description = State(initialValue: group.description)
    }

    var body: some View {
        VStack(spacing: 22) {
            VStack(spacing: 12) {
                Button { if canEdit { choosingAvatar = true } } label: {
                    ZStack(alignment: .bottomTrailing) {
                        if let avatarPreview {
                            Image(nsImage: avatarPreview)
                                .resizable()
                                .scaledToFill()
                                .frame(width: 96, height: 96)
                                .clipShape(RoundedRectangle(cornerRadius: 28, style: .continuous))
                        } else {
                            GroupAvatar(
                                name: group.name,
                                image: removeAvatar ? nil : group.avatar,
                                size: 96
                            )
                        }
                        if canEdit {
                            Image(systemName: "camera.fill")
                                .font(.system(size: 12, weight: .semibold))
                                .foregroundStyle(.white)
                                .frame(width: 28, height: 28)
                                .background(noisePurple, in: Circle())
                                .overlay(
                                    Circle().stroke(
                                        Color(nsColor: .windowBackgroundColor),
                                        lineWidth: 3
                                    )
                                )
                        }
                    }
                }
                .buttonStyle(.plain)

                Text(canEdit ? "frequency identity" : "frequency")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            VStack(alignment: .leading, spacing: 7) {
                Text("name")
                    .font(.system(size: 13, weight: .semibold))
                TextField("frequency name", text: $name)
                    .textFieldStyle(.roundedBorder)
                    .disabled(!canEdit)
            }

            VStack(alignment: .leading, spacing: 7) {
                HStack {
                    Text("description")
                        .font(.system(size: 13, weight: .semibold))
                    Spacer()
                    if canEdit {
                        Text("\(description.count)/200")
                            .font(.caption2.monospacedDigit())
                            .foregroundStyle(
                                description.count > 200
                                    ? Color.red
                                    : Color.secondary.opacity(0.55)
                            )
                    }
                }
                TextEditor(text: $description)
                    .font(.system(size: 14))
                    .scrollContentBackground(.hidden)
                    .padding(8)
                    .frame(height: 92)
                    .background(.white.opacity(0.065), in: RoundedRectangle(cornerRadius: 10))
                    .disabled(!canEdit)
            }

            if !canEdit {
                Label("managed by the frequency founder", systemImage: "signature")
                    .font(.caption)
                    .foregroundStyle(.tertiary)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }

            HStack {
                if canEdit, group.avatar != nil || avatarPreview != nil {
                    Button("remove icon", role: .destructive) {
                        avatarData = nil
                        avatarPreview = nil
                        removeAvatar = true
                    }
                }
                Spacer()
                Button(canEdit ? "cancel" : "done", role: .cancel) { dismiss() }
                if canEdit {
                    Button("save frequency") { save() }
                        .buttonStyle(.borderedProminent)
                        .tint(noisePurple)
                        .disabled(
                            cleanName.isEmpty
                                || name.count > 80
                                || description.count > 200
                                || model.isWorking
                        )
                }
            }
        }
        .padding(28)
        .frame(width: 440)
        .fileImporter(
            isPresented: $choosingAvatar,
            allowedContentTypes: [.image],
            allowsMultipleSelection: false
        ) { result in
            do {
                guard let url = try result.get().first else { return }
                let data = try prepareAvatarData(from: url)
                guard let preview = NSImage(data: data) else {
                    throw AvatarPreparationError.invalidImage
                }
                avatarData = data
                avatarPreview = preview
                removeAvatar = false
            } catch {
                imageError = error.localizedDescription
            }
        }
        .alert("couldn't use that image", isPresented: Binding(
            get: { imageError != nil },
            set: { if !$0 { imageError = nil } }
        )) {
            Button("okay", role: .cancel) { imageError = nil }
        } message: {
            Text(imageError ?? "unknown image error")
        }
    }

    private var canEdit: Bool {
        group.ownerPublicKey == model.summary?.identity.publicKey
    }

    private var cleanName: String {
        name.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private func save() {
        Task {
            if await model.updateGroup(
                name: cleanName,
                description: description,
                avatarData: avatarData,
                removeAvatar: removeAvatar
            ) {
                dismiss()
            }
        }
    }
}

private struct EditProfileSheet: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.dismiss) private var dismiss
    let profile: IdentitySummary

    @State private var bio: String
    @State private var avatarData: Data?
    @State private var avatarPreview: NSImage?
    @State private var removeAvatar = false
    @State private var choosingAvatar = false
    @State private var imageError: String?

    init(profile: IdentitySummary) {
        self.profile = profile
        _bio = State(initialValue: profile.bio)
    }

    var body: some View {
        VStack(spacing: 22) {
            VStack(spacing: 12) {
                Button { choosingAvatar = true } label: {
                    ZStack(alignment: .bottomTrailing) {
                        if let avatarPreview {
                            Image(nsImage: avatarPreview)
                                .resizable()
                                .scaledToFill()
                                .frame(width: 92, height: 92)
                                .clipShape(Circle())
                        } else {
                            ProfileAvatar(
                                username: profile.username,
                                image: removeAvatar ? nil : profile.avatar,
                                size: 92
                            )
                        }
                        Image(systemName: "camera.fill")
                            .font(.system(size: 12, weight: .semibold))
                            .foregroundStyle(.white)
                            .frame(width: 27, height: 27)
                            .background(noisePurple, in: Circle())
                            .overlay(Circle().stroke(Color(nsColor: .windowBackgroundColor), lineWidth: 3))
                    }
                }
                .buttonStyle(.plain)

                VStack(spacing: 3) {
                    Text("@\(profile.username)")
                        .font(.system(size: 20, weight: .semibold))
                    Text("your public identity")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }

            VStack(alignment: .leading, spacing: 7) {
                HStack {
                    Text("bio")
                        .font(.system(size: 13, weight: .semibold))
                    Spacer()
                    Text("\(bio.count)/160")
                        .font(.caption2.monospacedDigit())
                        .foregroundStyle(
                            bio.count > 160 ? Color.red : Color.secondary.opacity(0.55)
                        )
                }
                TextEditor(text: $bio)
                    .font(.system(size: 14))
                    .scrollContentBackground(.hidden)
                    .padding(8)
                    .frame(height: 88)
                    .background(.white.opacity(0.065), in: RoundedRectangle(cornerRadius: 10))
            }

            HStack {
                if profile.avatar != nil || avatarPreview != nil {
                    Button("remove photo", role: .destructive) {
                        avatarData = nil
                        avatarPreview = nil
                        removeAvatar = true
                    }
                }
                Spacer()
                Button("cancel", role: .cancel) { dismiss() }
                Button("save profile") { save() }
                    .buttonStyle(.borderedProminent)
                    .tint(noisePurple)
                    .disabled(bio.count > 160 || model.isWorking)
            }
        }
        .padding(28)
        .frame(width: 430)
        .fileImporter(
            isPresented: $choosingAvatar,
            allowedContentTypes: [.image],
            allowsMultipleSelection: false
        ) { result in
            do {
                guard let url = try result.get().first else { return }
                let data = try prepareAvatarData(from: url)
                guard let preview = NSImage(data: data) else {
                    throw AvatarPreparationError.invalidImage
                }
                avatarData = data
                avatarPreview = preview
                removeAvatar = false
            } catch {
                imageError = error.localizedDescription
            }
        }
        .alert("couldn't use that image", isPresented: Binding(
            get: { imageError != nil },
            set: { if !$0 { imageError = nil } }
        )) {
            Button("okay", role: .cancel) { imageError = nil }
        } message: {
            Text(imageError ?? "unknown image error")
        }
    }

    private func save() {
        Task {
            if await model.updateProfile(
                bio: bio,
                avatarData: avatarData,
                removeAvatar: removeAvatar
            ) {
                dismiss()
            }
        }
    }
}

private enum AvatarPreparationError: LocalizedError {
    case invalidImage
    case encodingFailed

    var errorDescription: String? {
        switch self {
        case .invalidImage: "that file is not a readable image"
        case .encodingFailed: "the image could not be prepared"
        }
    }
}

private func prepareAvatarData(from url: URL) throws -> Data {
    let accessed = url.startAccessingSecurityScopedResource()
    defer { if accessed { url.stopAccessingSecurityScopedResource() } }

    guard let source = NSImage(contentsOf: url), source.size.width > 0, source.size.height > 0 else {
        throw AvatarPreparationError.invalidImage
    }
    let pixels = 256
    guard let bitmap = NSBitmapImageRep(
        bitmapDataPlanes: nil,
        pixelsWide: pixels,
        pixelsHigh: pixels,
        bitsPerSample: 8,
        samplesPerPixel: 4,
        hasAlpha: true,
        isPlanar: false,
        colorSpaceName: .deviceRGB,
        bytesPerRow: 0,
        bitsPerPixel: 0
    ), let context = NSGraphicsContext(bitmapImageRep: bitmap) else {
        throw AvatarPreparationError.encodingFailed
    }

    let target = CGFloat(pixels)
    let scale = max(target / source.size.width, target / source.size.height)
    let drawSize = NSSize(width: source.size.width * scale, height: source.size.height * scale)
    let drawRect = NSRect(
        x: (target - drawSize.width) / 2,
        y: (target - drawSize.height) / 2,
        width: drawSize.width,
        height: drawSize.height
    )

    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = context
    context.imageInterpolation = .high
    NSColor.black.setFill()
    NSRect(x: 0, y: 0, width: target, height: target).fill()
    source.draw(in: drawRect, from: .zero, operation: .copy, fraction: 1)
    context.flushGraphics()
    NSGraphicsContext.restoreGraphicsState()

    guard let data = bitmap.representation(using: .jpeg, properties: [.compressionFactor: 0.78]) else {
        throw AvatarPreparationError.encodingFailed
    }
    return data
}

private struct MakeNoiseSheet: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.dismiss) private var dismiss
    @State private var name = ""
    @FocusState private var focused: Bool

    var body: some View {
        SheetFrame(icon: "waveform", title: "make noise", detail: "name the frequency") {
            TextField("group name", text: $name)
                .textFieldStyle(.roundedBorder)
                .focused($focused)
                .onSubmit { submit() }
            HStack {
                Button("cancel", role: .cancel) { dismiss() }
                Spacer()
                Button("make noise", action: submit)
                    .buttonStyle(.borderedProminent)
                    .tint(noisePurple)
                    .disabled(cleanName.isEmpty || model.isWorking)
            }
        }
        .onAppear { focused = true }
    }

    private var cleanName: String { name.trimmingCharacters(in: .whitespacesAndNewlines) }

    private func submit() {
        guard !cleanName.isEmpty else { return }
        Task { _ = await model.makeGroup(name: cleanName) }
    }
}

private struct TuneInSheet: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.dismiss) private var dismiss
    @State private var frequency = ""
    @FocusState private var focused: Bool

    var body: some View {
        SheetFrame(
            icon: "dial.medium",
            title: "tune in",
            detail: "enter a 12-digit frequency"
        ) {
            TextField("0000 0000 0000", text: $frequency)
                .textFieldStyle(.roundedBorder)
                .font(.system(size: 18, weight: .medium, design: .monospaced))
                .focused($focused)
                .onSubmit { submit() }
            HStack {
                Button("cancel", role: .cancel) { dismiss() }
                Spacer()
                Button("tune in", action: submit)
                    .buttonStyle(.borderedProminent)
                    .tint(noisePurple)
                    .disabled(digits.count != 12 || model.isWorking)
            }
        }
        .onAppear { focused = true }
    }

    private var digits: String { frequency.filter(\.isNumber) }

    private func submit() {
        guard digits.count == 12 else { return }
        Task {
            if await model.join(frequency: digits) { dismiss() }
        }
    }
}

private struct FrequencySheet: View {
    @Environment(\.dismiss) private var dismiss
    let group: String
    let frequency: String

    var body: some View {
        SheetFrame(
            icon: "dot.radiowaves.left.and.right",
            title: "you're live",
            detail: "share this frequency to invite people to \(group)"
        ) {
            Text(frequency)
                .font(.system(size: 27, weight: .semibold, design: .monospaced))
                .textSelection(.enabled)
                .frame(maxWidth: .infinity)
                .padding(.vertical, 16)
                .background(.white.opacity(0.06), in: RoundedRectangle(cornerRadius: 12))
            HStack {
                Button("copy frequency") {
                    NSPasteboard.general.clearContents()
                    NSPasteboard.general.setString(frequency, forType: .string)
                }
                Spacer()
                Button("done") { dismiss() }
                    .buttonStyle(.borderedProminent)
                    .tint(noisePurple)
            }
        }
    }
}

private struct SheetFrame<Content: View>: View {
    let icon: String
    let title: String
    let detail: String
    @ViewBuilder let content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Image(systemName: icon)
                .font(.system(size: 26, weight: .light))
                .foregroundStyle(noisePurple)
            VStack(alignment: .leading, spacing: 5) {
                Text(title)
                    .font(.system(size: 24, weight: .semibold))
                Text(detail)
                    .foregroundStyle(.secondary)
            }
            content
        }
        .padding(28)
        .frame(width: 410)
    }
}
