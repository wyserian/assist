set windows-shell := ["pwsh", "-NoLogo", "-Command"]

# the host build of the core, read by uniffi-bindgen for the Kotlin bindings
ffi_lib := if os() == "windows" { "assist_ffi.dll" } else { "libassist_ffi.so" }

# cargo build --workspace
build:
    cargo build --workspace

# nextest for unit + integration tests; doctests need plain cargo test
test:
    cargo nextest run --workspace
    cargo test --workspace --doc

# the daemon; pass e.g. `--config config/config.example.toml`
run *args:
    cargo run -p assist-cli -- serve {{args}}

# interactive assist shell; pass e.g. `--config config/config.example.toml`
shell *args:
    cargo run -p assist-cli -- shell {{args}}

# fmt check + clippy (warnings are errors) + dependency licenses/advisories
lint:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    cargo deny check

# Android debug APK: core for phones (arm64) and the emulator (x86_64), Kotlin bindings, Gradle
android:
    cargo ndk -t arm64-v8a -t x86_64 -P 26 -o android/app/src/main/jniLibs build -p assist-ffi --release
    cargo build -p assist-ffi
    cargo run -p assist-ffi --features bindgen --bin uniffi-bindgen -- generate --library target/debug/{{ffi_lib}} --language kotlin --out-dir android/app/src/main/java --no-format
    cd android && ./gradlew assembleDebug
