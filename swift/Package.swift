// swift-tools-version:5.10
import PackageDescription

let package = Package(
    name: "mat-au",
    platforms: [.macOS(.v14)],
    targets: [
        .executableTarget(name: "mat-au", path: "Sources/mat-au"),
    ]
)
