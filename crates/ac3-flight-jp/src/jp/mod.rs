//! The JP patch layer: hand-written code that the generator
//! (`tools/jp/readdress.py`) never overwrites. See `PATCHES.md`.
//!
//! * [`REPLACES`]: generated functions replaced by a function of the same
//!   name and signature in `crate::jp::<module>` (file `src/jp/<module>.rs`)
//!   or in a wave part's file `src/jp/res_pN_<module>[_<subsystem>].rs`.
//!   The generator turns the generated body into a call of the patch, so the
//!   dispatch arm and every native caller reach it.
//! * [`TAKES_OVER`]: JP addresses a patch dispatcher claims instead of the
//!   generated arm, which the generator removes (a replacement with another
//!   signature).
//! * Every `pub fn dispatch` of a patch module is asked first by
//!   [`dispatch::call`](crate::dispatch::call): [`resident`] for JP-only
//!   resident routines, [`ov1`] .. [`ov4`] for the JP overlays.
//! * A target no owner claims goes to the development interpreter fallback
//!   (feature `dev-fallback`, `crate::fallback::miss`), else it raises
//!   `Fault::Untranslated`: the PAL crate's hooks, carried over.

pub mod game;
pub mod init_39a68;
pub mod ov1;
pub mod ov1_p1;
pub mod ov1_p1_card;
pub mod ov1_p1_load;
pub mod ov1_p1_save;
pub mod ov1_p1_screens;
pub mod ov1_p1_settings;
pub mod ov1_p2;
pub mod ov1_p2_menu;
pub mod ov1_p2_save;
pub mod ov1_p2_text;
pub mod ov1_p2_visual;
pub mod ov2;
pub mod ov2_p1;
pub mod ov2_p1_archive;
pub mod ov2_p1_keys;
pub mod ov2_p1_keyscreen;
pub mod ov2_p1_options;
pub mod ov2_p1_pages;
pub mod ov2_p1_search;
pub mod ov2_p2;
pub mod ov2_p2_cells;
pub mod ov2_p2_config;
pub mod ov2_p2_display;
pub mod ov2_p2_draw;
pub mod ov2_p2_grid;
pub mod ov2_p2_hub;
pub mod ov2_p2_music;
pub mod ov2_p2_options;
pub mod ov2_p2_records;
pub mod ov2_p2_screen;
pub mod ov2_p3;
pub mod ov2_p3_card;
pub mod ov2_p3_menu;
pub mod ov2_p3_panels;
pub mod ov2_p3_records;
pub mod ov2_p4;
pub mod ov2_p4_disc;
pub mod ov2_p4_graph;
pub mod ov2_p4_load;
pub mod ov2_p4_scene;
pub mod ov2_p4_story;
pub mod ov2_p4_stream;
pub mod ov2_p4_text;
pub mod ov2_p5;
pub mod ov2_p5_disc;
pub mod ov2_p5_lost;
pub mod ov2_p5_map;
pub mod ov2_p5_movie;
pub mod ov2_p5_net;
pub mod ov2_p5_net2;
pub mod ov2_p5_util;
pub mod ov2_p6;
pub mod ov2_p6_map;
pub mod ov2_p6_models;
pub mod ov2_p6_scene;
pub mod ov2_p6_story;
pub mod ov2_p6_terrain;
pub mod ov2_p6_viewer;
pub mod ov2_stubs;
pub mod ov3;
pub mod ov3_p1;
pub mod ov3_p1_draw;
pub mod ov3_p1_player;
pub mod ov4;
pub mod ov4_p1;
pub mod ov4_p1_music;
pub mod ov4_p1_text;
pub mod res_p1_frame;
pub mod res_p1_init_tail;
pub mod res_p1_player;
pub mod res_p1_render;
pub mod res_p1_replay;
pub mod res_p1_session;
pub mod res_p1_shared;
pub mod res_p1_states;
pub mod res_p2_ai;
pub mod res_p2_boot;
pub mod res_p2_entities;
pub mod res_p2_init;
pub mod res_p2_init_38818;
pub mod res_p2_loading;
pub mod res_p2_mainloop;
pub mod res_p2_render;
pub mod res_p2_script;
pub mod res_p3_cdstream;
pub mod res_p3_effects;
pub mod res_p3_hud;
pub mod res_p3_hud_extra;
pub mod res_p3_hud_guide;
pub mod res_p3_hud_lock;
pub mod res_p3_hud_marks;
pub mod res_p3_hud_radar;
pub mod res_p3_hud_sight;
pub mod res_p3_hud_warn;
pub mod res_p3_init_39a68;
pub mod res_p3_init_objects;
pub mod res_p3_mainloop;
pub mod res_p3_resident;
pub mod res_p3_sky;
pub mod res_p3_terrain;
pub mod res_sdk;
pub mod res_sdk_format;
pub mod res_sdk_game;
pub mod resident;

/// Generated functions replaced by patch functions: (JP address, generated
/// path `module::fn` or `module::inline_module::fn`). The patch function is
/// `crate::jp::<module>::<fn>`, or `crate::jp::res_pN_<module>[_<subsystem>]::<fn>`
/// (exactly one of them), with the same signature; the generator checks
/// that the generated function still exists, is the routine at that address
/// (its doc address or its dispatch arm), and has the same signature.
pub const REPLACES: &[(u32, &str)] = &[
    (0x8004_09ac, "init_39a68::reset_gp_344"),
    // jp-res3
    (0x8002_a79c, "cdstream::start_read"),
    (0x8002_9bd8, "cdstream::stream_sector"),
    (0x8002_a498, "cdstream::stream_service"),
    (0x8002_8c24, "mainloop::map_pad_input"),
    (0x8002_ac3c, "mainloop::cd_stop"),
    (0x8002_c41c, "mainloop::cd_command_pause"),
    (0x8002_e1f4, "mainloop::update_sound"),
    (0x8002_df98, "mainloop::update_voice_volume"),
    (0x8002_e0d0, "mainloop::key_on_voice"),
    (0x8002_d3f4, "hud::stream::overlay_2cfcc"),
    (0x8005_c680, "hud::tree_5cbf4::heading_5af88"),
    (0x8005_cb48, "hud::tree_5cbf4::altitude_5b450"),
    (0x8005_d000, "hud::tree_5cbf4::timer_5b908"),
    (0x8005_d468, "hud::tree_5cbf4::damage_5bd40"),
    (0x8005_d998, "hud::tree_5cbf4::missiles_5c270"),
    (0x8003_a008, "init_39a68::initialize_39a68"),
    (0x8005_c5b0, "init_39a68::block_4f4_008"),
    (0x8005_c990, "init_39a68::block_4f4_09c"),
    (0x8005_cedc, "init_39a68::block_4f4_1b8"),
    (0x8005_d344, "init_39a68::block_4f4_30c"),
    (0x8005_d844, "init_39a68::block_4f4_244"),
    (0x8005_dca8, "init_39a68::block_4f4_384"),
    (0x8005_defc, "init_39a68::block_4f4_3dc"),
    (0x8003_b924, "init_39a68::packets_424"),
    (0x8003_c250, "init_39a68::packets_424_quads"),
    (0x8008_4a6c, "init_objects::init_effect_tables"),
    (0x8008_49ec, "init_objects::reset_effect_lists"),
    (0x8006_ce70, "init_objects::init_player_flight"),
    (0x8006_cd6c, "init_objects::aircraft_parameters"),
    (0x8004_76f0, "hud::tree_40a24::area_status_46aa8"),
    (0x8003_346c, "terrain::render_world"),
    (0x8006_1fa8, "terrain::subdivide_level"),
    (0x8009_57a4, "terrain::draw_sky"),
    (0x8009_4718, "terrain::sky_camera"),
    (0x8009_4d10, "terrain::sky_colors"),
    (0x8009_4ac8, "terrain::sky_bands"),
    (0x8009_48a0, "terrain::project_bands"),
    (0x8009_5074, "terrain::draw_bands"),
    (0x8009_5930, "terrain::draw_sun"),
    (0x8009_5cb8, "terrain::draw_stars"),
    (0x8009_6184, "terrain::draw_cloud_bands"),
    (0x8009_65d4, "terrain::screen_outcode"),
    (0x8009_7eb4, "terrain::draw_dome_sky"),
    (0x8009_8044, "terrain::draw_night_sky"),
    (0x8008_faa8, "terrain::fade_overlay"),
    (0x8009_8714, "terrain::night_ground_row"),
    (0x8003_b34c, "hud::tree_39d84::screen_targets_3abac"),
    (0x8003_ef40, "hud::tree_39d84::lockon_3e780"),
    (0x8004_09d4, "hud::tree_39d84::lock_start_4011c"),
    (0x8004_0a24, "hud::tree_39d84::lock_cursor_4016c"),
    (0x8008_fc9c, "hud::tree_39d84::fade_8e42c"),
    (0x8003_d458, "hud::tree_3bdf8_a::frame_tiles_3cca0"),
    (0x8003_d5c0, "hud::tree_3bdf8_b::radar_map_3ce04"),
    (0x8003_da08, "hud::tree_3bdf8_b::entity_blips_3d24c"),
    (0x8003_dcf0, "hud::tree_3bdf8_b::ground_blips_3d534"),
    (0x8003_de5c, "hud::tree_3bdf8_b::draw_blips_3d6a0"),
    (0x8003_e69c, "hud::tree_3bdf8_b::map_window_3dedc"),
    (0x8003_c5b0, "hud::compose::gauges_3bdf8"),
    (0x8003_a0b4, "hud::compose::compose_39b80"),
    (0x8004_1794, "hud::tree_40a24::frame_40de4"),
    (0x8004_14d8, "hud::tree_40a24::link_frame_40b00"),
    (0x8004_2544, "hud::tree_40a24::warnings_41a68"),
    (0x8004_1f88, "hud::tree_40a24::warning_label_415d8"),
    (0x8003_f520, "hud::tree_3e828::targets_3ed64"),
    (0x8006_4cb4, "hud::tree_62680_a::center_lines_634e8"),
    (0x8006_5d8c, "hud::tree_62680_a::target_arrow_645c0"),
    (0x8003_e274, "hud::extras::radar_marker_3dab4"),
    (0x8006_6590, "hud::extras::impact_marker_64dc4"),
    (0x8006_477c, "hud::tree_62ea0::dart_62fb0"),
    (0x8004_2b20, "hud::tree_41ff0::guidance_41ff0"),
    (0x8004_2d98, "hud::tree_41ff0::approach_42258"),
    (0x8008_29bc, "effects::fragments_80081374"),
    // resident part 1 (jp-res1)
    (0x8002_f70c, "shared::request_kind_sound"),
    (0x8003_a5c0, "shared::target_available"),
    (0x8003_a62c, "shared::target_mode_code"),
    (0x8004_63b8, "shared::set_lock"),
    (0x8005_7720, "states::movie_player"),
    (0x8005_7cd0, "states::movie_prepare"),
    (0x8005_7ebc, "states::movie_setup"),
    (0x8005_81c4, "states::movie_frame"),
    (0x8005_b308, "states::picture_state_setup"),
    (0x8005_b4e4, "states::picture_state_frame"),
    (0x8009_b6b8, "states::module_load_begin"),
    (0x8009_b7cc, "states::module_load_poll"),
    (0x8003_5740, "init_tail::update_gameplay_bit0"),
    (0x8003_2518, "init_tail::set_flight_bounds"),
    (0x8003_2080, "init_tail::enter_mission_phase"),
    (0x8004_e620, "init_tail::activate_group_entities_high"),
    (0x8005_b654, "init_tail::setup_phase_400"),
    (0x8003_3ef0, "frame::flight_callback"),
    (0x8003_39e0, "frame::frame"),
    (0x8003_3dc0, "frame::frame_tail"),
    (0x8003_4040, "frame::replay_state"),
    (0x8003_42ec, "frame::pause_state"),
    (0x8003_53e8, "frame::result_caption"),
    (0x8003_5578, "frame::fade_bar"),
    (0x8003_61bc, "frame::load_aircraft_state"),
    (0x8006_d078, "frame::reset_player"),
    (0x8006_ef30, "player::flight_model_8006d8d4"),
    (0x8007_29dc, "player::update_player_80071358"),
    (0x8007_2178, "player::player_effects_80070af4"),
    (0x8004_6124, "player::choose_target_800454ec"),
    (0x8005_1414, "player::update_units_8004fdf0"),
    (0x8005_1840, "player::update_unit_sound_80050214"),
    (0x8005_157c, "player::move_units_8004ff54"),
    (0x8004_658c, "player::update_sensors_80045950"),
    (0x8006_d0dc, "player::control_step_8006b90c"),
    (0x8004_8658, "player::record_flight_path_8004790c"),
    (0x8004_5a40, "player::apply_damage_80044ef8"),
    (0x8009_9894, "player::ground_hit_80097edc"),
    (0x8009_a800, "player::ground_ray_cells_80098db0"),
    (0x8009_a944, "player::object_hit_80098ef4"),
    (0x8005_2fa8, "player::unit_attack_80051974"),
    (0x8007_5b04, "player::takeoff_run_80074488"),
    (0x8007_4e64, "player::follow_entity_800737e0"),
    (0x8007_5084, "player::join_formation_80073a08"),
    (0x8005_3214, "player::gun_attack_80051bdc"),
    (0x8005_35d4, "player::missile_attack_80051f9c"),
    (0x8007_2004, "player::launch_missile_80070980"),
    (0x8007_97fc, "player::focus_kill_camera_8007818c"),
    (0x8004_8758, "player::record_kill_80047a0c"),
    (0x8005_20c8, "player::destroy_unit_80050a98"),
    (0x8005_22b0, "player::unit_motion_80050c84"),
    (0x8006_dd5c, "player::analog_control_8006c58c"),
    (0x8007_0a80, "player::collide_entities_8006f3fc"),
    (0x8007_f448, "replay::replay_start"),
    (0x8007_f5b4, "replay::replay_frame"),
    (0x8007_f808, "replay::replay_finish"),
    (0x8009_3bc4, "replay::screen_fade"),
    (0x8009_3c90, "replay::tint_rectangle"),
    (0x8009_3d1c, "replay::sequence_sparks"),
    (0x8005_73e8, "replay::movie_start"),
    (0x8005_7640, "replay::movie_end"),
    (0x8005_ae8c, "replay::picture_start"),
    (0x8003_9430, "replay::sequence_start"),
    (0x8003_95d4, "replay::sequence_end"),
    (0x8003_99d4, "replay::sequence_fade"),
    (0x8005_bc64, "replay::course_start"),
    (0x8005_be0c, "replay::course_end"),
    (0x8003_9230, "replay::command_effect"),
    (0x8003_3324, "render::draw_lit_systems"),
    (0x8007_f8e4, "session::update_8007e284"),
    (0x8008_4560, "session::intro_fade"),
    (0x8008_4474, "session::screen_fade"),
    (0x8007_953c, "session::camera_follow_default"),
    (0x8003_4718, "session::pause_input"),
    (0x8003_43c8, "session::draw_box"),
    (0x8003_71c4, "session::session_step"),
    (0x8005_40e0, "session::mission_logic"),
    (0x8005_628c, "session::radio_update"),
    (0x8005_6518, "session::radio_reset"),
    (0x8003_96e8, "session::update_8003931c"),
    (0x8005_c270, "session::update_8005a874"),
    // resident part 2 (jp-res2)
    (0x8002_57e8, "boot::init_globals"),
    (0x8002_5b00, "boot::init_devices"),
    (0x8002_ae0c, "boot::open_resource_files"),
    (0x8002_d79c, "boot::set_sound_attributes"),
    (0x8003_2754, "init::gameplay_initialization"),
    (0x8007_7880, "init::initialize_camera"),
    (0x8003_58f8, "loading::initialize"),
    (0x8003_57c0, "loading::poll"),
    (0x8003_5a38, "loading::prepare_mission"),
    (0x8003_5f18, "loading::queue_aircraft"),
    (0x8003_5f98, "loading::prepare_aircraft"),
    (0x8003_6e84, "loading::draw_aperture_screen"),
    (0x8003_6824, "loading::draw_texts"),
    (0x8003_62a0, "loading::fade"),
    (0x8003_8b2c, "init_38818::initialize_38818"),
    (0x8005_3bac, "init_38818::start_mission_state_80052574"),
    (0x8005_6198, "init_38818::reset_message_queue_80054b54"),
    (0x8004_856c, "init_38818::reset_result_800477c8"),
    (0x8005_4ebc, "script::interpret"),
    (0x8003_7e50, "script::step_80037b24"),
    (0x8005_5f3c, "script::block_context_flag_800547d4"),
    (0x8005_60e4, "script::block_context_byte398_8005497c"),
    (0x8003_8100, "entities::mark_entities_37dfc"),
    (0x8003_809c, "entities::run_entity_logic_37da0"),
    (0x8004_6600, "entities::retire_entities_459c0"),
    (0x8004_69ec, "entities::select_behaviour_45da8"),
    (0x8004_701c, "entities::run_actions_463d8"),
    (0x8002_f9ac, "entities::update_track_sounds_2f590"),
    (0x8003_83ac, "entities::integrate_entities_3809c"),
    (0x8007_d7e0, "entities::entity_trails_7c164"),
    (0x8008_50d8, "entities::update_effects_83a64"),
    (0x8008_4f6c, "entities::approach_effect_83914"),
    (0x8009_28f4, "entities::update_explosion_91084"),
    (0x8003_aa60, "entities::update_player_lock_3a2c4"),
    (0x8003_ac8c, "entities::auto_lock_3a4f0"),
    (0x8006_7144, "render::list_entities"),
    (0x8006_7380, "render::list_objects"),
    (0x8009_0a74, "render::draw_effect_lines"),
    (0x8009_0bf8, "render::draw_thin_trails"),
    (0x8009_0ed0, "render::draw_medium_trails"),
    (0x8009_1370, "render::draw_wide_trails"),
    (0x8009_11b0, "render::draw_sparks"),
    (0x8007_cfa4, "render::draw_ribbon_trails"),
    (0x8008_e038, "render::draw_ambient_particles"),
    (0x8008_54b8, "render::draw_objects"),
    (0x8008_6320, "render::draw_tracer"),
    (0x8008_6a0c, "render::draw_exhaust"),
    (0x8008_b310, "render::draw_moon"),
    (0x8007_b438, "render::update_camera"),
    (0x8007_affc, "render::replay_camera"),
    (0x8008_ffac, "render::draw_overlays"),
    (0x8008_f950, "render::screen_blend"),
    (0x8008_488c, "render::noise_overlay"),
    (0x8008_c644, "render::heat_shimmer"),
    (0x8009_2168, "render::clip_segment"),
    (0x8008_9f44, "render::draw_debris_star"),
    (0x8008_abf0, "render::draw_debris_ember"),
    (0x8008_e4f8, "render::draw_rain"),
    (0x8008_f1b8, "render::draw_motes"),
    (0x8008_f580, "render::draw_leaves"),
    (0x8003_6440, "render::copy_frame"),
    (0x8003_65fc, "render::border_quads"),
    (0x8003_6a84, "render::fade_screen"),
    (0x8008_3fe0, "render::starfield"),
    (0x8003_64c4, "render::star_screen"),
    (0x8007_961c, "render::start_camera"),
    (0x8008_ba8c, "render::draw_sun"),
    (0x8004_7d68, "ai::missiles_47124"),
    (0x8004_8a80, "ai::latest_event_47d78"),
    (0x8004_986c, "ai::dock_48134"),
    (0x8004_a678, "ai::attitude_48f40"),
    (0x8004_aa3c, "ai::catapult_49300"),
    (0x8004_b108, "ai::turn_back_499cc"),
    (0x8004_b308, "ai::blow_up_49bc4"),
    (0x8004_b4d8, "ai::spin_fall_49d94"),
    (0x8004_dc38, "ai::phase_entities_4c644"),
    (0x8004_f0c8, "ai::attack_run_4dac0"),
    (0x8004_f570, "ai::evade_4df64"),
    (0x8004_fc04, "ai::destroyed_4e5e0"),
    (0x8005_0184, "ai::timed_flight_4eb6c"),
    (0x8005_02d4, "ai::escort_duty_4ecbc"),
    (0x8005_0de8, "ai::follow_route_4f7c8"),
    (0x8005_1010, "ai::keyframes_4f9f0"),
    (0x8005_1304, "ai::special_state_4fce4"),
    (0x8005_13d8, "ai::check_finished_4fdb0"),
    (0x8006_a11c, "entities::projectile_collisions_6894c"),
    (0x8006_b504, "entities::update_particles_69d34"),
    (0x800a_3f64, "mainloop::pending_sdk::cd_command"),
];

/// JP addresses whose generated dispatch arm the generator removes because a
/// patch dispatcher claims them.
pub const TAKES_OVER: &[u32] = &[
    0x8009_9894,
    0x8009_a944,
    0x8003_8054,
    0x8003_a07c,
    0x8004_ceb0,
    0x8008_5204,
];
