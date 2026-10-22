import Foundation

// The UI source language is English. Translations live in `<lang>.lproj/Localizable.strings`
// inside the app bundle (see `gui/Localization/`), and the device language picks one.

/// Localized UI text. Interpolate only `String` values so every placeholder is `%@`.
func loc(_ key: String.LocalizationValue) -> String { String(localized: key) }

/// A count with its noun in singular or plural ("1 item" / "3 items"). English and Spanish only
/// distinguish one/other; `one` and `other` receive the formatted count.
func locCount(_ count: some BinaryInteger, one: (String) -> String, other: (String) -> String) -> String {
    let formatted = Int(count).formatted()
    return count == 1 ? one(formatted) : other(formatted)
}

func countItems(_ n: some BinaryInteger) -> String { locCount(n, one: { loc("\($0) item") }, other: { loc("\($0) items") }) }
func countFiles(_ n: some BinaryInteger) -> String { locCount(n, one: { loc("\($0) file") }, other: { loc("\($0) files") }) }
func countCopies(_ n: some BinaryInteger) -> String { locCount(n, one: { loc("\($0) copy") }, other: { loc("\($0) copies") }) }
func countFailures(_ n: some BinaryInteger) -> String { locCount(n, one: { loc("\($0) failure") }, other: { loc("\($0) failures") }) }

/// Text that comes from the Rust core (rule and task names, descriptions, errors…), translated
/// through the `Core` table. Keys with `{placeholders}` are templates for texts the core builds
/// with runtime values. Unknown text is shown as is.
func locCore(_ text: String) -> String {
    let exact = Bundle.main.localizedString(forKey: text, value: nil, table: "Core")
    if exact != text { return exact }
    return CoreTemplates.shared.translate(text) ?? text
}

/// Templates of the `Core` table, such as `"Quit {source} before cleaning"`.
private final class CoreTemplates {
    static let shared = CoreTemplates()

    private struct Template {
        let pattern: Regex<AnyRegexOutput>
        let names: [String]
        let translation: String
    }

    private let templates: [Template]

    private init() {
        let table = Bundle.main.url(forResource: "Core", withExtension: "strings")
            .flatMap { NSDictionary(contentsOf: $0) as? [String: String] } ?? [:]
        let placeholder = #/\{([a-z_]+)\}/#
        templates = table.compactMap { key, value in
            let names = key.matches(of: placeholder).map { String($0.output.1) }
            guard !names.isEmpty else { return nil }
            // Escape the literal parts and turn each placeholder into a capture group.
            var source = "^"
            var rest = Substring(key)
            for match in key.matches(of: placeholder) {
                source += NSRegularExpression.escapedPattern(for: String(rest[..<match.range.lowerBound])) + "(.+?)"
                rest = rest[match.range.upperBound...]
            }
            source += NSRegularExpression.escapedPattern(for: String(rest)) + "$"
            return (try? Regex(source)).map { Template(pattern: $0, names: names, translation: value) }
        }
    }

    func translate(_ text: String) -> String? {
        for template in templates {
            guard let match = text.wholeMatch(of: template.pattern) else { continue }
            var result = template.translation
            for (i, name) in template.names.enumerated() {
                if let value = match.output[i + 1].substring { result = result.replacingOccurrences(of: "{\(name)}", with: value) }
            }
            return result
        }
        return nil
    }
}
