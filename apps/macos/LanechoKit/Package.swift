// swift-tools-version: 6.0
// LanechoKit: the core library of the native macOS client (the counterpart of
// deps/lanecho-core on the Rust side), plus the Lanecho executable shell that
// hosts the menu bar app.
//
// A plain SPM package: `swift build` / `swift test` are enough to develop and
// verify, and `swift run Lanecho` launches the menu bar app directly. Bundling
// it into a .app (Info.plist, code signing) is handled by build-app.sh.
import PackageDescription

let package = Package(
    name: "LanechoKit",
    platforms: [
        // macOS 14 (Sonoma) is the minimum supported release
        .macOS(.v14)
    ],
    products: [
        .library(name: "LanechoKit", targets: ["LanechoKit"]),
        .executable(name: "Lanecho", targets: ["Lanecho"]),
    ],
    dependencies: [
        // Apple's own libraries for parsing and generating TLS certificate
        // material; they fill the role rcgen plays on the Rust side
        .package(url: "https://github.com/apple/swift-crypto.git", from: "3.10.0"),
        .package(url: "https://github.com/apple/swift-certificates.git", from: "1.7.0"),
        // TLS for the sync transport. Certificate and private key DER bytes go
        // straight into memory with no keychain involved: Network.framework's
        // SecIdentity forces the deprecated keychain API and risks an
        // authorisation prompt in installed builds. Discovery still uses
        // Network.framework, which needs no TLS
        .package(url: "https://github.com/apple/swift-nio.git", from: "2.80.0"),
        .package(url: "https://github.com/apple/swift-nio-ssl.git", from: "2.29.0"),
    ],
    targets: [
        // The official BLAKE3 C reference implementation (1.5.5, vendored,
        // dual-licensed CC0 / Apache-2.0). Fingerprints must match the Rust
        // client byte for byte, so no third-party Swift port is used.
        // All x86 SIMD paths are off (x86_64 falls back to portable) and NEON
        // is gated per architecture through blake3_neon_arch.c, because SPM
        // cannot select source files per architecture.
        .target(
            name: "CBlake3",
            exclude: ["blake3_neon.c"],
            cSettings: [
                .define("BLAKE3_NO_SSE2"),
                .define("BLAKE3_NO_SSE41"),
                .define("BLAKE3_NO_AVX2"),
                .define("BLAKE3_NO_AVX512"),
            ]
        ),
        .target(
            name: "LanechoKit",
            dependencies: [
                "CBlake3",
                .product(name: "Crypto", package: "swift-crypto"),
                .product(name: "X509", package: "swift-certificates"),
                .product(name: "NIOCore", package: "swift-nio"),
                .product(name: "NIOPosix", package: "swift-nio"),
                .product(name: "NIOSSL", package: "swift-nio-ssl"),
            ]
        ),
        // The menu bar app shell (AppKit + SwiftUI). All logic lives in
        // LanechoKit; the shell only builds the interface.
        .executableTarget(
            name: "Lanecho",
            dependencies: ["LanechoKit"],
            resources: [
                // App icon and menu bar template icon, the same assets the
                // Tauri client ships
                .copy("Resources")
            ]
        ),
        .testTarget(
            name: "LanechoKitTests",
            dependencies: ["LanechoKit"],
            resources: [
                // Official BLAKE3 test vectors plus certificate and private
                // key fixtures generated with openssl
                .copy("Fixtures")
            ]
        ),
    ],
    swiftLanguageModes: [.v6]
)
