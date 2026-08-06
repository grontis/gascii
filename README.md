# GASCII

Paint your matrix and hack the planet.


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

## License

[MIT](LICENSE)
