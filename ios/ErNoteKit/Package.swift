// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "ErNoteKit",
    platforms: [.iOS(.v17), .watchOS(.v10), .macOS(.v13)],
    products: [.library(name: "ErNoteKit", targets: ["ErNoteKit"])],
    targets: [
        .target(name: "ErNoteKit"),
        .testTarget(name: "ErNoteKitTests", dependencies: ["ErNoteKit"])
    ]
)
