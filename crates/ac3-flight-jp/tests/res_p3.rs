//! Part 3 of the JP resident wave (`src/jp/res_p3_*.rs`): the patched near
//! and changed routines of `cdstream`, `effects`, `hud`, `init_39a68`,
//! `init_objects`, `mainloop` and `terrain`, and the JP-only resident
//! routines of the part.
//!
//! * [`part3_captures`]: every JP Redux capture of a part-3 routine
//!   (`~/ac3-jp/analysis/redux/*/snapshots-module0.jsonl`) runs through
//!   `dispatch::call` from its entry snapshot and must reproduce the return
//!   snapshot, as `tests/captures.rs` does, with a CD drive that accepts
//!   every command (libcd's command write is a host call; without a drive it
//!   faults, see `mainloop::pending_sdk`).
//! * [`part3_oracle`]: the JP original (`support::oracle`) and the
//!   translation run from the same states and must agree (RAM outside the
//!   stack, scratchpad, GTE, `sp`, fault, `v0`): each captured entry
//!   snapshot, then variants of it with the fields that select the other
//!   paths randomised; the routines no capture reaches start from the JP
//!   Redux RAM dumps of the right state.
//!
//! `cargo test --release -p ac3-flight-jp --features dev-fallback --test
//! res_p3 -- --include-ignored --nocapture`.
mod support;

use ac3_flight_jp::machine::{Device, HostCall, Machine};
use ac3_flight_jp::snapshot::{self, Snapshot, SnapshotInfo};
use std::collections::{BTreeMap, BTreeSet};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use support::{oracle, scan};

/// A routine's variant generator: mutate the state and the arguments of a
/// base state (variant > 0; variant 0 is the base itself).
type Mutate = fn(&mut Machine, &mut oracle::Rng, &mut [u32; 4], u32);

/// One routine of the part: JP address, Rust path, variant generator.
struct Routine {
    address: u32,
    name: &'static str,
    mutate: Option<Mutate>,
}

const fn routine(address: u32, name: &'static str, mutate: Option<Mutate>) -> Routine {
    Routine {
        address,
        name,
        mutate,
    }
}

/// The part's routines (JP addresses) whose translation the part provides,
/// in reach order.
const ROUTINES: &[Routine] = &[
    routine(0x8002_a79c, "cdstream::start_read", Some(mutate_start_read)),
    routine(
        0x8002_8c24,
        "mainloop::map_pad_input",
        Some(mutate_map_pad_input),
    ),
    routine(
        0x8002_e1f4,
        "mainloop::update_sound",
        Some(mutate_update_sound),
    ),
    routine(0x8002_ac3c, "mainloop::cd_stop", Some(mutate_cd_stop)),
    routine(0x8002_e0d0, "mainloop::key_on_voice", Some(mutate_voice)),
    routine(
        0x8002_a498,
        "cdstream::stream_service",
        Some(mutate_stream_service),
    ),
    routine(
        0x8002_9bd8,
        "cdstream::stream_sector",
        Some(mutate_stream_sector),
    ),
    routine(
        0x8002_c41c,
        "mainloop::cd_command_pause",
        Some(mutate_cd_pause),
    ),
    routine(
        0x8002_abe0,
        "res_p3_resident::stream_pause",
        Some(mutate_stream_pause),
    ),
    routine(
        0x8002_df98,
        "mainloop::update_voice_volume",
        Some(mutate_voice),
    ),
    routine(
        0x8002_d3f4,
        "hud::stream::overlay_2cfcc",
        Some(mutate_overlay_2cfcc),
    ),
    routine(0x8005_c5b0, "init_39a68::block_4f4_008", None),
    routine(0x8005_c990, "init_39a68::block_4f4_09c", None),
    routine(0x8005_cedc, "init_39a68::block_4f4_1b8", None),
    routine(0x8005_d344, "init_39a68::block_4f4_30c", None),
    routine(0x8005_d844, "init_39a68::block_4f4_244", None),
    routine(0x8005_dca8, "init_39a68::block_4f4_384", None),
    routine(0x8005_defc, "init_39a68::block_4f4_3dc", None),
    routine(
        0x8005_c680,
        "hud::tree_5cbf4::heading_5af88",
        Some(mutate_panel),
    ),
    routine(
        0x8005_cb48,
        "hud::tree_5cbf4::altitude_5b450",
        Some(mutate_panel),
    ),
    routine(
        0x8005_d000,
        "hud::tree_5cbf4::timer_5b908",
        Some(mutate_panel),
    ),
    routine(
        0x8005_d468,
        "hud::tree_5cbf4::damage_5bd40",
        Some(mutate_panel),
    ),
    routine(
        0x8005_d998,
        "hud::tree_5cbf4::missiles_5c270",
        Some(mutate_panel),
    ),
    routine(0x8003_a008, "init_39a68::initialize_39a68", None),
    routine(
        0x8004_09ac,
        "init_39a68::reset_gp_344 (readdress patch)",
        None,
    ),
    routine(0x8003_b924, "init_39a68::packets_424", None),
    routine(0x8003_c250, "init_39a68::packets_424_quads", None),
    routine(
        0x8008_4a6c,
        "init_objects::init_effect_tables",
        Some(mutate_effect_tables),
    ),
    routine(0x8008_49ec, "init_objects::reset_effect_lists", None),
    routine(
        0x8006_ce70,
        "init_objects::init_player_flight",
        Some(mutate_player_flight),
    ),
    routine(
        0x8006_cd6c,
        "init_objects::aircraft_parameters",
        Some(mutate_aircraft),
    ),
    routine(
        0x8005_ebb8,
        "res_p3_resident::palettes_5ebb8",
        Some(mutate_palettes),
    ),
    routine(0x8005_66f0, "res_p3_resident::init_script_566f0", None),
    routine(
        0x8005_671c,
        "res_p3_resident::reset_script_5671c",
        Some(mutate_reset_script),
    ),
    routine(0x8004_8bdc, "res_p3_resident::reset_48bdc", None),
    routine(
        0x8005_6634,
        "res_p3_resident::message_state_56634",
        Some(mutate_message_state),
    ),
    routine(
        0x8005_6778,
        "res_p3_resident::step_script_56778",
        Some(mutate_step_script),
    ),
    routine(
        0x8004_0900,
        "res_p3_resident::label_40900",
        Some(mutate_label),
    ),
    routine(
        0x8005_65e8,
        "res_p3_resident::start_message_565e8",
        Some(mutate_start_message),
    ),
    routine(
        0x8005_6a54,
        "res_p3_resident::queue_message_56a54",
        Some(mutate_queue_message),
    ),
    routine(
        0x8004_d124,
        "res_p3_resident::glyph_index_4d124",
        Some(mutate_character),
    ),
    routine(
        0x8003_4360,
        "res_p3_resident::sprite_34360",
        Some(mutate_sprite_34360),
    ),
    routine(
        0x8004_76f0,
        "hud::tree_40a24::area_status_46aa8",
        Some(mutate_area_status),
    ),
    routine(
        0x8004_9438,
        "res_p3_resident::radio_cue_49438",
        Some(mutate_radio_cue),
    ),
    routine(
        0x8008_5204,
        "res_p3_sky::draw_object_systems_85204",
        Some(mutate_object_systems),
    ),
    routine(
        0x8008_d9f4,
        "res_p3_sky::cloud_layers_8d9f4",
        Some(mutate_cloud_layers),
    ),
    routine(0x8009_43a8, "res_p3_terrain::fog_943a8", Some(mutate_fog)),
    routine(0x8009_57a4, "terrain::draw_sky", Some(mutate_draw_sky)),
    routine(0x8009_4718, "terrain::sky_camera", Some(mutate_sky_camera)),
    routine(0x8009_4d10, "terrain::sky_colors", Some(mutate_sky_params)),
    routine(0x8009_4ac8, "terrain::sky_bands", Some(mutate_sky_params)),
    routine(
        0x8009_48a0,
        "terrain::project_bands",
        Some(mutate_project_bands),
    ),
    routine(0x8009_5074, "terrain::draw_bands", Some(mutate_draw_bands)),
    routine(0x8009_5cb8, "terrain::draw_stars", Some(mutate_stars)),
    routine(0x8009_5930, "terrain::draw_sun", Some(mutate_sun)),
    routine(
        0x8006_1fa8,
        "terrain::subdivide_level",
        Some(mutate_subdivide),
    ),
    routine(
        0x8003_b34c,
        "hud::tree_39d84::screen_targets_3abac",
        Some(mutate_screen_targets),
    ),
    routine(
        0x8003_ef40,
        "hud::tree_39d84::lockon_3e780",
        Some(mutate_lockon),
    ),
    routine(
        0x8003_f2cc,
        "hud::tree_39d84::lock_states_3eb14 (generated)",
        None,
    ),
    routine(
        0x8004_0a24,
        "hud::tree_39d84::lock_cursor_4016c",
        Some(mutate_lock_cursor),
    ),
    routine(
        0x8003_a68c,
        "res_p3_hud_warn::counters_3a68c",
        Some(mutate_counters),
    ),
    routine(
        0x8004_1794,
        "hud::tree_40a24::frame_40de4",
        Some(mutate_frame),
    ),
    routine(
        0x8004_14d8,
        "hud::tree_40a24::link_frame_40b00",
        Some(mutate_link_frame),
    ),
    routine(
        0x8003_c5b0,
        "hud::compose::gauges_3bdf8",
        Some(mutate_gauges),
    ),
    routine(
        0x8003_d458,
        "hud::tree_3bdf8_a::frame_tiles_3cca0",
        Some(mutate_radar_range),
    ),
    routine(
        0x8003_d5c0,
        "hud::tree_3bdf8_b::radar_map_3ce04",
        Some(mutate_radar_range),
    ),
    routine(
        0x8003_da08,
        "hud::tree_3bdf8_b::entity_blips_3d24c",
        Some(mutate_radar_range),
    ),
    routine(
        0x8003_dcf0,
        "hud::tree_3bdf8_b::ground_blips_3d534",
        Some(mutate_ground_blips),
    ),
    routine(
        0x8003_de5c,
        "hud::tree_3bdf8_b::draw_blips_3d6a0",
        Some(mutate_draw_blips),
    ),
    routine(
        0x8003_f520,
        "hud::tree_3e828::targets_3ed64",
        Some(mutate_targets),
    ),
    routine(
        0x8004_0e70,
        "hud::tree_3e828::reticle_4049c (generated)",
        None,
    ),
    routine(0x8006_4cb4, "hud::tree_62680_a::center_lines_634e8", None),
    routine(0x8003_fa80, "hud::tree_3e828::box_3f28c (generated)", None),
    routine(
        0x8003_e69c,
        "hud::tree_3bdf8_b::map_window_3dedc",
        Some(mutate_map_window),
    ),
    routine(
        0x8004_ceb0,
        "res_p3_resident::banner_4ceb0",
        Some(mutate_banner),
    ),
    routine(
        0x8004_d038,
        "res_p3_resident::text_4d038",
        Some(mutate_banner_line),
    ),
    routine(
        0x8004_2544,
        "hud::tree_40a24::warnings_41a68",
        Some(mutate_warnings),
    ),
    routine(
        0x8006_5d8c,
        "hud::tree_62680_a::target_arrow_645c0",
        Some(mutate_target_arrow),
    ),
    routine(
        0x8004_8c0c,
        "res_p3_resident::radio_cue_48c0c",
        Some(mutate_radio_cue_48c0c),
    ),
    routine(
        0x8004_1f88,
        "hud::tree_40a24::warning_label_415d8",
        Some(mutate_warning_label),
    ),
    routine(
        0x8008_fc9c,
        "hud::tree_39d84::fade_8e42c",
        Some(mutate_fade_level),
    ),
    routine(
        0x8003_4930,
        "res_p3_resident::menu_34930",
        Some(mutate_menu),
    ),
    routine(
        0x8003_5184,
        "res_p3_resident::menu_35184",
        Some(mutate_menu),
    ),
    routine(
        0x8003_545c,
        "res_p3_resident::prompt_3545c",
        Some(mutate_prompt),
    ),
    // no capture reaches these: RAM dumps (STATES)
    routine(
        0x8004_925c,
        "res_p3_resident::unit_flag_4925c",
        Some(mutate_unit_flag),
    ),
    routine(0x8002_5fe8, "res_p3_resident::shutdown_25fe8", None),
    routine(
        0x8003_6130,
        "res_p3_resident::next_mission_36130",
        Some(mutate_mission_index),
    ),
    routine(
        0x8003_8078,
        "res_p3_resident::select_38078",
        Some(mutate_select),
    ),
    routine(
        0x8004_8a40,
        "res_p3_resident::context_48a40",
        Some(mutate_context_48a40),
    ),
    routine(
        0x8005_6ac8,
        "res_p3_resident::rect_56ac8",
        Some(mutate_rect),
    ),
    routine(
        0x8003_346c,
        "terrain::render_world",
        Some(mutate_render_world),
    ),
    routine(
        0x8009_6184,
        "terrain::draw_cloud_bands",
        Some(mutate_cloud_bands),
    ),
    routine(0x8009_65d4, "terrain::screen_outcode", Some(mutate_outcode)),
    routine(0x8009_7eb4, "terrain::draw_dome_sky", Some(mutate_sky_kind)),
    routine(
        0x8009_8044,
        "terrain::draw_night_sky",
        Some(mutate_sky_kind),
    ),
    routine(0x8008_faa8, "terrain::fade_overlay", Some(mutate_fade)),
    routine(
        0x8009_8714,
        "terrain::night_ground_row",
        Some(mutate_ground_row),
    ),
    routine(
        0x8009_39d4,
        "res_p3_terrain::letterbox_939d4",
        Some(mutate_letterbox),
    ),
    routine(
        0x8004_09d4,
        "hud::tree_39d84::lock_start_4011c",
        Some(mutate_lock_start),
    ),
    routine(
        0x8003_a0b4,
        "hud::compose::compose_39b80",
        Some(mutate_compose),
    ),
    routine(
        0x8003_e274,
        "hud::extras::radar_marker_3dab4",
        Some(mutate_radar_marker),
    ),
    routine(
        0x8006_6590,
        "hud::extras::impact_marker_64dc4",
        Some(mutate_impact_marker),
    ),
    routine(
        0x8006_477c,
        "hud::tree_62ea0::dart_62fb0",
        Some(mutate_dart),
    ),
    routine(
        0x8004_2b20,
        "hud::tree_41ff0::guidance_41ff0",
        Some(mutate_guidance),
    ),
    routine(
        0x8004_2d98,
        "hud::tree_41ff0::approach_42258",
        Some(mutate_guidance),
    ),
    routine(
        0x8008_29bc,
        "effects::fragments_80081374",
        Some(mutate_fragments),
    ),
    routine(
        0x8003_fd48,
        "hud::marker_shapes::hexagon_3f54c (generated)",
        Some(mutate_marker_shape),
    ),
    routine(
        0x8004_01ac,
        "hud::marker_shapes::corner_ticks_3f9a8 (generated)",
        Some(mutate_marker_shape),
    ),
];

/// The interrupt context written outside the CD handler when the CD
/// interrupt arrives during a main-program call (JP; as in the PAL
/// `tests/cdstream.rs`): BIOS kernel RAM of its exception handler
/// (0x80008500..0x80008600 and the exception frame 0x8000E200..0x8000E340)
/// and libetc's dispatcher frame and state on the JP interrupt stack
/// (0x800B5E00..0x800B60A0).
const IRQ_CONTEXT: &[(u32, u32)] = &[
    (0x8000_8500, 0x8000_8600),
    (0x8000_e200, 0x8000_e340),
    (0x800b_5e00, 0x800b_60a0),
];
/// The top of the JP interrupt stack: a capture whose `sp` is below it ran
/// inside the CD interrupt.
const IRQ_STACK_TOP: u32 = 0x800b_60a0;
/// The native host runs the CD interrupt on the main program's stack (the
/// original's interrupt stack lies below `Machine`'s stack region), so
/// calls captured inside the interrupt run from here.
const HOST_IRQ_SP: u32 = 0x801f_f000;
/// Routines whose captures an interrupt arrived during: the CD interrupt
/// (a blocking libcd command waits for its acknowledge) or VBlank (DrawSync
/// waits).
const INTERRUPTED: &[u32] = &[0x8002_ac3c, 0x8005_ebb8];
/// What the VBlank interrupt changes while a routine waits in DrawSync
/// (8005EBB8 waits 17 times): the VSync counter 0x800B6174 and libetc's
/// 0x800C7DF4 count, libpad's wait start 0x800C7E98, libgpu's DrawSync
/// deadline (VSync + 0xF0) and polls 0x800BBAA0..A8, and libgpu's saved
/// interrupt mask 0x800BBA94 (an interrupt-controller register the host
/// does not model).
const VBLANK_CONTEXT: &[(u32, u32)] = &[
    (0x800b_6174, 0x800b_6178),
    (0x800c_7df4, 0x800c_7df8),
    (0x800c_7e98, 0x800c_7ea0),
    (0x800b_ba94, 0x800b_ba98),
    (0x800b_baa0, 0x800b_baa8),
];
/// Callees of another part that are not in this workspace, with the
/// halfwords their JP patch changes ((PAL-derived native value, JP value)):
/// in the captures of the listed routines exactly those halfword
/// differences are allowed.
/// (routine, pending callee, its owner, the (native, JP) halfword pairs).
type Pending = (u32, u32, &'static str, &'static [(u16, u16)]);

/// Empty since resident part 1 (800343C8, the callee of 80034930 and
/// 80035184) is merged.
const PENDING: &[Pending] = &[];

/// Routines whose captures waited in DrawSync across VBlanks.
const VBLANK_WAITS: &[u32] = &[0x8005_ebb8];

/// The original's drive, replayed from a capture pair (test fixture data,
/// as the PAL `Replay` of `tests/cdstream.rs`): a blocking command polls
/// as often as the original did (`0x800C7EDC` of the return snapshot), the
/// interrupt handler then reads one acknowledge with the status byte the
/// original kept (`0x800C7EC0`), a sector transfer delivers the bytes the
/// original's DMA left at its destination (a destination in the host's
/// interrupt frame maps to the original's frame), and StoreImage the VRAM
/// pixels the original read.
struct CaptureDrive {
    exit: Vec<u8>,
    polls: u32,
    acknowledge: Option<u8>,
    frame_shift: u32,
}
impl CaptureDrive {
    fn new(exit: &Snapshot, frame_shift: u32) -> Self {
        let at = |a: u32| (a & 0x1f_ffff) as usize;
        let polls = u32::from_le_bytes(
            exit.ram[at(0x800c_7edc)..at(0x800c_7edc) + 4]
                .try_into()
                .unwrap(),
        );
        Self {
            exit: exit.ram.clone(),
            polls: polls.max(1),
            acknowledge: Some(exit.ram[at(0x800c_7ec0)]),
            frame_shift,
        }
    }
}
impl Device for CaptureDrive {
    fn call(&mut self, ram: &mut [u8], _: &mut [u8], call: HostCall) -> Option<u32> {
        use ac3_flight_jp::mainloop::pending_sdk as cd;
        let [a0, a1, ..] = call.args;
        match call.function {
            cd::CD_COMMAND => Some(self.polls),
            cd::CD_INTERRUPT => {
                if a0 == cd::SITE_HANDLER
                    && let Some(status) = self.acknowledge.take()
                {
                    let mut put = |offset: u32, byte: u8| {
                        ram[(a1.wrapping_add(offset) & 0x1f_ffff) as usize] = byte;
                    };
                    put(0x10, 3);
                    put(0x11, 1);
                    put(0x18, status);
                }
                Some(0)
            }
            cd::CD_GET_SECTOR | cd::CD_GET_SECTOR2 => {
                let source = if (0x801f_c000..0x8020_0000).contains(&a0) {
                    a0.wrapping_add(self.frame_shift)
                } else {
                    a0
                };
                for i in 0..a1.wrapping_mul(4) {
                    let to = (a0.wrapping_add(i) & 0x1f_ffff) as usize;
                    let from = (source.wrapping_add(i) & 0x1f_ffff) as usize;
                    ram[to] = self.exit[from];
                }
                Some(0)
            }
            // StoreImage(rect, pixels) (canonical host-call ID 800A6970):
            // the VRAM rectangle as the original read it
            0x800a_6970 => {
                let rect = (a0 & 0x1f_ffff) as usize;
                let half = |at: usize| u32::from(u16::from_le_bytes([ram[at], ram[at + 1]]));
                let bytes = half(rect + 4) * half(rect + 6) * 2;
                for i in 0..bytes {
                    let at = (a1.wrapping_add(i) & 0x1f_ffff) as usize;
                    ram[at] = self.exit[at];
                }
                Some(0)
            }
            _ => None,
        }
    }
}

/// The capture root (`AC3_JP_CAPTURES`, else `AC3_JP_DIR/analysis/redux`).
fn root() -> PathBuf {
    std::env::var_os("AC3_JP_CAPTURES")
        .map(PathBuf::from)
        .unwrap_or_else(|| scan::jp_dir().join("analysis/redux"))
}

/// Every resident snapshot index (`<run>/snapshots-module0.jsonl`).
fn indexes() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(root())
        .map(|runs| {
            runs.flatten()
                .map(|r| r.path().join("snapshots-module0.jsonl"))
                .filter(|p| p.exists())
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

fn run_name(index: &Path) -> String {
    index
        .parent()
        .and_then(Path::file_name)
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The return snapshot matching an entry (as `snapshot::check_call`).
fn exit_of<'a>(index: &'a [SnapshotInfo], entry: &SnapshotInfo) -> Option<&'a SnapshotInfo> {
    index.iter().find(|s| {
        s.callee == entry.callee
            && s.stage == "return"
            && s.sequence > entry.sequence
            && s.ra == entry.ra
            && s.registers.gpr[29] == entry.registers.gpr[29]
    })
}

/// The oracle cases' CD drive: every command is accepted at once (one
/// status poll) and acknowledged (status 0x02, motor on) at the next poll
/// of the interrupt handler, so that blocking commands complete; sector
/// transfers deliver nothing (the stale buffer bytes stand for the data).
/// Both runs of a case get a fresh drive.
#[derive(Default)]
struct AcceptingDrive {
    acknowledge: bool,
}
impl Device for AcceptingDrive {
    fn call(&mut self, ram: &mut [u8], _: &mut [u8], call: HostCall) -> Option<u32> {
        use ac3_flight_jp::mainloop::pending_sdk as cd;
        let [a0, a1, ..] = call.args;
        match call.function {
            cd::CD_COMMAND => {
                self.acknowledge = true;
                Some(1)
            }
            cd::CD_INTERRUPT => {
                if a0 == cd::SITE_HANDLER && std::mem::take(&mut self.acknowledge) {
                    let mut put = |offset: u32, byte: u8| {
                        ram[(a1.wrapping_add(offset) & 0x1f_ffff) as usize] = byte;
                    };
                    put(0x10, 3);
                    put(0x11, 1);
                    put(0x18, 2);
                }
                Some(0)
            }
            _ => None,
        }
    }
}

fn panic_text(e: &(dyn std::any::Any + Send)) -> String {
    e.downcast_ref::<String>()
        .cloned()
        .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default()
}

/// The JP function extents of the map (resident).
fn extents() -> BTreeMap<u32, u32> {
    scan::jp_map(&scan::jp_dir())
        .iter()
        .filter(|r| r.region.starts_with("resident"))
        .map(|r| (r.jp, r.jp + r.size))
        .collect()
}

/// Routines whose dispatch arm returns no meaningful `v0`: the `{ ...; 0 }`
/// arms the scan finds, and the generated `{ f(m); Some(0) }` arms
/// (`hud::tree_39d84`, `hud::extras`) of routines whose translation returns
/// nothing (80040A24 leaves the cursor size halfword in `v0` on a frame it
/// moves, 80066590 its last computation; no caller reads them).
fn void_set() -> std::collections::BTreeSet<u32> {
    let mut void = scan::jp_resident_claims().1;
    void.extend([
        0x8003_b34c,
        0x8003_ef40,
        0x8003_f2cc,
        0x8004_0a24,
        0x8006_6590,
    ]);
    // `match target { .. => f(m, index, color), .. } Some(0)` (hud::marker_shapes):
    // JP returns 1 from their early exits (PAL: the last value); no caller reads it
    void.extend([0x8003_fd48, 0x8004_01ac]);
    void
}

/// With the development feature `dev-fallback`, routines no module claims
/// run interpreted from the JP original (`ac3_fallback`), as in a
/// development run; without it they raise `Fault::Untranslated`. Dropping
/// the guard uninstalls the fallback.
struct Interpreted;
impl Interpreted {
    fn install() -> Self {
        #[cfg(feature = "dev-fallback")]
        ac3_fallback::install(ac3_fallback::Fallback::new(ac3_fallback::Version::Jp));
        Self
    }
}
impl Drop for Interpreted {
    fn drop(&mut self) {
        #[cfg(feature = "dev-fallback")]
        ac3_fallback::uninstall();
    }
}

#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux captures)"]
fn part3_captures() {
    let _fallback = Interpreted::install();
    let void = void_set();
    let hook = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));
    let (mut passed, mut failures, mut allowances) = (0, Vec::new(), Vec::new());
    let mut seen: BTreeMap<u32, usize> = BTreeMap::new();
    for path in indexes() {
        let run = run_name(&path);
        let directory = path.parent().unwrap().to_path_buf();
        let index = snapshot::read_index(&path).unwrap();
        for entry in index
            .iter()
            .filter(|s| s.stage == "entry" && ROUTINES.iter().any(|r| r.address == s.callee))
        {
            let Some(exit) = exit_of(&index, entry) else {
                continue;
            };
            let callee = entry.callee;
            *seen.entry(callee).or_default() += 1;
            let entry = Snapshot::load(&directory, entry).unwrap();
            let exit = Snapshot::load(&directory, exit).unwrap();
            let mut m = entry.machine();
            let sp = entry.gpr(29);
            let mut excluded: Vec<(u32, u32)> = Vec::new();
            let mut frame_shift = 0;
            if sp < IRQ_STACK_TOP {
                // captured inside the CD interrupt: run on the host's
                // interrupt stack; the original's frames on its interrupt
                // stack (saved registers) are not compared
                m.sp = HOST_IRQ_SP;
                frame_shift = sp.wrapping_sub(HOST_IRQ_SP);
                excluded.push((sp - 0x200, sp));
                allowances.push(format!(
                    "{run} #{} {callee:08x}: inside the CD interrupt (sp {sp:#x}): runs at sp \
                     {HOST_IRQ_SP:#x}; {:#x}..{sp:#x} (its interrupt-stack frames) not compared",
                    entry.info.sequence,
                    sp - 0x200
                ));
            } else if INTERRUPTED.contains(&callee) {
                excluded.extend_from_slice(IRQ_CONTEXT);
                if VBLANK_WAITS.contains(&callee) {
                    excluded.extend_from_slice(VBLANK_CONTEXT);
                    allowances.push(format!(
                        "{run} #{} {callee:08x}: waits in DrawSync across VBlanks: {VBLANK_CONTEXT:x?} not compared",
                        entry.info.sequence
                    ));
                }
                allowances.push(format!(
                    "{run} #{} {callee:08x}: an interrupt arrives during the call: its \
                     context {IRQ_CONTEXT:x?} not compared",
                    entry.info.sequence
                ));
            }
            m.device = Some(Box::new(CaptureDrive::new(&exit, frame_shift)));
            let args = [entry.gpr(4), entry.gpr(5), entry.gpr(6), entry.gpr(7)];
            let result = panic::catch_unwind(AssertUnwindSafe(|| {
                ac3_flight_jp::dispatch::call(&mut m, callee, args)
            }));
            let verdict = match result {
                Err(e) => Err(format!("panic: {}", panic_text(&*e))),
                Ok(v0) => match m.take_fault() {
                    Err(f) => Err(format!("fault: {f}")),
                    Ok(()) => {
                        let mut d = snapshot::compare(&m, &exit);
                        d.ram
                            .retain(|a| !excluded.iter().any(|(lo, hi)| (*lo..*hi).contains(a)));
                        if let Some((_, pending, owner, pairs)) =
                            PENDING.iter().find(|p| p.0 == callee)
                        {
                            let original = exit.machine();
                            let half = |ram: &[u8], a: u32| {
                                let i = (a & 0x1f_fffe) as usize;
                                u16::from_le_bytes([ram[i], ram[i + 1]])
                            };
                            let before = d.ram.len();
                            d.ram.retain(|&a| {
                                !pairs.contains(&(half(m.ram(), a), half(original.ram(), a)))
                            });
                            if d.ram.len() < before {
                                allowances.push(format!(
                                    "{run} #{} {callee:08x}: {} bytes written by {pending:08x} \
                                     ({owner}, not in this workspace) hold its PAL values \
                                     (native, JP) {:04x?}",
                                    entry.info.sequence,
                                    before - d.ram.len(),
                                    pairs
                                ));
                            }
                        }
                        if !d.is_empty() {
                            Err(format!("state differs: {}", d.summary()))
                        } else if !void.contains(&callee) && v0 != exit.gpr(2) {
                            Err(format!("v0 {v0:#x}, original {:#x}", exit.gpr(2)))
                        } else {
                            Ok(snapshot::changed_bytes(&entry, &exit))
                        }
                    }
                },
            };
            match verdict {
                Ok(n) => {
                    passed += 1;
                    println!(
                        "pass {run} #{} {callee:08x}: {n} bytes changed",
                        entry.info.sequence
                    );
                }
                Err(e) => failures.push(format!(
                    "{run} #{} {callee:08x}: {e}; host calls {:08x?}",
                    entry.info.sequence,
                    m.host_calls
                        .iter()
                        .take(12)
                        .map(|c| (c.function, c.args))
                        .collect::<Vec<_>>()
                )),
            }
        }
    }
    panic::set_hook(hook);
    for s in &allowances {
        println!("allowance: {s}");
    }
    let uncaptured: Vec<String> = ROUTINES
        .iter()
        .filter(|r| !seen.contains_key(&r.address))
        .map(|r| format!("{:08x}", r.address))
        .collect();
    println!(
        "part 3 captures: {passed} pass ({} with a documented allowance), {} fail; not captured: {}",
        allowances.len(),
        failures.len(),
        uncaptured.join(" ")
    );
    assert!(failures.is_empty(), "{failures:#?}");
    assert!(passed > 0);
}

// ---- the oracle -------------------------------------------------------------------

/// Variants per base state.
const VARIANTS: u32 = 200;

/// A word pick for a field: zero, small, a medium value or a random word.
fn pick(rng: &mut oracle::Rng) -> u32 {
    match rng.next() % 6 {
        0 => 0,
        1 => rng.next() % 4,
        2 => rng.next() % 0x40,
        3 => rng.next() % 0x1000,
        4 => (rng.next() << 8) ^ rng.next(),
        _ => (rng.next() % 0x200).wrapping_neg(),
    }
}

const GP: u32 = ac3_flight_jp::GP;

/// The base states of a routine: its captured entries (with their
/// arguments) and the RAM dumps of [`STATES`].
fn bases(address: u32) -> Vec<(String, Machine, [u32; 4], bool)> {
    let mut out = Vec::new();
    for path in indexes() {
        let run = run_name(&path);
        let directory = path.parent().unwrap().to_path_buf();
        let index = snapshot::read_index(&path).unwrap();
        for entry in index
            .iter()
            .filter(|s| s.stage == "entry" && s.callee == address)
        {
            let snap = Snapshot::load(&directory, entry).unwrap();
            let args = [snap.gpr(4), snap.gpr(5), snap.gpr(6), snap.gpr(7)];
            let mut m = snap.machine();
            if !(0x801f_c000..0x8020_0000).contains(&m.sp) {
                // an interrupt-context entry (libcd's stack): run on the main stack
                m.sp = 0x801f_ff00;
            }
            out.push((format!("{run} #{}", entry.sequence), m, args, false));
        }
    }
    for (_, dumps) in STATES.iter().filter(|(a, _)| *a == address) {
        for dump in *dumps {
            out.push((dump.to_string(), dump_machine(dump), [0; 4], true));
        }
    }
    out
}

/// RAM dumps (`<run>/ram-<field>`, JP Redux, `CAPTURES.md` §4) the routines
/// no capture reaches start from, in the state they belong to: flight
/// (`flight-001`, fields 50100.. after gameplay initialization; 51700
/// MISSION FAILED; 52300 replay), the hub (`flight-001/ram-053900`).
const STATES: &[(u32, &[&str])] = &[
    (
        0x8002_5fe8,
        &["flight-001/ram-050100", "flight-001/ram-053900"],
    ),
    (
        0x8002_c41c,
        &["flight-001/ram-050100", "flight-001/ram-052300"],
    ),
    (
        0x8003_6130,
        &["flight-001/ram-050100", "flight-001/ram-053900"],
    ),
    (
        0x8003_8078,
        &["flight-001/ram-050100", "flight-001/ram-053900"],
    ),
    (
        0x8004_8a40,
        &["flight-001/ram-050100", "flight-001/ram-051700"],
    ),
    (
        0x8005_6ac8,
        &["flight-001/ram-050100", "flight-001/ram-052300"],
    ),
    (0x8004_925c, &["flight-001/ram-050100"]),
    (
        0x8003_346c,
        &["flight-001/ram-050100", "flight-001/ram-052300"],
    ),
    (
        0x8004_09d4,
        &["flight-001/ram-050100", "flight-001/ram-052300"],
    ),
    (
        0x8003_a0b4,
        &["flight-001/ram-050100", "flight-001/ram-052300"],
    ),
    (
        0x8003_e274,
        &["flight-001/ram-050100", "flight-001/ram-052300"],
    ),
    (
        0x8006_6590,
        &["flight-001/ram-050100", "flight-001/ram-052300"],
    ),
    (
        0x8006_477c,
        &["flight-001/ram-050100", "flight-001/ram-052300"],
    ),
    (
        0x8004_2b20,
        &["flight-001/ram-050100", "flight-001/ram-052300"],
    ),
    (
        0x8004_2d98,
        &["flight-001/ram-050100", "flight-001/ram-052300"],
    ),
    (
        0x8008_29bc,
        &["flight-001/ram-050100", "flight-001/ram-052300"],
    ),
    (
        0x8003_fd48,
        &["flight-001/ram-050100", "flight-001/ram-052300"],
    ),
    (
        0x8004_01ac,
        &["flight-001/ram-050100", "flight-001/ram-052300"],
    ),
    (
        0x8009_6184,
        &["flight-001/ram-050100", "flight-001/ram-052300"],
    ),
    (
        0x8009_65d4,
        &["flight-001/ram-050100", "flight-001/ram-052300"],
    ),
    (
        0x8009_7eb4,
        &["flight-001/ram-050100", "flight-001/ram-052300"],
    ),
    (
        0x8009_8044,
        &["flight-001/ram-050100", "flight-001/ram-052300"],
    ),
    (
        0x8008_faa8,
        &["flight-001/ram-050100", "flight-001/ram-052300"],
    ),
    (
        0x8009_8714,
        &["flight-001/ram-050100", "flight-001/ram-052300"],
    ),
    (
        0x8009_39d4,
        &["flight-001/ram-050100", "flight-001/ram-052300"],
    ),
];

/// A machine from a RAM dump: RAM, scratchpad, GTE registers, `sp` at the
/// main stack.
fn dump_machine(name: &str) -> Machine {
    let base = root().join(name);
    let path = |ext: &str| PathBuf::from(format!("{}.{ext}", base.display()));
    let ram = std::fs::read(path("ram.bin")).unwrap_or_else(|e| panic!("{name}: {e}"));
    let scratch = std::fs::read(path("scratch.bin")).unwrap_or_else(|e| panic!("{name}: {e}"));
    let regs: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path("regs.json")).unwrap()).unwrap();
    let words = |key: &str| -> [u32; 32] {
        std::array::from_fn(|i| regs[key][i].as_u64().unwrap_or(0) as u32)
    };
    let mut m = Machine::from_images(&ram, &scratch).expect("images");
    m.gte.data = words("cp2d");
    m.gte.control = words("cp2c");
    m.sp = 0x801f_ff00;
    m
}

/// One routine: the base states and their variants through the oracle.
/// Returns (cases, RAM bytes the original changed in all, instruction
/// addresses executed).
fn oracle_routine(r: &Routine, end: u32, void: bool) -> (usize, usize, BTreeSet<u32>) {
    let address = r.address;
    let mut cases = 0;
    let mut changed = 0;
    let mut covered = BTreeSet::new();
    for (name, base, args, dump) in bases(address) {
        let variants = r.mutate.map_or(1, |_| VARIANTS);
        for variant in 0..variants {
            let mut m = base.clone();
            let mut a = args;
            let mut rng = oracle::Rng(address ^ variant.wrapping_mul(0x9e37_79b9));
            // a capture's variant 0 is the captured call itself; a dump has
            // no call, so every variant is generated
            if let Some(mutate) = r.mutate.filter(|_| variant > 0 || dump) {
                mutate(&mut m, &mut rng, &mut a, variant);
            }
            let label = format!("{address:08x} {name} v{variant} args {a:08x?}");
            changed += diff(&label, &m, (address, end), a, void, &mut covered);
            cases += 1;
        }
    }
    (cases, changed, covered)
}

/// Uncovered instruction ranges of `start..end` (for the report).
fn gaps(start: u32, end: u32, covered: &BTreeSet<u32>) -> String {
    let mut out = Vec::new();
    let mut a = start;
    while a < end {
        if covered.contains(&a) {
            a += 4;
            continue;
        }
        let from = a;
        while a < end && !covered.contains(&a) {
            a += 4;
        }
        out.push(if a - from == 4 {
            format!("{from:08x}")
        } else {
            format!("{from:08x}-{:08x}", a - 4)
        });
    }
    out.join(" ")
}

/// `oracle::diff` with the accepting CD drive attached to both runs (a
/// clone drops the device): the original interpreted, the translation
/// through `dispatch::call`. A third run of the original (the coverage copy
/// of the oracle) records the instructions executed. As in the snapshot
/// comparisons and the differential harness, the stack window
/// (`snapshot::STACK_START` up) is not compared: natives keep locals and
/// argument home slots in Rust.
fn diff(
    label: &str,
    base: &Machine,
    range: (u32, u32),
    args: [u32; 4],
    void: bool,
    covered: &mut BTreeSet<u32>,
) -> usize {
    let mut measured = base.clone();
    measured.device = Some(Box::new(AcceptingDrive::default()));
    coverage::run(&mut measured, range.0, range.1, args, covered);
    let mut original = base.clone();
    original.device = Some(Box::new(AcceptingDrive::default()));
    let v0 = oracle::run(&mut original, range.0, range.1, args);
    let mut native = base.clone();
    native.device = Some(Box::new(AcceptingDrive::default()));
    let nv0 = ac3_flight_jp::dispatch::call(&mut native, range.0, args);
    let (fo, fnat) = (original.take_fault(), native.take_fault());
    assert_eq!(fo, fnat, "{label}: faults (original, native)");
    if let Err(ac3_flight_jp::Fault::Trap { .. }) = fo {
        // the same compiler trap (a crash of the original): the oracle
        // stops at `break`, the translation runs on, so only the trap site
        // is compared
        return 0;
    }
    let dead = |i: usize| i >= snapshot::STACK_START;
    let ram: Vec<u32> = (0..0x20_0000usize)
        .filter(|&i| !dead(i) && original.ram()[i] != native.ram()[i])
        .map(|i| 0x8000_0000 + i as u32)
        .collect();
    let scratch: Vec<usize> = (0..0x400)
        .filter(|&i| original.scratch()[i] != native.scratch()[i])
        .collect();
    let bytes: Vec<String> = ram
        .iter()
        .take(8)
        .map(|&a| {
            let i = (a & 0x1f_ffff) as usize;
            format!(
                "{a:08x}: base {:02x} original {:02x} native {:02x}",
                base.ram()[i],
                original.ram()[i],
                native.ram()[i]
            )
        })
        .collect();
    assert!(
        ram.is_empty()
            && scratch.is_empty()
            && original.gte == native.gte
            && original.sp == native.sp,
        "{label}: ram {:08x?} ({} bytes; {bytes:?}) scratch {:x?} gte equal {} sp {:x}/{:x} fault {fo:?}",
        &ram[..ram.len().min(16)],
        ram.len(),
        &scratch[..scratch.len().min(16)],
        original.gte == native.gte,
        original.sp,
        native.sp
    );
    if !void {
        assert_eq!(nv0, v0, "{label}: v0");
    }
    (0..0x20_0000usize)
        .filter(|&i| !dead(i) && base.ram()[i] != original.ram()[i])
        .count()
}

#[test]
#[ignore = "requires AC3_JP_DIR (the JP Redux captures)"]
fn part3_oracle() {
    let _fallback = Interpreted::install();
    let extents = extents();
    let void = void_set();
    // ONLY=addr[,addr..] (hex) limits the run to those routines
    let only: Option<Vec<u32>> = std::env::var("ONLY").ok().map(|s| {
        s.split(',')
            .filter_map(|a| u32::from_str_radix(a.trim().trim_start_matches("0x"), 16).ok())
            .collect()
    });
    let mut total = 0;
    let mut failures = Vec::new();
    let hook = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));
    for r in ROUTINES
        .iter()
        .filter(|r| only.as_ref().is_none_or(|o| o.contains(&r.address)))
    {
        let address = r.address;
        let end = extents[&address];
        let result = panic::catch_unwind(AssertUnwindSafe(|| {
            oracle_routine(r, end, void.contains(&address))
        }));
        let (cases, changed, covered) = match result {
            Ok(done) => done,
            Err(e) => {
                let text = panic_text(&*e);
                println!("oracle {address:08x} {}: FAIL {text}", r.name);
                failures.push(format!("{address:08x} {}: {text}", r.name));
                continue;
            }
        };
        let size = (end - address) / 4;
        println!(
            "oracle {address:08x} {}: {cases} cases, {changed} RAM bytes changed by the original, \
             {}/{size} instructions executed; not executed: {}",
            r.name,
            covered.len(),
            gaps(address, end, &covered)
        );
        total += cases;
    }
    panic::set_hook(hook);
    assert!(failures.is_empty(), "{failures:#?}");
    assert!(total > 0);
}

// ---- variants -----------------------------------------------------------------

/// The CD driver block `[gp+0x38]` and monitor `[gp+0x34]`.
fn cd_blocks(m: &mut Machine) -> (u32, u32) {
    (m.lw(GP + 0x38), m.lw(GP + 0x34))
}

/// 8002A79C: record kind/length/shift/channel, flags, monitor state.
fn mutate_start_read(m: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], _: u32) {
    let (_, monitor) = cd_blocks(m);
    let record = a[0];
    let length = if rng.next().is_multiple_of(6) {
        0
    } else {
        rng.next() & 0x7_ffff
    };
    let word = length
        | ((rng.next() % 8) << 24)
        | ((rng.next() % 32) << 19)
        | ((rng.next() % 4) << 27)
        | if rng.next().is_multiple_of(5) {
            0
        } else {
            0x8000_0000
        };
    m.sw(record, word);
    if rng.next().is_multiple_of(8) {
        m.sw(monitor, 1);
    }
    let flags = [0, 0x800, 0x1000, 0x2000, 0x4000, 0x3800, 0x4800, 0x6000][rng.next() as usize % 8];
    let sp = m.sp;
    m.sw(sp + 0x10, flags);
    a[2] = [0x8000, 0x10000, 0x4800, 0x123][rng.next() as usize % 4];
}

/// 80029BD8: driver flags, last sector, channel.
fn mutate_stream_sector(m: &mut Machine, rng: &mut oracle::Rng, _: &mut [u32; 4], _: u32) {
    let (d, _) = cd_blocks(m);
    let flags = [
        0x8000_1000,
        0x8000_5000,
        0x8000_2000,
        0x8000_0400,
        0x8000_0010,
        0x8000_3410,
    ][rng.next() as usize % 6];
    m.sw(d, flags);
    m.sw(d + 0x2c, pick(rng));
    m.sb(d + 0x1d, (rng.next() % 3) as u8);
    // the sector header CdGetSector leaves in the frame (sp - 0x28 + 0x10):
    // the drive writes nothing, so the stale stack bytes are the header
    let header = m.sp - 0x28 + 0x10;
    for i in 0..12 {
        m.sb(header + i, rng.next() as u8);
    }
    m.sb(header + 5, (rng.next() % 3) as u8);
    let submode = m.lbu(header + 6);
    m.sb(
        header + 6,
        if rng.next().is_multiple_of(4) {
            submode & !8
        } else {
            submode | 8
        },
    );
}

/// 8002A498: driver flags (arrival, pause, request), times, counts.
fn mutate_stream_service(m: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], _: u32) {
    let (d, monitor) = cd_blocks(m);
    let bits = [0x0100_0000u32, 0x4000_0000, 0x40, 0x10, 0x8000_0000];
    let mut flags = 0;
    for b in bits {
        if rng.next().is_multiple_of(2) {
            flags |= b;
        }
    }
    m.sw(d, flags);
    for off in [0x10, 0x14, 0x24, 0x30, 0x3c, 0x44] {
        if rng.next().is_multiple_of(2) {
            m.sw(d + off, pick(rng));
        }
    }
    m.sb(monitor + 0x28, (rng.next() % 4) as u8);
    a[0] = [2, 1, 0, 5][rng.next() as usize % 4];
}

/// A random choice from `values`.
fn choose<T: Copy>(rng: &mut oracle::Rng, values: &[T]) -> T {
    values[rng.next() as usize % values.len()]
}

/// The game context `[gp+0x2EC]`.
fn context(m: &mut Machine) -> u32 {
    m.lw(GP + 0x2ec)
}

/// 80028C24 (`pad`, `input`, `config`): pad type and mode, events, buttons,
/// analog values, the configuration's repeat and curve flags, the input
/// block's counters and previous buttons, the pause flag.
fn mutate_map_pad_input(m: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], _: u32) {
    let [pad, input, config, _] = *a;
    m.sb(
        pad + 0xc,
        choose(rng, &[0x41, 0x43, 0x53, 0x73, 0x23, 0, 0x12]),
    );
    m.sh(pad + 0xa, choose(rng, &[0, 7, 1, 4]));
    m.sb(pad + 0xd, (rng.next() % 16) as u8);
    m.sh(pad + 0xe, rng.next() as u16);
    m.sb(pad + 1, (rng.next() % 4) as u8);
    for i in 0..4 {
        m.sh(pad + 0x12 + i * 2, (rng.next() % 0x100) as u16);
    }
    for off in [0, 0x5c, 0xb8, 0x114, 0x170] {
        let table = config + off;
        m.sw(table + 0x58, choose(rng, &[0, 8, 0x10, 0x18]));
        m.sh(table + 0x50, (rng.next() % 0x20) as u16);
    }
    for off in [0x38, 0x39, 0x3a, 0x3b] {
        m.sb(
            input + off,
            choose(rng, &[0, 1, 5, 6, 7, 9, 10, 0x16, 0x17, 0x18, 0x1c]),
        );
    }
    m.sw(input + 8, rng.next() ^ (rng.next() << 8));
    m.sw(input, rng.next() % 4);
    let c = context(m);
    let word = m.lw(c);
    let paused = rng.next().is_multiple_of(4);
    m.sw(
        c,
        if paused {
            word | 0x2000_0000
        } else {
            word & !0x2000_0000
        },
    );
}

/// 8002E1F4: the current CD file, the context volume and mono bytes, the
/// pause flag, and the 24 voice records.
fn mutate_update_sound(m: &mut Machine, rng: &mut oracle::Rng, _: &mut [u32; 4], _: u32) {
    let queue = m.lw(GP + 0x48);
    let file = choose(
        rng,
        &[
            0x1e6,
            0x23,
            0x46,
            0x22,
            0x47,
            0x2a,
            0x44,
            0x9c9,
            u32::MAX,
            0x801e6,
        ],
    );
    m.sw(queue + 0x174, file);
    let c = context(m);
    for off in [0x398, 0x399] {
        m.sb(c + off, rng.next() as u8);
    }
    m.sb(c + 0x39a, (rng.next() % 2) as u8);
    let word = m.lw(c);
    let paused = rng.next().is_multiple_of(3);
    m.sw(
        c,
        if paused {
            word | 0x2000_0000
        } else {
            word & !0x2000_0000
        },
    );
    for v in 0..24 {
        let voice = 0x800c_5830 + v * 0x14;
        if rng.next().is_multiple_of(3) {
            m.sb(voice + 1, (rng.next() % 0x20) as u8);
            m.sb(voice + 2, (rng.next() % 3) as u8);
            m.sb(voice + 3, (rng.next() % 2) as u8);
            m.sb(voice + 0x10, rng.next() as u8);
            m.sb(voice + 0x11, choose(rng, &[0x80, 0, 0x40, 0xff]));
            m.sh(voice + 0x12, choose(rng, &[0, 1, 5, 0xffff]));
        }
    }
}

/// 8002AC3C (`mode`).
fn mutate_cd_stop(_: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], _: u32) {
    a[0] = rng.next() % 4;
}

/// 8002E0D0 / 8002DF98 (`index`): the voice, its sound, volumes, fade, the
/// context's effects volume and mono flag.
fn mutate_voice(m: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], _: u32) {
    let index = rng.next() % 24;
    a[0] = index;
    let voice = 0x800c_5830 + index * 0x14;
    m.sb(voice, (rng.next() % 0x40) as u8);
    m.sh(voice + 4, rng.next() as u16);
    m.sh(voice + 6, rng.next() as u16);
    m.sb(voice + 0x11, choose(rng, &[0x80, 0, 0x40, 0xff]));
    let c = context(m);
    m.sb(c + 0x398, rng.next() as u8);
    m.sb(c + 0x39a, (rng.next() % 2) as u8);
}

/// 8002D3F4: the overlay block mode, the context and gp switches, the last
/// stream word.
fn mutate_overlay_2cfcc(m: &mut Machine, rng: &mut oracle::Rng, _: &mut [u32; 4], _: u32) {
    let block = m.lw(GP + 0x314);
    m.sw(block, rng.next() % 12);
    let c = context(m);
    m.sb(c + 0x39c, (rng.next() % 3) as u8);
    m.sb(GP + 0x318, (rng.next() % 3) as u8);
    let word = rng.next();
    m.sw(GP + 0x31c, choose(rng, &[0, u32::MAX, word]));
}

/// The instrument panel drawers (`panel`, `ot`, `block`): heading,
/// altitude, damage, counts, timer mode and ticks, colors, flags, the
/// blink byte and the buffer.
fn mutate_panel(m: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], _: u32) {
    let panel = a[0];
    m.sw(panel, choose(rng, &[0x7f, 0x5f, 0xff, 0x7f | 0x20]));
    let altitude = pick(rng) << (rng.next() % 12);
    m.sw(panel + 4, altitude);
    m.sw(panel + 8, rng.next() % 0x2000);
    let damage = rng.next() % 0x1200;
    m.sw(
        panel + 0xc,
        choose(rng, &[0, 0x1000, 0x1001, 0x800, 0x40, u32::MAX, damage]),
    );
    m.sw(panel + 0x10, choose(rng, &[0, 5, 9, 10, 99, 100, 150]));
    m.sw(panel + 0x14, rng.next() % 12);
    m.sw(panel + 0x18, rng.next() % 6);
    m.sh(panel + 0x24, (rng.next() % 5) as u16);
    m.sh(panel + 0x26, (rng.next() % 3) as u16);
    let ticks = rng.next() % 0x20000;
    m.sw(
        panel + 0x28,
        choose(rng, &[0, 29, 30, 1799, 1800, 1801, 0x1234, ticks]),
    );
    for i in 0..7 {
        m.sb(panel + 0x1c + i, rng.next() as u8);
    }
    if rng.next().is_multiple_of(3) {
        m.sb(panel + 0x20, 0);
    }
    m.sb(0x800c_6d6f, (rng.next() % 2) as u8);
    let c = context(m);
    m.sw(c + 4, rng.next() % 2);
}

/// 80084A6C: the context's mode word, mission and difficulty bits.
fn mutate_effect_tables(m: &mut Machine, rng: &mut oracle::Rng, _: &mut [u32; 4], _: u32) {
    let c = context(m);
    m.sw(c + 0x96c, choose(rng, &[0, 0x10, 0x30]));
    let mission = choose(
        rng,
        &[0xd, 0x2a, 0x38, 0x20, 0x2f, 0x35, 0x37, 0, 1, 0x13, 0x22],
    );
    m.sh(c + 0x38c, mission);
    let bits = choose(rng, &[0x1008, 0x4008, 0x8008, 0x2008, 0, 0xf008, 0x5008]);
    m.sh(c + 0x5e, bits);
}

/// 8006CE70 (`entity`): the aircraft id, the difficulty word and the mode
/// byte.
fn mutate_player_flight(m: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], _: u32) {
    m.sh(a[0] + 0x48, (rng.next() % 32) as u16);
    let c = context(m);
    m.sw(c + 0x394, rng.next() % 4);
    m.sb(c + 0x3aa, (rng.next() % 12) as u8);
}

/// 8006CD6C (`aircraft`).
fn mutate_aircraft(_: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], _: u32) {
    let any = rng.next() % 32;
    a[0] = choose(rng, &[0, 1, 0x1e, 0x1f, 0x2a, u32::MAX, any]);
}

/// Free RAM the variants place crafted records in (unused by the flight
/// engine: below the deepest flight stack frame, above the arena).
const SCRATCH_RAM: u32 = 0x801f_8000;

/// 8002ABE0 (`pause`): the stream flags.
fn mutate_stream_pause(m: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], _: u32) {
    let (d, _) = cd_blocks(m);
    let flags = choose(
        rng,
        &[0, 0x8000_0000, 0xc000_0000, 0x4000_0000, 0x8000_0040],
    );
    m.sw(d, flags | (rng.next() & 0x3f));
    a[0] = rng.next() % 3;
}

/// 8002C41C (`entry`): an entry in free RAM with a pause word, the stream
/// flags.
fn mutate_cd_pause(m: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], v: u32) {
    a[0] = SCRATCH_RAM;
    m.sw(SCRATCH_RAM + 4, rng.next() % 3);
    mutate_stream_pause(m, rng, &mut [0; 4], v);
}

/// 80034360 (`x`, `y`, `slot`, `prim`; `uv`, `grey` on the stack).
fn mutate_sprite_34360(m: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], _: u32) {
    a[0] = rng.next() % 0x200;
    a[1] = rng.next() % 0x100;
    let sp = m.sp;
    m.sw(sp + 0x14, rng.next() % 0x100);
    m.sw(a[2], rng.next() ^ (rng.next() << 8));
}

/// 80036130: the mission index byte.
fn mutate_mission_index(m: &mut Machine, rng: &mut oracle::Rng, _: &mut [u32; 4], _: u32) {
    let c = context(m);
    m.sb(c + 0x3a6, rng.next() as u8);
}

/// 80038078 (`row`, `column`).
fn mutate_select(_: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], _: u32) {
    a[0] = rng.next() % 40;
    a[1] = rng.next() % 4;
}

/// 80040900 (`label`, `x`, `y`): the label and the sprite count.
fn mutate_label(m: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], _: u32) {
    a[0] = rng.next() % 64;
    a[1] = rng.next() % 0x140;
    a[2] = rng.next() % 0xf0;
    let set = m.lw(GP + 0x444);
    m.sw(set + 0x10, rng.next() % 60);
}

/// 80048A40 (passed through to 80054DBC).
fn mutate_context_48a40(_: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], _: u32) {
    for arg in a.iter_mut() {
        *arg = rng.next() % 8;
    }
}

/// 8004D124 (`c`).
fn mutate_character(_: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], _: u32) {
    a[0] = if rng.next().is_multiple_of(8) {
        rng.next()
    } else {
        rng.next() % 0x80
    };
}

/// 800565E8 (`message`): the mission.
fn mutate_start_message(m: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], _: u32) {
    a[0] = choose(rng, &[0xff, 0, 1, 2, 3]);
    let c = context(m);
    m.sh(c + 0x38c, (rng.next() % 52) as u16);
}

/// 80056634: the message state, the context switch, the gameplay mode.
fn mutate_message_state(m: &mut Machine, rng: &mut oracle::Rng, _: &mut [u32; 4], _: u32) {
    m.sw(GP + 0x4a8, rng.next() % 4);
    let c = context(m);
    m.sb(c + 0x39c, (rng.next() % 3) as u8);
    let gameplay = m.lw(GP + 0x2e0);
    m.sw(gameplay + 4, choose(rng, &[4, 2, 0x10]));
}

/// 8005671C (`script`).
fn mutate_reset_script(_: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], _: u32) {
    let any = rng.next();
    a[0] = choose(rng, &[0, SCRATCH_RAM, any]);
}

/// 80056778: a crafted radio script in free RAM (entries of every kind,
/// times around the counter), or none, and the block's channel.
fn mutate_step_script(m: &mut Machine, rng: &mut oracle::Rng, _: &mut [u32; 4], _: u32) {
    let block = m.lw(GP + 0x4bc);
    let time = rng.next() % 4;
    m.sh(block + 4, time as u16);
    m.sb(block + 7, choose(rng, &[0xff, 0, 3]));
    if rng.next().is_multiple_of(5) {
        m.sw(block, 0);
        return;
    }
    m.sw(block, SCRATCH_RAM);
    let mut t = time;
    for i in 0..6 {
        let entry = SCRATCH_RAM + i * 4;
        if rng.next().is_multiple_of(3) {
            t += rng.next() % 2;
        }
        let at = if i == 5 || rng.next().is_multiple_of(9) {
            0xffff
        } else {
            t as u16
        };
        m.sh(entry, at);
        m.sb(entry + 2, choose(rng, &[0, 1, 2, 3, 4, 7]));
        m.sb(entry + 3, (rng.next() % 0x40) as u8);
    }
    let c = context(m);
    m.sb(c + 0x39c, (rng.next() % 2) as u8);
}

/// 80056A54 (`mission`, `message`).
fn mutate_queue_message(_: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], _: u32) {
    a[0] = rng.next() % 52;
    a[1] = choose(rng, &[0xff, 0, 1, 2, 5]);
}

/// 80056AC8 (`rect`): a RECT in free RAM, the block's buffer byte.
fn mutate_rect(m: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], _: u32) {
    a[0] = SCRATCH_RAM;
    let block = m.lw(GP + 0x4d4);
    m.sb(block + 2, (rng.next() % 3) as u8);
}

/// 8005EBB8 (`far`, `patches`, `count`): the patch count and indices.
fn mutate_palettes(m: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], _: u32) {
    a[2] = rng.next() % 6;
    a[1] = SCRATCH_RAM;
    for i in 0..6 {
        m.sh(SCRATCH_RAM + i * 0xc, (rng.next() % 0x400) as u16);
    }
}

/// 800476F0 (`index`): the entity's position and velocity around the area
/// box, the mission.
fn mutate_area_status(m: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], _: u32) {
    let index = rng.next() % 8;
    a[0] = index;
    let table = m.lw(GP + 0x2e8);
    let entity = m.lw(table + index * 4);
    if !(0x8001_0000..0x8020_0000).contains(&entity) {
        return;
    }
    let area = m.lw(GP + 0x33c);
    for (to, from) in [(8, 0x10u32), (0x10, 0x18)] {
        let edge = m.lw(area + from);
        let span = m.lw(area + from + 4).wrapping_sub(edge);
        let at = match rng.next() % 4 {
            0 => edge.wrapping_sub(1),
            1 => edge.wrapping_add(span).wrapping_add(1),
            _ => edge.wrapping_add(span / 2),
        };
        m.sw(entity + to, at);
    }
    for v in [0x38, 0x3c, 0x40] {
        let speed = (rng.next() % 0x4_0000).wrapping_sub(0x2_0000);
        m.sw(entity + v, speed);
    }
    m.sw(
        entity + 0xc,
        choose(
            rng,
            &[0, 0x03a9_8000, 0xf15a_0000, 0xffd0_0000, 0x0400_0000],
        ),
    );
    let c = context(m);
    m.sh(c + 0x38c, choose(rng, &[0x2a, 0x3a, 0, 0x2b]));
}

/// 80049438: the cue record's state, timer, channel and the gameplay flag.
fn mutate_radio_cue(m: &mut Machine, rng: &mut oracle::Rng, _: &mut [u32; 4], _: u32) {
    const CUE: u32 = 0x800c_7190;
    m.sh(CUE + 4, choose(rng, &[1, 2, 0, 3]));
    m.sh(CUE + 6, choose(rng, &[0, 0xc, 0xffff, 5, 0x8000]));
    m.sb(CUE + 8, choose(rng, &[0xff, 0, 2]));
    m.sb(CUE + 9, (rng.next() % 0x40) as u8);
    m.sb(CUE + 0xa, (rng.next() % 8) as u8);
    let gameplay = m.lw(GP + 0x2e0);
    let flags = m.lw(gameplay + 4);
    m.sw(gameplay + 4, flags ^ (rng.next() % 2 * 4));
}

/// 80085204 (`ot`, `primitives`): the sky record's sun, moon, cloud and
/// ambient switches, the camera mode, the context switches.
fn mutate_object_systems(m: &mut Machine, rng: &mut oracle::Rng, _: &mut [u32; 4], _: u32) {
    let gameplay = m.lw(GP + 0x2e0);
    let sky = m.lw(gameplay + 0x244);
    if rng.next().is_multiple_of(3) {
        m.sh(sky + 0x6c, 0);
    }
    if rng.next().is_multiple_of(3) {
        m.sw(sky + 0x70, 0);
    }
    m.sb(sky + 0x6e, (rng.next() % 2) as u8);
    m.sb(sky + 0x6f, (rng.next() % 7) as u8);
    let camera = m.lw(gameplay + 0x28);
    m.sw(
        camera + 0x50,
        choose(rng, &[0, 2, 0x200, 0x10, 0x870, 0x202]),
    );
    let c = context(m);
    m.sh(c + 0x38e, choose(rng, &[0x19, 0, 3]));
    m.sb(c + 0x3a5, (rng.next() % 2) as u8);
    let word = m.lw(c);
    m.sw(c, word ^ (rng.next() % 2 * 0x2000_0000));
}

/// 8008D9F4: the layer words (height, count, drift), the colour word
/// (clut row bit), the eye position.
fn mutate_cloud_layers(m: &mut Machine, rng: &mut oracle::Rng, _: &mut [u32; 4], _: u32) {
    let gameplay = m.lw(GP + 0x2e0);
    let layers = m.lw(gameplay + 0x268) + 8;
    for i in 0..2 {
        let height = (rng.next() % 0x4000).wrapping_sub(0x2000) & 0xffff;
        let count = rng.next() % 0x90;
        let drift = rng.next() % 0x80;
        m.sw(layers + i * 4, height | (count << 16) | (drift << 25));
    }
    let colour = m.lw(layers + 0xc) & 0x00ff_ffff;
    let row = if rng.next().is_multiple_of(2) {
        0
    } else {
        0x0100_0000
    };
    let colour = if rng.next().is_multiple_of(6) {
        row
    } else {
        colour | row
    };
    m.sw(layers + 0xc, colour);
    for off in [8, 0xc, 0x10] {
        let shift = (rng.next() % 0x2_0000) << 8;
        let eye = m.lw(0x1f80_0000 + off).wrapping_add(shift);
        m.sw(0x1f80_0000 + off, eye);
    }
}

/// 8004925C (`side`, `unit`, `kind`).
fn mutate_unit_flag(_: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], _: u32) {
    a[0] = choose(rng, &[1, 4, 0, 2]);
    a[1] = choose(
        rng,
        &[
            0x1e, 0x1f, 0x20, 0x21, 0x26, 0x27, 0x2f, 0x30, 0x35, 0x3a, 0x3b,
        ],
    );
    a[2] = rng.next() % 11;
}

/// The gameplay block `[gp+0x2E0]`.
fn gameplay(m: &mut Machine) -> u32 {
    m.lw(GP + 0x2e0)
}

/// The sky work block of 800957A4 (scratchpad +0x1C) and its base.
const SKY_BASE: u32 = 0x1f80_0000;
const SKY: u32 = SKY_BASE + 0x1c;

/// 8003346C (dump): the world-pass flags (first view 0x100, second view 8,
/// depth masks 4, fog 0x200), the sky type, the palette animation count.
fn mutate_render_world(m: &mut Machine, rng: &mut oracle::Rng, _: &mut [u32; 4], _: u32) {
    let g = gameplay(m);
    let flags = m.lw(g);
    m.sw(
        g,
        flags ^ choose(rng, &[0, 0x100, 8, 4, 0x200, 0x30c, 0x108]),
    );
    let params = m.lw(g + 0x244);
    let kind = m.lhu(params + 2);
    m.sh(params + 2, (kind & !7) | (rng.next() % 8) as u16);
    if rng.next().is_multiple_of(3) {
        m.sw(params + 0x74, 0);
    }
}

/// 800943A8 (`ot`, `packets`, `rect`, `offset`): the mask flag, the draw
/// area and the drawing offset.
fn mutate_fog(m: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], _: u32) {
    let flags = m.lw(GP + 0x41c);
    let word = m.lw(flags + 0x5c8);
    m.sw(flags + 0x5c8, word ^ (rng.next() % 2 * 4));
    if rng.next().is_multiple_of(2) {
        let x = rng.next() % 0x400;
        let y = rng.next() % 0x200;
        m.sw(a[2], x | (y << 16));
        m.sw(
            a[2] + 4,
            (rng.next() % 0x180) | ((rng.next() % 0x110) << 16),
        );
        m.sw(a[3], pick(rng));
    }
}

/// 800957A4 (`ot`, `packets`): the mission, the sun switch, the cloud and
/// mask flags, the camera height and pitch.
fn mutate_draw_sky(m: &mut Machine, rng: &mut oracle::Rng, _: &mut [u32; 4], _: u32) {
    let c = context(m);
    let any = (rng.next() % 0x40) as u16;
    m.sh(c + 0x38c, choose(rng, &[6, 0, 0x19, 0x2a, any]));
    let g = gameplay(m);
    let params = m.lw(g + 0x244);
    let kind = m.lhu(params + 2);
    m.sh(
        params + 2,
        kind ^ (choose(rng, &[0, 0x200, 0x400, 0x100]) as u16),
    );
    let flags = m.lw(GP + 0x41c);
    m.sw(flags + 0x5c8, rng.next() % 8);
    mutate_camera(m, rng);
}

/// The camera height and angles of `[[gp+0x2E0]+0x28]`.
fn mutate_camera(m: &mut Machine, rng: &mut oracle::Rng) {
    let g = gameplay(m);
    let camera = m.lw(g + 0x28);
    let y = m.lw(camera + 8);
    m.sw(
        camera + 8,
        y.wrapping_add((rng.next() % 0x0400_0000).wrapping_sub(0x0200_0000)),
    );
    for off in [0x14, 0x16, 0x18] {
        if rng.next().is_multiple_of(2) {
            m.sh(camera + off, rng.next() as u16);
        }
    }
    m.sh(camera + 0x28, (rng.next() % 0x8000) as u16);
    m.sh(camera + 0x2a, (rng.next() % 0x8000) as u16);
}

/// 80094718 (`base`, `sky`): the camera, the cloud flags.
fn mutate_sky_camera(m: &mut Machine, rng: &mut oracle::Rng, _: &mut [u32; 4], _: u32) {
    mutate_camera(m, rng);
    let flags = m.lw(GP + 0x41c);
    m.sw(flags + 0x5c8, rng.next() % 8);
    let g = gameplay(m);
    let params = m.lw(g + 0x244);
    let kind = m.lhu(params + 2);
    m.sh(params + 2, kind ^ (rng.next() % 2 * 0x400) as u16);
}

/// 80094D10 / 80094AC8 (`base`, `sky`): the band heights and colours of the
/// mission parameters, the camera height, pitch and distance, the indoor
/// and cloud flags.
fn mutate_sky_params(m: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], _: u32) {
    let [base, sky, ..] = *a;
    let g = gameplay(m);
    let params = m.lw(g + 0x244);
    for off in [0x10, 0x20, 0x22, 0x3e] {
        if rng.next().is_multiple_of(2) {
            m.sh(params + off, (rng.next() % 0x4000) as u16);
        }
    }
    for off in [4, 8, 0x1c] {
        if rng.next().is_multiple_of(3) {
            m.sw(params + off, rng.next() & 0x00ff_ffff);
        }
    }
    let height = m.lw(base + 0xc);
    m.sw(
        base + 0xc,
        height.wrapping_add((rng.next() % 0x2_0000).wrapping_sub(0x1_0000)),
    );
    m.sh(base + 0x14, rng.next() as u16);
    if rng.next().is_multiple_of(8) {
        m.sh(base + 0x1a, choose(rng, &[0, 0xffff, 0x100]));
    }
    m.sw(sky + 0x170, u32::from(rng.next().is_multiple_of(4)));
    m.sw(sky + 0x16c, rng.next() % 2);
    let camera = m.lw(g + 0x28);
    m.sh(camera + 0x28, (rng.next() % 0x8000) as u16);
    m.sh(camera + 0x2a, (rng.next() % 0x8000) as u16);
    m.sw(sky + 0x168, rng.next() % 8);
}

/// 800948A0 (`sky`, `bands`): the band edge points and depths, the band
/// count.
fn mutate_project_bands(m: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], _: u32) {
    let sky = a[0];
    a[1] = choose(rng, &[4, 5, a[1]]);
    for i in 0..10 {
        if rng.next().is_multiple_of(3) {
            let p = sky + 0x2c + i * 4;
            let x = m.lhu(p);
            m.sh(
                p,
                x.wrapping_add((rng.next() % 0x800) as u16)
                    .wrapping_sub(0x400),
            );
            let y = m.lhu(p + 2);
            m.sh(
                p + 2,
                y.wrapping_add((rng.next() % 0x800) as u16)
                    .wrapping_sub(0x400),
            );
        }
    }
    for i in 0..5 {
        if rng.next().is_multiple_of(3) {
            m.sw(sky + 0x7c + i * 4, rng.next() % 0x8000);
        }
    }
    m.sh(sky + 0x28, 0);
}

/// 80095074 / 80096184 (`base`, `sky`): the visible-band bits, the
/// background switch, the star and cloud switches.
fn mutate_draw_bands(m: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], _: u32) {
    let sky = a[1];
    m.sh(sky + 0x28, (rng.next() % 0x20) as u16);
    m.sb(sky + 0x10b, (rng.next() % 2) as u8);
    let g = gameplay(m);
    let params = m.lw(g + 0x244);
    let kind = m.lhu(params + 2);
    m.sh(
        params + 2,
        kind ^ (choose(rng, &[0, 0x100, 0x400, 0x500]) as u16),
    );
    m.sw(sky + 0x16c, rng.next() % 2);
}

/// 80095CB8 (`base`, `sky`, `count`): the star count, the camera drift.
fn mutate_stars(m: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], _: u32) {
    let any = rng.next() % 0x101;
    a[2] = choose(rng, &[0, 1, 7, 8, 0x40, 0x100, any]);
    let base = a[0];
    for off in [8, 0xc, 0x10] {
        let v = m.lw(base + off);
        m.sw(
            base + off,
            v.wrapping_add((rng.next() % 0x1000).wrapping_sub(0x800)),
        );
    }
    m.sh(base + 0x14, rng.next() as u16);
    m.sh(base + 0x16, rng.next() as u16);
}

/// 80095930 (`base`, `sky`): the sun word (height, wide bit), the colour
/// word's clut-row byte, the camera height, heading and projection
/// distance.
fn mutate_sun(m: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], _: u32) {
    let base = a[0];
    let g = gameplay(m);
    let sun = m.lw(g + 0x268);
    let word = m.lw(sun + 4);
    let height = (rng.next() % 0x3_0000).wrapping_sub(0x1_0000) << 16;
    m.sw(
        sun + 4,
        if rng.next().is_multiple_of(2) {
            word ^ 1
        } else {
            (word & 0xffff) | height
        },
    );
    let color = m.lw(sun + 0x18);
    m.sw(
        sun + 0x18,
        (color & 0x00ff_ffff) | (choose(rng, &[0, 1, 0x80, 0xff]) << 24),
    );
    m.sh(base + 0x16, rng.next() as u16);
    if rng.next().is_multiple_of(4) {
        m.sh(base + 0x1a, (rng.next() % 0x300) as u16);
    }
    let altitude = m.lw(base + 0xc);
    m.sw(
        base + 0xc,
        altitude.wrapping_add((rng.next() % 0x2000).wrapping_sub(0x1000)),
    );
}

/// 80061FA8 (`quad`, `work`, `level`, `bits`): the subdivision level, the
/// fourth vertex's stored projection, the screen offset (moving every
/// projected y across the 240-line limits), the split diagonal.
fn mutate_subdivide(m: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], _: u32) {
    let quad = a[0];
    a[2] = rng.next() % 5;
    if rng.next().is_multiple_of(2) {
        let y = choose(
            rng,
            &[0x77, 0x78, 0x79, 0x80, 0x81, 0xff88, 0xff87, 0xff80, 0xff7f],
        );
        m.sh(quad + 0x3e, y);
        m.sh(quad + 0x3c, (rng.next() % 0x400).wrapping_sub(0x200) as u16);
    }
    if rng.next().is_multiple_of(2) {
        // OFY (control register 25), 16.16
        m.gte.control[25] = (rng.next() % 0x180).wrapping_sub(0xc0) << 16;
    }
    if rng.next().is_multiple_of(4) {
        a[3] = rng.next() % 2;
    }
}

/// 80096184 (dump): the cloud-band draw on the dump's sky block.
fn mutate_cloud_bands(m: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], v: u32) {
    *a = [SKY_BASE, SKY, 0, 0];
    let g = gameplay(m);
    let packets = m.lw(g + 0x14);
    m.sw(SKY_BASE + 4, packets);
    mutate_draw_bands(m, rng, a, v);
}

/// 800965D4 (dump): a screen point around the limits.
fn mutate_outcode(m: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], _: u32) {
    let point = SCRATCH_RAM;
    let (any_x, any_y) = (rng.next() as u16, rng.next() as u16);
    let x = choose(rng, &[0xff5f, 0xff60, 0xa0, 0xa1, 0, any_x]);
    let y = choose(
        rng,
        &[0xff87, 0xff88, 0x78, 0x79, 0x80, 0x81, 0xff80, 0, any_y],
    );
    m.sh(point, x);
    m.sh(point + 2, y);
    *a = [point, 0, 0, 0];
}

/// 80097EB4 / 80098044 (dump; `ot`, `packets`): the frame's OT end and free
/// primitive space, the clock and the camera.
fn mutate_sky_kind(m: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], _: u32) {
    let g = gameplay(m);
    let ot = m.lw(g + 0x10).wrapping_add(0x3ffc);
    let packets = m.lw(g + 0x14);
    *a = [ot, packets, 0, 0];
    let c = context(m);
    m.sw(c + 0x28, rng.next() % 0x10_0000);
    mutate_camera(m, rng);
    let flags = m.lw(GP + 0x41c);
    m.sw(flags + 0x5c8, rng.next() % 8);
}

/// 8008FAA8 (dump; `ot`, `packets`, `environment`, `brightness`).
fn mutate_fade(m: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], _: u32) {
    let g = gameplay(m);
    let ot = m.lw(g + 0x10).wrapping_add(4 * (rng.next() % 0x1000));
    let packets = m.lw(g + 0x14);
    let any = rng.next() % 0x1100;
    let brightness = choose(rng, &[0, 0x1000, 0x800, 0x1400, any]);
    *a = [ot, packets, rng.next() % 2, brightness];
    let c = context(m);
    m.sw(c + 4, rng.next() % 2);
}

/// 80098714 (dump; `block`, `out`, `row`): a night-ground block in the
/// scratch RAM (cells, camera), the output at the free primitive space.
fn mutate_ground_row(m: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], _: u32) {
    let block = SCRATCH_RAM;
    for off in (0..0x40).step_by(4) {
        m.sw(block + off, pick(rng));
    }
    for off in [0x20, 0x24, 0x28] {
        m.sw(block + off, (rng.next() % 0x4000).wrapping_sub(0x2000));
    }
    let g = gameplay(m);
    let out = m.lw(g + 0x14);
    *a = [block, out, rng.next() % 0x10, 0];
}

/// 800939D4 (dump; `ot`, `packets`, `value`): the bar value, the display
/// buffer.
fn mutate_letterbox(m: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], _: u32) {
    let g = gameplay(m);
    let ot = m.lw(g + 0x10);
    let packets = m.lw(g + 0x14);
    let any = rng.next() % 0x1000;
    let value = choose(rng, &[0, 1, 4, 0xfff, 0x1000, 0x2000, 0xffff_ffff, any]);
    *a = [ot, packets, value, 0];
    let c = context(m);
    m.sw(c + 4, rng.next() % 2);
}

/// The JP mission halfword at context +0x38C.
fn set_mission(m: &mut Machine, mission: u16) {
    let c = context(m);
    m.sh(c + 0x38c, mission);
}

/// `gp+0x354`: the lock cursor (+0 state, +1 owner slot, +2 size, +4/+6 x/y).
const CURSOR: u32 = GP + 0x354;
/// 800C6FD0 / 800C67A0: per-slot event flags and screen records.
const EVENTS: u32 = 0x800c_6fd0;
const SCREENS: u32 = 0x800c_67a0;

/// 8003B34C: the mission (6: no sighting delay), the HUD bit, the frames
/// on screen, the screen offset (projected y across 0..0xF0).
fn mutate_screen_targets(m: &mut Machine, rng: &mut oracle::Rng, _: &mut [u32; 4], _: u32) {
    let any = (rng.next() % 0x40) as u16;
    set_mission(m, choose(rng, &[6, 0, any]));
    let g = gameplay(m);
    let flags = m.lw(g + 4);
    m.sw(g + 4, flags ^ (rng.next() % 2 * 4));
    if rng.next().is_multiple_of(2) {
        m.gte.control[25] = (rng.next() % 0x100).wrapping_sub(0x80) << 16;
    }
    for slot in 2..64 {
        let target = m.lw(0x800c_6ed0 + slot * 4);
        if target != 0 && rng.next().is_multiple_of(3) {
            m.sw(target + 0x24, 0x58 + rng.next() % 4);
        }
    }
}

/// 8003EF40: the weapon modes of context +0x3AA/+0x3AB.
fn mutate_lockon(m: &mut Machine, rng: &mut oracle::Rng, _: &mut [u32; 4], _: u32) {
    let c = context(m);
    for off in [0x3aa, 0x3ab] {
        m.sb(c + off, choose(rng, &[0, 1, 4, 5, 6, 7, 9, 10]));
    }
}

/// 80040A24: the cursor state, position and size against the owner's
/// marker, the owner's event bits (1, 0x800000), the mission and the
/// owner's entity flag 0x100.
fn mutate_lock_cursor(m: &mut Machine, rng: &mut oracle::Rng, _: &mut [u32; 4], _: u32) {
    let owners: Vec<u32> = (2..64)
        .filter(|&s| m.lw(SCREENS + s * 4) != 0 && m.lw(0x800c_6ed0 + s * 4) != 0)
        .collect();
    if !owners.is_empty() && !rng.next().is_multiple_of(4) {
        let owner = owners[rng.next() as usize % owners.len()];
        m.sb(CURSOR + 1, owner as u8);
        let record = m.lw(SCREENS + owner * 4);
        m.sb(record, choose(rng, &[1, 2, 3, 0]));
        let target = m.lw(0x800c_6ed0 + owner * 4);
        let word = m.lw(target);
        m.sw(
            target,
            word | if rng.next().is_multiple_of(6) { 0 } else { 8 },
        );
    }
    let owner = u32::from(m.lbu(CURSOR + 1));
    let record = m.lw(SCREENS + owner * 4);
    m.sb(CURSOR, choose(rng, &[1, 1, 2, 0]));
    if record != 0 && rng.next().is_multiple_of(2) {
        for (at, from) in [(4, 8), (6, 0xa), (2, 6)] {
            let v = m.lhu(record + from);
            let near = if rng.next().is_multiple_of(2) {
                0
            } else {
                (rng.next() % 9) as u16
            };
            m.sh(CURSOR + at, v.wrapping_sub(near));
        }
    }
    let event = m.lw(EVENTS + owner * 4);
    m.sw(
        EVENTS + owner * 4,
        event ^ (rng.next() % 2 * 0x0080_0000) | 1,
    );
    let any = (rng.next() % 0x40) as u16;
    set_mission(m, choose(rng, &[4, 8, 0xf, 0x1e, any]));
    let table = m.lw(GP + 0x2e8);
    let entity = m.lw(table + owner * 4);
    let flags = m.lw(entity);
    m.sw(entity, flags ^ (rng.next() % 2 * 0x100));
}

/// 8003A68C: the mission (30, 3), the first-frame words, the mission state
/// (+0x3D, +0x2D, +0x2E, +4) and gameplay +4 bit 1.
fn mutate_counters(m: &mut Machine, rng: &mut oracle::Rng, _: &mut [u32; 4], _: u32) {
    let any = (rng.next() % 0x40) as u16;
    set_mission(m, choose(rng, &[0x1e, 3, any]));
    let g = gameplay(m);
    if rng.next().is_multiple_of(2) {
        m.sw(g + 8, 0);
        m.sw(g + 0xc, 0);
    }
    let flags = m.lw(g + 4);
    m.sw(g + 4, flags ^ (rng.next() % 2 * 2));
    let state = m.lw(GP + 0x340);
    m.sb(state + 0x3d, choose(rng, &[0, 1, 3]));
    m.sb(state + 0x2d, (rng.next() % 100) as u8);
    m.sb(state + 0x2e, (rng.next() % 100) as u8);
    let any = rng.next() % 100_000;
    m.sw(state + 4, choose(rng, &[0, 7, 1234, any, 0xffff_fff6]));
    let c = context(m);
    for i in 0..16 {
        if rng.next().is_multiple_of(3) {
            m.sb(c + 0x948 + i, (0x20 + rng.next() % 0x60) as u8);
        }
    }
}

/// 80041794 (`shift`).
fn mutate_frame(_: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], _: u32) {
    a[0] = choose(rng, &[0, 0xe, 0xf, 0x40, 0xc8]);
}

/// 800414D8: the frame-linked byte.
fn mutate_link_frame(m: &mut Machine, rng: &mut oracle::Rng, _: &mut [u32; 4], _: u32) {
    m.sb(GP + 0x460, (rng.next() % 2) as u8);
}

/// 8003C5B0 (`view`): the context map switches +0x95A and +0x128 bit 0x8000.
fn mutate_gauges(m: &mut Machine, rng: &mut oracle::Rng, _: &mut [u32; 4], _: u32) {
    let c = context(m);
    m.sb(c + 0x95a, (rng.next() % 2) as u8);
    m.sb(c + 0x95e, (rng.next() % 2) as u8);
    let word = m.lw(c + 0x128);
    m.sw(c + 0x128, word ^ (rng.next() % 2 * 0x8000));
}

/// The radar range (0x800C6D7C).
fn mutate_radar_range(m: &mut Machine, rng: &mut oracle::Rng, _: &mut [u32; 4], _: u32) {
    let any = 0x2000 + rng.next() % 0x3_e000;
    m.sw(
        0x800c_6d7c,
        choose(rng, &[0x4000, 0x4001, 0x8000, 0x8001, any]),
    );
}

/// 8003DCF0: the ground objects' live mask, the range.
fn mutate_ground_blips(m: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], v: u32) {
    let block = m.lw(0x800b_d080);
    if block != 0 {
        let any = rng.next();
        m.sw(block + 0x1548, choose(rng, &[0xffff_ffff, any, 0]));
    }
    mutate_radar_range(m, rng, a, v);
}

/// 8003DE5C: the mission (0x12/0x25 add the marker), gameplay +8, the range.
fn mutate_draw_blips(m: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], v: u32) {
    let any = (rng.next() % 0x40) as u16;
    set_mission(m, choose(rng, &[0x12, 0x25, any]));
    let g = gameplay(m);
    m.sw(g + 8, choose(rng, &[0, 1, 2]));
    mutate_radar_range(m, rng, a, v);
}

/// 8003E69C: the player position (map scroll and cells).
fn mutate_map_window(m: &mut Machine, rng: &mut oracle::Rng, _: &mut [u32; 4], _: u32) {
    let table = m.lw(GP + 0x2e8);
    let player = m.lw(table);
    for off in [8, 0x10] {
        let v = m.lw(player + off);
        m.sw(
            player + off,
            v.wrapping_add((rng.next() % 0x1000_0000).wrapping_sub(0x800_0000)),
        );
    }
    m.sh(player + 0x1a, rng.next() as u16);
}

/// 8003F520: the screen records' kind, status and marker byte (+4, 0xFF
/// none), the event flags (0x40000, 0x400).
fn mutate_targets(m: &mut Machine, rng: &mut oracle::Rng, _: &mut [u32; 4], _: u32) {
    for slot in 0..64 {
        let record = m.lw(SCREENS + slot * 4);
        if record == 0 {
            continue;
        }
        m.sb(record + 4, choose(rng, &[0xff, 0xff, 0, 1, 2, 5]));
        if rng.next().is_multiple_of(4) {
            m.sb(record, choose(rng, &[0, 1, 2, 3]));
            m.sb(record + 5, choose(rng, &[0, 1, 2, 3]));
        }
        if rng.next().is_multiple_of(4) {
            let event = m.lw(EVENTS + slot * 4);
            m.sw(EVENTS + slot * 4, event ^ 0x4_0000);
        }
    }
}

/// 80065D8C: the target slot and its screen position around the centre.
fn mutate_target_arrow(m: &mut Machine, rng: &mut oracle::Rng, _: &mut [u32; 4], _: u32) {
    let live: Vec<u32> = (2..64)
        .filter(|&s| {
            let object = m.lw(0x800c_6ed0 + s * 4);
            object != 0 && m.lw(object) & 1 != 0
        })
        .collect();
    if !live.is_empty() && !rng.next().is_multiple_of(4) {
        let slot = live[rng.next() as usize % live.len()];
        m.sb(0x800c_6d6e, slot as u8);
        let object = m.lw(0x800c_6ed0 + slot * 4);
        m.sh(object + 0x18, choose(rng, &[0x400, 0x3ff, 0x800]));
        let word = m.lw(object + 0x20);
        m.sw(object + 0x20, word ^ (rng.next() % 2 * 0x2_0000));
    }
    let slot = u32::from(m.lbu(0x800c_6d6e));
    if slot != 0xff {
        let record = m.lw(SCREENS + slot * 4);
        if record != 0 {
            m.sh(record + 8, (0x80 + rng.next() % 0x40) as u16);
            m.sh(record + 0xa, (0x50 + rng.next() % 0x50) as u16);
        }
    }
}

/// 8004CEB0 (`index`): the captured banner, none (0xFF), the current one.
fn mutate_banner(m: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], _: u32) {
    let block = m.lw(GP + 0x478);
    let current = m.lw(block + 0x11c);
    let captured = a[0];
    a[0] = choose(rng, &[captured, captured, 0xff, current]);
    if captured < 0x40 && rng.next().is_multiple_of(2) {
        let entry = 0x800b_1a10 + captured * 28;
        let flags = m.lbu(entry) & !3;
        let kind = choose(rng, &[0, 1, 2]);
        m.sb(entry, flags | kind);
        if kind == 2 {
            m.sw(entry + 0x18, rng.next() % 4);
        }
    }
    if rng.next().is_multiple_of(2) {
        m.sw(block + 0x11c, 0xff);
    }
}

/// 8004D038 (`line`): its position.
fn mutate_banner_line(m: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], _: u32) {
    m.sh(a[0], (rng.next() % 0x140) as u16);
    m.sh(a[0] + 2, (rng.next() % 0xf0) as u16);
    let block = m.lw(GP + 0x478);
    m.sw(block + 0x110, rng.next() % 0x10);
}

/// 80042544 (`view`): the view warning bits, the alert flags, the mission,
/// the altitude-hold counter, the area limits.
fn mutate_warnings(m: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], _: u32) {
    let view = a[0];
    let word = m.lw(view);
    m.sw(view, (word & !0x1f) | (rng.next() % 0x20));
    m.sw(0x800c_6ecc, choose(rng, &[0, 0, 1, 2]));
    let any = (rng.next() % 0x40) as u16;
    set_mission(m, choose(rng, &[0x20, 0x2f, 0x35, 0x2a, 0x3a, 3, any]));
    m.sb(GP + 0x44c, choose(rng, &[0, 0x1c, 0x1d, 0x1e]));
    let area = m.lw(GP + 0x33c);
    if rng.next().is_multiple_of(3) {
        m.sw(area + 0xc, choose(rng, &[0, 0x100, 0x10_0000, 0x7fff_ffff]));
    }
    if rng.next().is_multiple_of(3) {
        m.sw(
            view + 0x30,
            choose(rng, &[0xffff_0000, 0x8000_0000, 0, 0x10_0000]),
        );
    }
    if rng.next().is_multiple_of(3) {
        m.sw(view + 0x1c, 0);
    }
    if rng.next().is_multiple_of(4) {
        m.sw(0x800c_7180, 0xffff_ffff);
    }
}

/// 80048C0C (`kind`, `arg`, `value`, `message`): every kind, values 0..7
/// (1 and 4 for the unit kinds), the cue flags, the enable halfword, the
/// frame counter, the mission.
fn mutate_radio_cue_48c0c(m: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], _: u32) {
    a[0] = rng.next() % 12;
    a[1] = rng.next() % 0x100;
    a[2] = choose(rng, &[0, 1, 2, 3, 4, 5, 6, 7, 1, 4]);
    a[3] = rng.next() % 0x100;
    let any = rng.next();
    m.sw(0x800c_7190, choose(rng, &[0, any, 0xffff_ffff]));
    m.sh(0x800c_7194, choose(rng, &[1, 1, 1, 0]));
    let g = gameplay(m);
    let frame = m.lw(g + 0xc);
    m.sw(g + 0xc, frame & !0xf | choose(rng, &[0, 0, 4, 8]));
    let flags = m.lw(g + 4);
    m.sw(g + 4, flags | 4);
    let any = (rng.next() % 0x40) as u16;
    set_mission(m, choose(rng, &[0x1f, 0x20, 0x26, 0x2f, any]));
    if rng.next().is_multiple_of(2) {
        for record in 0..9 {
            m.sw(0x800b_17c4 + record * 4, 0xffff_ffff);
        }
    }
}

/// 80041F88 (`slot`): the label's blink nibbles (high: the JP arrow), the
/// slot frame.
fn mutate_warning_label(m: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], _: u32) {
    let slot = a[0];
    let id = u32::from(m.lbu(slot + 1));
    if id != 0xff {
        let label = 0x800b_1254 + (id << 4);
        let kind = m.lbu(label) & 0xf;
        let kind = if rng.next().is_multiple_of(2) {
            kind
        } else {
            choose(rng, &[0, 1, 2])
        };
        m.sb(label, kind | choose(rng, &[0, 0x10, 0x20]));
        let glyph = m.lbu(label + 0xa);
        m.sb(label + 0xa, choose(rng, &[glyph, 0xff, 3]));
        m.sb(label + 1, choose(rng, &[0, 1, 2, 3]));
    }
    m.sh(slot + 2, (rng.next() % 0x40) as u16);
}

/// 8008FC9C (`ot`, `prim`, `level`).
fn mutate_fade_level(_: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], _: u32) {
    let any = rng.next() % 0x1100;
    a[2] = choose(rng, &[0xffff_ffff, 0, 0x800, 0x1000, 0x1400, any]);
}

/// 800409D4 (dump; `index`): the cursor state.
fn mutate_lock_start(m: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], _: u32) {
    a[0] = rng.next() % 64;
    m.sb(CURSOR, choose(rng, &[0, 0, 1, 2]));
}

/// 8003A0B4 (dump; `view`): a view block in the scratch RAM, built as
/// 80033814 builds it in its frame (tracking the player block), and the
/// guidance flag gameplay +4 0x800.
fn mutate_compose(m: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], _: u32) {
    let view = SCRATCH_RAM + 0x100;
    let zero = SCRATCH_RAM + 0x180;
    m.sw(zero, 0);
    m.sw(zero + 4, 0);
    let table = m.lw(GP + 0x2e8);
    let player = m.lw(table);
    let state = m.lw(0x800b_d098);
    let speed = i32::from(m.lh(player + 0x4e)) << 12;
    let scale = m.lw(state + 0x14) as i32;
    let ratio = if scale == 0 {
        0
    } else {
        speed.wrapping_div(scale)
    };
    let g = gameplay(m);
    let own = m.lw(g + 0x28);
    let words = [
        (0, m.lw(state + 0x24)),
        (4, own + 4),
        (8, own + 0x14),
        (0xc, zero),
        (0x10, own + 0xb4),
        (0x14, 0),
        (0x18, ratio.clamp(0, 0x1000) as u32),
        (0x1c, m.lw(state + 0x1c)),
        (0x20, m.lw(state + 8)),
        (0x24, (m.lw(state + 4) as i32).min(6) as u32),
        (0x28, (m.lw(state + 0x20) as i32).min(6) as u32),
        (0x2c, m.lw(state + 0x28)),
        (0x30, m.lw(state + 0x2c)),
        (0x34, own),
        (0x38, m.lw(0x800b_d0a0)),
    ];
    for (offset, word) in words {
        m.sw(view + offset, word);
    }
    let flags = m.lw(g + 4);
    m.sw(g + 4, flags ^ (rng.next() % 2 * 0x800));
    *a = [view, 0, 0, 0];
}

/// 80034930 / 80035184 (`slot`, `prim`): the selected rows and the context
/// option bytes +0x39D/+0x39E.
fn mutate_menu(m: &mut Machine, rng: &mut oracle::Rng, _: &mut [u32; 4], _: u32) {
    let g = gameplay(m);
    m.sh(g + 0x29c, (rng.next() % 5) as u16);
    m.sh(g + 0x29e, (rng.next() % 2) as u16);
    let c = context(m);
    m.sb(c + 0x39d, (rng.next() % 3) as u8);
    m.sb(c + 0x39e, (rng.next() % 2) as u8);
}

/// 8003545C (`slot`, `prim`): the clock and the frame counter around 0x5B.
fn mutate_prompt(m: &mut Machine, rng: &mut oracle::Rng, _: &mut [u32; 4], _: u32) {
    let c = context(m);
    m.sw(c + 0x28, rng.next());
    let g = gameplay(m);
    m.sw(g + 0xc, choose(rng, &[0, 0x5a, 0x5b, 0x5c, 0x6a, 0x100]));
}

/// 8003E274 (dump; `marker`): a marker in the scratch RAM near the player
/// (x/y/z words, heading +0x12), the radar range.
fn mutate_radar_marker(m: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], _: u32) {
    let marker = SCRATCH_RAM + 0x200;
    // around the player, or the camera (the near marker triangles)
    let g = gameplay(m);
    let camera = m.lw(g + 0x28);
    let table = m.lw(GP + 0x2e8);
    let player = m.lw(table);
    let (centre, spread) = if rng.next().is_multiple_of(2) {
        (player + 8, 0x0400_0000)
    } else {
        (camera + 4, 0x0080_0000)
    };
    for k in 0..3 {
        let v = m.lw(centre + k * 4);
        let offset = rng.next() % spread;
        m.sw(
            marker + k * 4,
            v.wrapping_add(offset).wrapping_sub(spread / 2),
        );
    }
    m.sh(marker + 0x12, rng.next() as u16);
    let any = 0x2000 + rng.next() % 0x3_e000;
    m.sw(0x800c_6d7c, choose(rng, &[0x4000, 0x8000, any]));
    *a = [marker, 0, 0, 0];
}

/// 80066590 (dump; `view`): the view's ground level +0x2C, the target slot
/// and its flag 0x400, the mission (0xD), the weapon modes.
fn mutate_impact_marker(m: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], v: u32) {
    mutate_compose(m, rng, a, v);
    let view = a[0];
    let any = rng.next();
    m.sw(view + 0x2c, choose(rng, &[0, 0x10_0000, any]));
    let any = (2 + rng.next() % 30) as u8;
    let slot = choose(rng, &[0xff, 2, 3, any]);
    m.sb(0x800c_6d6e, slot);
    if slot != 0xff {
        let flags = m.lw(EVENTS + u32::from(slot) * 4);
        m.sw(
            EVENTS + u32::from(slot) * 4,
            flags ^ (rng.next() % 2 * 0x400),
        );
    }
    let any = (rng.next() % 0x40) as u16;
    set_mission(m, choose(rng, &[0xd, any]));
    let c = context(m);
    m.sb(c + 0x3aa, choose(rng, &[9, 0, 5]));
    m.sb(c + 0x3ab, choose(rng, &[9, 0]));
}

/// 8006477C (dump; `x`, `y`).
fn mutate_dart(_: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], _: u32) {
    *a = [
        choose(rng, &[0xa0, 0x40]),
        choose(rng, &[0xc8, 0x78, 0x20]),
        0,
        0,
    ];
}

/// 80042B20 / 80042D98 (dump; `view`): the view block, the course kind of
/// the training mode `[gp+0x334]`, the mission (0x19).
fn mutate_guidance(m: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], v: u32) {
    mutate_compose(m, rng, a, v);
    let mode = m.lw(GP + 0x334) & 0xf;
    m.sw(GP + 0x334, mode);
    m.sb(0x800b_1fc4 + mode * 4, (rng.next() % 8) as u8);
    let any = (rng.next() % 0x40) as u16;
    set_mission(m, choose(rng, &[0x19, any]));
}

/// 800829BC (dump; `position`, `velocity`, `kind`): vectors in the scratch
/// RAM, every kind, the mission (0x2A keeps kind 1).
fn mutate_fragments(m: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], _: u32) {
    let position = SCRATCH_RAM + 0x300;
    let velocity = SCRATCH_RAM + 0x310;
    for k in 0..3 {
        m.sw(position + k * 4, pick(rng));
        m.sw(velocity + k * 4, (rng.next() % 0x4000).wrapping_sub(0x2000));
    }
    let any = (rng.next() % 0x40) as u16;
    set_mission(m, choose(rng, &[0x2a, any]));
    let any = rng.next() % 12;
    *a = [
        position,
        velocity,
        choose(rng, &[1, 1, 0, 9, 0xa, 0xb, any]),
        0,
    ];
}

/// 8003FD48 / 800401AC (dump; `index`, `color`): a slot with or without a
/// screen record and a distance record, the full-box flag 0x100.
fn mutate_marker_shape(m: &mut Machine, rng: &mut oracle::Rng, a: &mut [u32; 4], _: u32) {
    let marked: Vec<u32> = (0..64)
        .filter(|&s| m.lw(SCREENS + s * 4) != 0 && m.lw(0x800c_6ed0 + s * 4) != 0)
        .collect();
    let index = if !marked.is_empty() && !rng.next().is_multiple_of(4) {
        marked[rng.next() as usize % marked.len()]
    } else {
        rng.next() % 64
    };
    let distance = m.lw(0x800c_6ed0 + index * 4);
    if distance != 0 {
        let flags = m.lw(distance);
        m.sw(distance, flags ^ (rng.next() % 2 * 0x100));
    }
    let record = m.lw(SCREENS + index * 4);
    if record != 0 {
        m.sh(record + 6, (2 + rng.next() % 0x20) as u16);
        m.sh(record + 8, (rng.next() % 0x140) as u16);
        m.sh(record + 0xa, (rng.next() % 0xf0) as u16);
    }
    *a = [
        index,
        choose(rng, &[0xff00, 0xff, 0x0040_ff40, 0x00ff_4000]),
        0,
        0,
    ];
}

/// `RES_P3_DIAG=<run>:<callee hex>:<sequence>`, optional
/// `RES_P3_DIAG_FORCE=<spec>[;<spec>..]` (`Fallback::force` specs): replay
/// one captured call natively, then once per spec with those routines
/// interpreted, and print the fault, the RAM byte count and the GTE
/// registers that differ from the capture (native / original). Bisects the
/// routine of a native call tree that leaves a capture difference.
#[cfg(feature = "dev-fallback")]
#[test]
#[ignore = "requires AC3_JP_DIR and RES_P3_DIAG"]
fn part3_diagnose() {
    let Ok(spec) = std::env::var("RES_P3_DIAG") else {
        return;
    };
    let parts: Vec<&str> = spec.split(':').collect();
    let run = parts[0];
    let callee = u32::from_str_radix(parts[1], 16).expect("callee");
    let sequence: u64 = parts[2].parse().expect("sequence");
    let path = root().join(run).join("snapshots-module0.jsonl");
    let directory = path.parent().unwrap().to_path_buf();
    let index = snapshot::read_index(&path).unwrap();
    let entry = index
        .iter()
        .find(|s| s.stage == "entry" && s.callee == callee && s.sequence == sequence)
        .expect("entry");
    let exit = exit_of(&index, entry).expect("exit");
    let entry = Snapshot::load(&directory, entry).unwrap();
    let exit = Snapshot::load(&directory, exit).unwrap();
    let mut original = exit.machine();
    let mut forces = vec![String::new()];
    if let Ok(list) = std::env::var("RES_P3_DIAG_FORCE") {
        forces.extend(list.split(';').map(String::from));
    }
    let hook = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));
    for force in forces {
        let mut fallback = ac3_fallback::Fallback::new(ac3_fallback::Version::Jp);
        let forced = if force.is_empty() {
            0
        } else {
            fallback.force(&force).expect("force spec")
        };
        ac3_fallback::install(fallback);
        let mut m = entry.machine();
        let args = [entry.gpr(4), entry.gpr(5), entry.gpr(6), entry.gpr(7)];
        let result = panic::catch_unwind(AssertUnwindSafe(|| {
            ac3_flight_jp::dispatch::call(&mut m, callee, args)
        }));
        ac3_fallback::uninstall();
        let d = snapshot::compare(&m, &exit);
        let gte: Vec<String> = d
            .gte_data
            .iter()
            .map(|&r| format!("{r}: {:08x}/{:08x}", m.gte.data[r], original.gte.data[r]))
            .collect();
        println!(
            "force [{force}] ({forced} routines): {} fault {:?} ram {} gte data [{}]",
            if result.is_ok() { "ran" } else { "panicked" },
            m.take_fault(),
            d.ram.len(),
            gte.join(", ")
        );
        // RES_P3_DIAG_WORD=<hex>: the stack words (0x801FC000..) whose low
        // halfword is that value, here and in the capture
        if let Ok(word) = std::env::var("RES_P3_DIAG_WORD") {
            let low = u16::from_str_radix(&word, 16).expect("word");
            let mut out = Vec::new();
            for a in (0x801f_c000u32..0x8020_0000).step_by(2) {
                let here = m.lhu(a);
                let there = original.lhu(a);
                if (here == low || there == low) && a % 4 == 0 {
                    let (h, t) = (m.lw(a), original.lw(a));
                    if h != t {
                        out.push(format!("{a:08x}: {h:08x}/{t:08x}"));
                    }
                }
            }
            println!(
                "  stack words with low {low:#06x} (here/capture): {}",
                out.join(" ")
            );
        }
    }
    panic::set_hook(hook);
}

// ---- coverage ------------------------------------------------------------------

/// A copy of `support::oracle::run` (the test-only interpreter of the
/// original instructions) that also records every instruction address it
/// executes inside the routine: the instruction coverage the part's oracle
/// cases reach. The differential comparison itself runs `support::oracle`;
/// this copy only measures.
mod coverage {
    use ac3_flight_jp::{Fault, Machine};
    use std::collections::BTreeSet;

    /// A return address that is never code (not even in an overlay): reaching
    /// it ends the run.
    const RETURN: u32 = 0xffff_fff0;

    /// Interpret the original instructions in `start..end` over `m` with
    /// arguments `args` (a fifth and later argument stay in the caller's frame at
    /// `m.sp + 0x10..`). Returns `v0`.
    pub fn run(
        m: &mut Machine,
        start: u32,
        end: u32,
        args: [u32; 4],
        covered: &mut BTreeSet<u32>,
    ) -> u32 {
        let mut r = [0u32; 32];
        let (mut hi, mut lo) = (0u32, 0u32);
        r[4..8].copy_from_slice(&args);
        r[28] = ac3_flight_jp::GP;
        r[29] = m.sp;
        r[31] = RETURN;
        let (mut pc, mut next) = (start, start + 4);
        let mut steps = 0u32;
        loop {
            if pc == RETURN {
                m.sp = r[29];
                return r[2];
            }
            if pc < start || pc >= end {
                m.sp = r[29];
                let wide = |hi: u32, lo: u32| (u64::from(hi) << 32) | u64::from(lo);
                match pc {
                    // libgcc's 64-bit divisions return v1:v0; `dispatch` carries
                    // only v0, so run them natively with both result words.
                    0x8009_ea48 => {
                        let q = ac3_flight_jp::sdk::udivdi3(m, wide(r[5], r[4]), wide(r[7], r[6]));
                        (r[2], r[3]) = (q as u32, (q >> 32) as u32);
                    }
                    0x8009_e9b0 => {
                        let q = ac3_flight_jp::sdk::divdi3(
                            m,
                            wide(r[5], r[4]) as i64,
                            wide(r[7], r[6]) as i64,
                        ) as u64;
                        (r[2], r[3]) = (q as u32, (q >> 32) as u32);
                    }
                    _ => r[2] = ac3_flight_jp::dispatch::call(m, pc, [r[4], r[5], r[6], r[7]]),
                }
                pc = r[31];
                next = pc + 4;
                continue;
            }
            steps += 1;
            covered.insert(pc);
            assert!(steps < 10_000_000, "runaway at {pc:08x}");
            let w = m.lw(pc);
            let cur = pc;
            pc = next;
            next = next.wrapping_add(4);
            let op = w >> 26;
            let (rs, rt, rd) = (
                ((w >> 21) & 31) as usize,
                ((w >> 16) & 31) as usize,
                ((w >> 11) & 31) as usize,
            );
            let sa = (w >> 6) & 31;
            let simm = (w & 0xffff) as u16 as i16 as i32 as u32;
            let zimm = w & 0xffff;
            let bt = cur.wrapping_add(4).wrapping_add(simm << 2);
            let jt = (cur.wrapping_add(4) & 0xf000_0000) | ((w & 0x03ff_ffff) << 2);
            let (a, b) = (r[rs], r[rt]);
            let mut out: Option<(usize, u32)> = None;
            match op {
                0 => match w & 63 {
                    0x00 => out = Some((rd, b << sa)),
                    0x02 => out = Some((rd, b >> sa)),
                    0x03 => out = Some((rd, ((b as i32) >> sa) as u32)),
                    0x04 => out = Some((rd, b << (a & 31))),
                    0x06 => out = Some((rd, b >> (a & 31))),
                    0x07 => out = Some((rd, ((b as i32) >> (a & 31)) as u32)),
                    0x08 => next = a,
                    0x09 => {
                        out = Some((rd, cur + 8));
                        next = a;
                    }
                    0x0d => {
                        // break code: bits 6..25; the compiler's traps put the
                        // 10-bit code in the upper half (0x1C00 -> 7).
                        let code = ((w >> 6) & 0xf_ffff) >> 10;
                        m.raise(Fault::Trap { address: cur, code });
                        m.sp = r[29];
                        return r[2];
                    }
                    0x10 => out = Some((rd, hi)),
                    0x11 => hi = a,
                    0x12 => out = Some((rd, lo)),
                    0x13 => lo = a,
                    0x18 => {
                        let p = i64::from(a as i32) * i64::from(b as i32);
                        lo = p as u32;
                        hi = (p >> 32) as u32;
                    }
                    0x19 => {
                        let p = u64::from(a) * u64::from(b);
                        lo = p as u32;
                        hi = (p >> 32) as u32;
                    }
                    0x1a => {
                        let (n, d) = (a as i32, b as i32);
                        if d == 0 {
                            lo = if n < 0 { 1 } else { u32::MAX };
                            hi = a;
                        } else {
                            lo = n.wrapping_div(d) as u32;
                            hi = n.wrapping_rem(d) as u32;
                        }
                    }
                    0x1b => {
                        // divu by zero: quotient 0xFFFFFFFF, remainder = dividend.
                        lo = a.checked_div(b).unwrap_or(u32::MAX);
                        hi = a.checked_rem(b).unwrap_or(a);
                    }
                    0x20 | 0x21 => out = Some((rd, a.wrapping_add(b))),
                    0x22 | 0x23 => out = Some((rd, a.wrapping_sub(b))),
                    0x24 => out = Some((rd, a & b)),
                    0x25 => out = Some((rd, a | b)),
                    0x26 => out = Some((rd, a ^ b)),
                    0x27 => out = Some((rd, !(a | b))),
                    0x2a => out = Some((rd, u32::from((a as i32) < (b as i32)))),
                    0x2b => out = Some((rd, u32::from(a < b))),
                    f => panic!("special {f:#x} at {cur:08x}"),
                },
                1 => {
                    let take = if rt & 1 == 0 {
                        (a as i32) < 0
                    } else {
                        (a as i32) >= 0
                    };
                    if rt & 0x10 != 0 {
                        r[31] = cur + 8;
                    }
                    if take {
                        next = bt;
                    }
                }
                2 => next = jt,
                3 => {
                    r[31] = cur + 8;
                    next = jt;
                }
                4 if a == b => next = bt,
                5 if a != b => next = bt,
                6 if (a as i32) <= 0 => next = bt,
                7 if (a as i32) > 0 => next = bt,
                4..=7 => {}
                8 | 9 => out = Some((rt, a.wrapping_add(simm))),
                0xa => out = Some((rt, u32::from((a as i32) < (simm as i32)))),
                0xb => out = Some((rt, u32::from(a < simm))),
                0xc => out = Some((rt, a & zimm)),
                0xd => out = Some((rt, a | zimm)),
                0xe => out = Some((rt, a ^ zimm)),
                0xf => out = Some((rt, zimm << 16)),
                0x12 => {
                    if w & (1 << 25) != 0 {
                        m.gte.command(w & 0x1ff_ffff).unwrap();
                    } else {
                        match rs {
                            0 => out = Some((rt, m.gte.mfc2(rd))),
                            2 => out = Some((rt, m.gte.cfc2(rd))),
                            4 => m.gte.mtc2(rd, b),
                            6 => m.gte.ctc2(rd, b),
                            x => panic!("cop2 {x} at {cur:08x}"),
                        }
                    }
                }
                0x20 => out = Some((rt, m.lb(a.wrapping_add(simm)) as i32 as u32)),
                0x21 => out = Some((rt, m.lh(a.wrapping_add(simm)) as i32 as u32)),
                0x23 => out = Some((rt, m.lw(a.wrapping_add(simm)))),
                0x24 => out = Some((rt, u32::from(m.lbu(a.wrapping_add(simm))))),
                0x25 => out = Some((rt, u32::from(m.lhu(a.wrapping_add(simm))))),
                0x28 => m.sb(a.wrapping_add(simm), b as u8),
                0x29 => m.sh(a.wrapping_add(simm), b as u16),
                0x2b => m.sw(a.wrapping_add(simm), b),
                0x22 | 0x26 | 0x2a | 0x2e => {
                    let addr = a.wrapping_add(simm);
                    let aligned = addr & !3;
                    let shift = (addr & 3) as usize;
                    let mem = m.lw(aligned);
                    match op {
                        0x22 => {
                            const MASK: [u32; 4] = [0x00ff_ffff, 0x0000_ffff, 0x0000_00ff, 0];
                            const SH: [u32; 4] = [24, 16, 8, 0];
                            out = Some((rt, (b & MASK[shift]) | (mem << SH[shift])));
                        }
                        0x26 => {
                            const MASK: [u32; 4] = [0, 0xff00_0000, 0xffff_0000, 0xffff_ff00];
                            const SH: [u32; 4] = [0, 8, 16, 24];
                            out = Some((rt, (b & MASK[shift]) | (mem >> SH[shift])));
                        }
                        0x2a => {
                            const MASK: [u32; 4] = [0xffff_ff00, 0xffff_0000, 0xff00_0000, 0];
                            const SH: [u32; 4] = [24, 16, 8, 0];
                            m.sw(aligned, (mem & MASK[shift]) | (b >> SH[shift]));
                        }
                        _ => {
                            const MASK: [u32; 4] = [0, 0x0000_00ff, 0x0000_ffff, 0x00ff_ffff];
                            const SH: [u32; 4] = [0, 8, 16, 24];
                            m.sw(aligned, (mem & MASK[shift]) | (b << SH[shift]));
                        }
                    }
                }
                0x32 => {
                    let v = m.lw(a.wrapping_add(simm));
                    m.gte.mtc2(rt, v);
                }
                0x3a => {
                    let v = m.gte.mfc2(rt);
                    m.sw(a.wrapping_add(simm), v);
                }
                o => panic!("op {o:#x} at {cur:08x}"),
            }
            if let Some((i, v)) = out
                && i != 0
            {
                r[i] = v;
            }
        }
    }
}
