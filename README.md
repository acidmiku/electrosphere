# Electrosphere

A native port of **Ace Combat 3: Electrosphere** (PlayStation, 1999) to Rust,
for Windows, macOS and Linux. The game's own program, every routine of it, is
translated by hand into Rust and runs natively: no emulator, no BIOS and no
interpreter. The PlayStation hardware it talks to (GPU, sound, CD drive, MDEC
movies, pad, memory cards) is reimplemented around it.

It plays the **Japanese release** (two discs, Rev 1) with the **English fan
translation** applied, from your own disc images.

> **No game files are included.** You need your own copy of the game. This is
> an unofficial fan project, not affiliated with or endorsed by Bandai Namco
> Entertainment. *Ace Combat* is a trademark of Bandai Namco Entertainment.

## What you get

- **A launcher** with Play, Display and Controls pages. It finds your discs,
  makes a memory card on the first run, and starts the game.
- **Sharper pictures:** the GPU renderer draws the game at up to 8x the
  PlayStation's resolution, with optional dithering and sub-pixel geometry
  (no vertex wobble).
- **Widescreen flight** (16:9, 16:10 or 21:9) with the HUD kept as a centred
  4:3 block; menus, the hub and movies stay 4:3.
- **A CRT screen:** five bundled looks from [Lola Retro](https://github.com/zoran123456/lola-retro),
  each curved or flat, drawn through [librashader](https://github.com/SnowflakePowered/librashader),
  with a live preview in the launcher.
- **Flight effects** (optional): atmosphere and haze, far terrain, the sun
  with bloom and glare.
- **Controls:** keyboard with rebinding; gamepads (XInput on Windows, evdev on
  Linux, IOKit on macOS) with rumble where the platform has it, a fix for the
  game's large stick dead zone, and an option to swap the triggers and bumpers.
- **Saves:** memory card files, plus quick save and load (F5 / F8). Both discs
  are mounted and changed for you when the game asks.

## The discs

The port expects *Ace Combat 3: Electrosphere*, the Japanese release, **Rev 1**,
both discs (SLPS-02020 and SLPS-02021), with the English fan translation
patch applied. Use disc images: a `.cue` sheet with its `.bin`, or a
`.bin`, one per disc. The file names don't matter; the launcher tells the
discs by their contents, and says what it found and what is missing.

Where the launcher looks for them (its Play page has an **Open the folder**
button):

| Platform | Folder |
| --- | --- |
| Windows, Linux | the folder of the executable (`ac3.exe`, `ac3`) |
| macOS app | `~/Library/Application Support/Ace Combat 3` |
| Nix package | `$XDG_DATA_HOME/ace-combat-3` (`~/.local/share/ace-combat-3`) |

The settings (`ac3.toml`), the memory card (`card1.mcr`) and quick saves are
kept in the same folder. `[paths] disc` in `ac3.toml` can point at discs kept
elsewhere.

## Playing

Start `ac3` (on Windows `ac3.exe`, on macOS the app). The launcher opens;
**Play** (or Enter) starts the game, Esc quits.

| Keyboard (default) | PlayStation | In flight |
| --- | --- | --- |
| Arrows | D-pad | pitch and roll |
| W / S | R1 / L1 | throttle up / down |
| A / D | L2 / R2 | yaw left / right |
| Space or J | Square | |
| K | Cross | |
| L | Circle | |
| I | Triangle | |
| Enter / Backspace | Start / Select | |

The Controls page rebinds every key (two per button) and maps gamepad buttons.

| While playing | |
| --- | --- |
| F2 | CRT screen off / on |
| Shift+F3 | next CRT look |
| F4 | curved / flat glass |
| F3 | frame rate |
| F5 / F8 | quick save / quick load |
| F6 | flight effects off / on |
| F7 | change discs by hand |
| Alt+Enter | window / borderless full screen |
| Esc | quit |

On a Mac keyboard, hold fn with the F keys. If the game runs slowly, lower the
internal resolution on the Display page (2x keeps most of the smoothing) or
switch the CRT screen off.

### Command line

```sh
ac3                              # the launcher
ac3 machine                      # the game, with ac3.toml's settings
ac3 machine --help               # every option (window, renderer, discs, cards, ...)
ac3 settings --write-default     # a commented ac3.toml with the recommended settings
ac3 gamepad                      # what a gamepad sends, and a rumble test
```

`ac3 machine --headless --vblanks N` runs the game without a window and writes
pictures, the audio and a JSON report, which is how it is tested.

## Building

See [docs/BUILDING.md](docs/BUILDING.md): Linux, Windows (cross-built from
Linux), macOS, and Nix / NixOS (`nix run github:acidmiku/electrosphere`).

## How it works

- **The translation.** The game's resident program and its five overlays are
  translated routine by routine from MIPS into Rust (`crates/ac3-flight` for
  the European release, `crates/ac3-flight-jp` for the Japanese one, generated
  from the European crate and patched where the releases differ). The code
  keeps the original addresses and memory layout, so it runs on an
  address-preserving machine model (`crates/ac3-machine`) and can be compared
  with the original exactly.
- **Checked against the original.** Every translated routine was checked
  against captures of the original game running in an emulator, or against
  the original instructions run as an oracle where no capture reaches it;
  boot, menus, the intro movie and flight were compared frame by frame.
- **The host.** `src/` holds the player: the CD drive reading your disc images,
  the GPU (a software renderer at 1x and a wgpu renderer above it), the sound
  processor, the MDEC movie decoder, pads and memory cards, the window, the
  launcher, and the enhancements.
- **Tests.** `cargo test --release --workspace` runs close to 900 tests. Many more
  are `#[ignore]`d: they need game data or emulator captures that are not
  part of this repository.

Comments in the code sometimes refer to design notes (`docs/*.md`) and capture
tools that are not published here.

## Credits

- **Team NEMO**, for the English fan translation of the Japanese release.
- **[Lola Retro](https://github.com/zoran123456/lola-retro)** by zoran123456 (MIT):
  the CRT looks in `shaders/lola-retro`.
- **[librashader](https://github.com/SnowflakePowered/librashader)** (MPL-2.0),
  [wgpu](https://github.com/gfx-rs/wgpu), [winit](https://github.com/rust-windowing/winit),
  [egui](https://github.com/emilk/egui), [rodio](https://github.com/RustAudio/rodio),
  [gilrs](https://gitlab.com/gilrs-project/gilrs) and the other crates listed in
  [docs/THIRD-PARTY-NOTICES.txt](docs/THIRD-PARTY-NOTICES.txt).
- **[psx-spx](https://psx-spx.consoledev.net/)**, for the PlayStation hardware
  and file format documentation.
- The launcher's background art is by acidmiku.

## Licence

There is no licence: all rights are reserved. The translated game code is
derived from Bandai Namco's program, which is not ours to license. The
third-party parts keep their own licences ([docs/THIRD-PARTY-NOTICES.txt](docs/THIRD-PARTY-NOTICES.txt)).
