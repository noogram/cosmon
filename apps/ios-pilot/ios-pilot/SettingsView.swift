import SwiftUI

struct SettingsView: View {
    @EnvironmentObject var settings: SettingsStore
    @EnvironmentObject var session: SessionStore

    @State private var testResult: String?
    @State private var testing: Bool = false

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("URL cs-api", text: $settings.apiURL)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled(true)
                        .keyboardType(.URL)
                    Button {
                        Task { await testConnection() }
                    } label: {
                        HStack {
                            if testing {
                                ProgressView().controlSize(.small)
                            } else {
                                Image(systemName: "antenna.radiowaves.left.and.right")
                            }
                            Text(testing ? "Test en cours…" : "Tester la connexion")
                        }
                    }
                    .disabled(testing)
                    if let r = testResult {
                        Text(r)
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                    }
                } header: {
                    Text("cs-api")
                } footer: {
                    Text("cs-api doit être lancé sur le Mac derrière Tailscale. Binder le daemon sur 0.0.0.0:4222 et s'assurer que la machine est dans le même tailnet.")
                }

                Section {
                    Toggle("Polling automatique", isOn: $settings.pollingEnabled)
                    if settings.pollingEnabled {
                        Picker("Intervalle", selection: $settings.pollingInterval) {
                            ForEach(SettingsStore.pollingIntervalChoices, id: \.self) { v in
                                Text("\(Int(v)) s").tag(v)
                            }
                        }
                    }
                } header: {
                    Text("Rafraîchissement")
                } footer: {
                    Text("Chaque onglet (Session, Whispers, Inbox, Galaxies) polle cs-api à cet intervalle. v0 utilise du polling HTTP ; v1 intégrera des notifications push.")
                }

                Section {
                    Toggle("Inbox — only temp:hot", isOn: $settings.onlyHot)
                } header: {
                    Text("Filtres")
                } footer: {
                    Text("Quand activé, l'onglet Inbox n'affiche que les molécules taggées `temp:hot`.")
                }

                Section {
                    Picker("Thème markdown", selection: $settings.markdownTheme) {
                        ForEach(MarkdownThemeID.allCases) { id in
                            Text(id.label).tag(id)
                        }
                    }
                    VStack(alignment: .leading, spacing: 8) {
                        Text("Aperçu").font(.caption).foregroundStyle(.secondary)
                        MarkdownView(
                            text: Self.themePreviewSample,
                            theme: settings.markdownTheme.theme
                        )
                        .padding(10)
                        .background(
                            RoundedRectangle(cornerRadius: 6)
                                .fill(Color.secondary.opacity(0.06))
                        )
                    }
                    .padding(.vertical, 4)
                } header: {
                    Text("Rendu Markdown")
                } footer: {
                    Text("Appliqué au topic des molécules, au corps des whispers et aux aperçus Inbox. Les lignes de liste conservent toujours la variante compacte pour rester lisibles.")
                }

                Section {
                    Picker("Niveau de log", selection: $settings.logLevel) {
                        ForEach(LogLevel.allCases) { level in
                            Text(level.label).tag(level)
                        }
                    }
                } header: {
                    Text("Debug")
                } footer: {
                    Text("Détaille les messages de diagnostic lors de la connexion cs-api (visible dans la console Xcode).")
                }

                Section {
                    LabeledContent("Bundle") {
                        Text("dev.noogram.cosmon.ios-pilot")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    LabeledContent("Version") {
                        Text(Bundle.main.shortVersion)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                } header: {
                    Text("À propos")
                }
            }
            .navigationTitle("Réglages")
        }
    }

    /// Small-but-complete sample exercising headings, emphasis, code,
    /// link, blockquote, list and fenced code block — so the picker
    /// preview shows every token the renderer supports.
    static let themePreviewSample = """
    ## Aperçu du rendu

    Paragraphe avec **gras**, *italique* et `code inline`.
    Lien : [cosmon](https://example.com)

    > Une baguette magique sur une page grise.

    - puce un
    - puce deux
    """

    private func testConnection() async {
        testing = true
        defer { testing = false }
        let probe = CosmonAPI()
        do {
            let h = try await probe.healthz()
            let binary = h.csBinary ?? "?"
            let version = h.version ?? "?"
            testResult = "OK — cs \(version) (\(binary))"
        } catch let CosmonAPIError.serverError(msg) {
            testResult = "Erreur serveur : \(msg)"
        } catch {
            testResult = "Échec : \(error.localizedDescription)"
        }
    }
}

private extension Bundle {
    var shortVersion: String {
        (infoDictionary?["CFBundleShortVersionString"] as? String) ?? "0.0"
    }
}

#Preview {
    SettingsView()
        .environmentObject(SessionStore(api: MockCosmonAPI()))
        .environmentObject(SettingsStore())
}
