# mirelo-vst-rs

Prototype of VST3 / CLAP plugin for generating samples with [Mirelo AI](https://mirelo.ai) and easy playback via MIDI input.
Built with [truce](https://truce.audio).

## Requirements

- Rust 1.92+ with the MSVC toolchain (`x86_64-pc-windows-msvc`) and Visual Studio C++ build tools
- `cargo-truce`: `cargo install cargo-truce`
- A Mirelo API key

## Build & run

```powershell
# Standalone app (no DAW needed)
cargo truce run

# Build bundles into target/bundles/ without installing
cargo truce build --vst2 --vst3 --clap
```

## Usage

1. Start truce standalone (daw editor currently bugged on Ableton 12 for vst3)
2. Paste your Mirelo API key into the key and **save**.
   The key is stored in `%APPDATA%\MireloVst\config.json`.
   If pasting the key doesn't work then edit the json directly.
3. Write a prompt, e.g. *"glass shattering on a stone floor"*, pick a duration and press **Generate**.
4. Wait for the status to show **Ready**. Sometimes doesn't work right away and needs waiting or tabbing out.
5. Sample is played back after any MIDI note trigger. You can use the computer keyboard in standalone mode (Settings)
6. Enter a new prompt and press **Generate** again to replace the sample.
