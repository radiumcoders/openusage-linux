// swift-tools-version: 6.2
import PackageDescription

#if os(Linux)
// The Linux edition: the same accounts, sign-ins, providers, and refresh code as the Mac app, without
// the AppKit/SwiftUI shell. `openusage` signs in, refreshes, and feeds a Waybar module (docs/linux.md).
//
// `os` and `CryptoKit` are small Linux stand-ins for the two Apple modules the shared code imports
// (a lock, a logger that writes to stderr, and SHA-256), so the shared files compile unchanged. The
// Mac-only files below are excluded; `Platform/Linux` holds the Linux versions of the ones the core needs
// (the credential vault, the OAuth callback server, the proxy, and notifications).
let macOnlySources = [
    "App",
    // Views/DashboardFamilies.swift holds plain dashboard models the core shares, so it stays in.
    "Views/AccountDetailView.swift",
    "Views/AccountListRow.swift",
    "Views/AccountListView.swift",
    "Views/AccountsView.swift",
    "Views/AddAccountFlow.swift",
    "Views/AddAccountSheet.swift",
    "Views/AddHubStep.swift",
    "Views/CLIProxyHubRow.swift",
    "Views/CapsuleSegments.swift",
    "Views/ClosureMenuItem.swift",
    "Views/CopyFeedbackButton.swift",
    "Views/DashboardContentView.swift",
    "Views/DashboardGrid.swift",
    "Views/DashboardLiveRow.swift",
    "Views/DashboardMenus.swift",
    "Views/DashboardMetricRow.swift",
    "Views/DashboardProviderSection.swift",
    "Views/DashboardView.swift",
    "Views/EmptyDashboardView.swift",
    "Views/HoverHighlight.swift",
    "Views/HoverPopoverState.swift",
    "Views/HoverTooltip.swift",
    "Views/InlineRenameField.swift",
    "Views/LevelIndicator.swift",
    "Views/MotionAwareHoverPopover.swift",
    "Views/PanelHeightModifier.swift",
    "Views/PopoverFooter.swift",
    "Views/PopoverScrollView.swift",
    "Views/PopoverTopBar.swift",
    "Views/PriorityDashboardView.swift",
    "Views/RateLimitResetsDetail+Timeline.swift",
    "Views/RateLimitResetsDetail.swift",
    "Views/ReorderGeometry.swift",
    "Views/SettingsScreen.swift",
    "Views/ShareCardView.swift",
    "Views/ShortcutRecorderField.swift",
    "Views/TransientPill.swift",
    "Views/UpdateBannerCard.swift",
    "Views/UsageBreakdownDetail.swift",
    "Views/WidgetGroupedListView.swift",
    "Resources",
    "Services/AccountCredentialVault.swift",
    "Services/CommandLineToolInstaller.swift",
    "Services/LoopbackCallbackServer.swift",
    "Services/ProxyConfig.swift",
    "Services/ScreenCaptureProbe.swift",
    "Services/Telemetry.swift",
    "Stores/AppearanceSetting.swift",
    "Stores/HideFromScreenShareSetting.swift",
    "Stores/LaunchAtLoginSetting.swift",
    "Stores/PopoverState.swift",
    "Stores/TelemetryRecorder.swift",
    "Support/AboutPanel.swift",
    "Support/Animations.swift",
    "Support/AppNotifications.swift",
    "Support/AppShortcuts.swift",
    "Support/Haptics.swift",
    "Support/MenuBarStripRenderer.swift",
    "Support/PanelMetrics.swift",
    "Support/PopoverKeyReader.swift",
    "Support/ProviderMarks.swift",
    "Support/ResourceBundle.swift",
    "Support/ShareCardRenderer.swift",
    "Support/Theme.swift"
]

let package = Package(
    name: "OpenUsage",
    products: [
        .executable(name: "openusage", targets: ["OpenUsageLinux"])
    ],
    targets: [
        .target(name: "os", path: "Sources/LinuxShims/os", swiftSettings: [.swiftLanguageMode(.v6)]),
        .target(name: "CryptoKit", path: "Sources/LinuxShims/CryptoKit", swiftSettings: [.swiftLanguageMode(.v6)]),
        .target(
            name: "OpenUsage",
            dependencies: ["os", "CryptoKit"],
            path: "Sources/OpenUsage",
            exclude: macOnlySources,
            swiftSettings: [
                .swiftLanguageMode(.v6)
            ]
        ),
        .executableTarget(
            name: "OpenUsageLinux",
            dependencies: ["OpenUsage"],
            path: "Sources/OpenUsageLinux",
            swiftSettings: [
                .swiftLanguageMode(.v6)
            ]
        ),
        // The Mac suite's platform-neutral tests (mappers, parsers, logins), run on Linux. The excluded files
        // test the Mac app's windows, AppKit/SwiftUI types, or Mac-only services.
        .testTarget(
            name: "OpenUsageTests",
            dependencies: ["OpenUsage", "os"],
            path: "Tests/OpenUsageTests",
            exclude: [
                // Mac-only: these test the app's windows, AppKit/SwiftUI types, or Mac-only services (or use
                // fixtures from a file excluded here).
                "2.9.4",
                "3.0.1",
                "3.62.0",
                "AccountChangeTests.swift",
                "AccountMetricSettingsTests.swift",
                "AccountRemovalTests.swift",
                "AccountRenameStoreTests.swift",
                "AddAccountFlowTests.swift",
                "AddAccountSheetTests.swift",
                "AppNotificationsTests.swift",
                "CLIProxyHubOffTests.swift",
                "CLIProxyHubTests.swift",
                "CLIProxyHubTunnelTests.swift",
                "ClaudeResetGrantsTests.swift",
                "CommandLineToolInstallerTests.swift",
                "ContainerTelemetryTests.swift",
                "CopilotOrgBillingTests.swift",
                "CryptoKit",
                "DashboardLabelFitTests.swift",
                "DashboardLiveLayoutTests.swift",
                "DashboardMenusTests.swift",
                "DashboardSectionTests.swift",
                "DefaultTabTests.swift",
                "DuplicateLoginTests.swift",
                "EmptyDashboardTests.swift",
                "HTTPClientCacheTests.swift",
                "HideFromScreenShareSettingTests.swift",
                "HoverPopoverStateTests.swift",
                "InkMap.swift",
                "InlineRenameFocusTests.swift",
                "KeyboardShortcuts",
                "LaunchAtLoginSettingTests.swift",
                "LayoutStoreTests.swift",
                "MeasuredPanelHeightsTests.swift",
                "MenuBarContentTests.swift",
                "MenuBarPanelTests.swift",
                "MenuBarStripGateTests.swift",
                "MenuBarStripTrimTests.swift",
                "OAuthFoundationTests.swift",
                "OAuthRefreshPersistenceTests.swift",
                "OpenRouterUsageMapperTests.swift",
                "OpenUsage",
                "OpenUsageApp",
                "OpenUsageCLI",
                "OpenUsageCLITests",
                "OpenUsageLinuxTests",
                "OpenUsageTests",
                "PanelContentHeightTests.swift",
                "PanelGeometryTests.swift",
                "PanelHeightBridgeTests.swift",
                "PanelOutsideClickPolicyTests.swift",
                "PanelScrollEdgeTests.swift",
                "Platform/Linux",
                "PointerEvents.swift",
                "PopoverTopBarTests.swift",
                "PostHog",
                "PriorityDashboardTests.swift",
                "ProviderMarksTests.swift",
                "ProxyConfigTests.swift",
                "QuietTestProcess.swift",
                "ReorderGeometryTests.swift",
                "ResetClaimServiceTests.swift",
                "ResetDisplayTests.swift",
                "Resources/ProviderIcons",
                "ScreenTransitionTests.swift",
                "ShareCardRendererTests.swift",
                "SignInAgainTests.swift",
                "SignInAgainVisibilityTests.swift",
                "SingleInstanceLockTests.swift",
                "Sources/OpenUsage",
                "Sources/OpenUsageApp",
                "Sources/OpenUsageCLI",
                "Sparkle",
                "TelemetryRecorderTests.swift",
                "Tests/OpenUsageCLITests",
                "Tests/OpenUsageLinuxTests",
                "Tests/OpenUsageTests",
                "TransientNoticeTests.swift",
                "UpdaterControllerTests.swift",
                "UsageBreakdownDetailTests.swift",
                "https://github.com/PostHog/posthog-ios.git",
                "https://github.com/sindresorhus/KeyboardShortcuts",
                "https://github.com/sparkle-project/Sparkle",
                "openusage-cli",
                "posthog-ios",
                // Linux XCTest cannot run synchronous tests in a @MainActor XCTestCase (its generated test list
                // casts them to nonisolated methods and aborts) or a Sendable-checked test class, so these run
                // on the Mac only.
                "APIKeyV1FilesTests.swift",
                "AccountStoreTests.swift",
                "AntigravityProjectQuotaTests.swift",
                "ClaudeRequestCadenceTests.swift",
                "ClaudeResetClaimTests.swift",
                "CodexResetClaimTests.swift",
                "CodexUsageHistoryTests.swift",
                "CursorCurrentPeriodUsageTests.swift",
                "CursorMetricsTests.swift",
                "CursorTokenRefreshTests.swift",
                "CursorUsageHistoryTests.swift",
                "LayoutMigrationTests.swift",
                "LayoutPersistenceTests.swift",
                "LimitsEnvelopeTests.swift",
                "OllamaUsageHistoryTests.swift",
                "OpenCodeProviderTests.swift",
                "OpenRouterProviderTests.swift",
                "OpenRouterUsageHistoryTests.swift",
                "PriorityBoardTests.swift",
                "ProviderErrorBoundaryTests.swift",
                "ProviderOrderTests.swift",
                "RefreshBackoffTests.swift",
                "RefreshSettingTests.swift",
                "RefreshTimeoutTests.swift",
                "SecondaryReadCadenceTests.swift",
                "StalenessLabelTests.swift",
                "TokenRefreshMarginTests.swift",
                "UnauthorizedUsageReadTests.swift",
                "UsageReaderTests.swift",
                "WidgetDataStoreNotificationTests.swift",
                "WidgetDataStorePlanTests.swift",
                "WidgetDataStoreTests.swift",
                "WidgetMeterStyleTests.swift",
                "WidgetNoDataTests.swift",
                "WidgetPercentClampTests.swift"
            ],
            swiftSettings: [
                .swiftLanguageMode(.v6)
            ]
        ),
        .testTarget(
            name: "OpenUsageLinuxTests",
            dependencies: ["OpenUsage", "CryptoKit"],
            path: "Tests/OpenUsageLinuxTests",
            swiftSettings: [
                .swiftLanguageMode(.v6)
            ]
        )
    ]
)
#else
let package = Package(
    name: "OpenUsage",
    platforms: [
        .macOS(.v15)
    ],
    products: [
        .executable(name: "OpenUsage", targets: ["OpenUsageApp"]),
        .executable(name: "openusage-cli", targets: ["OpenUsageCLI"])
    ],
    dependencies: [
        // The de-facto standard recorder + global hotkey for Mac apps (System Settings-style field).
        .package(url: "https://github.com/sindresorhus/KeyboardShortcuts", from: "3.0.1"),
        // In-app auto-updates (appcast + EdDSA-signed downloads). 2.9.4 fixes the update window opening
        // behind other apps for menu-bar (dockless) apps (sparkle-project/Sparkle#2889).
        .package(url: "https://github.com/sparkle-project/Sparkle", from: "2.9.4"),
        // Anonymous usage analytics and mandatory crash reporting (official first-party Swift SDK).
        .package(url: "https://github.com/PostHog/posthog-ios.git", from: "3.62.0")
    ],
    targets: [
        .target(
            name: "OpenUsage",
            dependencies: [
                .product(name: "KeyboardShortcuts", package: "KeyboardShortcuts"),
                .product(name: "Sparkle", package: "Sparkle"),
                .product(name: "PostHog", package: "posthog-ios")
            ],
            path: "Sources/OpenUsage",
            exclude: [
                "Platform/Linux"
            ],
            resources: [
                .copy("Resources/ProviderIcons")
            ],
            swiftSettings: [
                .swiftLanguageMode(.v6)
            ]
        ),
        .executableTarget(
            name: "OpenUsageApp",
            dependencies: ["OpenUsage"],
            path: "Sources/OpenUsageApp",
            swiftSettings: [
                .swiftLanguageMode(.v6)
            ]
        ),
        .executableTarget(
            name: "OpenUsageCLI",
            dependencies: ["OpenUsage"],
            path: "Sources/OpenUsageCLI",
            swiftSettings: [
                .swiftLanguageMode(.v6)
            ]
        ),
        .testTarget(
            name: "OpenUsageTests",
            dependencies: ["OpenUsage"],
            path: "Tests/OpenUsageTests",
            swiftSettings: [
                .swiftLanguageMode(.v6)
            ]
        ),
        .testTarget(
            name: "OpenUsageCLITests",
            dependencies: ["OpenUsageCLI"],
            path: "Tests/OpenUsageCLITests",
            swiftSettings: [
                .swiftLanguageMode(.v6)
            ]
        )
    ]
)
#endif
