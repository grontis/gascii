# GASCII

A free and open source ASCII and ANSI art editor and animation studio, written in Rust.

Paint your matrix and hack the planet.

<!-- TODO: replace with a real screenshot or demo GIF before the first release -->
![GASCII screenshot](docs/screenshot.png)

## Features

- Grid-based ASCII canvas with drawing tools, brushes, and keyboard-driven workflows
- Layers with show/hide and reordering
- Frame-by-frame animation with playback, reordering, and export
- Plugin system — the density brush, animation pane, and layers panel are all plugins
- Windows pen/stylus support, including the barrel button
- PNG export and image-reference backgrounds
- Runs as a single native binary — no browser, no runtime

## Install

Download the build for your platform from the [latest release](https://github.com/grontis/gascii/releases/latest), or use an installer script:

**Windows (PowerShell):**

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://github.com/grontis/gascii/releases/latest/download/gascii-installer.ps1 | iex"
```

**macOS / Linux:**

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/grontis/gascii/releases/latest/download/gascii-installer.sh | sh
```

> **Note for Windows users:** builds are not code-signed, so SmartScreen may warn on first
> launch. Choose "More info" → "Run anyway". On macOS, right-click the binary and choose
> "Open" the first time.

GASCII is developed on Windows; Linux and macOS builds are provided but less battle-tested.
Bug reports from those platforms are very welcome.

## Build from source

Requires Rust 1.85 or newer.

```sh
git clone https://github.com/grontis/gascii.git
cd gascii
cargo build --release
```

On Linux you'll need some system libraries first (Debian/Ubuntu):

```sh
sudo apt-get install libgtk-3-dev libxcb-render0-dev libxcb-shape0-dev \
  libxcb-xfixes0-dev libxkbcommon-dev libssl-dev
```

The binary lands in `target/release/gascii`.

## Development

The workspace is split into focused crates:

| Crate | Purpose |
|-------|---------|
| `gascii` | The application: UI, canvas, viewport, export |
| `gascii-core` | Document model, editing operations, undo history |
| `gascii-plugin-api` | Plugin trait and host interface |
| `gascii-anim` | Animation frames and playback |
| `gascii-layers` | Layers panel |
| `gascii-density-brush` | Brightness-ramp brush |
| `gascii-stylus` | Raw Windows pointer-message hook for pen input |

Run the test suite with `cargo test --workspace`.

Releases are cut by pushing a version tag (e.g. `v0.2.0`); GitHub Actions builds and
publishes the binaries via [dist](https://axodotdev.github.io/cargo-dist/).

## License

[MIT](LICENSE)
