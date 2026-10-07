// Interface text: English source strings are the keys; French in Resources/fr.lproj
// (Localizable.strings), chosen by macOS from the system language. Placeholders are always
// %@ (arguments passed as text). Daemon messages and published channel names stay English.

import Foundation

/// Localized text.
func L(_ key: String) -> String {
    NSLocalizedString(key, comment: "")
}

/// Localized text with %@ placeholders, filled in order.
func L(_ key: String, _ args: String...) -> String {
    String(format: NSLocalizedString(key, comment: ""), arguments: args)
}
