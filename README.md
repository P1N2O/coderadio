# coderadio 🎧

A tiny, single-binary, zero configuration cross-platform **terminal player for [freeCodeCamp Code Radio](https://coderadio.freecodecamp.org)**.

```text
▄█████ ▄████▄ ████▄  ██████   █████▄  ▄████▄ ████▄  ██ ▄████▄
██     ██  ██ ██  ██ ██▄▄     ██▄▄██▄ ██▄▄██ ██  ██ ██ ██  ██
▀█████ ▀████▀ ████▀  ██▄▄▄▄   ██   ██ ██  ██ ████▀  ██ ▀████▀

        https://github.com/p1n2o/coderadio

🔊 [█████████████████████████████████████████░░░] 100

💿  Now Playing: P1N2O — Coding on...

["Space" to Play/Pause] ["0-9" for Volume] ["Ctrl+C" to Quit]
```

## Features

- **TUI** — Minimal terminal interface.
- **Zero configuration** — Just run `coderadio` and you're good to go.
- **Small single binary** — ~2–4 MB, no install "just works".

## Install

### Option A — download a release binary

Pick the file for your platform from the [Releases](../../releases) page:

| Platform                      | File                           |
| ----------------------------- | ------------------------------ |
| Linux (arm64)                 | `coderadio-linux-arm64`        |
| Linux (x86_64)                | `coderadio-linux-x86_64`       |
| Windows (arm64)               | `coderadio-windows-arm64.exe`  |
| Windows (x86_64)              | `coderadio-windows-x86_64.exe` |
| macOS (arm64) (Apple Silicon) | `coderadio-macos-arm64`        |
| macOS (x86_64) (Intel)        | `coderadio-macos-x86_64`       |
| macOS (Universal)             | `coderadio-macos-universal`    |

```sh
# Linux
chmod +x coderadio-linux-x86_64
./coderadio-linux-x86_64

# macOS
chmod +x coderadio-macos-universal
./coderadio-macos-universal

# Windows (PowerShell/cmd)
.\coderadio-windows-x86_64.exe
```

### Option B — build from source

Requires Rust 1.85+.

```sh
cargo build --release
./target/release/coderadio
```

## Usage

```
coderadio [OPTIONS]
```

| Flag               | Description                                            |
| ------------------ | ------------------------------------------------------ |
| `--low`            | Use the 64 kbps stream instead of the default 128 kbps |
| `--volume <0-100>` | Initial playback volume (default `100`)                |
| `--url <URL>`      | Override the MP3 stream URL                            |
| `--probe <BYTES>`  | Fetch+decode a chunk and print stats, then exit        |
| `-V, --version`    | Print the version from Cargo.toml                      |
| `-h, --help`       | Print help                                             |

### Keys

| Key      | Action                    |
| -------- | ------------------------- |
| `Space`  | Play / Pause              |
| `0`      | Mute / Unmute             |
| `1`–`9`  | Set volume (linear 0–100) |
| `Ctrl+C` | Stop / Quit               |

## Platform guides

### Linux

Audio uses ALSA. On a desktop distro (PulseAudio or PipeWire installed as the system sound server) it plays straight away:

```sh
# Ubuntu/Debian — only if the ALSA runtime lib isn't present
sudo apt install libasound2
coderadio-linux-x86_64
```

### WSL2 (Windows Subsystem for Linux)

WSL2 is a VM with **no sound card**, so audio must be bridged to Windows. WSLg already runs a PulseAudio server on the Windows host (reachable at `$PULSE_SERVER`). To let the ALSA-based binary use it, install the ALSA→Pulse plugin once and point the default PCM at PulseAudio:

```sh
sudo apt install libasound2-plugins
cat >> ~/.asoundrc <<'EOF'
pcm.!default { type pulse }
ctl.!default { type pulse }
EOF
```

Then `coderadio-linux-x86_64` plays through your Windows speakers.

### macOS

Download `coderadio-macos-universal` (Intel + Apple Silicon) and run it.

### Windows

Download the `.exe` and run it from a terminal. Audio uses WASAPI.

## Building all platforms / cross-compiling

`scripts/build-all.sh` cross-compiles every target into `dist/`:

- Linux `x86_64` + `arm64` (glibc)
- Windows `x86_64` (GNU) + `arm64` (GNUnullvm)
- macOS `x86_64` + `arm64`

Requires `docker`; the cargo registry is cached in a named volume so repeat
runs are fast. The two macOS targets need an **Apple SDK** (set `SDKROOT` to a
`MacOSX.sdk`, or build on a Mac / macOS CI runner). GitHub Actions builds the
releases on tag push with the same image — see `.github/workflows/release.yml`.

## Development

```sh
# unit tests for render/volume/mute logic
cargo test
# headless network+decode check (no audio device required):
./target/release/coderadio --probe 1000000
```

## How it works

- `src/stream.rs` — a reconnecting byte pump: one background thread owns the HTTP connection and fills a bounded ring buffer; decoders read it as an endless `Read+Seek`, so a dropped connection just stalls (buffers) and reconnects with backoff.
- `src/nowplaying.rs` — polls the AzuraCast now-playing JSON and keeps the current track in shared UI state.
- `src/main.rs` — the TUI: render loop + `crossterm` raw-mode input, rodio/cpal playback, music metadata.

TLS is **vendored** (`rustls`), so no system certificate/SSL library is needed. The MP3 decode is pure-Rust through rodio's symphonia backend.

## License

MIT
