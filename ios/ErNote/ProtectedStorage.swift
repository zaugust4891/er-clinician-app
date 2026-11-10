import Foundation

enum StorageBootstrapError: Error {
    case invalidDirectory
}

enum ProtectedStorage {
    /// Prepares the parent directory before any database or sidecar is created.
    /// The caller must not open storage when this operation throws.
    static func databaseURL() throws -> URL {
        let manager = FileManager.default
        let support = try manager.url(
            for: .applicationSupportDirectory,
            in: .userDomainMask,
            appropriateFor: nil,
            create: true
        )
        var directory = support.appendingPathComponent(
            "ClinicalData", isDirectory: true
        )
        let protection = FileProtectionType.completeUntilFirstUserAuthentication
        try manager.createDirectory(
            at: directory,
            withIntermediateDirectories: true,
            attributes: [.protectionKey: protection]
        )

        let attributes = try directory.resourceValues(
            forKeys: [.isDirectoryKey, .isSymbolicLinkKey]
        )
        guard attributes.isDirectory == true,
              attributes.isSymbolicLink != true else {
            throw StorageBootstrapError.invalidDirectory
        }

        // Reapply attributes when opening a directory from an earlier launch.
        try manager.setAttributes(
            [.protectionKey: protection],
            ofItemAtPath: directory.path
        )
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        try directory.setResourceValues(values)

        // SQLite will create its WAL and SHM siblings in this same directory.
        // No App Group container or database file is opened here.
        return directory.appendingPathComponent("device.sqlite")
    }
}
