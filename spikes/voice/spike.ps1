# Runs the spike with the Vosk DLLs on PATH. Usage: ./spike.ps1 record | eval
$env:PATH = "$PSScriptRoot\models\vosk-win64-0.3.45;$env:PATH"
cargo run --release --manifest-path "$PSScriptRoot\Cargo.toml" -- @args
