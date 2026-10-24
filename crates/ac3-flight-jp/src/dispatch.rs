//! Indirect calls (`jalr` through stored code addresses). Each module owns
//! `dispatch(m, target, args) -> Option<u32>` for the routines it translates;
//! an address nobody claims raises `Fault::Untranslated` and returns 0.
//!
//! Every original routine has one implementation, so exactly one module
//! claims each address (`tests/dispatch.rs` scans every `dispatch` and lists
//! the remaining duplicates, in a module still being extended:
//! 800273FC, 8002ACD4, 8002AD90, 8002AE0C in `boot.rs`/`mainloop.rs`).
//!
//! Overlay modules all load at [`OVERLAY_BASE`](crate::common::OVERLAY_BASE),
//! so their addresses overlap: an overlay module (`ov<N>*.rs`) claims a
//! target only while `common::loaded_module` is its module number, and the
//! one-owner rule holds per overlay.
//!
//! With the development feature `dev-fallback` (never in the shipped
//! binary), a call nobody claims goes to the interpreter fallback
//! ([`crate::fallback`]), and `--interpret` can force translated routines
//! through it first.
use crate::machine::{Fault, Machine};

/// A module's claim on indirect-call targets.
pub type Owner = fn(&mut Machine, u32, [u32; 4]) -> Option<u32>;

/// Every module's claim, in the order they are asked. Only the pending
/// duplicates listed in the module docs depend on it: `boot.rs` answers the
/// CD resource routines (its 8002AE0C translates CdPosToInt).
pub const OWNERS: [Owner; 97] = [
    crate::jp::ov1::dispatch,
    crate::jp::ov1_p1::dispatch,
    crate::jp::ov1_p1_card::dispatch,
    crate::jp::ov1_p1_load::dispatch,
    crate::jp::ov1_p1_save::dispatch,
    crate::jp::ov1_p1_screens::dispatch,
    crate::jp::ov1_p1_settings::dispatch,
    crate::jp::ov1_p2_menu::dispatch,
    crate::jp::ov1_p2_save::dispatch,
    crate::jp::ov1_p2_text::dispatch,
    crate::jp::ov1_p2_visual::dispatch,
    crate::jp::ov2::dispatch,
    crate::jp::ov2_p1::dispatch,
    crate::jp::ov2_p1_archive::dispatch,
    crate::jp::ov2_p1_keys::dispatch,
    crate::jp::ov2_p1_keyscreen::dispatch,
    crate::jp::ov2_p1_options::dispatch,
    crate::jp::ov2_p1_pages::dispatch,
    crate::jp::ov2_p1_search::dispatch,
    crate::jp::ov2_p2::dispatch,
    crate::jp::ov2_p2_cells::dispatch,
    crate::jp::ov2_p2_config::dispatch,
    crate::jp::ov2_p2_display::dispatch,
    crate::jp::ov2_p2_draw::dispatch,
    crate::jp::ov2_p2_grid::dispatch,
    crate::jp::ov2_p2_hub::dispatch,
    crate::jp::ov2_p2_music::dispatch,
    crate::jp::ov2_p2_options::dispatch,
    crate::jp::ov2_p2_records::dispatch,
    crate::jp::ov2_p2_screen::dispatch,
    crate::jp::ov2_p3::dispatch,
    crate::jp::ov2_p3_card::dispatch,
    crate::jp::ov2_p3_menu::dispatch,
    crate::jp::ov2_p3_panels::dispatch,
    crate::jp::ov2_p3_records::dispatch,
    crate::jp::ov2_p4_disc::dispatch,
    crate::jp::ov2_p4_graph::dispatch,
    crate::jp::ov2_p4_load::dispatch,
    crate::jp::ov2_p4_scene::dispatch,
    crate::jp::ov2_p4_story::dispatch,
    crate::jp::ov2_p4_stream::dispatch,
    crate::jp::ov2_p4_text::dispatch,
    crate::jp::ov2_p5::dispatch,
    crate::jp::ov2_p5_disc::dispatch,
    crate::jp::ov2_p5_lost::dispatch,
    crate::jp::ov2_p5_map::dispatch,
    crate::jp::ov2_p5_movie::dispatch,
    crate::jp::ov2_p5_net::dispatch,
    crate::jp::ov2_p5_net2::dispatch,
    crate::jp::ov2_p5_util::dispatch,
    crate::jp::ov2_p6::dispatch,
    crate::jp::ov2_stubs::dispatch,
    crate::jp::ov3::dispatch,
    crate::jp::ov3_p1::dispatch,
    crate::jp::ov3_p1_draw::dispatch,
    crate::jp::ov3_p1_player::dispatch,
    crate::jp::ov4::dispatch,
    crate::jp::ov4_p1::dispatch,
    crate::jp::ov4_p1_music::dispatch,
    crate::jp::ov4_p1_text::dispatch,
    crate::jp::res_p1_player::dispatch,
    crate::jp::res_p2_boot::dispatch,
    crate::jp::res_p2_script::dispatch,
    crate::jp::res_p3_hud_warn::dispatch,
    crate::jp::res_p3_resident::dispatch,
    crate::jp::res_p3_sky::dispatch,
    crate::jp::res_p3_terrain::dispatch,
    crate::jp::res_sdk::dispatch,
    crate::jp::res_sdk_format::dispatch,
    crate::jp::res_sdk_game::dispatch,
    crate::jp::resident::dispatch,
    crate::init::dispatch,
    crate::session::dispatch,
    crate::replay::dispatch,
    crate::frame::dispatch,
    crate::init_objects::dispatch,
    crate::init_tail::dispatch,
    crate::init_39a68::dispatch,
    crate::init_38818::dispatch,
    crate::script::dispatch,
    crate::shared::dispatch,
    crate::sdk::dispatch,
    crate::loading::dispatch,
    crate::boot::dispatch,
    crate::mainloop::dispatch,
    crate::cdstream::dispatch,
    crate::player::dispatch,
    crate::entities::dispatch,
    crate::render::dispatch,
    crate::terrain::dispatch,
    crate::hud::dispatch,
    crate::effects::dispatch,
    crate::ai::dispatch,
    crate::states::dispatch,
    crate::commands::dispatch,
    crate::arena::dispatch,
    crate::bios::dispatch,
];

pub fn call(m: &mut Machine, target: u32, args: [u32; 4]) -> u32 {
    #[cfg(feature = "dev-fallback")]
    if let Some(result) = crate::fallback::forced(m, target, args) {
        return result;
    }
    for owner in OWNERS {
        if let Some(result) = owner(m, target, args) {
            return result;
        }
    }
    #[cfg(feature = "dev-fallback")]
    if let Some(result) = crate::fallback::miss(m, target, args) {
        return result;
    }
    m.raise(Fault::Untranslated { address: target });
    0
}
