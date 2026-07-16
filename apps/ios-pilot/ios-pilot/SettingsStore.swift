import Foundation

/// Log level surfaced in the UI — used to decide which debug noise to
/// surface during connection diagnostics. String-backed so `UserDefaults`
/// round-trips cleanly.
public enum LogLevel: String, CaseIterable, Identifiable {
    case silent = "silent"
    case errors = "errors"
    case info   = "info"
    case debug  = "debug"

    public var id: String { rawValue }
    public var label: String {
        switch self {
        case .silent: return "Silencieux"
        case .errors: return "Erreurs"
        case .info:   return "Infos"
        case .debug:  return "Debug"
        }
    }
}

/// Persists settings to UserDefaults. Published changes drive SettingsView
/// and are read by `CosmonAPI.baseURL` at call time.
@MainActor
public final class SettingsStore: ObservableObject {
    @Published public var apiURL: String {
        didSet {
            UserDefaults.standard.set(apiURL, forKey: CosmonAPI.defaultsKey)
        }
    }

    @Published public var pollingEnabled: Bool {
        didSet {
            UserDefaults.standard.set(pollingEnabled, forKey: Self.pollingKey)
        }
    }

    /// Seconds between poll ticks. v1 clamps this to one of the
    /// discrete values in `Self.pollingIntervalChoices` to match the
    /// ios-pilot v1 spec (5 / 10 / 30 / off).
    @Published public var pollingInterval: Double {
        didSet {
            UserDefaults.standard.set(pollingInterval, forKey: Self.pollingIntervalKey)
        }
    }

    /// When true, the Inbox tab only shows molecules with `temp:hot`.
    @Published public var onlyHot: Bool {
        didSet {
            UserDefaults.standard.set(onlyHot, forKey: Self.onlyHotKey)
        }
    }

    /// Debug log level for the diagnostics panel.
    @Published public var logLevel: LogLevel {
        didSet {
            UserDefaults.standard.set(logLevel.rawValue, forKey: Self.logLevelKey)
        }
    }

    /// Canonical markdown theme applied to `MarkdownView` (Inbox,
    /// Whispers, molecule detail). Defaults to `.relaxed`; the picker
    /// in SettingsView lets the operator flip to Obsidian-style palettes.
    /// List-row renderings keep the `compact` theme regardless of this
    /// setting — it only governs detail/body rendering so dense lists
    /// stay readable.
    @Published public var markdownTheme: MarkdownThemeID {
        didSet {
            UserDefaults.standard.set(markdownTheme.rawValue, forKey: Self.markdownThemeKey)
        }
    }

    public static let pollingKey = "polling_enabled"
    public static let pollingIntervalKey = "polling_interval_seconds"
    public static let onlyHotKey = "inbox_only_hot"
    public static let logLevelKey = "log_level"
    public static let markdownThemeKey = "markdown_theme"

    public static let defaultPollingInterval: Double = 10.0

    /// Discrete polling choices surfaced to the user — values in seconds.
    public static let pollingIntervalChoices: [Double] = [5, 10, 30]

    public init() {
        let defaults = UserDefaults.standard
        self.apiURL = defaults.string(forKey: CosmonAPI.defaultsKey) ?? CosmonAPI.fallbackURL
        if defaults.object(forKey: Self.pollingKey) == nil {
            self.pollingEnabled = true
        } else {
            self.pollingEnabled = defaults.bool(forKey: Self.pollingKey)
        }
        let raw = defaults.double(forKey: Self.pollingIntervalKey)
        self.pollingInterval = raw == 0 ? Self.defaultPollingInterval : raw
        self.onlyHot = defaults.bool(forKey: Self.onlyHotKey)
        let rawLevel = defaults.string(forKey: Self.logLevelKey) ?? LogLevel.errors.rawValue
        self.logLevel = LogLevel(rawValue: rawLevel) ?? .errors
        let rawTheme = defaults.string(forKey: Self.markdownThemeKey) ?? MarkdownThemeID.relaxed.rawValue
        self.markdownTheme = MarkdownThemeID(rawValue: rawTheme) ?? .relaxed
    }
}
