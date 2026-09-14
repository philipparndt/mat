// swift-tools-version:5.10
import PackageDescription

let package = Package(
    name: "mat-au",
    // Process taps (mat-capture) need macOS 14.2.
    platforms: [.macOS("14.2")],
    targets: [
        .executableTarget(name: "mat-au", path: "Sources/mat-au"),
        .executableTarget(name: "mat-capture", path: "Sources/mat-capture"),
    ]
)
