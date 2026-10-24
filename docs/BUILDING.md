# Building Electrosphere

The port is one Rust workspace. Every platform builds the same `ac3`
executable: the launcher, the game and the command-line tools in one.

You need:

- **Rust 1.96 or newer**, from [rustup](https://rustup.rs) (or your system's
  packages, if they are recent enough).
- **A C++ compiler.** librashader (the CRT screen) builds glslang and
  SPIRV-Cross from source.
- **Python 3.9 or newer**, only for the packaging scripts in `tools/`.

The game's files are never needed to build; they are read from the player's
discs at run time (see the [README](../README.md)).

## Linux

Install a compiler, `pkg-config` and the ALSA and udev headers:

| Distribution | Command |
| --- | --- |
| Debian, Ubuntu | `sudo apt install build-essential pkg-config libasound2-dev libudev-dev` |
| Fedora | `sudo dnf install gcc-c++ pkgconf-pkg-config alsa-lib-devel systemd-devel` |
| Arch | `sudo pacman -S --needed base-devel alsa-lib systemd-libs` |
| openSUSE | `sudo zypper install gcc-c++ pkg-config alsa-devel libudev-devel` |

Then:

```sh
cargo build --release --locked
```

The executable is `target/release/ac3`. To play from the checkout, keep the
player's files in the repository's root, where the bundled CRT looks are
(`shaders/lola-retro`). These files are ignored by git:

```sh
# Put the two disc images in the repository's root first.
cargo run --release --locked -- launcher --settings ac3.toml
```

At run time the game needs a Vulkan driver (or OpenGL ES through EGL) and an
X11 session; on Wayland it runs through XWayland. Sound goes through ALSA,
which PipeWire and PulseAudio provide.

To make a folder to copy elsewhere, put `target/release/ac3` and the
`shaders` folder side by side; the game keeps its settings, memory card and
the discs in that folder.

## Windows

The Windows build is made on Linux, with the pinned
[llvm-mingw](https://github.com/mstorsjo/llvm-mingw) toolchain (downloaded
into `vendor/` and checked by SHA-256):

```sh
python3 tools/build-windows.py
```

It writes `dist/windows/` (`ac3.exe`, its three runtime DLLs and the CRT
looks) and `dist/Ace-Combat-3-Windows.zip`, a folder with a Read Me and the
third-party notices, ready to share. The target is `x86_64-pc-windows-gnullvm`.

A native build on Windows with Rust's MSVC toolchain has not been tried.

## macOS

Install the Xcode command line tools (`xcode-select --install`) and Rust from
rustup. Then, on the Mac:

```sh
python3 tools/build-macos.py              # Apple silicon and Intel in one app
python3 tools/build-macos.py --arch arm64 # one architecture
```

It writes `dist/macos/Ace Combat 3/` with `Ace Combat 3.app` (signed ad hoc,
not notarised), a Read Me and the third-party notices, and the same folder
zipped as `dist/macos/Ace-Combat-3-macOS.zip`. The app keeps the player's
files in `~/Library/Application Support/Ace Combat 3`. The first time, macOS
asks to allow an app that is not notarised: System Settings, Privacy &
Security, **Open Anyway**.

`cargo build --release --locked` also works on a Mac, for a plain
executable that keeps its files beside itself, as on Linux.

## Nix and NixOS

The repository is a flake. With flakes enabled:

```sh
nix run github:acidmiku/electrosphere      # build and start the launcher
nix build github:acidmiku/electrosphere    # ./result/bin/ac3
nix develop                                # a shell with Rust and the libraries
```

On NixOS, add it to a configuration:

```nix
# flake.nix of the system
inputs.electrosphere.url = "github:acidmiku/electrosphere";
# a module
environment.systemPackages = [ inputs.electrosphere.packages.${pkgs.system}.default ];
```

The package keeps the player's files in `$XDG_DATA_HOME/ace-combat-3`
(`~/.local/share/ace-combat-3`): put the discs there, or use the launcher's
**Open the folder**. It needs:

- graphics drivers: `hardware.graphics.enable = true;`;
- sound through ALSA: with PipeWire, `services.pipewire.alsa.enable = true;`;
- X11 or XWayland: on Wayland compositors without built-in XWayland (niri),
  `xwayland-satellite`.

Nix on another distribution builds the same package, but programs built by Nix
don't see the system's graphics drivers; run it through
[nixGL](https://github.com/nix-community/nixGL).

## Tests

```sh
cargo test --release --workspace
```

The tests that draw on the graphics card skip when there is no adapter;
set `AC3_REQUIRE_GPU=1` to make that a failure. The `#[ignore]`d tests need
game data or emulator captures that are not in this repository.

## Third-party notices

`docs/THIRD-PARTY-NOTICES.txt` lists the open-source code in the players, and
the packaging scripts ship it. After a dependency change, regenerate it on
Linux or macOS:

```sh
python3 tools/third-party-notices.py
```
