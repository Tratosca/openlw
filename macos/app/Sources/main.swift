// OpenLW entry point.
// Development options:
//   --snapshot <file.png>  render window to PNG after 3 s, then exit (headless verification)
//   --service <name> --user    daemon published by LaunchAgent rather than LaunchDaemon
//   --listen <channel>          preview this stereo channel once the interface is known

import AppKit

final class AppDelegate: NSObject, NSApplicationDelegate {
    private var main: MainWindowController?

    func applicationDidFinishLaunching(_ notification: Notification) {
        buildMenu()
        let args = CommandLine.arguments
        var service = DaemonClient.serviceName
        if let i = args.firstIndex(of: "--service"), i + 1 < args.count { service = args[i + 1] }
        let client = DaemonClient(service: service, privileged: !args.contains("--user"))
        let controller = MainWindowController(client: client)
        main = controller
        controller.showWindow(nil)
        controller.start()
        NSApp.activate(ignoringOtherApps: true)

        if let i = args.firstIndex(of: "--listen"), i + 1 < args.count, let ch = Int(args[i + 1]) {
            DispatchQueue.main.asyncAfter(deadline: .now() + 1) {
                controller.listen(to: DiscoveredSource(channel: ch, name: "", kind: "stereo", terminal: ""))
            }
        }
        if let i = args.firstIndex(of: "--snapshot"), i + 1 < args.count {
            let path = args[i + 1]
            DispatchQueue.main.asyncAfter(deadline: .now() + 3) {
                Self.snapshot(controller.window, to: path)
                NSApp.terminate(nil)
            }
        }
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }

    /// Uninstall script provided by installer (macos/installer/payload).
    static let uninstaller = "/Library/Application Support/OpenLW/uninstall.sh"

    @objc private func uninstall(_ sender: Any) {
        guard FileManager.default.isExecutableFile(atPath: Self.uninstaller) else {
            alert("Désinstalleur introuvable", "\(Self.uninstaller) est absent. OpenLW n'a pas été installé par son installeur.")
            return
        }
        let confirm = NSAlert()
        confirm.messageText = "Désinstaller OpenLW ?"
        confirm.informativeText = "Le périphérique « OpenLW », le service réseau et cette app sont retirés. Les réglages sont supprimés. Le son du Mac s'interrompt quelques secondes."
        confirm.addButton(withTitle: "Désinstaller")
        confirm.addButton(withTitle: "Annuler")
        guard confirm.runModal() == .alertFirstButtonReturn else { return }
        // Administrator privileges requested by macOS (standard authentication dialog).
        let script = "do shell script quoted form of \"\(Self.uninstaller)\" with administrator privileges"
        var error: NSDictionary?
        NSAppleScript(source: script)?.executeAndReturnError(&error)
        if let e = error {
            if (e[NSAppleScript.errorNumber] as? Int) == -128 { return } // Authentication canceled
            alert("Désinstallation incomplète", e[NSAppleScript.errorMessage] as? String ?? "Erreur inconnue.")
            return
        }
        alert("OpenLW est désinstallé", "Cette app va se fermer.")
        NSApp.terminate(nil)
    }

    private func alert(_ title: String, _ text: String) {
        let a = NSAlert()
        a.messageText = title
        a.informativeText = text
        a.runModal()
    }

    private static func snapshot(_ window: NSWindow?, to path: String) {
        guard let view = window?.contentView, let rep = view.bitmapImageRepForCachingDisplay(in: view.bounds) else { return }
        view.cacheDisplay(in: view.bounds, to: rep)
        try? rep.representation(using: .png, properties: [:])?.write(to: URL(fileURLWithPath: path))
    }

    private func buildMenu() {
        let bar = NSMenu()
        let appItem = NSMenuItem()
        bar.addItem(appItem)
        let appMenu = NSMenu()
        appMenu.addItem(withTitle: "À propos de OpenLW", action: #selector(NSApplication.orderFrontStandardAboutPanel(_:)), keyEquivalent: "")
        appMenu.addItem(.separator())
        appMenu.addItem(withTitle: "Désinstaller OpenLW…", action: #selector(uninstall(_:)), keyEquivalent: "")
        appMenu.addItem(.separator())
        appMenu.addItem(withTitle: "Masquer OpenLW", action: #selector(NSApplication.hide(_:)), keyEquivalent: "h")
        appMenu.addItem(withTitle: "Quitter OpenLW", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
        appItem.submenu = appMenu
        // Edit menu: copy/paste in text fields.
        let editItem = NSMenuItem()
        bar.addItem(editItem)
        let edit = NSMenu(title: "Édition")
        edit.addItem(withTitle: "Couper", action: #selector(NSText.cut(_:)), keyEquivalent: "x")
        edit.addItem(withTitle: "Copier", action: #selector(NSText.copy(_:)), keyEquivalent: "c")
        edit.addItem(withTitle: "Coller", action: #selector(NSText.paste(_:)), keyEquivalent: "v")
        edit.addItem(withTitle: "Tout sélectionner", action: #selector(NSText.selectAll(_:)), keyEquivalent: "a")
        editItem.submenu = edit
        NSApp.mainMenu = bar
    }
}

let app = NSApplication.shared
let delegate = AppDelegate()
app.delegate = delegate
app.setActivationPolicy(.regular)
app.run()
