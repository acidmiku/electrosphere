//! Host-side foundation for the native port. No CPU interpreter is used here.
pub mod aircraft_scene;
pub mod asset_dump;
pub mod assets;
pub mod bootstrap;
pub mod briefing_scene;
pub mod effects;
pub mod flight_assets;
pub mod flight_camera;
pub mod flight_gameplay;
pub mod flight_host;
pub mod flight_initialization;
pub mod flight_lighting;
pub mod flight_loader;
pub mod flight_resource_loader;
pub mod flight_runtime;
pub mod flight_world;
pub mod frontend;
pub mod gamepad;
pub mod gpu_renderer;
pub mod keyboard;
pub mod launcher;
pub mod machine_audio;
pub mod machine_card;
pub mod machine_mdec;
pub mod machine_movie;
pub mod machine_player;
pub mod memory;
pub mod menu_audio;
pub mod menu_fonts;
pub mod menu_runtime;
pub mod menu_scene;
pub mod menu_transform;
pub mod menu_validation;
pub mod mission_assets;
pub mod mission_audio;
pub mod mission_backdrop;
pub mod mission_background;
pub mod mission_hub;
pub mod mission_map;
pub mod mission_math;
pub mod mission_minimap;
pub mod mission_runtime;
pub mod movie_disc;
pub mod movie_export;
pub mod movie_pack;
pub mod movie_pack_mkv;
pub mod present;
pub mod resident_display;
pub mod savestate;
pub mod settings;
pub mod stream_audio;
pub mod validation;
pub mod version;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

pub fn sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}
