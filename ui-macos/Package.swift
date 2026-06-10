// swift-tools-version:5.9
import PackageDescription

// Links the prebuilt Rust staticlib from ../target/<profile>. Build the core
// first (`cargo build -p scp-core`), then `swift run` from this directory.
//
// The native libs below come from `cargo rustc -- --print native-static-libs`.
// openssl@3 is Homebrew-provided; the rest ship with the macOS SDK.
let opensslLib = "/opt/homebrew/opt/openssl@3/lib"
let coreLib = "../target/debug"

let package = Package(
    name: "ScpCommander",
    platforms: [.macOS(.v13)],
    targets: [
        .target(name: "CScpCore"),
        .executableTarget(
            name: "ScpCommander",
            dependencies: ["CScpCore"],
            linkerSettings: [
                .unsafeFlags([
                    "-L", coreLib, "-lscp_core",
                    "-L", opensslLib, "-lssl", "-lcrypto",
                    "-lz", "-liconv",
                ]),
                .linkedFramework("CoreFoundation"),
                .linkedFramework("Security"),
            ]
        ),
    ]
)
