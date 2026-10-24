# Lola Retro CRT presets (bundled)

Ten presets from [Lola Retro](https://github.com/zoran123456/lola-retro) by Zoki
(MIT, see `LICENSE`), commit `330feccbcae72d336479844d22ce156bd390a5bd`
(2026-10-22), `shaders/lola-retro/`, unchanged. Only the five looks the port
offers and the passes they use are here:

| Name in `[display] crt_preset` | Curved | Flat |
| --- | --- | --- |
| `trinitron-s-video` (default) | `curved/trinitron-s-video.slangp` | `flat/trinitron-s-video.slangp` |
| `consumer-tv-composite` | `curved/consumer-tv-composite.slangp` | `flat/consumer-tv-composite.slangp` |
| `dos-pc` | `systems/curved/dos-pc.slangp` | `systems/flat/dos-pc.slangp` |
| `gamecube` | `systems/curved/gamecube.slangp` | `systems/flat/gamecube.slangp` |
| `playstation` | `systems/curved/playstation.slangp` | `systems/flat/playstation.slangp` |

Passes (`shaders/`): native, encode, decode, glow-h, glow-v, beam, crt, present,
with `common.inc` and `crt.inc`. The packaging scripts ship this folder beside
the executable (Windows), in the app's Resources (macOS) or in the package's
`share/ace-combat-3` (Nix); the game looks next to its settings file first
(`src/settings.rs`, `src/present/crt.rs`).
