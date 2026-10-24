//! Flight visual effects (Milestone 1 Phase 4): display-only, every one a
//! setting and off by default. `docs/EFFECTS.md` is the integration contract.
//!
//! * [`atmosphere`]: the JP CLUT-fog redirect, the sky model, the fog colour
//!   and the haze model (`docs/EFFECTS_ATMOSPHERE.md`).
//! * [`atmosphere_fx`]: the atmosphere in the game (`[effects] atmosphere`):
//!   the redirect on the recorder and the haze post pass on the card.
//! * [`far_terrain`]: the mission's terrain beyond the game's range, to the
//!   horizon, beneath the game's own (`[effects] far_terrain`,
//!   `docs/EFFECTS_TERRAIN.md`).
//! * [`sun`]: the HDR sun, its lens flare and bloom, in place of the game's
//!   sun and flare (`[effects] sun`, `docs/EFFECTS_SUN.md`).
//! * [`terrain_map`]: the mission's terrain map built from a flight frame's RAM
//!   and VRAM, for far terrain, the ocean and the forest (`docs/EFFECTS_TERRAIN.md`).
pub mod atmosphere;
pub mod atmosphere_fx;
pub mod far_terrain;
pub mod sun;
pub mod terrain_map;
