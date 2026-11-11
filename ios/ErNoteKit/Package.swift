// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "ErNoteKit",
    platforms: [.iOS(.v17)],
    products: [.library(name: "ErNoteKit", targets: ["ErNoteKit"])],
    targets: [
        .binaryTarget(name: "RustCoreFFI", path: "Artifacts/RustCore.xcframework"),
        .target(name: "ErNoteKit", dependencies: ["RustCoreFFI"]),
        .testTarget(name: "ErNoteKitTests", dependencies: ["ErNoteKit"])
    ]
)
