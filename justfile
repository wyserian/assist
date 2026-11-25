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

# sherpa-onnx's Android libraries: fetched once with a pinned sha256 into target/, then handed
# to sherpa-onnx-sys through SHERPA_ONNX_ARCHIVE_DIR so its build script downloads nothing.
# Keep the version in step with sherpa-onnx-sys in Cargo.lock and android/app/build.gradle.kts.
sherpa_android := "sherpa-onnx-v2.0.3-android.tar.bz2"
sherpa_android_sha256 := "2a30b222abae6d5cf544df4a1c27944279449c25dbb9c755097189cb45db1df0"
sherpa_archives := justfile_directory() / "target" / "sherpa-archives"

# Android debug APK: core for phones (arm64) and the emulator (x86_64), Kotlin bindings, Gradle
android: _android-core
    cargo build -p assist-ffi
    cargo run -p assist-ffi --features bindgen --bin uniffi-bindgen -- generate --library target/debug/{{ffi_lib}} --language kotlin --out-dir android/app/src/main/java --no-format
    cd android && ./gradlew assembleDebug

_sherpa-android:
    just {{ if path_exists(sherpa_archives / sherpa_android) == "true" { "_sha256 " + quote(sherpa_archives / sherpa_android) + " " + sherpa_android_sha256 } else { "_fetch-sherpa-android" } }}

_fetch-sherpa-android:
    curl --fail --location --silent --show-error --proto =https --create-dirs --output "{{ sherpa_archives / sherpa_android }}.part" "https://github.com/k2-fsa/sherpa-onnx/releases/download/v2.0.3/{{ sherpa_android }}"
    just _sha256 "{{ sherpa_archives / sherpa_android }}.part" {{ sherpa_android_sha256 }}
    {{ if os() == "windows" { "Move-Item -Force" } else { "mv -f" } }} "{{ sherpa_archives / sherpa_android }}.part" "{{ sherpa_archives / sherpa_android }}"

# fails unless the file has this sha256
_sha256 file sum:
    @echo "{{ if sha256_file(file) == sum { "sha256 ok: " + file } else { error("sha256 mismatch: " + file) } }}"

# the core for Android; only this build reads the checked sherpa-onnx archive
_android-core $SHERPA_ONNX_ARCHIVE_DIR=sherpa_archives: _sherpa-android
    cargo ndk -t arm64-v8a -t x86_64 -P 26 -o android/app/src/main/jniLibs build -p assist-ffi --release
