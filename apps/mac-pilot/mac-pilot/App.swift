//
//  App.swift
//  mac-pilot
//
//  Menubar-only app using AppKit NSStatusBar directly (bypasses
//  SwiftUI MenuBarExtra which has rendering quirks on Sonoma+ notched Macs).
//  The SwiftUI Scene graph still declares a `WindowGroup(for: URL.self)` for
//  Skylight windows (per-galaxy whisper surface) so `openWindow(value:)`
//  from GalaxiesView spawns one standalone window per galaxy path.
//

import SwiftUI
import AppKit

@main
struct MacPilotApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) var appDelegate

    var body: some Scene {
        Settings { EmptyView() }

        WindowGroup("Skylight", for: URL.self) { $galaxyPath in
            if let path = galaxyPath {
                SkylightView(galaxyPath: path)
            } else {
                Text("skylight: galaxie introuvable")
                    .font(.footnote.monospaced())
                    .foregroundColor(.secondary)
                    .padding()
            }
        }
    }
}

final class AppDelegate: NSObject, NSApplicationDelegate {
    private var statusItem: NSStatusItem!
    private var popover: NSPopover!

    func applicationDidFinishLaunching(_ notification: Notification) {
        statusItem = NSStatusBar.system.statusItem(withLength: 90)
        if let button = statusItem.button {
            let img = NSImage(systemSymbolName: "safari", accessibilityDescription: "cosmon pilot")
            img?.isTemplate = true
            button.image = img
            button.title = " cosmon"
            button.imagePosition = .imageLeft
            button.action = #selector(togglePopover(_:))
            button.target = self
        }
        popover = NSPopover()
        popover.contentSize = NSSize(width: 400, height: 500)
        popover.behavior = .transient
        popover.contentViewController = NSHostingController(rootView: PilotView())
    }

    @objc func togglePopover(_ sender: AnyObject?) {
        guard let button = statusItem.button else { return }
        if popover.isShown {
            popover.performClose(sender)
        } else {
            popover.show(relativeTo: button.bounds, of: button, preferredEdge: .minY)
            popover.contentViewController?.view.window?.becomeKey()
        }
    }
}
