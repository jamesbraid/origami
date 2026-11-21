import Foundation

/// The bundle identifier changed from fans.irix.origami to fans.irix.Origami.
enum PrefsMigration {
    static let oldDomain = "fans.irix.origami"
    static let doneKey = "preferencesMigratedFromOldBundleId"

    /// Copies each key the old domain has and `target` lacks, once. The old domain is left alone.
    /// Returns the keys copied.
    @discardableResult
    static func run(from old: String = oldDomain, into target: UserDefaults = .standard) -> [String] {
        if target.bool(forKey: doneKey) { return [] }
        var copied: [String] = []
        for (key, value) in UserDefaults.standard.persistentDomain(forName: old) ?? [:] where target.object(forKey: key) == nil {
            target.set(value, forKey: key)
            copied.append(key)
        }
        target.set(true, forKey: doneKey)
        return copied.sorted()
    }
}
