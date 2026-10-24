//! Patches of `player` routines whose JP instructions differ from PAL
//! (REVIEW.md §1-§3b, resident part 1). Each is listed in
//! [`REPLACES`](super::REPLACES). Private helpers of the generated
//! `player.rs` that the patches need are copied here unchanged.
use crate::player::{
    aim_camera_8006d7dc, airborne_start_80074798, altitude_limits_8006d42c,
    apply_attitude_8006f1c8, approach_area_80074db8, auto_climb_800729f8, auto_level_80072494,
    camera_shake_8006b374, control_input_800712b4, control_step_8006bea8, crash_80072edc,
    crash_camera_80077ef4, direction_8005173c, distance_volume_8002e430, doppler_pitch_8002e82c,
    engine_sound_80070a90, enter_crash_80074d5c, exit_attitude_80075450, exit_flight_800749ac,
    fill_free_slots_8007021c, fire_gun_80070a10, fire_weapon_80070a50, flight_model_free_8006ec54,
    formation_check_800752b4, ground_height_8006fb78, ground_roll_800731f4, gun_override_8003a0f0,
    integrate_position_8006f3b0, orientation_80030d20, outside_area_800753c8, pan_volume_8002e4c0,
    place_on_ground_80074320, position_sound_8002e2c0, queue_bullet_80069864,
    restore_voice_8002e260, scale_by_speed_8006d218, scale_outputs_by_speed_8006d184,
    scripted_control_80073db0, scripted_fly_in_80073524, set_approach_mode_800754a8,
    set_exit_mode_80074cf0, set_start_mode_8007577c, set_target_mode_800755e4, spawn_800673d8,
    spawn_800674b0, spawn_800677d8, spawn_8006743c, spawn_80067030, spawn_80067218,
    speed_change_8006d2ac, start_sound_8002d888, tick_reload_timers_8007025c,
    unit_destroyed_8005080c, update_linked_entities_8004a784, update_sight_800446f8,
    update_unit_80050428, vector_angles_80031970, vector_length_800828f8, vibrate_80028fd0,
    vibrate_preset_800291ac, vibrate_preset_800291e4, vibrate_random_80029168,
    world_collision_8006f670,
};
use crate::{
    dispatch as calls,
    machine::{Machine, SmallData},
};
use ac3_gte::reg;

/// 80072178 (JP; PAL 80070AF4, near, `imm:4`): per-frame player effects selected by `flags` (the
/// caller passes masks such as -1, -9, 0x47, 6, 4). The player entity is
/// entry 0 of the table at `[gp+0x2E8]`; `[gp+0x520]` is the player flight
/// state, `[gp+0x524]` the player control block whose word 0 points to the
/// aircraft data.
/// - bit 0: pending hits in entity +0x54 (bits 0..5, then 6..14) subtract
///   aircraft +0x88 / +0x8A from the hit points (entity +0x4E); the frame's
///   event word (control +0x24) gets 1 / 2, plus 0x10 below altitude 8 or
///   8 below 0x600 (state +0x4C).
/// - bit 2: attitude updates (8007084C) and position integration (80070A34).
/// - state +0x10 bit 0x1000 moves to entity +5 bit 3; entity contacts
///   (80070A80) after clearing state bit 0x20; state bits 0..3 cleared.
/// - bit 1: world bounds and terrain (80070CF4), then the ground height under
///   the player (800711FC) into control +0x28 and the height above it into
///   control +0x2C (state bit 8 below 0x42 units).
/// - state bit 0x20 (entity contact): hit points set to -1, velocity cleared
///   and the position reset to state +0x1C..+0x24.
/// - bit 1 with a contact in state bits 0..3: bit 6 sets the hit points to -1
///   on contact bit 0; bit 4 damps entity +0x18 / +0x1C by 15/16 and drops a
///   positive (downward) vertical velocity; bit 5 then
///   subtracts 3 / 5 / 100 hit points by the context's level (+0x394;
///   PAL +0x398),
///   raises event 4, plays sound 0x27 (8002DBE0) unless context +0x28 has
///   bits 0..2, and reacts through 80082BAC.
/// - unless state word 0 has bits 0x7000: event vibrations (800295CC,
///   80029604, 80029588, each restarting the shake counter state +0x54),
///   8006CB44(0) below altitude 0x400, and the engine sound (80072114).
/// - shake counter: 8006CB44(1) while it runs; above speed 0x8C000 (state
///   +0x48) 8006CB44(2) and a 0x800 vibration.
/// - weapons: gun = context +0x3A9 (PAL +0x3AD), special weapon and its
///   reload slots from context +0x3AA (PAL +0x3AE) and state +0x58 (or
///   +0x3AB (PAL +0x3AF) and +0x64 when 8003A5C0 is
///   set), both overridable (8003A624, 8003A62C); control +4 = the
///   aircraft's count (+0x7C / +0x7E) capped by the table at 800B3B28; the
///   reload timers tick (800718E0); bit 3 runs the fire handlers (gun on
///   context +0x128 bit 0x800, weapon on +0x12C bit 0x1000) and counts down
///   state +0x56; control +0x20 = slots in use (800718A0 marks free ones).
pub fn player_effects_80070af4(m: &mut Machine, flags: u32) {
    let entities = m.gw(0x2e8);
    let control = m.gw(0x524);
    let player = m.lw(entities);
    let aircraft = m.lw(control);
    let p = |offset: u32| player.wrapping_add(offset);
    if flags & 1 != 0 {
        let mut hits = m.lw(p(0x54));
        let mut events = 0;
        if hits & 0x3f != 0 {
            m.sw(p(0x54), hits & 0xffff_ffc0);
            let points = m.lhu(p(0x4e));
            let loss = m.lhu(aircraft.wrapping_add(0x88));
            m.sh(p(0x4e), points.wrapping_sub(loss));
            events = 1;
            hits = m.lw(p(0x54));
        }
        if hits & 0x7fc0 != 0 {
            m.sw(p(0x54), hits & 0xffff_803f);
            let points = m.lhu(p(0x4e));
            let loss = m.lhu(aircraft.wrapping_add(0x8a));
            m.sh(p(0x4e), points.wrapping_sub(loss));
            events |= 2;
        }
        let state = m.gw(0x520);
        let altitude = m.lh(state.wrapping_add(0x4c));
        if altitude < 8 {
            events |= 0x10;
        } else if altitude < 0x600 {
            events |= 8;
        }
        let control = m.gw(0x524);
        m.sw(control.wrapping_add(0x24), events);
    }
    if flags & 4 != 0 {
        let state = m.gw(0x520);
        apply_attitude_8006f1c8(m, player, state);
    }
    let state = m.gw(0x520);
    let status = m.lw(state.wrapping_add(0x10));
    if status & 0x1000 != 0 {
        m.sw(state.wrapping_add(0x10), status & !0x1000);
        let marks = m.lbu(p(5));
        m.sb(p(5), marks | 8);
    } else {
        let marks = m.lbu(p(5));
        m.sb(p(5), marks & 0xf7);
    }
    let state = m.gw(0x520);
    let status = m.lw(state.wrapping_add(0x10));
    m.sw(state.wrapping_add(0x10), status & !0x20);
    collide_entities_8006f3fc(m, player, state);
    let state = m.gw(0x520);
    let status = m.lw(state.wrapping_add(0x10));
    m.sw(state.wrapping_add(0x10), status & !0xf);
    if flags & 2 != 0 {
        world_collision_8006f670(m, player, state);
    }
    if flags & 4 != 0 {
        integrate_position_8006f3b0(m, player);
    }
    if flags & 2 != 0 {
        let control = m.gw(0x524);
        let ground = ground_height_8006fb78(
            m,
            p(8),
            control.wrapping_add(0x30),
            control.wrapping_add(0x38),
        );
        let y = m.lw(p(0xc));
        if ((ground.wrapping_sub(y) as i32) >> 12) < 0x42 {
            let state = m.gw(0x520);
            let status = m.lw(state.wrapping_add(0x10));
            m.sw(state.wrapping_add(0x10), status | 8);
        }
        let control = m.gw(0x524);
        m.sw(control.wrapping_add(0x28), ground);
        let y = m.lw(p(0xc));
        m.sw(control.wrapping_add(0x2c), ground.wrapping_sub(y));
    }
    let state = m.gw(0x520);
    if m.lw(state.wrapping_add(0x10)) & 0x20 != 0 {
        m.sh(p(0x4e), 0xffff);
        m.sw(p(0x38), 0);
        m.sw(p(0x3c), 0);
        m.sw(p(0x40), 0);
        for offset in [0, 4, 8] {
            let value = m.lw(state.wrapping_add(0x1c + offset));
            m.sw(p(8 + offset), value);
        }
    }
    if flags & 2 != 0 {
        let state = m.gw(0x520);
        let status = m.lw(state.wrapping_add(0x10));
        if status & 0xf != 0 {
            if status & 1 != 0 && flags & 0x40 != 0 {
                m.sh(p(0x4e), 0xffff);
            }
            if flags & 0x10 != 0 {
                contact_damping(m, player);
                if flags & 0x20 != 0 {
                    contact_damage(m, player);
                }
            }
        }
    }
    let state = m.gw(0x520);
    if m.lw(state) & 0x7000 == 0 {
        // Event vibrations, each restarting the shake counter.
        let presets: [(u32, Vibration, u16); 3] = [
            (1, vibrate_preset_800291ac, 1),
            (2, vibrate_preset_800291e4, 0xc),
            (4, vibrate_random_80029168, 1),
        ];
        for (event, vibrate, shake) in presets {
            let control = m.gw(0x524);
            if m.lw(control.wrapping_add(0x24)) & event != 0 {
                vibrate(m);
                let state = m.gw(0x520);
                m.sh(state.wrapping_add(0x54), shake);
            }
        }
        let state = m.gw(0x520);
        if m.lh(state.wrapping_add(0x4c)) < 0x400 {
            camera_shake_8006b374(m, 0);
        }
        engine_sound_80070a90(m);
    }
    let state = m.gw(0x520);
    let shake = m.lhu(state.wrapping_add(0x54));
    if shake != 0 {
        m.sh(state.wrapping_add(0x54), shake - 1);
        camera_shake_8006b374(m, 1);
    }
    let state = m.gw(0x520);
    if m.lw(state.wrapping_add(0x48)) as i32 > 0x8_c000 {
        camera_shake_8006b374(m, 2);
        vibrate_80028fd0(m, 0x800);
    }
    // Weapon selection.
    let alternate = crate::shared::f_8003a08c(m, [0; 4]);
    let context = m.gw(0x2ec);
    let mut gun = u32::from(m.lbu(context.wrapping_add(0x3a9)));
    let (weapon, mut count, slots) = if alternate == 0 {
        let weapon = m.lbu(context.wrapping_add(0x3aa));
        let state = m.gw(0x520);
        let count = m.lh(aircraft.wrapping_add(0x7c));
        (weapon, count, state.wrapping_add(0x58))
    } else {
        let weapon = m.lbu(context.wrapping_add(0x3ab));
        let state = m.gw(0x520);
        let count = m.lh(aircraft.wrapping_add(0x7e));
        (weapon, count, state.wrapping_add(0x64))
    };
    let mut weapon = u32::from(weapon);
    let gun_override = gun_override_8003a0f0();
    if gun_override as i32 >= 0 {
        gun = gun_override;
    }
    let weapon_override = crate::shared::f_8003a0f8(m, [0; 4]);
    if weapon_override as i32 >= 0 {
        weapon = weapon_override;
    }
    let capacity = m.lh(WEAPON_CAPACITY.wrapping_add(weapon << 1));
    if capacity < count {
        count = capacity;
    }
    let control = m.gw(0x524);
    m.sw(control.wrapping_add(4), i32::from(count) as u32);
    tick_reload_timers_8007025c(m);
    if flags & 8 != 0 {
        let state = m.gw(0x520);
        let delay = m.lhu(state.wrapping_add(0x56));
        if delay != 0 {
            m.sh(state.wrapping_add(0x56), delay - 1);
        }
        let state = m.gw(0x520);
        let status = m.lw(state.wrapping_add(0x10));
        let context = m.gw(0x2ec);
        m.sw(state.wrapping_add(0x10), status & !0x2000);
        if m.lw(context.wrapping_add(0x128)) & 0x800 != 0 {
            fire_gun_80070a10(m, gun);
        }
        let state = m.gw(0x520);
        let status = m.lw(state.wrapping_add(0x10));
        let context = m.gw(0x2ec);
        m.sw(state.wrapping_add(0x10), status & !0x4000);
        if m.lw(context.wrapping_add(0x12c)) & 0x1000 != 0 {
            fire_weapon_80070a50(m, weapon, slots);
        }
    }
    let control = m.gw(0x524);
    let count = m.lw(control.wrapping_add(4));
    let free = fill_free_slots_8007021c(m, slots, count, 0);
    let control = m.gw(0x524);
    let count = m.lw(control.wrapping_add(4));
    m.sw(control.wrapping_add(0x20), count.wrapping_sub(free));
}

/// Halfword capacity of each special weapon.
const WEAPON_CAPACITY: u32 = 0x800b_3b28;

/// A vibration preset routine (80029588, 800295CC, 80029604).
type Vibration = fn(&mut Machine);

/// 80072460..800724DC (part of 80072178): contact damage of 3, 5 or 100
/// hit points (context level +0x394 <= 0, 1, >= 2; PAL +0x398), event 4,
/// sound 0x27
/// (8002DBE0) unless context +0x28 has bits 0..2, then 80082BAC(position,
/// velocity, state +0x1C, 0).
fn contact_damage(m: &mut Machine, player: u32) {
    let p = |offset: u32| player.wrapping_add(offset);
    let context = m.gw(0x2ec);
    let level = m.lw(context.wrapping_add(0x394)) as i32;
    let mut damage = if level > 0 { 5 } else { 3 };
    if level >= 2 {
        damage = 100;
    }
    let points = m.lhu(p(0x4e));
    let control = m.gw(0x524);
    m.sh(p(0x4e), points.wrapping_sub(damage));
    let events = m.lw(control.wrapping_add(0x24));
    m.sw(control.wrapping_add(0x24), events | 4);
    if m.lw(context.wrapping_add(0x28)) & 7 == 0 {
        external(m, 0x8002_dbe0, [0x27, 0, 0, 0]);
    }
    let state = m.gw(0x520);
    crate::effects::contact_burst_80081564(m, p(8), p(0x38), state.wrapping_add(0x1c), 0);
}

/// A routine owned elsewhere, called by original address (`jal`), with the
/// original argument registers; returns its `v0`.
fn external(m: &mut Machine, address: u32, args: [u32; 4]) -> u32 {
    calls::call(m, address, args)
}

/// 80072404..80072450 (part of 80072178): on a contact, scale entity +0x18
/// and +0x1C by 0xF00/0x1000 (rounding towards zero) and drop a positive
/// vertical velocity (+0x3C).
fn contact_damping(m: &mut Machine, player: u32) {
    let p = |offset: u32| player.wrapping_add(offset);
    let scaled = |value: i16| {
        let product = i32::from(value) * 0xf00;
        let product = if product < 0 {
            product + 0xfff
        } else {
            product
        };
        (product >> 12) as u16
    };
    let a = m.lh(p(0x18));
    let b = m.lh(p(0x1c));
    m.sh(p(0x18), scaled(a));
    m.sh(p(0x1c), scaled(b));
    if m.lw(p(0x3c)) as i32 > 0 {
        m.sw(p(0x3c), 0);
    }
}

/// 8006EF30 (JP; PAL 8006D8D4, near, `del:8 ins:18 op:10 reg:26`): player
/// flight model, one step (called by 800729DC). JP weights both the pitch
/// and the roll command by their share of |pitch| + |roll| (PAL: pitch by
/// min(4 |pitch|, 0x1000), roll by its share).
/// - `entity` (a0): player entity; angles SVECTOR +0x18 (x pitch, y yaw,
///   z roll), angular velocity SVECTOR +0x30, velocity VECTOR +0x38, word
///   +0xC = altitude source (y position).
/// - `control` (a1, `[gp+0x518]`): stick axes from 8006D0DC, halfwords +4
///   pitch, +6 yaw, +8 roll, +0xE throttle (each +-0x1000).
/// - `params` (a2, `[[gp+0x524]]`): aircraft performance table.
/// - `state` (a3, `[gp+0x520]`): player flight state; flags +4 and +0x10,
///   rate SVECTOR +0x30, force SVECTOR +0x38, angles out +0x40, speed +0x48,
///   power +0x4C.
///
/// Steps: velocity in the body frame (800308F0 matrix) and top-speed
/// limiting; 8006EA88 altitude limits; lift ratio and stall flag 0x1000 of
/// `state+0x10`; power; gravity and lift into the velocity change; attitude
/// change from the force vector (80030778 matrices); stick rates (pitch
/// limits by sign, roll share, damping, thin-air scaling) into `state+0x30`;
/// throttle (8006E908) and drag; velocity update; then 80031BB4 (velocity
/// angles into `state+0x40`), 80030F64 (orientation from the angles and
/// rates), 8006EE38 (camera) and 800293F0 (vibration `0x1000 - power`).
/// Frame 0x158 bytes: vector locals whose address is passed stay at their
/// original offsets.
pub fn flight_model_8006d8d4(m: &mut Machine, a0: u32, a1: u32, a2: u32, a3: u32) {
    let (entity, control, params, state) = (a0, a1, a2, a3);
    let at = |base: u32, offset: u32| base.wrapping_add(offset);
    let sp = m.push_frame(0x158);
    let angles = at(entity, 0x18);
    let force = at(state, 0x38);
    let velocity = at(entity, 0x38);
    // sp+0x28: velocity change accumulated over the step.
    let accel = at(sp, 0x28);
    crate::sdk::memset(m, accel, 0, 0x10);
    crate::shared::f_800306ac(m, [angles, at(sp, 0x40), 0, 0]);
    crate::sdk::set_rot_matrix(m, at(sp, 0x40));

    // Body-frame velocity: SVECTOR(-velocity >> 5) rotated; |z| << 5 is the
    // speed. Above the top speed `[params+0x3C]` the velocity is rescaled.
    for i in 0..3u32 {
        let v = m.lw(at(velocity, i * 4)) as i32;
        m.sh(at(sp, 0x38 + i * 2), (v.wrapping_neg() >> 5) as u16);
    }
    fm_rotate(m, at(sp, 0x38), at(sp, 0x60));
    let body_z = m.lw(at(sp, 0x68)) as i32;
    let mut speed = (fm_abs(body_z) << 5) as u32;
    let top_speed = m.lw(at(params, 0x3c));
    if (top_speed as i32) < speed as i32 {
        let ratio = fm_div((top_speed << 6) as i32, speed as i32);
        for i in 0..3u32 {
            let address = at(velocity, i * 4);
            let mut product = (m.lw(address) as i32).wrapping_mul(ratio);
            if product < 0 {
                product = product.wrapping_add(0x3f);
            }
            m.sw(address, (product >> 6) as u32);
        }
        speed = m.lw(at(params, 0x3c));
    }

    let altitude = m.lw(at(entity, 0xc));
    let density = altitude_limits_8006d42c(
        m,
        altitude,
        speed,
        params,
        at(sp, 0x108),
        at(sp, 0x10c),
        at(sp, 0x110),
    ) as i32;
    let mut boost = 0i32;
    let mut gravity = -0x29d_i32;

    // Lift ratio: speed relative to the lift reference (at least 0x1D000),
    // shaped by the force x component and the +0x54..+0x60 limits.
    let mut lift_speed = m.lw(at(sp, 0x108)) as i32;
    if lift_speed <= 0x1_cfff {
        lift_speed = 0x1_d000;
    }
    let mut lift = fm_div((speed << 11) as i32, lift_speed) << 1;
    let mut push = i32::from(m.lh(force));
    let (limit, gain) = if push < 0 {
        let limit = m.lw(at(params, 0x54)) as i32;
        if push < limit {
            push = limit;
        }
        (limit, m.lw(at(params, 0x5c)).wrapping_sub(0x1000) as i32)
    } else {
        let limit = m.lw(at(params, 0x58)) as i32;
        if limit < push {
            push = limit;
        }
        (limit, m.lw(at(params, 0x60)).wrapping_sub(0x1000) as i32)
    };
    let load = fm_div(gain.wrapping_mul(push), limit).wrapping_add(0x1000);
    lift = fm_mul12(lift, load);
    if lift.wrapping_add(load) >= 0x2321 && lift >= 0x1801 && load >= 0x1401 {
        let flags = m.lw(at(state, 0x10));
        m.sw(at(state, 0x10), flags | 0x1000);
    }

    // Power from the lift above `[params+0x64]`; close to that threshold
    // (or below it) a boost blends in.
    let threshold = m.lw(at(params, 0x64)) as i32;
    let power = if lift < threshold {
        boost = 0x1000;
        0
    } else {
        let mut power = fm_mul12(lift.wrapping_sub(threshold), m.lw(at(params, 0x68)) as i32);
        if power >= 0x1001 {
            let excess = fm_mul12(power.wrapping_sub(0x1000), m.lw(at(params, 0x6c)) as i32);
            power = excess.wrapping_add(0x1000);
        }
        let cap = m.lw(at(params, 0x5c)) as i32;
        if cap < power {
            power = cap;
        }
        let margin = lift.wrapping_sub(m.lw(at(params, 0x64)) as i32);
        if margin < 0x100 {
            boost = 0x1000_i32.wrapping_sub(margin << 4);
        }
        power
    };
    let power_cap = if power >= 0x1001 { 0x1000 } else { power };
    m.sh(at(state, 0x4c), power_cap as u16);

    // Turn load: lift (<= 0x1000) scaled by 1 - |pitch| and |tan roll|
    // (cosine at least +-0x400), limited by vector length and 0x1785.
    let pitch_angle = i32::from(m.lh(angles));
    let mut sustain = fm_mul12(
        0x1000_i32.wrapping_sub(fm_abs(pitch_angle)),
        lift.min(0x1000),
    );
    let roll_angle = u32::from(m.lhu(at(angles, 4))) & 0x3fff;
    let mut cosine = i32::from(m.lh(at(SINE_TABLE, (roll_angle + 0x1000) << 1)));
    let sine = i32::from(m.lh(at(SINE_TABLE, roll_angle << 1)));
    if fm_abs(cosine) < 0x401 {
        cosine = if cosine >= 0 { 0x400 } else { -0x400 };
    }
    let tangent = fm_abs(fm_div(sine << 12, cosine));
    let turn = fm_mul12(sustain, tangent);
    let turn_limit = lift.min(0x1785);
    if turn_limit < turn {
        sustain = turn_limit;
    } else {
        sustain = vector_length_800828f8(m, turn as u32, sustain as u32, 0) as i32;
        if turn_limit < sustain {
            sustain = turn_limit;
        }
    }

    // Gravity, reduced by thin air and by the sustained lift, in the body
    // frame of the current angles.
    crate::shared::f_80030534(m, [angles, at(sp, 0x88), 0, 0]);
    crate::sdk::set_rot_matrix(m, at(sp, 0x88));
    if density < 0x1000 {
        gravity = fm_mul12(gravity, density);
    }
    gravity = fm_mul12(gravity, sustain);
    m.sh(at(sp, 0x70), 0);
    m.sh(at(sp, 0x72), gravity as u16);
    m.sh(at(sp, 0x74), 0);
    fm_rotate(m, at(sp, 0x70), at(sp, 0x78));
    fm_accumulate(m, accel, at(sp, 0x78));
    // World-y sink 0x29D * (1 + (0x1000 - density) / 0x1000) (sp+0xAC also
    // keeps the word: its high half later reaches VZ0).
    let thin_sink = 0x1000_i32.wrapping_sub(density).wrapping_mul(0x29d);
    let sink = fm_round12(thin_sink).wrapping_add(0x29d);
    m.sw(at(sp, 0xac), sink as u32);
    m.sw(at(sp, 0xa8), 0);
    m.sw(at(sp, 0xb0), 0);
    let accel_y = m.lw(at(accel, 4)).wrapping_add(sink as u32);
    m.sw(at(accel, 4), accel_y);

    // Attitude change from the force vector in the roll-only frame.
    let yaw_gain = m.lw(at(params, 0x24)) as i32;
    let mut pitch_gain = m.lw(at(params, 0x20)) as i32;
    let mut roll_gain = m.lw(at(params, 0x28)) as i32;
    let pitch_max = m.lw(at(params, 0x14)) as i32;
    let yaw_max = m.lw(at(params, 0x18)) as i32;
    let roll_max = m.lw(at(params, 0x1c)) as i32;
    if boost != 0 {
        pitch_gain = pitch_gain.wrapping_add(boost).min(0x1000);
        roll_gain = roll_gain.wrapping_add(boost).min(0x1000);
    }
    m.sh(at(sp, 0x70), 0);
    m.sh(at(sp, 0x72), 0);
    let roll_word = m.lhu(at(angles, 4));
    m.sh(at(sp, 0x74), roll_word);
    crate::shared::f_80030534(m, [at(sp, 0x70), at(sp, 0x88), 0, 0]);
    crate::sdk::set_rot_matrix(m, at(sp, 0x88));
    fm_rotate(m, force, at(sp, 0x78));
    if m.lw(at(state, 4)) & 0xc108 != 0 {
        pitch_gain >>= 2;
    }
    let turn_x = fm_mul12(m.lw(at(sp, 0x78)) as i32, pitch_gain);
    let turn_y = fm_mul12(m.lw(at(sp, 0x7c)) as i32, yaw_gain);
    let turn_z = fm_mul12(m.lw(at(sp, 0x80)) as i32, roll_gain);
    let turn_x = fm_clamp_seq(turn_x, pitch_max);
    let turn_y = fm_clamp_seq(turn_y, yaw_max);
    let turn_z = fm_clamp_seq(turn_z, roll_max);
    if !m.lw(at(state, 0x10)) & 2 != 0 {
        let x = m.lhu(angles);
        let z = m.lhu(at(angles, 4));
        m.sh(angles, x.wrapping_add(turn_x as u16));
        let y = m.lhu(at(angles, 2));
        m.sh(at(angles, 4), z.wrapping_add(turn_z as u16));
        m.sh(at(angles, 2), y.wrapping_add(turn_y as u16));
    }
    // With a boost and state+0x10 bits 0, 1 and 3 clear, the nose drops by
    // 37 * boost / 4096 while the pitch stays above -0x1000 - drop.
    if boost != 0 {
        let flags = !m.lw(at(state, 0x10));
        if flags & 1 != 0 && flags & 0xa == 0xa {
            let drop = fm_round12(boost.wrapping_mul(37).wrapping_neg());
            let floor = (-0x1000_i32).wrapping_sub(drop);
            if floor < i32::from(m.lh(angles)) {
                let x = m.lhu(angles);
                m.sh(angles, x.wrapping_add(drop as u16));
            }
        }
    }

    // Stick rates. Pitch uses `[params+0]` (stick >= 0) or `[params+4]`
    // (stick < 0), raised with |pitch stick| by up to 0xC78 minus a
    // force-dependent part; pitch and roll are weighted by their shares of
    // |pitch| + |roll| (8006F930..8006F9A4).
    let pitch_stick = i32::from(m.lh(at(control, 4)));
    let yaw_stick = i32::from(m.lh(at(control, 6)));
    let roll_stick = i32::from(m.lh(at(control, 8)));
    let mut pitch_rate = if pitch_stick < 0 {
        m.lw(at(params, 4)) as i32
    } else {
        m.lw(params) as i32
    };
    let mut loading = i32::from(m.lh(force)).wrapping_mul((speed as i32) >> 1);
    if loading < 0 {
        loading = loading.wrapping_add(0x1fff);
    }
    let loading = fm_abs(loading >> 13).min(0x3200);
    let relief = fm_div(loading.wrapping_mul(0xc78), 0x3200);
    let pitch_amount = fm_abs(i32::from(m.lh(at(control, 4))));
    let raise = fm_mul12(0xc78_i32.wrapping_sub(relief), pitch_amount);
    pitch_rate = pitch_rate.wrapping_add(fm_mul12(pitch_rate, raise));
    let roll_amount = fm_abs(i32::from(m.lh(at(control, 8))));
    let total = pitch_amount.wrapping_add(roll_amount);
    let (pitch_share, roll_share) = if total == 0 {
        (0, 0)
    } else {
        (
            fm_div(pitch_amount << 12, total),
            fm_div(roll_amount << 12, total),
        )
    };
    let pitch_command = fm_mul12(fm_mul12(pitch_stick, pitch_share), power);
    let roll_command = fm_mul12(fm_mul12(roll_stick, roll_share), power);
    let yaw_command = fm_mul12(yaw_stick, 0x1000_i32.wrapping_sub(boost));
    let yaw_rate = m.lw(at(params, 8)) as i32;
    let roll_rate = m.lw(at(params, 0xc)) as i32;
    let target_x = fm_mul12(pitch_command, pitch_rate);
    let target_y = fm_mul12(yaw_command, yaw_rate);
    let target_z = fm_mul12(roll_command, roll_rate);
    let response = m.lw(at(params, 0x10)) as i32;
    let rate_x = i32::from(m.lh(at(state, 0x30))).wrapping_add(fm_mul12(target_x, response));
    let rate_y = i32::from(m.lh(at(state, 0x32))).wrapping_add(fm_mul12(target_y, response));
    let rate_z = i32::from(m.lh(at(state, 0x34))).wrapping_add(fm_mul12(target_z, response));
    let mut rate_x = fm_clamp_either(rate_x, pitch_rate);
    let yaw_limit = m.lw(at(params, 8)) as i32;
    let mut rate_y = fm_clamp_either(rate_y, yaw_limit);
    let roll_limit = m.lw(at(params, 0xc)) as i32;
    let mut rate_z = fm_clamp_either(rate_z, roll_limit);
    // sp+0x20..0x24 in the original: the clamped rates, later the entity's
    // angular velocity.
    let entity_rates = [rate_x as u16, rate_y as u16, rate_z as u16];

    // Damping toward the targets: a rate beyond its target is scaled by
    // `damping` but not below the target.
    let damping = if power >= 0x1001 {
        0x1000_i32.wrapping_sub(m.lw(at(params, 0x10)) as i32)
    } else {
        let loss = fm_mul12(m.lw(at(params, 0x10)) as i32, power);
        0x1000_i32.wrapping_sub(loss).min(0xffc)
    };
    for (rate, target) in [
        (&mut rate_x, target_x),
        (&mut rate_y, target_y),
        (&mut rate_z, target_z),
    ] {
        if fm_abs(target) < fm_abs(*rate) {
            *rate = fm_mul12(*rate, damping);
            if fm_abs(*rate) < fm_abs(target) {
                *rate = target;
            }
        }
    }
    if density < 0x1000 {
        let thin = density.max(0x80);
        rate_x = fm_mul12(rate_x, thin);
        rate_y = fm_mul12(rate_y, thin);
        rate_z = fm_mul12(rate_z, thin);
    }
    let mode = m.lw(at(state, 4));
    m.sh(at(state, 0x30), rate_x as u16);
    m.sh(at(state, 0x32), rate_y as u16);
    m.sh(at(state, 0x34), rate_z as u16);

    // Throttle: mode overrides of the three speeds (and half drag), then
    // 8006E908 with the throttle stick (raised by |force x| << 2 at full
    // power when not braking).
    let mut drag = m.lw(at(params, 0x48)) as i32;
    if mode & 8 != 0 {
        drag >>= 1;
        m.sw(at(sp, 0x108), 0xa000);
        m.sw(at(sp, 0x10c), 0x1_0000);
        m.sw(at(sp, 0x110), 0x1_a000);
    } else if mode & 0x100 != 0 {
        m.sw(at(sp, 0x108), 0x8000);
        m.sw(at(sp, 0x10c), 0x1_9a00);
        m.sw(at(sp, 0x110), 0x3_7000);
    } else if mode & 0xc000 != 0 {
        drag >>= 1;
        m.sw(at(sp, 0x108), 0x1000);
        m.sw(at(sp, 0x10c), 0xf000);
        m.sw(at(sp, 0x110), 0x3_c000);
    }
    if (m.lw(at(sp, 0x108)) as i32) < 0 {
        m.sw(at(sp, 0x108), 0);
    }
    let mut throttle = i32::from(m.lh(at(control, 0xe)));
    if throttle >= 0 && power_cap >= 0x1000 {
        throttle = throttle.wrapping_add(fm_abs(i32::from(m.lh(force))) << 2);
        if throttle < -0x1000 {
            throttle = -0x1000;
        }
        if throttle >= 0x1001 {
            throttle = 0x1000;
        }
    } else {
        throttle = i32::from(m.lh(at(control, 0xe)));
    }
    let max_speed = m.lw(at(sp, 0x110));
    let cruise_speed = m.lw(at(sp, 0x10c));
    let altitude = m.lw(at(entity, 0xc));
    let change = speed_change_8006d2ac(
        speed,
        max_speed,
        cruise_speed,
        throttle as u32,
        altitude,
        drag as u32,
        drag as u32,
    ) as i32;
    let mut thrust = if change < -0x2000 {
        -0x2000
    } else if change >= 0x2001 {
        0x2000
    } else {
        change
    };
    // Thrust limit: `[params+0x4C]`, or nose up interpolated toward
    // `[params+0x50]` by 1 - sin(pitch) / 4; then scaled by the power.
    let pitch_word = m.lhu(angles);
    let climb_floor = m.lw(at(params, 0x50)) as i32;
    let pitch_sine = m.lhu(at(SINE_TABLE, (u32::from(pitch_word) & 0x3fff) << 1));
    let climb = 0x1000_i32.wrapping_sub(i32::from(pitch_sine as i16) >> 2);
    let thrust_max = m.lw(at(params, 0x4c)) as i32;
    let thrust_limit = if (pitch_word as i16) <= 0 {
        thrust_max
    } else {
        fm_mul12(thrust_max.wrapping_sub(climb_floor), climb).wrapping_add(climb_floor)
    };
    if thrust_limit < thrust {
        thrust = thrust_limit;
    }
    if thrust < thrust_limit.wrapping_neg() {
        thrust = thrust_limit.wrapping_neg();
    }
    let thrust = fm_mul12(thrust, power_cap);

    // Thrust along the body z axis (lift loss below full density for a
    // downward y part).
    m.sh(at(sp, 0xa8), 0);
    m.sh(at(sp, 0xaa), 0);
    m.sh(at(sp, 0xac), thrust as u16);
    crate::shared::f_80030534(m, [angles, at(sp, 0xb8), 0, 0]);
    crate::sdk::set_rot_matrix(m, at(sp, 0xb8));
    fm_rotate(m, at(sp, 0xa8), at(sp, 0xd8));
    let thrust_y = m.lw(at(sp, 0xdc)) as i32;
    if thrust_y < 0 && density < 0x1000 {
        m.sw(at(sp, 0xdc), fm_mul12(thrust_y, density) as u32);
    }
    fm_accumulate(m, accel, at(sp, 0xd8));
    m.sw(at(state, 0x48), speed);

    // Drag: -velocity * [params+0x70] (sp+0xA8), and its length / 2 as a
    // body-frame force along z, doubled.
    let drag_factor = fm_round12((m.lw(at(params, 0x70)) << 12) as i32);
    for i in 0..3u32 {
        let v = (m.lw(at(velocity, i * 4)) as i32).wrapping_neg();
        m.sw(at(sp, 0xa8 + i * 4), fm_mul12(v, drag_factor) as u32);
    }
    crate::shared::f_80030534(m, [angles, at(sp, 0xe8), 0, 0]);
    crate::sdk::set_rot_matrix(m, at(sp, 0xe8));
    let half_x = (m.lw(at(sp, 0xa8)) as i32) >> 1;
    let half_y = (m.lw(at(sp, 0xac)) as i32) >> 1;
    let half_z = (m.lw(at(sp, 0xb0)) as i32) >> 1;
    let length = vector_length_800828f8(m, half_x as u32, half_y as u32, half_z as u32);
    m.sh(at(sp, 0xbc), length as u16);
    m.sh(at(sp, 0xb8), 0);
    m.sh(at(sp, 0xba), 0);
    fm_rotate(m, at(sp, 0xb8), at(sp, 0xc0));
    for i in 0..3u32 {
        let value = m.lw(at(sp, 0xc0 + i * 4));
        m.sw(at(sp, 0xc0 + i * 4), value << 1);
    }
    let lift_y = m.lw(at(sp, 0xc4)) as i32;
    if density < 0x1000 && lift_y < 0 {
        m.sw(at(sp, 0xc4), fm_mul12(lift_y, density) as u32);
    }
    fm_accumulate(m, accel, at(sp, 0xc0));
    let drag_y = m.lw(at(sp, 0xac)) as i32;
    if density < 0x1000 && drag_y < 0 {
        m.sw(at(sp, 0xac), fm_mul12(drag_y, density) as u32);
    }
    fm_accumulate(m, accel, at(sp, 0xa8));
    fm_accumulate(m, velocity, accel);

    vector_angles_80031970(m, velocity, at(state, 0x40));
    for (i, rate) in entity_rates.into_iter().enumerate() {
        m.sh(at(entity, 0x30 + i as u32 * 2), rate);
    }
    orientation_80030d20(m, angles, at(entity, 0x30));
    aim_camera_8006d7dc(m, entity, state);
    vibrate_80028fd0(m, 0x1000_u32.wrapping_sub(power_cap as u32));
    m.pop_frame(0x158);
}

/// Add the VECTOR at `delta` to the VECTOR at `sum` (wrapping words).
fn fm_accumulate(m: &mut Machine, sum: u32, delta: u32) {
    for i in 0..3u32 {
        let a = m.lw(sum.wrapping_add(i * 4));
        let b = m.lw(delta.wrapping_add(i * 4));
        m.sw(sum.wrapping_add(i * 4), a.wrapping_add(b));
    }
}

/// Low word of `a * b` (`mult; mflo`) divided by 4096 toward zero.
fn fm_mul12(a: i32, b: i32) -> i32 {
    fm_round12(a.wrapping_mul(b))
}

/// `value / 4096` rounded toward zero (`bgez; addiu 0xFFF; sra 12`).
fn fm_round12(value: i32) -> i32 {
    if value < 0 {
        value.wrapping_add(0xfff) >> 12
    } else {
        value >> 12
    }
}

/// `lwc2 VXY0/VZ0` from the SVECTOR at `vector` (both whole words, so the
/// pad halfword reaches VZ0), MVMVA sf=1 lm=0 (rotation x V0, no
/// translation), then `swc2 MAC1..MAC3` to the VECTOR at `out`.
fn fm_rotate(m: &mut Machine, vector: u32, out: u32) {
    let xy = m.lw(vector);
    m.gte.mtc2(reg::VXY0, xy);
    let z = m.lw(vector.wrapping_add(4));
    m.gte.mtc2(reg::VZ0, z);
    m.gte.mvmva(true, false, 0, 0, 3);
    for i in 0..3u32 {
        let value = m.gte.mfc2(reg::MAC1 + i as usize);
        m.sw(out.wrapping_add(i * 4), value);
    }
}

/// 800104AC: resident sine table (halfwords, 1.0 = 0x1000). Entry
/// `angle & 0x3FFF` is the sine, entry `(angle & 0x3FFF) + 0x1000` the cosine.
const SINE_TABLE: u32 = 0x8001_04ac;

/// `bgez x; subu x, zero, x`: wrapping absolute value.
fn fm_abs(value: i32) -> i32 {
    if value < 0 {
        value.wrapping_neg()
    } else {
        value
    }
}

/// Branching symmetric clamp: below `-limit` gives `-limit`, otherwise above
/// `limit` gives `limit` (the form at 8006FAE4..8006FB4C).
fn fm_clamp_either(value: i32, limit: i32) -> i32 {
    let low = limit.wrapping_neg();
    if value < low {
        low
    } else if limit < value {
        limit
    } else {
        value
    }
}

/// Quotient (LO) of the original `div`: truncating; a zero divisor gives -1
/// (1 for a negative dividend) and `i32::MIN / -1` gives `i32::MIN`
/// (FLIGHT_MACHINE rule 2). The compiler's checks after every `div` would
/// execute `break 7` (zero divisor) or `break 6` (overflow) on the console;
/// those traps are not modelled.
fn fm_div(numerator: i32, divisor: i32) -> i32 {
    if divisor == 0 {
        if numerator < 0 { 1 } else { -1 }
    } else {
        numerator.wrapping_div(divisor)
    }
}

/// Sequential symmetric clamp: first `-limit` from below, then `limit` from
/// above (so a negative `limit` yields `limit`).
fn fm_clamp_seq(value: i32, limit: i32) -> i32 {
    let mut value = value;
    if value < limit.wrapping_neg() {
        value = limit.wrapping_neg();
    }
    if limit < value {
        value = limit;
    }
    value
}

/// 800729DC (JP; PAL 80071358, near, `imm:4`: sound requests 0x5A/0x5B for
/// PAL 0x5F/0x62, the counter at context +0x958 for PAL +0x95C): player
/// flight update (called by 80032F3C with no arguments).
/// Dispatches on the player state word `[gp+0x520]` +4 (one bit per mode),
/// then updates the flight camera block 0x800BD0A0 for modes 8/0x10 (+4 &
/// 0x18) and the target view for modes 0x100..0x1000 (+4 & 0x1F00), and
/// finally the crash/landing check at entity +0x4E.
///
/// Mode handlers (entity = `[[gp+0x2E8]]`, motion = `[[gp+0x524]]`):
/// - 1, 4: controls 80072938, speed scaling 8006E874, the output +8
///   clamp to +-0x100 when state +0x10 & 0xF, flight model 8006EF30,
///   effects 80072178(-1 / -9).
/// - 2: outputs cleared, flight model, effects(0x47).
/// - 0x200000 / 0x400000: auto-level 80073B18(.., 1 / 0), flight model,
///   effects(0x47).
/// - 8: 80076428 area check (out-of-area sounds/modes); 0x10: scripted
///   fly-in 80074BA8; 0x20 / 0x40: ground roll 80074878; 0x100: formation
///   check 80076924; 0x200: follow 80074E64; 0x400: join formation
///   80075084; 0x800: scripted control 8007542C; 0x1000: effects only;
///   0x2000: placed on the ground 8007599C; 0x4000 / 0x8000: take-off run
///   80075B04; 0x10000: airborne start 80075E08; 0x100000: crash 80074560;
///   0x1000000 / 0x2000000 / 0x4000000: pad step 8006D678 and the free
///   flight model 800702D8 (0x4000000 also exit checks 8007601C/80076AC0);
///   0x8000000 / 0x10000000: exit flight with 8007407C / cleared controls;
///   0x20000000: engine sound only.
///
/// Only modes 1, 2, 4 and 0x100000 occur in the first mission's captures;
/// the others are checked against the original instructions (dev oracle).
pub fn update_player_80071358(m: &mut Machine) {
    const FRAME: u32 = 0x128;
    let sp = m.push_frame(FRAME);
    let table = m.gw(crate::common::ENTITIES);
    let state = m.gw(0x520); // t0
    let motion_block = m.gw(0x524);
    let entity = m.lw(table); // s2
    let mode = m.lw(state.wrapping_add(4));
    let motion = m.lw(motion_block); // s1
    match mode {
        1 | 4 => {
            control_input_800712b4(m, entity, motion);
            scale_controls(m);
            let state = m.gw(0x520);
            if m.lw(state.wrapping_add(0x10)) & 0xf != 0 {
                let out = m.gw(0x518);
                if i32::from(m.lh(out.wrapping_add(8))) >= 0x101 {
                    m.sh(out.wrapping_add(8), 0x100);
                }
                let out = m.gw(0x518);
                if i32::from(m.lh(out.wrapping_add(8))) < -0x100 {
                    m.sh(out.wrapping_add(8), 0xff00);
                }
            }
            run_flight_model(m, entity, motion);
            player_effects_80070af4(m, if mode == 1 { u32::MAX } else { -9i32 as u32 });
        }
        2 => {
            let out = m.gw(0x518);
            clear_outputs(m, out);
            flight_model_8006d8d4(m, entity, out, motion, state);
            player_effects_80070af4(m, 0x47);
        }
        8 => {
            control_input_800712b4(m, entity, motion);
            let view = m.lw(VIEW);
            let result = approach_area_80074db8(m, entity.wrapping_add(8), view);
            let state = m.gw(0x520);
            if m.lw(state.wrapping_add(0x10)) & 9 != 0 {
                let mut setting_loaded = None;
                if result == 0 {
                    m.sh(entity.wrapping_add(0x4e), 0xffff);
                } else if result == 1 {
                    let flags = m.lw(state);
                    m.sw(state, flags | 0x80);
                    external(m, 0x8002_db4c, [0x5a, 0, 0, 0]);
                    let setting = m.lw(SETTING);
                    if setting == 1 {
                        external(m, 0x8002_db4c, [0x5b, 0, 0, 0]);
                    } else {
                        setting_loaded = Some(setting);
                    }
                } else {
                    let flags = m.lw(state);
                    m.sw(state, flags | 0x100);
                }
                let setting = match setting_loaded {
                    Some(value) => value,
                    None => m.lw(SETTING),
                };
                set_approach_mode_800754a8(m, u32::from(setting != 0), 2);
                let state = m.gw(0x520);
                m.sh(state.wrapping_add(0x4e), 0x1e);
            }
            run_flight_model(m, entity, motion);
            let flags = if result == 0 { -9i32 } else { -0x19 };
            player_effects_80070af4(m, flags as u32);
            let camera = m.lw(CAMERA_OUT);
            m.sw(camera.wrapping_add(0x34), result);
        }
        0x10 => {
            let controls = m.gw(0x51c);
            let view = m.lw(VIEW);
            let result = scripted_fly_in_80073524(m, entity, state, controls, view);
            if result != 0 {
                let first = u32::from(m.lw(SETTING) != 0);
                set_approach_mode_800754a8(m, first, 2);
            }
            player_effects_80070af4(m, 6);
            let camera = m.lw(CAMERA_OUT);
            m.sw(camera.wrapping_add(0x34), 1);
        }
        0x20 => {
            ground_roll_800731f4(m, entity, state, 0);
            player_effects_80070af4(m, 0x46);
            let position = entity.wrapping_add(8);
            let view = m.lw(VIEW);
            let result = approach_area_80074db8(m, position, view);
            let state = m.gw(0x520);
            if m.lw(state.wrapping_add(0x10)) & 9 != 0 && result == 0 {
                let motion_block = m.gw(0x524);
                let flags = m.lw(motion_block.wrapping_add(0x24));
                let context = m.gw(crate::common::CONTEXT);
                m.sw(motion_block.wrapping_add(0x24), flags | 4);
                if m.lw(context.wrapping_add(0x28)) & 7 == 0 {
                    external(m, 0x8002_dbe0, [0x27, 0, 0, 0]);
                }
                let state = m.gw(0x520);
                crate::effects::contact_burst_80081564(
                    m,
                    position,
                    entity.wrapping_add(0x38),
                    state.wrapping_add(0x1c),
                    0,
                );
                let state = m.gw(0x520);
                let timer = m.lhu(state.wrapping_add(0x4e)).wrapping_sub(1);
                m.sh(state.wrapping_add(0x4e), timer);
                if timer as i16 <= 0 {
                    m.sh(entity.wrapping_add(0x4e), 0xffff);
                }
            }
        }
        0x40 => {
            ground_roll_800731f4(m, entity, state, 1);
            let view = m.lw(VIEW);
            let result = approach_area_80074db8(m, entity.wrapping_add(8), view);
            let state = m.gw(0x520);
            if m.lw(state.wrapping_add(0x10)) & 9 != 0 && result == 0 {
                m.sh(entity.wrapping_add(0x4e), 0xffff);
            }
            player_effects_80070af4(m, 6);
        }
        0x100 => {
            control_input_800712b4(m, entity, motion);
            run_flight_model(m, entity, motion);
            let target = m.lw(TARGET);
            let aux = m.lw(TARGET_AUX);
            let found = formation_check_800752b4(m, entity, target, aux, sp.wrapping_add(0x18));
            player_effects_80070af4(m, -9i32 as u32);
            let state = m.gw(0x520);
            if m.lw(state.wrapping_add(8)) == 0 {
                m.sw(state.wrapping_add(8), 1);
                m.sw(state.wrapping_add(0xc), 0);
            }
            let mut counted = false;
            if found != 0 {
                let state = m.gw(0x520);
                if m.lw(state.wrapping_add(8)) == 1 {
                    let count = m.lw(state.wrapping_add(0xc)).wrapping_add(1);
                    m.sw(state.wrapping_add(0xc), count);
                    counted = true;
                }
            }
            if !counted {
                let state = m.gw(0x520);
                m.sw(state.wrapping_add(0xc), 0);
            }
            let state = m.gw(0x520);
            if 0x2d < m.lw(state.wrapping_add(0xc)) as i32 {
                let flags = m.lw(state);
                let first = setting_is_not_two(m);
                m.sw(state, flags | 0x80);
                set_target_mode_800755e4(m, first, 2);
            }
            let camera = m.lw(CAMERA_OUT);
            m.sw(camera.wrapping_add(0x34), found);
        }
        0x200 => {
            let controls = m.gw(0x51c);
            let target = m.lw(TARGET);
            follow_entity_800737e0(m, entity, state, controls, target);
            player_effects_80070af4(m, 4);
            let first = setting_is_not_two(m);
            set_target_mode_800755e4(m, first, 2);
        }
        0x400 => {
            let target = m.lw(TARGET);
            let index = m.lw(target);
            let aux = m.lw(TARGET_AUX);
            let other = m.lw(index.wrapping_shl(2).wrapping_add(table));
            formation_check_800752b4(m, entity, target, aux, sp.wrapping_add(0x58));
            let state = m.gw(0x520);
            let result = join_formation_80073a08(
                m,
                entity,
                state,
                sp.wrapping_add(0x7c),
                other.wrapping_add(0x38),
            );
            player_effects_80070af4(m, 4);
            if result != 0 {
                let first = setting_is_not_two(m);
                set_target_mode_800755e4(m, first, 3);
            }
        }
        0x800 => {
            let rates = m.gw(0x514);
            let out = m.gw(0x518);
            let controls = m.gw(0x51c);
            m.sw(sp.wrapping_add(0x10), controls);
            scripted_control_80073db0(m, entity, state, rates, out, controls);
            run_flight_model(m, entity, motion);
            player_effects_80070af4(m, 4);
            let camera = m.lw(CAMERA_OUT);
            m.sw(camera.wrapping_add(0x34), 0);
        }
        0x1000 => player_effects_80070af4(m, 4),
        0x2000 => {
            let view = m.lw(VIEW);
            place_on_ground_80074320(m, entity, state, view);
            scale_controls(m);
            let motion_block = m.gw(0x524);
            let state = m.gw(0x520);
            m.sw(motion_block.wrapping_add(0x2c), 0);
            m.sw(state.wrapping_add(0x48), 0);
            let out = m.gw(0x518);
            m.sh(out.wrapping_add(0xe), 0xf000);
            engine_sound_80070a90(m);
        }
        0x4000 | 0x8000 => {
            let state = m.gw(0x520);
            let out = m.gw(0x518);
            let rates = m.gw(0x514);
            let controls = m.gw(0x51c);
            m.sh(rates.wrapping_add(6), 0x1000);
            let replay = u32::from(m.lw(state.wrapping_add(4)) ^ 0x8000 == 0); // s0
            let view = m.lw(VIEW);
            m.sw(sp.wrapping_add(0x10), view);
            m.sw(sp.wrapping_add(0x14), replay);
            let result = takeoff_run_80074488(m, entity, state, out, controls, view, replay);
            if result == 0 {
                engine_sound_80070a90(m);
            } else {
                control_input_800712b4(m, entity, motion);
                let state = m.gw(0x520);
                let speed = m.lw(state.wrapping_add(0x48));
                let out = m.gw(0x518);
                scale_outputs_by_speed_8006d184(m, speed, out);
                scale_controls(m);
                run_flight_model(m, entity, motion);
                player_effects_80070af4(m, -0x29i32 as u32);
                let view = m.lw(VIEW);
                if outside_area_800753c8(m, entity.wrapping_add(8), view) != 0 {
                    set_start_mode_8007577c(m, replay, 2);
                }
            }
        }
        0x10000 => {
            let out = m.gw(0x518);
            let controls = m.gw(0x51c);
            let view = m.lw(VIEW);
            m.sw(sp.wrapping_add(0x14), 0);
            m.sw(sp.wrapping_add(0x10), view);
            airborne_start_80074798(m, entity, state, out, controls, view, 0);
            set_start_mode_8007577c(m, 0, 2);
        }
        0x100000 => {
            crash_80072edc(m, entity, state);
            player_effects_80070af4(m, 6);
        }
        0x200000 | 0x400000 => {
            let rates = m.gw(0x514);
            let out = m.gw(0x518);
            let controls = m.gw(0x51c);
            let climb = u32::from(mode == 0x200000);
            m.sw(sp.wrapping_add(0x10), climb);
            auto_level_80072494(m, entity, rates, out, controls, climb);
            run_flight_model(m, entity, motion);
            player_effects_80070af4(m, 0x47);
        }
        0x1000000 | 0x2000000 | 0x4000000 => {
            pad_mode_sequence(m, entity, motion);
            match mode {
                0x1000000 => player_effects_80070af4(m, -0x53i32 as u32),
                0x2000000 => player_effects_80070af4(m, -0x5bi32 as u32),
                _ => {
                    let state = m.gw(0x520);
                    exit_flight_800749ac(m, entity, state);
                    player_effects_80070af4(m, -0x5bi32 as u32);
                    let result = exit_attitude_80075450(m, entity.wrapping_add(8));
                    let state = m.gw(0x520);
                    if m.lw(state) == 0x10 && result != 0 {
                        let count = m.lhu(state.wrapping_add(0x50)).wrapping_add(2);
                        m.sh(state.wrapping_add(0x50), count);
                        if count as i16 >= 0x3d {
                            set_exit_mode_80074cf0(m, 1);
                            let state = m.gw(0x520);
                            let flags = m.lw(state);
                            m.sw(state, flags | 0x280);
                        }
                    }
                }
            }
        }
        0x8000000 => {
            let value = m.lh(entity.wrapping_add(0x1c));
            let raw = m.lhu(entity.wrapping_add(0x1c));
            m.sh(entity.wrapping_add(0x1a), 0);
            if value < 4 {
                m.sh(entity.wrapping_add(0x1c), raw.wrapping_add(4));
            }
            let value = m.lh(entity.wrapping_add(0x1c));
            let raw = m.lhu(entity.wrapping_add(0x1c));
            if value >= -3 {
                m.sh(entity.wrapping_add(0x1c), raw.wrapping_sub(4));
            }
            let context = m.gw(crate::common::CONTEXT);
            let manual = m.lw(context.wrapping_add(4)) != 0
                || i32::from(m.lh(entity.wrapping_add(0x1c))).wrapping_abs() >= 0x223;
            if manual {
                let rates = m.gw(0x514);
                let out = m.gw(0x518);
                let controls = m.gw(0x51c);
                auto_climb_800729f8(m, entity, rates, out, controls);
            } else {
                let out = m.gw(0x518);
                let rates = m.gw(0x514);
                for offset in [8, 6, 4, 0xe, 0xc, 0xa] {
                    m.sh(out.wrapping_add(offset), 0);
                }
                for offset in [0, 2, 4, 6] {
                    m.sh(rates.wrapping_add(offset), 0);
                }
            }
            scale_controls(m);
            let out = m.gw(0x518);
            let state = m.gw(0x520);
            flight_model_free_8006ec54(m, entity, out, motion, state);
            let state = m.gw(0x520);
            exit_flight_800749ac(m, entity, state);
            player_effects_80070af4(m, -0x5bi32 as u32);
        }
        0x10000000 => {
            let out = m.gw(0x518);
            clear_outputs(m, out);
            flight_model_free_8006ec54(m, entity, out, motion, state);
            let state = m.gw(0x520);
            exit_flight_800749ac(m, entity, state);
            player_effects_80070af4(m, -0x5bi32 as u32);
        }
        0x20000000 => engine_sound_80070a90(m),
        _ => {}
    }

    let state = m.gw(0x520);
    if m.lw(state.wrapping_add(4)) & 0x18 != 0 {
        update_flight_camera(m, sp, entity);
    }
    let state = m.gw(0x520);
    if m.lw(state.wrapping_add(4)) & 0x1f00 != 0 {
        update_target_view(m, sp, entity);
    }
    if m.lh(entity.wrapping_add(0x4e)) <= 0 {
        let state = m.gw(0x520);
        if m.lw(state) & 0x7000 == 0 {
            let context = m.gw(crate::common::CONTEXT);
            let count = m.lhu(context.wrapping_add(0x958)).wrapping_add(1);
            m.sh(context.wrapping_add(0x958), count);
            enter_crash_80074d5c(m);
            let gameplay = m.gw(crate::common::GAMEPLAY);
            let which = if !m.lw(gameplay) & 0x10 == 0 {
                2
            } else {
                let state = m.gw(0x520);
                u32::from(m.lw(state.wrapping_add(0x10)) & 0x2f != 0)
            };
            crash_camera_80077ef4(m, which);
        }
    }
    m.pop_frame(FRAME);
}

/// Tail of 800729DC for modes with state +4 & 0x1F00: 80076924 (entity,
/// target pair, frame +0xD0) and a copy of its results plus the target
/// entity's angles into 0x800BD0A0.
fn update_target_view(m: &mut Machine, sp: u32, entity: u32) {
    let target = m.lw(TARGET);
    let aux = m.lw(TARGET_AUX);
    let index = m.lw(target);
    let table = m.gw(crate::common::ENTITIES);
    let other = m.lw(index.wrapping_shl(2).wrapping_add(table));
    let result = formation_check_800752b4(m, entity, target, aux, sp.wrapping_add(0xd0));
    let camera = m.lw(CAMERA_OUT);
    for i in 0..3 {
        let value = m.lhu(other.wrapping_add(0x18 + i * 2));
        m.sh(camera.wrapping_add(0x10 + i * 2), value);
    }
    for i in 0..3 {
        let value = m.lw(sp.wrapping_add(0xd0 + i * 4));
        m.sw(camera.wrapping_add(i * 4), value);
    }
    for i in 0..3 {
        let value = m.lw(sp.wrapping_add(0xf4 + i * 4));
        m.sw(camera.wrapping_add(0x1c + i * 4), value);
    }
    for i in 0..3 {
        let value = m.lhu(sp.wrapping_add(0x104 + i * 2));
        m.sh(camera.wrapping_add(0x2c + i * 2), value);
    }
    let distance = m.lw(sp.wrapping_add(0xf0));
    m.sw(camera.wrapping_add(0x34), result);
    m.sw(camera.wrapping_add(0x18), distance);
}

/// `[0x800BD0A0]`: flight camera output block.
const CAMERA_OUT: u32 = 0x800b_d0a0;

const TARGET_AUX: u32 = 0x800b_ce20;

/// `[0x800BCE1C]`, `[0x800BCE20]`: the target selection pair passed to
/// 80076924 (word 0 of the first is an entity index).
const TARGET: u32 = 0x800b_ce1c;

/// Tail of 800729DC for modes with state +4 & 0x18: camera rotation from
/// the view block (80030778 into the frame at +0x98), the view-relative
/// position (entity +8 minus view, >> 12) rotated by it (RTV0, sf=1), and
/// the wrapped angle differences, all into 0x800BD0A0; +0x18 gets the
/// distance 80083DFC (view, entity +8).
fn update_flight_camera(m: &mut Machine, sp: u32, entity: u32) {
    let matrix = sp.wrapping_add(0x98);
    let view = m.lw(VIEW);
    crate::shared::f_80030534(m, [view.wrapping_add(0x10), matrix, 0, 0]);
    let position = entity.wrapping_add(8);
    for i in 0..3 {
        let delta = m
            .lw(position.wrapping_add(i * 4))
            .wrapping_sub(m.lw(view.wrapping_add(i * 4)));
        m.sh(sp.wrapping_add(0xb8 + i * 2), ((delta as i32) >> 12) as u16);
    }
    crate::sdk::set_rot_matrix(m, matrix);
    let xy = m.lw(sp.wrapping_add(0xb8));
    let z = m.lw(sp.wrapping_add(0xbc));
    m.gte.mtc2(reg::VXY0, xy);
    m.gte.mtc2(reg::VZ0, z);
    m.gte.mvmva(true, false, 0, 0, 3);
    for i in 0..3 {
        let value = m.gte.mfc2(reg::MAC1 + i);
        m.sw(sp.wrapping_add(0xc0 + i as u32 * 4), value);
    }
    let camera = m.lw(CAMERA_OUT);
    for i in 0..3 {
        let value = m.lw(sp.wrapping_add(0xc0 + i * 4));
        m.sw(camera.wrapping_add(0x1c + i * 4), value << 12);
    }
    let state = m.gw(0x520);
    let mut angles = [
        i32::from(m.lh(state.wrapping_add(0x40))),
        i32::from(m.lh(state.wrapping_add(0x42))),
        i32::from(m.lh(state.wrapping_add(0x44))),
    ];
    let reference0 = m.lhu(view.wrapping_add(0x10));
    for (i, angle) in angles.iter_mut().enumerate() {
        let reference = i32::from(m.lh(view.wrapping_add(0x10 + i as u32 * 2)));
        let mut delta = reference.wrapping_sub(*angle);
        if delta < 0x2000 {
            *angle = angle.wrapping_sub(0x4000);
            delta = reference.wrapping_sub(*angle);
        }
        if delta >= 0x2001 {
            *angle = angle.wrapping_add(0x4000);
        }
    }
    m.sh(
        camera.wrapping_add(0x2c),
        (angles[0] as u16).wrapping_sub(reference0),
    );
    let reference = m.lhu(view.wrapping_add(0x12));
    m.sh(
        camera.wrapping_add(0x2e),
        (angles[1] as u16).wrapping_sub(reference),
    );
    let reference = m.lhu(view.wrapping_add(0x14));
    let view_again = m.lw(VIEW);
    m.sh(
        camera.wrapping_add(0x30),
        (angles[2] as u16).wrapping_sub(reference),
    );
    let distance = crate::shared::f_800827b4(m, [view_again, position, 0, 0]);
    let camera = m.lw(CAMERA_OUT);
    m.sw(camera.wrapping_add(0x18), distance);
}

/// `[0x800BCE18]`: camera/view block (angles at +0x10, position at +0).
const VIEW: u32 = 0x800b_ce18;

/// The common "zero the control outputs" store sequence at `[gp+0x518]`
/// +8, +6, +4, +0xE, +0xC, +0xA (original order).
fn clear_outputs(m: &mut Machine, out: u32) {
    for offset in [8, 6, 4, 0xe, 0xc, 0xa] {
        m.sh(out.wrapping_add(offset), 0);
    }
}

/// 8006E874 with the state's speed (+0x48) and `[gp+0x51C]`.
fn scale_controls(m: &mut Machine) {
    let state = m.gw(0x520);
    let speed = m.lw(state.wrapping_add(0x48));
    let controls = m.gw(0x51c);
    scale_by_speed_8006d218(m, speed, controls);
}

/// Shared head of modes 0x1000000/0x2000000/0x4000000 of 800729DC:
/// 8006D678 (context +0x120, `[gp+0x514]`, `[gp+0x518]`, `[gp+0x51C]`),
/// speed scaling, then 800702D8 (entity, `[gp+0x518]`, motion,
/// `[gp+0x520]`).
fn pad_mode_sequence(m: &mut Machine, entity: u32, motion: u32) {
    let rates = m.gw(0x514);
    let out = m.gw(0x518);
    let context = m.gw(crate::common::CONTEXT);
    let controls = m.gw(0x51c);
    control_step_8006bea8(m, context.wrapping_add(0x120), rates, out, controls);
    scale_controls(m);
    let out = m.gw(0x518);
    let state = m.gw(0x520);
    flight_model_free_8006ec54(m, entity, out, motion, state);
}

/// 8006EF30 with the usual arguments (entity, `[gp+0x518]`, `motion`,
/// `[gp+0x520]`).
fn run_flight_model(m: &mut Machine, entity: u32, motion: u32) {
    let out = m.gw(0x518);
    let state = m.gw(0x520);
    flight_model_8006d8d4(m, entity, out, motion, state);
}

/// `[0x800BCE14] == 2 ? 0 : 1`, the first argument of 80076C54.
fn setting_is_not_two(m: &mut Machine) -> u32 {
    u32::from(m.lw(SETTING) != 2)
}

/// `[0x800BCE14]`: control setting read by the mode handlers (sound/vibration
/// variants are chosen on 1 and 2).
const SETTING: u32 = 0x800b_ce14;

/// 80048658 (JP; PAL 8004790C, near, `imm:1`): flight path log at context
/// +0x3B8 (PAL +0x3BC): every 120 frames (gameplay
/// word +0xC; the original divides by reciprocal multiplication), while the
/// count (+8) is below 450, append the player's x and z as bytes
/// `clamp(((v >> 12) + 0x40000) >> 11, 0, 255)` at +0xC + count * 2; the
/// halfword +2 counts every call.
pub fn record_flight_path_8004790c(m: &mut Machine) {
    let gameplay = m.gw(crate::common::GAMEPLAY);
    let frame = m.lw(gameplay.wrapping_add(0xc)) as i32;
    let player = slot_entity(m, 0);
    let log = m.gw(crate::common::CONTEXT).wrapping_add(0x3b8);
    let to_byte = |value: u32| {
        let scaled = ((value as i32) >> 12).wrapping_add(0x40000) >> 11;
        scaled.clamp(0, 0xff) as u8
    };
    if frame.wrapping_rem(120) == 0 {
        let count = m.lh(log.wrapping_add(8));
        if count < 0x1c2 {
            let x = m.lw(player.wrapping_add(8));
            let entry = log.wrapping_add((i32::from(count) as u32).wrapping_mul(2));
            m.sb(entry.wrapping_add(0xc), to_byte(x));
            let z = m.lw(player.wrapping_add(0x10));
            let count = m.lh(log.wrapping_add(8));
            let entry = log.wrapping_add((i32::from(count) as u32).wrapping_mul(2));
            m.sb(entry.wrapping_add(0xd), to_byte(z));
            let count = m.lhu(log.wrapping_add(8));
            m.sh(log.wrapping_add(8), count.wrapping_add(1));
        }
    }
    let calls = m.lhu(log.wrapping_add(2));
    m.sh(log.wrapping_add(2), calls.wrapping_add(1));
}

/// Entity pointer of `slot` from the table at `[gp+0x2E8]`.
fn slot_entity(m: &mut Machine, slot: u32) -> u32 {
    let table = m.gw(crate::common::ENTITIES);
    m.lw(table.wrapping_add(slot.wrapping_mul(4)))
}

/// 8006D0DC (JP; PAL 8006B90C, near: the roll rate integrates roll >> 5,
/// PAL roll >> 3): digital/analog control step. `input` is the pad block
/// (context +0x120): +0x08 held buttons, +0x18 analog-axis mask with axes at
/// +0x1C (pitch), +0x20 (roll), +0x24 (yaw), +0x28 (throttle). `rates`
/// (`[gp+0x514]`, four halfwords) and `controls` (`[gp+0x51C]`) are the
/// smoothed control state; `out` (`[gp+0x518]`) receives the rates at
/// +4/+6/+8/+0xE.
///
/// Without an analog axis the buttons give targets of +-0x1000: pitch
/// 0x00800000 (+) / 0x01000000 (-), yaw 0x20000000 (-) / 0x40000000 (+),
/// roll 0x02000000 (-) / 0x04000000 (+), throttle 0x08000000 (+) /
/// 0x10000000 (-). A rate whose magnitude exceeds its target decays to 64%
/// (throttle by the 0xEA0EA0EB reciprocal), the controls approach their
/// targets by `|delta| / 4 + 2` (snapping when closer), the rates
/// integrate the targets and are clamped to the analog magnitudes (or
/// 0x1000).
pub fn control_step_8006b90c(m: &mut Machine, input: u32, rates: u32, out: u32, controls: u32) {
    let mut pitch_limit: i32 = 0x1000; // t8
    let mut yaw_limit: i32 = 0x1000; // t6
    let mut roll_limit: i32 = 0x1000; // t7
    let mut pitch: i32 = 0; // t2
    let mut yaw: i32 = 0; // t0
    let mut roll: i32 = 0; // t1
    let mut throttle: i32 = 0; // t5
    let buttons = m.lw(input.wrapping_add(8));
    if m.lw(input.wrapping_add(0x18)) & 1 != 0 {
        pitch = m.lw(input.wrapping_add(0x1c)) as i32;
        pitch_limit = magnitude(pitch);
    } else {
        if buttons & 0x0080_0000 != 0 {
            pitch = 0x1000;
        }
        if buttons & 0x0100_0000 != 0 {
            pitch = pitch.wrapping_sub(0x1000);
        }
    }
    if m.lw(input.wrapping_add(0x18)) & 4 != 0 {
        yaw = yaw.wrapping_add(m.lw(input.wrapping_add(0x24)) as i32);
        yaw_limit = magnitude(yaw);
    } else {
        if buttons & 0x2000_0000 != 0 {
            yaw = yaw.wrapping_sub(yaw_limit);
        }
        if buttons & 0x4000_0000 != 0 {
            yaw = yaw.wrapping_add(yaw_limit);
        }
    }
    if m.lw(input.wrapping_add(0x18)) & 2 != 0 {
        roll = roll.wrapping_add(m.lw(input.wrapping_add(0x20)) as i32);
        roll_limit = magnitude(roll);
    } else {
        if buttons & 0x0200_0000 != 0 {
            roll = roll.wrapping_sub(roll_limit);
        }
        if buttons & 0x0400_0000 != 0 {
            roll = roll.wrapping_add(roll_limit);
        }
    }
    if m.lw(input.wrapping_add(0x18)) & 8 != 0 {
        throttle = throttle.wrapping_add(m.lw(input.wrapping_add(0x28)) as i32);
    } else {
        if buttons & 0x0800_0000 != 0 {
            throttle = throttle.wrapping_add(0x1000);
        }
        if buttons & 0x1000_0000 != 0 {
            throttle = throttle.wrapping_sub(0x1000);
        }
    }

    // Rates above their targets decay.
    for (offset, target) in [(0, pitch), (2, yaw), (4, roll)] {
        let rate = m.lh(rates.wrapping_add(offset));
        if magnitude(target) < magnitude(i32::from(rate)) {
            m.sh(rates.wrapping_add(offset), decay_64_percent(rate));
        }
    }
    let rate = m.lh(rates.wrapping_add(6));
    if magnitude(throttle) < magnitude(i32::from(rate)) {
        let scaled = i32::from(rate) << 7;
        let high = ((i64::from(scaled) * i64::from(0xea0e_a0ebu32 as i32)) >> 32) as i32;
        let value = (high.wrapping_add(scaled) >> 7).wrapping_sub(scaled >> 31);
        m.sh(rates.wrapping_add(6), value as u16);
    }

    // Controls approach their targets by |delta| / 4 + 2.
    let step = |current: i16, target: i32| -> i32 {
        let delta = i32::from(current).wrapping_sub(target);
        if delta >= 0 {
            (delta >> 2).wrapping_add(2)
        } else {
            (target.wrapping_sub(i32::from(current)) >> 2).wrapping_add(2)
        }
    };
    let roll_step = step(m.lh(controls), roll); // a0
    let pitch_step = step(m.lh(controls.wrapping_add(2)), pitch); // t3
    let yaw_step = step(m.lh(controls.wrapping_add(4)), yaw); // t4
    for (offset, target, amount) in [
        (0, roll, roll_step),
        (2, pitch, pitch_step),
        (4, yaw, yaw_step),
    ] {
        let at = controls.wrapping_add(offset);
        if target < i32::from(m.lh(at)) {
            let value = m.lhu(at);
            m.sh(at, value.wrapping_sub(amount as u16));
        }
        if i32::from(m.lh(at)) < target {
            let value = m.lhu(at);
            m.sh(at, value.wrapping_add(amount as u16));
        }
    }
    for (offset, target, amount) in [
        (0, roll, roll_step),
        (2, pitch, pitch_step),
        (4, yaw, yaw_step),
    ] {
        let at = controls.wrapping_add(offset);
        let current = i32::from(m.lh(at));
        let delta = current.wrapping_sub(target);
        let distance = if delta < 0 {
            target.wrapping_sub(current)
        } else {
            delta
        };
        if distance < amount {
            m.sh(at, target as u16);
        }
    }
    // Throttle lever (+8) moves by 0x80 towards -2 * (throttle + 0x800)
    // when throttle < -0x800, else towards 0.
    let lever = if throttle < -0x800 {
        throttle.wrapping_add(0x800).wrapping_neg() << 1
    } else {
        0
    };
    let at = controls.wrapping_add(8);
    if i32::from(m.lh(at)) < lever {
        let value = m.lhu(at);
        m.sh(at, value.wrapping_add(0x80));
    }
    if lever < i32::from(m.lh(at)) {
        let value = m.lhu(at);
        m.sh(at, value.wrapping_sub(0x80));
    }
    let power = (i32::from(m.lh(rates.wrapping_add(6))) + 0x1000) >> 1;
    m.sh(controls.wrapping_add(6), power as u16);

    // Rates integrate the targets and are clamped.
    for (offset, increment) in [
        (0, pitch >> 3),
        (2, yaw >> 2),
        (4, roll >> 5),
        (6, throttle >> 5),
    ] {
        let at = rates.wrapping_add(offset);
        let value = m.lhu(at);
        m.sh(at, value.wrapping_add(increment as u16));
    }
    for (offset, limit) in [
        (0, pitch_limit),
        (2, yaw_limit),
        (4, roll_limit),
        (6, 0x1000),
    ] {
        let at = rates.wrapping_add(offset);
        let low = limit.wrapping_neg();
        if i32::from(m.lh(at)) < low {
            m.sh(at, low as u16);
        }
        if limit < i32::from(m.lh(at)) {
            m.sh(at, limit as u16);
        }
    }
    for (from, to) in [(0, 4), (2, 6), (4, 8), (6, 0xe)] {
        let value = m.lhu(rates.wrapping_add(from));
        m.sh(out.wrapping_add(to), value);
    }
}

/// `|value|` as MIPS computes it (`bgez`/`subu zero`): -0x80000000 stays.
fn magnitude(value: i32) -> i32 {
    value.wrapping_abs()
}

/// `value * 128 / 200` with the original reciprocal multiply
/// (0x51EB851F, shift 6, rounded toward zero).
fn decay_64_percent(value: i16) -> u16 {
    let scaled = i32::from(value) << 7;
    let high = ((i64::from(scaled) * 0x51eb_851f) >> 32) as i32;
    ((high >> 6).wrapping_sub(scaled >> 31)) as u16
}

/// 8004658C (JP; PAL 80045950, near, `imm:1 ins:1 op:2`: the mission
/// from context +0x38C): per-frame sensor pass: linked entities (8004BED4), sight records
/// (80045240), and, with gameplay bit 0x4 (except stage 9 while gameplay
/// word +8 is positive), the flight path log (80048658). No arguments, no
/// result.
pub fn update_sensors_80045950(m: &mut Machine) {
    update_linked_entities_8004a784(m);
    update_sight_800446f8(m);
    let gameplay = m.gw(crate::common::GAMEPLAY);
    if m.lw(gameplay.wrapping_add(4)) & 4 == 0 {
        return;
    }
    if mission(m) == 9 && m.lw(gameplay.wrapping_add(8)) as i32 > 0 {
        return;
    }
    record_flight_path_8004790c(m);
}

/// The mission number (JP: context +0x38C, a signed halfword; PAL: the
/// byte 800BAEB4), which selects mission-specific rules.
fn mission(m: &mut Machine) -> i32 {
    let context = m.gw(crate::common::CONTEXT);
    i32::from(m.lh(context.wrapping_add(0x38c)))
}

/// 8005157C (JP; PAL 8004FF54, near, `imm:2 ins:1 op:2`: the mission from
/// context +0x38C; kinds 0x41/0x3E for PAL 0x51/0x4E): ground units with entity flags 0xC000 and no state bit 0x400000
/// move by their velocity (`+0x38..+0x40` added to `+0x08..+0x10`) when
/// `+0x54` bit 31 is set, and keep only the top byte of `+0x54`. On mission 8
/// (kind 0x41) and mission 0x30 (kind 0x3E) units in play within 0x3000 of the
/// player call 80083CA4(position, context[+0x28] + unit * 8, 0 or 1).
pub fn move_units_8004ff54(m: &mut Machine) {
    let mut units = m.gw(UNITS_2C8);
    for unit in 0..0x20u32 {
        let entity = m.lw(units);
        if !m.lw(entity) & 0xc000 == 0 && m.lw(SLOT_STATE_4D98 + 0x80 + unit * 4) & 0x40_0000 == 0 {
            let control = entity.wrapping_add(0x54);
            if m.lw(control) & 0x8000_0000 != 0 {
                let x = m.lw(entity.wrapping_add(8));
                let vx = m.lw(entity.wrapping_add(0x38));
                let vy = m.lw(entity.wrapping_add(0x3c));
                let vz = m.lw(entity.wrapping_add(0x40));
                m.sw(entity.wrapping_add(8), x.wrapping_add(vx));
                let y = m.lw(entity.wrapping_add(0xc));
                let z = m.lw(entity.wrapping_add(0x10));
                m.sw(entity.wrapping_add(0xc), y.wrapping_add(vy));
                m.sw(entity.wrapping_add(0x10), z.wrapping_add(vz));
            }
            let bits = m.lw(control);
            m.sw(control, bits & 0xff00_0000);
        }
        units = units.wrapping_add(4);
    }
    let stage = mission(m);
    if stage == 8 {
        unit_effects_8004ff54(m, 0x41, 0);
    }
    if stage == 0x30 {
        unit_effects_8004ff54(m, 0x3e, 1);
    }
}

/// 8005157C stage loops: units with flag 0x4000, in play, within 0x3000 and
/// of `kind` call 80083CA4(position, context[+0x28] + unit * 8, `mode`).
fn unit_effects_8004ff54(m: &mut Machine, kind: i16, mode: u32) {
    for unit in 0..0x20u32 {
        let units = m.gw(UNITS_2C8);
        let entity = m.lw(units.wrapping_add(unit * 4));
        if m.lw(entity) & 0x4000 == 0 {
            continue;
        }
        let slot4 = 0x80 + unit * 4;
        if m.lw(SLOT_STATE_4D98 + slot4) & 1 == 0 {
            continue;
        }
        let record = m.lw(SIGHT_4C98 + slot4);
        if m.lw(record.wrapping_add(4)) as i32 >= 0x3001 {
            continue;
        }
        if m.lh(entity.wrapping_add(0x48)) != kind {
            continue;
        }
        let context = m.gw(crate::common::CONTEXT);
        let base = m.lw(context.wrapping_add(0x28));
        crate::effects::unit_trail_8008265c(
            m,
            entity.wrapping_add(8),
            base.wrapping_add(unit * 8),
            mode,
        );
    }
}

/// 800C6ED0: 64 per-slot pointers to sight records (word 0 flags, word +4
/// distance to the player, +8 relative vector, +0x18 off-axis angle,
/// halfword timers +0x1A/+0x1C/+0x1E).
const SIGHT_4C98: u32 = 0x800c_6ed0;

/// 800C6FD0: 64 per-slot state words (bit 0: slot in play; 0x2: unit
/// destroyed; 0x4: hit timer running; 0x10: unit wrecked; 0x20: home record
/// due; 0x400000: frozen).
const SLOT_STATE_4D98: u32 = 0x800c_6fd0;

/// `[gp+0x2D8]`: pointer to the 32 ground-unit entity pointers (slots 0x20..).
const UNITS_2C8: u32 = 0x2d8;

/// 80051840 (JP; PAL 80050214, near, `imm:3`: kinds 0x23/0x24/0x2B for PAL
/// 0x32/0x33/0x3D, sound 0x5C for 0x5B): sound of the nearest ground unit
/// of those kinds (entity
/// flags 0xC000, sight record distance). The voice `[gp+0x47C]` stops
/// (8002DDA8) when gameplay bit 0x4 is clear, or when no unit is within
/// 0x2000; otherwise it starts (8002DCB0(0x5C)) and is positioned
/// (8002E6DC/8002E67C/8002E84C/8002E8DC/8002EC48 with 800C77E0).
pub fn update_unit_sound_80050214(m: &mut Machine) {
    if m.lbu(crate::GP + UNIT_VOICE_474) != 0xff {
        let gameplay = m.gw(crate::common::GAMEPLAY);
        if m.lw(gameplay.wrapping_add(4)) & 4 == 0 {
            let voice = m.lbu(crate::GP + UNIT_VOICE_474);
            external(m, 0x8002_dda8, [u32::from(voice), 0, 0, 0]);
            m.sb(crate::GP + UNIT_VOICE_474, 0xff);
        }
    }
    let mut units = m.gw(UNITS_2C8);
    m.sb(crate::GP + UNIT_NEAREST_475, 0xff);
    m.set_gw(UNIT_DISTANCE_478, 0x10_0000);
    for unit in 0..0x20u32 {
        let entity = m.lw(units);
        if !m.lw(entity) & 0xc000 == 0 {
            let kind = m.lhu(entity.wrapping_add(0x48));
            if u32::from(kind).wrapping_sub(0x23) < 2 || kind as i16 == 0x2b {
                let record = m.lw(SIGHT_4C98 + 0x80 + unit * 4);
                if record != 0 {
                    let nearest = m.gw(UNIT_DISTANCE_478);
                    if (m.lw(record.wrapping_add(4)) as i32) < nearest as i32 {
                        let distance = m.lw(record.wrapping_add(4));
                        m.sb(crate::GP + UNIT_NEAREST_475, (unit + 0x20) as u8);
                        m.set_gw(UNIT_DISTANCE_478, distance);
                    }
                }
            }
        }
        units = units.wrapping_add(4);
    }
    if m.lbu(crate::GP + UNIT_VOICE_474) != 0xff
        && (m.lbu(crate::GP + UNIT_NEAREST_475) == 0xff || m.gw(UNIT_DISTANCE_478) as i32 >= 0x2000)
    {
        let voice = m.lbu(crate::GP + UNIT_VOICE_474);
        external(m, 0x8002_dda8, [u32::from(voice), 0, 0, 0]);
        m.sb(crate::GP + UNIT_VOICE_474, 0xff);
        return;
    }
    if m.lbu(crate::GP + UNIT_NEAREST_475) == 0xff {
        return;
    }
    if m.lbu(crate::GP + UNIT_VOICE_474) == 0xff {
        if (m.gw(UNIT_DISTANCE_478) as i32) < 0x2000 {
            let voice = start_sound_8002d888(m, 0x5c);
            m.sb(crate::GP + UNIT_VOICE_474, voice as u8);
        }
        if m.lbu(crate::GP + UNIT_VOICE_474) == 0xff {
            return;
        }
    }
    let nearest = u32::from(m.lbu(crate::GP + UNIT_NEAREST_475));
    let table = m.gw(crate::common::ENTITIES);
    let gameplay = m.gw(crate::common::GAMEPLAY);
    let camera = m.lw(gameplay.wrapping_add(0x28)).wrapping_add(4);
    let unit = m.lw(table.wrapping_add(nearest * 4)).wrapping_add(8);
    position_sound_8002e2c0(m, UNIT_SOUND_55A8, camera, unit);
    let voice = u32::from(m.lbu(crate::GP + UNIT_VOICE_474));
    restore_voice_8002e260(m, voice);
    let voice = u32::from(m.lbu(crate::GP + UNIT_VOICE_474));
    distance_volume_8002e430(m, voice, UNIT_SOUND_55A8, 2);
    let voice = u32::from(m.lbu(crate::GP + UNIT_VOICE_474));
    pan_volume_8002e4c0(m, voice, UNIT_SOUND_55A8);
    let voice = u32::from(m.lbu(crate::GP + UNIT_VOICE_474));
    doppler_pitch_8002e82c(m, voice, UNIT_SOUND_55A8);
}

/// 800C77E0: sound position block filled by 8002E6DC.
const UNIT_SOUND_55A8: u32 = 0x800c_77e0;

/// `[gp+0x47C]` unit sound voice (0xFF none), `[gp+0x47D]` nearest unit slot
/// (0xFF none), `[gp+0x480]` its distance.
const UNIT_VOICE_474: u32 = 0x47c;

const UNIT_NEAREST_475: u32 = 0x47d;

const UNIT_DISTANCE_478: u32 = 0x480;

/// 80051414 (JP; PAL 8004FDF0, near, `del:3 ins:4`: the mission from
/// context +0x38C): 80051A54(unit) for each ground unit with entity flags 0xC000,
/// then the unit sound (80051840) unless the stage is 0x29.
pub fn update_units_8004fdf0(m: &mut Machine) {
    for unit in 0..0x20u32 {
        let units = m.gw(UNITS_2C8);
        let entity = m.lw(units.wrapping_add(unit * 4));
        if !m.lw(entity) & 0xc000 == 0 {
            update_unit_80050428(m, unit);
        }
    }
    if mission(m) != 0x29 {
        update_unit_sound_80050214(m);
    }
}

/// 80046124 (JP; PAL 800454EC, near, `del:3 imm:15 ins:4`: the level at
/// context +0x394 (PAL +0x398), the mission from context +0x38C): pick a
/// target for entity `slot` (only on its frame,
/// `gameplay +0xC & 0x3F == slot`, else 0xFF): the player (0) when the
/// entity has flag 0x80, the player's attacker bytes (lock slot 0 +2, +3
/// on harder levels) have room and the player is not targeting it; else the
/// nearest (80083DFC, within 0x100000 and per axis) slot 2..63 in play with
/// flag 0x4000, no state bit 0x800000, another side (flags & 0x1C0, not
/// 0x100), a free attacker byte, not already attacking this entity, and
/// for side 0x40 no state bit 0x100 except on mission 0x1E (context +0x38C). Links the choice
/// through 800463B8(`slot`, target) and returns its `v0`.
pub fn choose_target_800454ec(m: &mut Machine, slot: u32) -> u32 {
    let gameplay = m.gw(crate::common::GAMEPLAY);
    let table = m.gw(crate::common::ENTITIES);
    let frame = m.lw(gameplay.wrapping_add(0xc));
    let own = m.lw(table.wrapping_add(slot.wrapping_mul(4)));
    if slot != frame & 0x3f {
        return 0xff;
    }
    let mut best = 0x10_0000i32;
    let own_flags = m.lw(own);
    let side = own_flags & 0x1c0;
    let mut target = 0xffu32;
    if own_flags & 0x80 != 0 {
        let context = m.gw(crate::common::CONTEXT);
        let room = if m.lw(context.wrapping_add(0x394)) == 0 {
            m.lbu(LOCK_SLOTS_4B48 + 2) == 0xff
        } else {
            m.lbu(LOCK_SLOTS_4B48 + 2) == 0xff || m.lbu(LOCK_SLOTS_4B48 + 3) == 0xff
        };
        if room && u32::from(m.lbu(LOCK_SLOTS_4B48 + 1)) != slot {
            target = 0;
        }
    }
    if target == 0xff {
        let own_lock = LOCK_SLOTS_4B48.wrapping_add(slot.wrapping_shl(2));
        for candidate in 2..0x40u32 {
            if candidate == slot {
                continue;
            }
            let other = slot_entity(m, candidate);
            let flags = m.lw(other);
            if flags & 0x4000 == 0 {
                continue;
            }
            let state = m.lw(a2_state(candidate));
            if state & 1 == 0 || state & 0x80_0000 != 0 {
                continue;
            }
            let other_side = flags & 0x1c0;
            if other_side == side || other_side == 0x100 {
                continue;
            }
            let lock = LOCK_SLOTS_4B48 + candidate * 4;
            if m.lbu(lock + 2) != 0xff && m.lbu(lock + 3) != 0xff {
                continue;
            }
            if u32::from(m.lbu(own_lock.wrapping_add(2))) == candidate
                || u32::from(m.lbu(own_lock.wrapping_add(3))) == candidate
            {
                continue;
            }
            if side == 0x40 && state & 0x100 != 0 && mission(m) != 0x1e {
                continue;
            }
            let dx = a2_axis_distance(m.lw(own.wrapping_add(8)), m.lw(other.wrapping_add(8)));
            if best < dx {
                continue;
            }
            let dz = a2_axis_distance(m.lw(own.wrapping_add(0x10)), m.lw(other.wrapping_add(0x10)));
            if best < dz {
                continue;
            }
            let distance =
                crate::shared::f_800827b4(m, [own.wrapping_add(8), other.wrapping_add(8), 0, 0])
                    as i32;
            if distance < best {
                target = candidate;
                best = distance;
            }
        }
    }
    external(m, 0x8004_63b8, [slot, target, 0, 0])
}

/// a2: `|(a - b) >> 12|` (`subu; sra 12; bgez; subu zero`), wrapping.
fn a2_axis_distance(a: u32, b: u32) -> i32 {
    ((a.wrapping_sub(b) as i32) >> 12).wrapping_abs()
}

/// 800C6D80: 64 four-byte lock slots (byte 0 per slot; bytes 1..3 of slot 0
/// are the current lock targets).
const LOCK_SLOTS_4B48: u32 = 0x800c_6d80;

/// a2: address of the state word of entity slot `slot` (800C6FD0).
fn a2_state(slot: u32) -> u32 {
    SLOT_STATE_4D98.wrapping_add(slot.wrapping_mul(4))
}

// ---- 80045A40: damage ------------------------------------------------------------------

/// 80048C0C (JP-only, resident part 3): a flight event `(kind, 0, value)`.
const FLIGHT_EVENT: u32 = 0x8004_8c0c;
/// 8004CEB0 (JP-only, resident part 3; PAL 8004B760): an on-screen message.
const SHOW_MESSAGE: u32 = 0x8004_ceb0;

/// The damage record of attacker `source` (1..0x3F): word `field` of its
/// 0x40-byte entry at `[gp+0x384]` for slots < 0x20, word +0x10 of its
/// 0x28-byte entry at `[gp+0x388]` (from slot 0x20) otherwise.
fn damage_record(m: &mut Machine, source: u32, field: u32) -> Option<u32> {
    if source < 0x20 {
        let table = m.gw(0x384);
        Some(m.lw((source << 6).wrapping_add(table).wrapping_add(field)))
    } else if source < 0x40 {
        let table = m.gw(0x388);
        Some(m.lw((source * 0x28).wrapping_add(table).wrapping_sub(0x4f0)))
    } else {
        None
    }
}

/// 8002F3C0 for a destroyed or hit entity: id 0x58 on mission 0x2A (PAL
/// 0x64), else 0x1B for slots < 0x20 and 0x1D for ground units.
fn damage_effect(m: &mut Machine, slot: u32, entity: u32) {
    let id = if mission(m) == 0x2a {
        0x58
    } else if slot < 0x20 {
        0x1b
    } else {
        0x1d
    };
    crate::player::play_positioned_sound_8002efa4(m, id, entity.wrapping_add(8), 0);
}

/// Whether mission 0x16's object of kind 0x46 (PAL: kinds 0x56/0x57) is
/// the one being hit (`+0x48`).
fn mission_16_object(m: &mut Machine, entity: u32) -> bool {
    mission(m) == 0x16 && m.lh(entity.wrapping_add(0x48)) == 0x46
}

/// The event of the 0x40-byte record of slot `slot` (`[[gp+0x384] + slot *
/// 0x40 + 0x10] + 3`) through 80048C0C with `kind`.
fn slot_event(m: &mut Machine, kind: u32, slot: u32) {
    let table = m.gw(0x384);
    let record = m.lw((slot << 6).wrapping_add(table).wrapping_add(0x10));
    let value = u32::from(m.lbu(record.wrapping_add(3)));
    external(m, FLIGHT_EVENT, [kind, 0, value, 0]);
}

/// 80045A40 (JP; PAL 80044EF8, changed, `call+:3 del:23 imm:21 ins:83 op:20
/// reg:2`): apply this frame's hits to entity slots 2..63 (entity flag
/// 0x200, state bit 0, hit points `+0x4E` below 10000). Word `+0x50` bits
/// 0..5 name the attacker, word `+0x54` holds hit bits: 0..5 (direct,
/// damage by bit or by the attacker's record +0x20), 6..14 (splash, by bit
/// or record +0x1C); damage doubles when context word +0x394 (PAL +0x398)
/// is 0. Effects via 8002F3C0. New in JP: bit 15 and the splash hits of a
/// flag-0x80 entity raise events 5 and 9 (80048C0C) for slots in state
/// 0x800000, and an attacker below 0x20 in that state raises event 4 when
/// its target dies; missions 9, 0x29 and 0x2B show messages 8 / 0x24 below
/// 0x15 / 0x3C hit points (8004CEB0; PAL mission 9 had its own). The hit
/// bits 0..14 are then cleared.
pub fn apply_damage_80044ef8(m: &mut Machine) {
    m.push_frame(0x30);
    for slot in 2..0x40u32 {
        let entity = slot_entity(m, slot);
        if m.lw(entity) & 0x200 == 0 {
            continue;
        }
        let state = SLOT_STATE_4D98 + slot * 4;
        if m.lw(state) & 1 == 0 {
            continue;
        }
        let health = entity.wrapping_add(0x4e);
        let hits_address = entity.wrapping_add(0x54);
        let points = m.lh(health);
        let points_u = m.lhu(health);
        if points >= 10000 {
            continue;
        }
        let source = m.lw(entity.wrapping_add(0x50)) & 0x3f;
        let mut hits = m.lw(hits_address);
        let doubled = |m: &mut Machine| {
            let context = m.gw(crate::common::CONTEXT);
            m.lw(context.wrapping_add(0x394)) == 0
        };
        if hits & 0x3f != 0 {
            let gameplay = m.gw(crate::common::GAMEPLAY);
            let vulnerable = m.lw(gameplay.wrapping_add(4)) & 4 != 0
                && mission(m) != 0xd
                && !mission_16_object(m, entity);
            if vulnerable {
                if source == 0 {
                    let mut damage: u32 = if hits & 1 != 0 {
                        0xa
                    } else if hits & 2 != 0 {
                        0x10
                    } else if hits & 4 != 0 {
                        0x32
                    } else if hits & 8 != 0 {
                        0x12
                    } else if hits & 0x10 != 0 {
                        0x20
                    } else if hits & 0x20 != 0 {
                        0x82
                    } else {
                        0
                    };
                    if doubled(m) {
                        damage <<= 1;
                    }
                    let current = m.lhu(health);
                    m.sh(health, current.wrapping_sub(damage as u16));
                } else if let Some(record) = damage_record(m, source, 0x20)
                    && record != 0
                {
                    let damage = m.lhu(record.wrapping_add(2));
                    m.sh(health, points_u.wrapping_sub(damage));
                }
            }
            if m.lh(health) <= 0 {
                damage_effect(m, slot, entity);
            }
            hits = m.lw(hits_address);
        }
        if hits & 0x7fc0 != 0 {
            let gameplay = m.gw(crate::common::GAMEPLAY);
            let vulnerable = m.lw(gameplay.wrapping_add(4)) & 4 != 0
                && !(mission_16_object(m, entity) && hits & 0x4000 == 0);
            if vulnerable {
                if source == 0 {
                    let bits = m.lw(hits_address);
                    let mut damage = if bits & 0x40 != 0 {
                        0x46
                    } else if bits & 0x80 != 0 {
                        0x5a
                    } else if bits & 0x100 != 0 {
                        0x28
                    } else if bits & 0x200 != 0 {
                        0xd2
                    } else if bits & 0xc00 != 0 {
                        m.lh(health) as u32
                    } else if bits & 0x3000 != 0 {
                        0x46
                    } else if bits & 0x4000 != 0 {
                        m.lh(health) as u32
                    } else {
                        0
                    };
                    if doubled(m) {
                        damage <<= 1;
                    }
                    let current = m.lhu(health);
                    m.sh(health, current.wrapping_sub(damage as u16));
                } else if let Some(record) = damage_record(m, source, 0x1c)
                    && record != 0
                {
                    let current = m.lhu(health);
                    let damage = m.lhu(record.wrapping_add(2));
                    m.sh(health, current.wrapping_sub(damage));
                }
            }
            if m.lw(hits_address) & 0x4000 == 0 {
                damage_effect(m, slot, entity);
            }
        }
        // 80045E24..80045ED4 (JP): the events of tracked (state 0x800000)
        // slots.
        let tracked = |m: &mut Machine| m.lw(state) & 0x80_0000 != 0;
        let marked = m.lw(hits_address) & 0x8000 != 0;
        if !marked || tracked(m) {
            if marked {
                slot_event(m, 5, slot);
            }
            if tracked(m)
                && m.lh(health) > 0
                && m.lw(hits_address) & 0x7fc0 != 0
                && m.lw(entity) & 0x80 != 0
            {
                slot_event(m, 9, source);
            }
        }
        if m.lw(hits_address) & 0x7fff != 0 {
            if source < 0x20
                && m.lw(SLOT_STATE_4D98 + source * 4) & 0x80_0000 != 0
                && m.lh(health) <= 0
            {
                slot_event(m, 4, source);
            }
            if matches!(mission(m), 9 | 0x29 | 0x2b) && m.lw(entity) & 0x40 != 0 {
                let points = m.lh(health);
                let message = if points < 0x15 {
                    Some(8)
                } else if points < 0x3c {
                    Some(0x24)
                } else {
                    None
                };
                if let Some(message) = message {
                    external(m, SHOW_MESSAGE, [message, 0, 0, 0]);
                }
            }
        }
        let bits = m.lw(hits_address);
        m.sw(hits_address, bits & 0xffff_8000);
    }
    m.pop_frame(0x30);
}

// ---- 80099894: the terrain ray (JP) ------------------------------------------------------
//
// The map pairs JP 80099894 with PAL 80098EF4 and JP 8009A944 with PAL
// 80097EDC (both "changed"), but the instructions and every caller show the
// opposite: JP 80099894 (frame 0x88, a block list as the fifth argument) is
// the terrain ray of PAL 80097EDC, called by 8009A484, 8009A5D0 and
// 8009A800 exactly where PAL calls 80097EDC; JP 8009A944 (frame 0x80, a
// vertex list and an offset) is the model ray of PAL 80098EF4, called by
// 8009B4EC. The generated crate therefore labels `ground_hit_80097edc` with
// 8009A944 and dispatches 80099894 to `object_hit_80098ef4`; the native
// calls are right. This patch puts the JP terrain ray into
// `ground_hit_80097edc` (REPLACES, under the generated label 8009A944) and
// takes over both dispatch arms ([`dispatch`]).

/// `[gp+0x54C]`/`[gp+0x550]`: normal address and flags word of the face
/// last hit.
const GROUND_HIT_NORMAL: u32 = 0x54c;
const GROUND_HIT_FLAGS: u32 = 0x550;
/// GTE control register 9 (L13/L21 of the light matrix).
const L13L21: usize = ac3_gte::ctrl::L11L12 + 1;

/// MIPS `div` quotient (LO): division by zero gives -1 (1 for a negative
/// dividend) and `i32::MIN / -1` gives `i32::MIN`.
fn div_quotient(dividend: u32, divisor: u32) -> u32 {
    let (n, d) = (dividend as i32, divisor as i32);
    if d == 0 {
        if n < 0 { 1 } else { u32::MAX }
    } else {
        n.wrapping_div(d) as u32
    }
}

/// Inline `MulMatrix0` column step: IR1..IR3 = the column halfwords
/// zero-extended, MVMVA sf=1 lm=0 rotation x IR (0x049E012), IR1..IR3 read
/// back.
fn rotate_column(m: &mut Machine, column: [u16; 3]) -> [u32; 3] {
    for (i, value) in column.into_iter().enumerate() {
        m.gte.mtc2(reg::IR1 + i, u32::from(value));
    }
    m.gte.mvmva(true, false, 0, 3, 3);
    std::array::from_fn(|i| m.gte.mfc2(reg::IR1 + i))
}

/// The ray-space setup of both rays (the same instructions in JP and PAL):
/// the SVECTOR at +0x10 = the low halfwords of the VECTOR `delta`,
/// normalized (VectorNormalSS; its squared length through 80083D88 is the
/// returned ray length), the 80099428 matrix at +0x18 (translation zero),
/// the inline CompMatrixLV of it into +0x38, loaded as rotation and
/// translation, and SXY2 = 0.
fn c2_enter_ray_space(m: &mut Machine, sp: u32, delta: u32) -> u32 {
    let direction = sp.wrapping_add(0x10);
    let ray = sp.wrapping_add(0x18);
    let view = sp.wrapping_add(0x38);
    for i in 0..3 {
        let half = m.lhu(delta.wrapping_add(i * 4));
        m.sh(direction.wrapping_add(i * 2), half);
    }
    let squared = crate::sdk::vector_normal_ss_v0(m, direction, direction);
    let length = crate::shared::f_80082740(m, [squared, 0, 0, 0]);
    crate::player::direction_matrix_80097a70(m, ray, direction);
    for offset in [0x34, 0x30, 0x2c] {
        m.sw(sp.wrapping_add(offset), 0);
    }
    for (i, value) in [0x1000, 0, 0, 0, 0x1000, 0, 0, 0, 0x1000]
        .into_iter()
        .enumerate()
    {
        m.sh(view.wrapping_add(i as u32 * 2), value);
    }
    for offset in [0x54, 0x50, 0x4c] {
        m.sw(sp.wrapping_add(offset), 0);
    }
    crate::sdk::set_rot_matrix(m, ray);
    for column in 0..3 {
        let at = view.wrapping_add(column * 2);
        let source = [
            m.lhu(at),
            m.lhu(at.wrapping_add(6)),
            m.lhu(at.wrapping_add(0xc)),
        ];
        let ir = rotate_column(m, source);
        for (row, value) in ir.into_iter().enumerate() {
            m.sh(at.wrapping_add(row as u32 * 6), value as u16);
        }
    }
    crate::sdk::set_trans_matrix(m, ray);
    let translation = view.wrapping_add(0x14);
    let y = m.lhu(translation.wrapping_add(4));
    let x = m.lhu(translation);
    m.gte.mtc2(reg::VXY0, u32::from(x) | u32::from(y) << 16);
    let z = m.lw(translation.wrapping_add(8));
    m.gte.mtc2(reg::VZ0, z);
    m.gte.mvmva(true, false, 0, 0, 0);
    for i in 0..3 {
        let value = m.gte.mfc2(reg::MAC1 + i);
        m.sw(translation.wrapping_add(i as u32 * 4), value);
    }
    crate::sdk::set_rot_matrix(m, view);
    crate::sdk::set_trans_matrix(m, view);
    m.gte.mtc2(reg::SXY2, 0);
    length
}

/// The search state of the JP rays: nearest distance, ray length, the
/// nearest face's normal address and flags, and `carry`, the register the
/// JP face loop keeps the address of a face's vertex 1 in (s1 in 80099894):
/// it is set by every face whose (first) normal faces the ray and survives
/// to the next faces, and a split face's second triangle takes its plane
/// point from it before setting it again. Its value before the first such
/// face is the caller's register.
struct JpNearest {
    distance: u32,
    limit: u32,
    normal: u32,
    flags: u32,
    carry: u32,
}

impl JpNearest {
    /// Keep `t` when 0 <= t <= limit and t < distance (signed).
    fn offer(&mut self, t: u32, normal: u32, flags: u32) {
        let t = t as i32;
        if !(0..=self.limit as i32).contains(&t) || t >= self.distance as i32 {
            return;
        }
        self.distance = t as u32;
        self.normal = normal;
        self.flags = flags;
    }
}

/// `lwc2` of a screen-coordinate register.
fn load_sxy(m: &mut Machine, register: usize, address: u32) {
    let word = m.lw(address);
    m.gte.mtc2(register, word);
}

/// `lwc2 VXY0/VZ0` of the normal at `normal`.
fn load_normal(m: &mut Machine, normal: u32) {
    let xy = m.lw(normal);
    m.gte.mtc2(reg::VXY0, xy);
    let z = m.lw(normal.wrapping_add(4));
    m.gte.mtc2(reg::VZ0, z);
}

/// MVMVA sf=1 rotation x V0 (0x0486012), IR1..IR3 read (`mfc2`) and written
/// back as VZ0 and VXY0. Returns IR3, the normal's component along the ray.
fn rotate_normal(m: &mut Machine) -> i32 {
    m.gte.mvmva(true, false, 0, 0, 3);
    let x = m.gte.mfc2(reg::IR1);
    let y = m.gte.mfc2(reg::IR2);
    let z = m.gte.mfc2(reg::IR3);
    m.gte.mtc2(reg::VZ0, z);
    m.gte.mtc2(reg::VXY0, (x & 0xffff) | y << 16);
    z as i32
}

/// Light matrix row 1 = -(the point at `point`) (L11L12, L13L21).
fn load_plane_point(m: &mut Machine, point: u32) {
    let x = i32::from(m.lh(point)) as u32;
    let y = i32::from(m.lh(point.wrapping_add(2))) as u32;
    let z = i32::from(m.lh(point.wrapping_add(4))) as u32;
    let row = (0u32.wrapping_sub(x) & 0xffff) | 0u32.wrapping_sub(y) << 16;
    m.gte.ctc2(ac3_gte::ctrl::L11L12, row);
    m.gte.ctc2(L13L21, 0u32.wrapping_sub(z));
}

/// The plane step after [`load_plane_point`]: MVMVA sf=1 lm=1 light x V0
/// (0x04A6412), SXY0/SXY1 = the words at `first`/`second`, MAC1; returns the
/// `div` quotient -(MAC1 << 12) / `along` (the ray distance to the plane).
fn plane_distance(m: &mut Machine, first: u32, second: u32, along: i32) -> u32 {
    m.gte.mvmva(true, true, 1, 0, 3);
    load_sxy(m, reg::SXY0, first);
    load_sxy(m, reg::SXY1, second);
    let mac1 = m.gte.mfc2(reg::MAC1);
    div_quotient(0u32.wrapping_sub(mac1 << 12), along as u32)
}

/// The JP edge test: NCLIP, MAC0 read, the next screen point loaded, and
/// the next NCLIP issued before the previous result decides (JP schedules
/// the next NCLIP ahead of the branch; PAL branched first). `points` are
/// (register, address) loads between the NCLIPs; `accept(i, side)` judges
/// the i-th MAC0. Returns whether every side passed; the NCLIPs after a
/// failed side are the ones the original has already issued.
fn edge_test(
    m: &mut Machine,
    points: &[(usize, u32)],
    accept: impl Fn(usize, i32) -> bool,
) -> bool {
    m.gte.nclip();
    for (i, &(register, address)) in points.iter().enumerate() {
        let side = m.gte.mfc2(reg::MAC0) as i32;
        load_sxy(m, register, address);
        m.gte.nclip();
        if !accept(i, side) {
            return false;
        }
    }
    let side = m.gte.mfc2(reg::MAC0) as i32;
    accept(points.len(), side)
}

/// Address of vertex `index` (byte `index` of `indices`) in `work`.
fn face_vertex(work: u32, indices: u32, index: u32) -> u32 {
    work.wrapping_add(((indices >> (index * 8)) & 0xff) << 3)
}

/// One face list of the JP rays (80099B48..80099F74 for 0x30-byte faces,
/// 80099F9C..8009A3C8 for 0x24-byte faces; the same instructions): a count
/// word, then `stride`-byte faces {+0 flags, +4/+6 normal offsets into
/// `normals`, +0x10 four vertex index bytes into `work`}. Flag 0x200 tests
/// two triangles with their own normals (edge signs >= 0, <= 0, >= 0 and
/// <= 0, >= 0, <= 0), otherwise a triangle (flag 0x100; >= 0, <= 0, >= 0)
/// or a quad (>= 0, <= 0, >= 0, <= 0).
fn jp_scan_faces(
    m: &mut Machine,
    faces: u32,
    stride: u32,
    work: u32,
    normals: u32,
    nearest: &mut JpNearest,
) {
    let count = m.lw(faces) as i32;
    let mut face = faces.wrapping_add(4);
    let mut index = 0;
    while index < count {
        let offsets = face.wrapping_add(4);
        let normal = normals.wrapping_add(u32::from(m.lhu(offsets)));
        load_normal(m, normal);
        let flags = m.lw(face);
        if flags & 0x200 != 0 {
            // First triangle (vertices 0, 1, 2).
            let along = rotate_normal(m);
            let indices = m.lw(offsets.wrapping_add(0xc));
            let vertex = |i| face_vertex(work, indices, i);
            if along < 0 {
                load_plane_point(m, vertex(0));
                nearest.carry = vertex(1);
                let t = plane_distance(m, vertex(0), nearest.carry, along);
                let pass = edge_test(
                    m,
                    &[(reg::SXY0, vertex(2)), (reg::SXY1, vertex(0))],
                    |i, side| {
                        if i == 1 { side <= 0 } else { side >= 0 }
                    },
                );
                if pass {
                    nearest.offer(t, normal, flags);
                }
            }
            // Second triangle (vertices 1, 2, 3): its plane point is the
            // carried register, read before it is set again.
            let second = normals.wrapping_add(u32::from(m.lhu(offsets.wrapping_add(2))));
            load_normal(m, second);
            let along = rotate_normal(m);
            let indices = m.lw(offsets.wrapping_add(0xc));
            let vertex = |i| face_vertex(work, indices, i);
            if along < 0 {
                load_plane_point(m, nearest.carry);
                nearest.carry = vertex(1);
                let t = plane_distance(m, nearest.carry, vertex(2), along);
                let pass = edge_test(
                    m,
                    &[(reg::SXY0, vertex(3)), (reg::SXY1, vertex(1))],
                    |i, side| {
                        if i == 1 { side >= 0 } else { side <= 0 }
                    },
                );
                if pass {
                    let flags = m.lw(face);
                    nearest.offer(t, second, flags);
                }
            }
        } else {
            let along = rotate_normal(m);
            let indices = m.lw(offsets.wrapping_add(0xc));
            let vertex = |i| face_vertex(work, indices, i);
            if along < 0 {
                load_plane_point(m, vertex(0));
                nearest.carry = vertex(1);
                let t = plane_distance(m, vertex(0), nearest.carry, along);
                let pass = if flags & 0x100 != 0 {
                    edge_test(
                        m,
                        &[(reg::SXY0, vertex(2)), (reg::SXY1, vertex(0))],
                        |i, side| {
                            if i == 1 { side <= 0 } else { side >= 0 }
                        },
                    )
                } else {
                    edge_test(
                        m,
                        &[
                            (reg::SXY0, vertex(3)),
                            (reg::SXY1, vertex(2)),
                            (reg::SXY0, vertex(0)),
                        ],
                        |i, side| if i % 2 == 1 { side <= 0 } else { side >= 0 },
                    )
                };
                if pass {
                    let normal = normals.wrapping_add(u32::from(m.lhu(offsets)));
                    let flags = m.lw(face);
                    nearest.offer(t, normal, flags);
                }
            }
        }
        index += 1;
        face = face.wrapping_add(stride);
    }
}

/// 80099894 (JP; the terrain ray of PAL 80097EDC, which the map pairs with
/// 8009A944) under its generated label 8009A944: nearest terrain face hit
/// by the ray from the origin along the VECTOR `delta`, not beyond its
/// length. `list` (the caller's stack word +0x10) holds 12-byte entries {x,
/// y, z offset halfwords; +8 word: block index in bits 0..8, 0x200000 ends
/// the list}; `blocks` is the block pointer table. For each entry the
/// block's vertices (block +4) are rotated into ray space into `work`
/// (80099750), then the faces of the lists at block +0x10 (0x30-byte faces)
/// and +0xC (0x24-byte faces) are intersected ([`jp_scan_faces`]). Returns
/// the hit distance (0x7FFFFFFF when none); `out` = the normalized
/// direction x distance, or zero beyond 0x7FFF; a hit stores the face's
/// normal address and flags at `[gp+0x54C]`/`[gp+0x550]`, otherwise
/// `[gp+0x54C]` = 0. The carried vertex register starts as `out`, the
/// value 8009A484 and 8009A5D0 hold in s1 (8009A800 passes its own through
/// [`ground_hit_jp`]).
pub fn ground_hit_80097edc(
    m: &mut Machine,
    delta: u32,
    out: u32,
    work: u32,
    blocks: u32,
    list: u32,
) -> u32 {
    ground_hit_jp(m, delta, out, work, blocks, list, out)
}

/// [`ground_hit_80097edc`] with the caller's s1 (`carry`).
pub fn ground_hit_jp(
    m: &mut Machine,
    delta: u32,
    out: u32,
    work: u32,
    blocks: u32,
    list: u32,
    carry: u32,
) -> u32 {
    const SIZE: u32 = 0x88;
    let sp = m.push_frame(SIZE);
    // `sw a2,0x90(sp); sw a3,0x94(sp)`: the caller's argument home words.
    m.sw(sp.wrapping_add(SIZE + 8), work);
    m.sw(sp.wrapping_add(SIZE + 0xc), blocks);
    let limit = c2_enter_ray_space(m, sp, delta);
    let mut nearest = JpNearest {
        distance: 0x7fff_ffff,
        limit,
        normal: 0,
        flags: 0,
        carry,
    };
    let mut entry = list;
    loop {
        let flags = m.lw(entry.wrapping_add(8));
        if flags & 0x20_0000 != 0 {
            break;
        }
        let block = m.lw(blocks.wrapping_add((flags & 0x1ff) << 2));
        m.sw(sp.wrapping_add(0x58), block);
        let vertices = m.lw(block.wrapping_add(4));
        crate::player::transform_vertices_80097d98(m, vertices, entry, work);
        let block = m.lw(sp.wrapping_add(0x58));
        let gameplay = m.gw(crate::common::GAMEPLAY);
        let faces = m.lw(block.wrapping_add(0x10));
        let normals = m.lw(gameplay.wrapping_add(0x25c));
        jp_scan_faces(m, faces, 0x30, work, normals, &mut nearest);
        let block = m.lw(sp.wrapping_add(0x58));
        let faces = m.lw(block.wrapping_add(0xc));
        jp_scan_faces(m, faces, 0x24, work, normals, &mut nearest);
        entry = entry.wrapping_add(0xc);
    }
    let distance = jp_store_hit(m, sp.wrapping_add(0x10), out, &nearest);
    m.pop_frame(SIZE);
    distance
}

/// The hit store of both JP rays: beyond 0x7FFF `out` = 0 and `[gp+0x54C]`
/// = 0; otherwise `out` = the normalized direction (SVECTOR at `direction`)
/// x the distance and `[gp+0x54C]`/`[gp+0x550]` = the face's normal address
/// and flags. Returns the distance.
fn jp_store_hit(m: &mut Machine, direction: u32, out: u32, nearest: &JpNearest) -> u32 {
    let distance = nearest.distance;
    if distance as i32 > 0x7fff {
        m.sw(out.wrapping_add(8), 0);
        m.sw(out.wrapping_add(4), 0);
        m.sw(out, 0);
        m.set_gw(GROUND_HIT_NORMAL, 0);
    } else {
        let scaled = |m: &mut Machine, i: u32| {
            let component = i32::from(m.lh(direction.wrapping_add(i * 2)));
            component.wrapping_mul(distance as i32) as u32
        };
        let x = scaled(m, 0);
        m.sw(out, x);
        let y = scaled(m, 1);
        m.sw(out.wrapping_add(4), y);
        let z = scaled(m, 2);
        m.set_gw(GROUND_HIT_NORMAL, nearest.normal);
        m.set_gw(GROUND_HIT_FLAGS, nearest.flags);
        m.sw(out.wrapping_add(8), z);
    }
    distance
}

/// 8009A944 (JP; the model ray of PAL 80098EF4, which the map pairs with
/// 80099894) under its generated label 80099894: nearest face of one model
/// hit by the ray from the origin along the VECTOR `delta`, not beyond its
/// length. The JP instructions are 80099894's for a single block (the same
/// face loops, registers renamed; the carried vertex register is s4):
/// `vertices` and `offset` (the caller's stack words +0x10/+0x14) are
/// rotated into ray space into `work` (80099750), then the face lists of
/// `model` (+0x10, 0x30-byte faces; +0xC, 0x24-byte faces; normals from
/// `[[gp+0x2E0]+0x25C]`) are intersected ([`jp_scan_faces`]). Returns the
/// hit distance (0x7FFFFFFF when none); `out` and `[gp+0x54C]`/`[gp+0x550]`
/// as [`ground_hit_jp`]. Called without the caller's s4, the carried
/// register starts at 0 (see [`object_hit_jp`]).
pub fn object_hit_80098ef4(
    m: &mut Machine,
    delta: u32,
    out: u32,
    work: u32,
    model: u32,
    vertices: u32,
    offset: u32,
) -> u32 {
    object_hit_jp(m, delta, out, work, model, vertices, offset, 0)
}

/// [`object_hit_80098ef4`] with `carry`, the s4 its caller 8009B4EC leaves
/// untouched: the s4 of 8009B4EC's caller (8006A11C: the entity side bits
/// `flags & 0x1C0`; 8006B504: its scratchpad record pointer 0x1F80xxxx;
/// 80070A80: its s0). It is the plane point of a split face's second
/// triangle until a face facing the ray sets it.
#[allow(clippy::too_many_arguments)]
pub fn object_hit_jp(
    m: &mut Machine,
    delta: u32,
    out: u32,
    work: u32,
    model: u32,
    vertices: u32,
    offset: u32,
    carry: u32,
) -> u32 {
    const SIZE: u32 = 0x80;
    let sp = m.push_frame(SIZE);
    // `sw a1,0x84(sp); sw a3,0x8c(sp)`: the caller's argument home words.
    m.sw(sp.wrapping_add(SIZE + 4), out);
    m.sw(sp.wrapping_add(SIZE + 0xc), model);
    let limit = c2_enter_ray_space(m, sp, delta);
    crate::player::transform_vertices_80097d98(m, vertices, offset, work);
    let gameplay = m.gw(crate::common::GAMEPLAY);
    let faces = m.lw(model.wrapping_add(0x10));
    let normals = m.lw(gameplay.wrapping_add(0x25c));
    let mut nearest = JpNearest {
        distance: 0x7fff_ffff,
        limit,
        normal: 0,
        flags: 0,
        carry,
    };
    jp_scan_faces(m, faces, 0x30, work, normals, &mut nearest);
    let faces = m.lw(model.wrapping_add(0xc));
    jp_scan_faces(m, faces, 0x24, work, normals, &mut nearest);
    let distance = jp_store_hit(m, sp.wrapping_add(0x10), out, &nearest);
    m.pop_frame(SIZE);
    distance
}

/// The JP terrain-ray caller 8009A800 (exact; PAL 80098DB0) under the JP
/// ray: its s1 at the call is `from`, the carried vertex register of
/// [`ground_hit_jp`]. Otherwise the generated translation: the cells
/// crossed by (from >> 12)..(to >> 12) (80098DBC) are searched along (to >>
/// 12) - (from >> 12) with the block table `[[gp+0x2E0]+0x24C]`; `out` =
/// the hit point + `from`. Returns the hit distance.
pub fn ground_ray_cells_80098db0(m: &mut Machine, from: u32, to: u32, out: u32, work: u32) -> u32 {
    const SIZE: u32 = 0x290;
    let sp = m.push_frame(SIZE);
    let terrain = m.gw(crate::common::GAMEPLAY).wrapping_add(0x244);
    let list = sp.wrapping_add(0x18);
    let start = sp.wrapping_add(0x258);
    let end = sp.wrapping_add(0x268);
    let x = m.lw(from);
    let map = m.lw(terrain.wrapping_add(4));
    m.sw(start, sra(x, 12));
    for i in 1..3 {
        let value = m.lw(from.wrapping_add(i * 4));
        m.sw(start.wrapping_add(i * 4), sra(value, 12));
    }
    for i in 0..3 {
        let value = m.lw(to.wrapping_add(i * 4));
        m.sw(end.wrapping_add(i * 4), sra(value, 12));
    }
    m.sw(sp.wrapping_add(0x10), map);
    crate::player::ray_cell_list_80097404(m, start, end, work, list, map);
    let end_x = m.lw(end);
    let origin: [u32; 3] = std::array::from_fn(|i| m.lw(start.wrapping_add(i as u32 * 4)));
    m.sw(end, end_x.wrapping_sub(origin[0]));
    let end_y = m.lw(end.wrapping_add(4));
    let end_z = m.lw(end.wrapping_add(8));
    m.sw(end.wrapping_add(4), end_y.wrapping_sub(origin[1]));
    m.sw(end.wrapping_add(8), end_z.wrapping_sub(origin[2]));
    m.sw(sp.wrapping_add(0x10), list);
    let blocks = m.lw(terrain.wrapping_add(8));
    let distance = ground_hit_jp(m, end, out, work, blocks, list, from);
    for i in 0..3 {
        let point = m.lw(out.wrapping_add(i * 4));
        let base = m.lw(from.wrapping_add(i * 4));
        m.sw(out.wrapping_add(i * 4), point.wrapping_add(base));
    }
    m.pop_frame(SIZE);
    distance
}

fn sra(value: u32, shift: u32) -> u32 {
    ((value as i32) >> shift) as u32
}

/// The two ray routines whose generated dispatch arms the map swapped
/// (TAKES_OVER): 80099894 is the terrain ray, 8009A944 the model ray.
/// Fifth and sixth arguments: the caller's stack words +0x10/+0x14. The
/// carried vertex register starts as a1 (terrain; 8009A484 and 8009A5D0
/// hold `out` in s1) and 0 (model; see [`object_hit_jp`]).
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    let [a0, a1, a2, a3] = args;
    match target {
        0x8009_9894 => {
            let list = m.lw(m.sp.wrapping_add(0x10));
            Some(ground_hit_jp(m, a0, a1, a2, a3, list, a1))
        }
        0x8009_a944 => {
            let vertices = m.lw(m.sp.wrapping_add(0x10));
            let offset = m.lw(m.sp.wrapping_add(0x14));
            Some(object_hit_jp(m, a0, a1, a2, a3, vertices, offset, 0))
        }
        _ => None,
    }
}

/// 800797FC (JP; PAL 8007818C, near, `imm:1`: context +0x39D for PAL
/// +0x3A1): kill camera on entity `id` (an entity's first word: bit 31
/// none, low 6 bits the slot) when the effects block (`[800BD094]` word 0
/// bit 0) and context +0x39D allow it and the entity has flag 0x4000: the
/// camera (`[gameplay +0x28]`) +0xC0 = slot, 800793C4(camera, 0x800,
/// player entity), 80035740(0).
pub fn focus_kill_camera_8007818c(m: &mut Machine, id: u32) {
    let effects = m.lw(EFFECTS_AF1C);
    if m.lw(effects) & 1 == 0 || (id as i32) < 0 {
        return;
    }
    let context = m.gw(crate::common::CONTEXT);
    if m.lbu(context.wrapping_add(0x39d)) == 0 {
        return;
    }
    let slot = id & 0x3f;
    let table = m.gw(crate::common::ENTITIES);
    let entity = m.lw(table.wrapping_add(slot << 2));
    if m.lw(entity) & 0x4000 == 0 {
        return;
    }
    let gameplay = m.gw(crate::common::GAMEPLAY);
    let camera = m.lw(gameplay.wrapping_add(0x28));
    m.sw(camera.wrapping_add(0xc0), slot);
    let player = m.lw(table);
    external(m, 0x8007_93c4, [camera, 0x800, player, 0]);
    external(m, 0x8003_5740, [0, 0, 0, 0]);
}

/// 800BD094: pointer to the block whose word +0x10 bit 0x4000 enables
/// link effects.
const EFFECTS_AF1C: u32 = 0x800b_d094;

/// 80072004 (JP; PAL 80070980, near, `imm:1`): entry 11: reload 0xB4, sound
/// 0x58 (PAL 0x5E), no vibration, spawner
/// 80068FA8 from the entity position.
pub fn launch_missile_80070980(m: &mut Machine, timers: u32) {
    let Some(entity) = arm_launcher(m, timers, 0xb4) else {
        return;
    };
    external(m, 0x8002_db4c, [0x58, 0, 0, 0]);
    let motion_block = m.gw(0x524);
    let owner = m.lw(entity);
    let target = m.lw(motion_block.wrapping_add(0x18));
    spawn_800677d8(
        m,
        owner,
        entity.wrapping_add(8),
        entity.wrapping_add(0x38),
        target,
    );
}

/// The common head of the missile handlers: missiles left and 800718A0
/// (`timers`, motion block +4, `reload`) found a free launcher; then state
/// +0x10 |= 0x4000 and the missile count drops. Returns the player entity.
fn arm_launcher(m: &mut Machine, timers: u32, reload: u32) -> Option<u32> {
    let motion_block = m.gw(0x524);
    if m.lw(motion_block.wrapping_add(0x1c)) == 0 {
        return None;
    }
    let count = m.lw(motion_block.wrapping_add(4));
    if fill_free_slots_8007021c(m, timers, count, reload) == 0 {
        return None;
    }
    let state = m.gw(0x520);
    let flags = m.lw(state.wrapping_add(0x10));
    let motion_block = m.gw(0x524);
    m.sw(state.wrapping_add(0x10), flags | 0x4000);
    let missiles = m.lw(motion_block.wrapping_add(0x1c)).wrapping_sub(1);
    let table = m.gw(crate::common::ENTITIES);
    m.sw(motion_block.wrapping_add(0x1c), missiles);
    Some(m.lw(table))
}

/// 800535D4 (JP; PAL 80051F9C, near, `imm:2`: the level at context +0x394,
/// PAL +0x398): missile step of ground unit `unit`: a target that already has
/// an incoming projectile (mask at `[800BD080]`) returns 1. Aim at the
/// target position (copies at +0x6C/+0x84, angles into +0x7C through
/// 80031BB4 with the vector at sp+0x18 of the 0x40 frame; pause flag and
/// counter cleared meanwhile); out of the firing cone returns 1. Otherwise,
/// every attack +0x12 frames (halved or doubled for the player's side by
/// level; at half the interval the missile warning bit 0 of 800C6ECC is
/// set) a launch by kind byte 0 (jump table 800241FC: 0x11/0x14 80068800
/// with flags 0x100/0x10100/0x20100 for kind 0x11, 0x12 80068C0C, 0x13
/// 80068C80, 0x18 80068BA8, 0x19 800689E8), then a pause until +0x14.
/// Returns 0.
pub fn missile_attack_80051f9c(m: &mut Machine, unit: u32) -> u32 {
    const FRAME: u32 = 0x40;
    let sp = m.push_frame(FRAME);
    let result = a2_missile_step(m, unit, sp);
    m.pop_frame(FRAME);
    result
}

fn a2_missile_step(m: &mut Machine, unit: u32, sp: u32) -> u32 {
    let units = m.gw(UNITS_2C8);
    let record = a2_unit_record(m, unit);
    let entity = m.lw(units.wrapping_add(unit.wrapping_mul(4)));
    let target = u32::from(m.lbu(entity.wrapping_add(0x5b)));
    let table = m.gw(crate::common::ENTITIES);
    let attack = m.lw(record.wrapping_add(0x10));
    let other = m.lw(table.wrapping_add(target << 2));
    let incoming = m.lw(A2_INCOMING_AF08);
    let busy = if target < 0x20 {
        m.lw(incoming) & 1u32.wrapping_shl(target)
    } else {
        m.lw(incoming.wrapping_add(4)) & 1u32.wrapping_shl(target - 0x20)
    };
    if busy != 0 {
        return 1;
    }
    let pause = entity.wrapping_add(0x5a);
    let timer = entity.wrapping_add(0x64);
    let saved_pause = m.lb(pause);
    let saved_timer = m.lw(timer);
    m.sb(pause, 0);
    m.sw(timer, 0);
    copy_words(m, entity.wrapping_add(8), entity.wrapping_add(0x6c), 4);
    copy_words(m, other.wrapping_add(8), entity.wrapping_add(0x84), 4);
    a2_aim(m, entity, sp.wrapping_add(0x18));
    if !a2_in_cone(m, entity, attack) {
        return 1;
    }
    m.sb(pause, saved_pause as u8);
    m.sw(timer, saved_timer);
    let kind = m.lbu(attack);
    let flags: u32 = if kind != 0x11 {
        0
    } else {
        let context = m.gw(crate::common::CONTEXT);
        if m.lw(context.wrapping_add(0x394)) == 0 && m.lw(entity) & 0x80 != 0 {
            0x100
        } else {
            match m.lbu(attack.wrapping_add(1)) {
                0 => 0x2_0100,
                1 => 0x1_0100,
                _ => 0x100,
            }
        }
    };
    if m.lb(pause) == 0 {
        let mut interval = i32::from(m.lh(attack.wrapping_add(0x12)));
        if m.lw(entity) & 0x80 != 0 {
            let context = m.gw(crate::common::CONTEXT);
            let level = m.lw(context.wrapping_add(0x394)) as i32;
            if level == 0 {
                interval <<= 1;
            }
            if level >= 2 {
                interval >>= 1;
            }
        }
        if m.lw(timer) == (interval >> 1) as u32 {
            let warning = VIEW_4C68 + 0x2c;
            let bits = m.lw(warning) | 1;
            m.sw(warning, bits);
        }
        if m.lw(timer) as i32 >= interval {
            let origin = entity.wrapping_add(0x6c);
            let owner = m.lw(entity);
            let target_id = m.lw(other);
            let args = [owner, origin, A2_MISSILE_VELOCITY, target_id];
            match kind.wrapping_sub(0x11) {
                0 | 3 => {
                    m.sw(sp.wrapping_add(0x10), flags);
                    spawn_80067030(m, owner, origin, A2_MISSILE_VELOCITY, target_id, flags);
                }
                1 => {
                    spawn_8006743c(m, args[0], args[1], args[2], args[3]);
                }
                2 => {
                    spawn_800674b0(m, args[0], args[1], args[2], args[3]);
                }
                7 => {
                    spawn_800673d8(m, args[0], args[1], args[2], args[3]);
                }
                8 => {
                    spawn_80067218(m, args[0], args[1], args[2], args[3]);
                }
                _ => {}
            }
            m.sb(pause, 1);
            m.sw(timer, 0);
        }
    } else if m.lw(timer) as i32 >= i32::from(m.lh(attack.wrapping_add(0x14))) {
        m.sb(pause, 0);
        m.sw(timer, 0);
    }
    let count = m.lw(timer).wrapping_add(1);
    m.sw(timer, count);
    0
}

const A2_MISSILE_VELOCITY: u32 = 0x800b_20b8;

/// 800C6EA0: player view block: rotation matrix (800308F0) and range limits
/// at +0x20 (angle), +0x24 and +0x28 (distances).
const VIEW_4C68: u32 = 0x800c_6ea0;

/// a2: the firing cone of 80053214/800535D4: the aim pitch (+0x7C) relative
/// to the unit's (+0x18), folded into -0x1000..0x1000, within the attack
/// parameters' halfwords +8..+0xA, and the relative yaw (+0x7E vs +0x1A,
/// masked to 0x3FFF, +0x4000 below +0xC) not above +0xE.
fn a2_in_cone(m: &mut Machine, entity: u32, attack: u32) -> bool {
    let aim_pitch = i32::from(m.lh(entity.wrapping_add(0x7c)));
    let pitch = i32::from(m.lh(entity.wrapping_add(0x18)));
    let mut relative = aim_pitch.wrapping_sub(pitch);
    if relative > 0x1000 {
        relative = 0x2000 - relative;
    } else if relative < -0x1000 {
        relative = -0x2000 - relative;
    }
    if relative < i32::from(m.lh(attack.wrapping_add(8)))
        || i32::from(m.lh(attack.wrapping_add(0xa))) < relative
    {
        return false;
    }
    let aim_yaw = i32::from(m.lh(entity.wrapping_add(0x7e)));
    let yaw = i32::from(m.lh(entity.wrapping_add(0x1a)));
    let lower = i32::from(m.lh(attack.wrapping_add(0xc)));
    let mut relative = aim_yaw.wrapping_sub(yaw) & 0x3fff;
    if relative < lower {
        relative += 0x4000;
    }
    i32::from(m.lh(attack.wrapping_add(0xe))) >= relative
}

/// a2: aim angles of a unit at its target: VECTOR `(aim - own) >> 12` from the
/// copies at entity +0x84 (target position) and +8 into `vector`, then
/// 80031BB4 into entity +0x7C.
fn a2_aim(m: &mut Machine, entity: u32, vector: u32) {
    for i in 0..3 {
        let aim = m.lw(entity.wrapping_add(0x84 + i * 4));
        let own = m.lw(entity.wrapping_add(8 + i * 4));
        m.sw(
            vector.wrapping_add(i * 4),
            ((aim.wrapping_sub(own) as i32) >> 12) as u32,
        );
    }
    vector_angles_80031970(m, vector, entity.wrapping_add(0x7c));
}

/// Inline word copy of `words` words in groups of up to four loads followed by
/// their stores (the compiler's structure copy).
fn copy_words(m: &mut Machine, src: u32, dst: u32, words: u32) {
    let mut done = 0;
    while done < words {
        let group = (words - done).min(4);
        let mut buffer = [0u32; 4];
        for (i, word) in buffer.iter_mut().take(group as usize).enumerate() {
            *word = m.lw(src.wrapping_add((done + i as u32) * 4));
        }
        for (i, word) in buffer.iter().take(group as usize).enumerate() {
            m.sw(dst.wrapping_add((done + i as u32) * 4), *word);
        }
        done += group;
    }
}

/// a2: 800BD080: pointer to the 64-bit mask of targets with an incoming
/// projectile (word 0 slots 0..31, word 4 slots 32..63).
const A2_INCOMING_AF08: u32 = 0x800b_d080;

/// a2: the record of ground unit `unit` (`[gp+0x388] + unit * 0x28`).
fn a2_unit_record(m: &mut Machine, unit: u32) -> u32 {
    m.gw(0x388).wrapping_add(unit.wrapping_mul(0x28))
}

/// 80053214 (JP; PAL 80051BDC, near, `imm:1`: the level at context +0x394,
/// PAL +0x398): gun step of ground unit `unit` at `distance` (a1) from its
/// target: aim from the own position (copied to +0x6C) at the target
/// position (copied to +0x84) led by `t = distance / 144` (reciprocal
/// multiply): + velocity * t, y - 669 * t * t / 2; angles into +0x7C
/// (80031BB4, vector at sp+0x10 of the 0x38 frame), jittered by
/// `rand() % (2s) - s` with s = 0x258 (easy level against the player's side,
/// or descriptor byte 1 other than 0/1), 0xC8 (byte 1 = 1) or 0. Out of the
/// firing cone: returns 1. Otherwise a burst: every attack +0x10 frames of
/// the counter (+0x64) a bullet of kind byte 0 - 1 (8006B034) until +0x12,
/// then a pause (+0x5A = 1) until +0x14. Returns 0.
pub fn gun_attack_80051bdc(m: &mut Machine, unit: u32, distance: u32) -> u32 {
    const FRAME: u32 = 0x38;
    let sp = m.push_frame(FRAME);
    let result = a2_gun_step(m, unit, distance, sp);
    m.pop_frame(FRAME);
    result
}

fn a2_gun_step(m: &mut Machine, unit: u32, distance: u32, sp: u32) -> u32 {
    let units = m.gw(UNITS_2C8);
    let record = a2_unit_record(m, unit);
    let entity = m.lw(units.wrapping_add(unit.wrapping_mul(4)));
    let target = u32::from(m.lbu(entity.wrapping_add(0x5b)));
    let table = m.gw(crate::common::ENTITIES);
    let attack = m.lw(record.wrapping_add(0x10));
    let other = m.lw(table.wrapping_add(target << 2));
    let high = ((i64::from(distance as i32) * 0x38e3_8e39) >> 32) as i32;
    let lead = (high >> 5).wrapping_sub((distance as i32) >> 31);
    copy_words(m, entity.wrapping_add(8), entity.wrapping_add(0x6c), 4);
    copy_words(m, other.wrapping_add(8), entity.wrapping_add(0x84), 4);
    let vx = m.lw(other.wrapping_add(0x38)) as i32;
    let x = m.lw(entity.wrapping_add(0x84));
    m.sw(
        entity.wrapping_add(0x84),
        x.wrapping_add(vx.wrapping_mul(lead) as u32),
    );
    let vy = m.lw(other.wrapping_add(0x3c)) as i32;
    let rise = vy.wrapping_mul(lead) as u32;
    let fall = lead.wrapping_mul((lead as u32).wrapping_mul(669) as i32);
    let y = m.lw(entity.wrapping_add(0x88));
    m.sw(entity.wrapping_add(0x88), y.wrapping_add(rise));
    let vz = m.lw(other.wrapping_add(0x40)) as i32;
    let y = m.lw(entity.wrapping_add(0x88));
    m.sw(
        entity.wrapping_add(0x88),
        y.wrapping_sub((fall >> 1) as u32),
    );
    let z = m.lw(entity.wrapping_add(0x8c));
    m.sw(
        entity.wrapping_add(0x8c),
        z.wrapping_add(vz.wrapping_mul(lead) as u32),
    );
    a2_aim(m, entity, sp.wrapping_add(0x10));
    let context = m.gw(crate::common::CONTEXT);
    let spread: u32 = if m.lw(context.wrapping_add(0x394)) == 0 && m.lw(entity) & 0x80 != 0 {
        0x258
    } else {
        match m.lbu(attack.wrapping_add(1)) {
            0 => 0,
            1 => 0xc8,
            _ => 0x258,
        }
    };
    let range = spread << 1;
    if range != 0 {
        for offset in [0x7c, 0x7e] {
            let random = crate::sdk::rand(m);
            let jitter = (a2_rem(random as i32, range as i32) as u32).wrapping_sub(spread);
            let angle = m.lhu(entity.wrapping_add(offset));
            m.sh(
                entity.wrapping_add(offset),
                angle.wrapping_add(jitter as u16),
            );
        }
    }
    if !a2_in_cone(m, entity, attack) {
        return 1;
    }
    let timer = entity.wrapping_add(0x64);
    let pause = entity.wrapping_add(0x5a);
    if m.lb(pause) == 0 {
        let interval = i32::from(m.lh(attack.wrapping_add(0x10)));
        if a2_rem(m.lw(timer) as i32, interval) == 0 {
            let kind = u32::from(m.lbu(attack)).wrapping_sub(1);
            let owner = m.lw(entity);
            queue_bullet_80069864(
                m,
                owner,
                entity.wrapping_add(0x6c),
                A2_BULLET_VELOCITY,
                kind,
            );
        }
        if m.lw(timer) as i32 >= i32::from(m.lh(attack.wrapping_add(0x12))) {
            m.sb(pause, 1);
            m.sw(timer, 0);
        }
    } else if m.lw(timer) as i32 >= i32::from(m.lh(attack.wrapping_add(0x14))) {
        m.sb(pause, 0);
        m.sw(timer, 0);
    }
    let count = m.lw(timer).wrapping_add(1);
    m.sw(timer, count);
    0
}

/// a2: 800B20A8 / 800B20B8: zero launch velocities of unit bullets/missiles.
const A2_BULLET_VELOCITY: u32 = 0x800b_20a8;

/// a2: MIPS `div` remainder (HI): the dividend for a zero divisor, 0 for
/// `i32::MIN / -1`. The compiler's `break 7` / `break 6` checks after the
/// division are not modelled (see `fm_div`).
fn a2_rem(dividend: i32, divisor: i32) -> i32 {
    if divisor == 0 {
        dividend
    } else {
        dividend.wrapping_rem(divisor)
    }
}

/// 80075084 (JP; PAL 80073A08, near, `imm:2`): mode 0x400 of 800729DC, joining the target's formation.
/// `offset` is the 80076924 output +0x24 (relative position << 6 at
/// +0..+8, angle differences at +0x10..+0x14), `target` the target
/// entity's velocity (+0x38). The entity angles +0x18/+0x1A/+0x1C move by
/// [`h_angle_step`] of the differences. Phase (state +8) 0: the velocity
/// (entity +0x38..+0x40) steps by 0x100 toward `target`; when every axis is
/// within 0x100 the phase becomes 1 (state +0xC = 0). Phase 1: velocity =
/// `target` - (`offset` >> 5, clamped to +-0x1000); while the summed
/// magnitude of that correction is below 0x3000 the counter state +0xC
/// counts (sound 0x5F, or 0x5E with `[0x800BCE14]` = 2, at 0x96; PAL 0x37 /
/// 0x61), and from
/// 0x12D the routine returns 1 without aiming. Otherwise 8006EE38 (entity,
/// state) and 0.
pub fn join_formation_80073a08(
    m: &mut Machine,
    entity: u32,
    state: u32,
    offset: u32,
    target: u32,
) -> u32 {
    const FRAME: u32 = 0x20;
    m.push_frame(FRAME);
    let clamp = |value: u32| ((value as i32) >> 5).clamp(-0x1000, 0x1000);
    let correction = [
        clamp(m.lw(offset)),
        clamp(m.lw(offset.wrapping_add(4))),
        clamp(m.lw(offset.wrapping_add(8))),
    ];
    let steps: [i32; 3] = std::array::from_fn(|i| {
        h_angle_step(i32::from(m.lh(offset.wrapping_add(0x10 + i as u32 * 2))))
    });
    let pitch = m.lhu(entity.wrapping_add(0x18));
    let roll = m.lhu(entity.wrapping_add(0x1c));
    m.sh(
        entity.wrapping_add(0x18),
        pitch.wrapping_add(steps[0] as u16),
    );
    let yaw = m.lhu(entity.wrapping_add(0x1a));
    m.sh(
        entity.wrapping_add(0x1c),
        roll.wrapping_add(steps[2] as u16),
    );
    m.sh(entity.wrapping_add(0x1a), yaw.wrapping_add(steps[1] as u16));
    match m.lw(state.wrapping_add(8)) {
        0 => {
            for i in 0..3 {
                let velocity = entity.wrapping_add(0x38 + i * 4);
                let goal = target.wrapping_add(i * 4);
                let mut value = m.lw(velocity);
                if (value as i32) < m.lw(goal) as i32 {
                    m.sw(velocity, value.wrapping_add(0x100));
                    value = m.lw(velocity);
                }
                if (m.lw(goal) as i32) < value as i32 {
                    m.sw(velocity, value.wrapping_sub(0x100));
                }
            }
            let matched = (0..3).all(|i| {
                let value = m.lw(entity.wrapping_add(0x38 + i * 4));
                let goal = m.lw(target.wrapping_add(i * 4));
                magnitude(value.wrapping_sub(goal) as i32) < 0x100
            });
            if matched {
                m.sw(state.wrapping_add(8), 1);
                m.sw(state.wrapping_add(0xc), 0);
            }
        }
        1 => {
            for (i, value) in correction.iter().enumerate() {
                let goal = m.lw(target.wrapping_add(i as u32 * 4));
                m.sw(
                    entity.wrapping_add(0x38 + i as u32 * 4),
                    goal.wrapping_sub(*value as u32),
                );
            }
            let size = magnitude(correction[0])
                .wrapping_add(magnitude(correction[1]))
                .wrapping_add(magnitude(correction[2]));
            if size < 0x3000 {
                if m.lw(state.wrapping_add(0xc)) == 0x96 {
                    let sound = if m.lw(SETTING) == 2 { 0x5e } else { 0x5f };
                    external(m, 0x8002_db4c, [sound, 0, 0, 0]);
                }
                let count = m.lw(state.wrapping_add(0xc)).wrapping_add(1);
                m.sw(state.wrapping_add(0xc), count);
                if count as i32 >= 0x12d {
                    m.pop_frame(FRAME);
                    return 1;
                }
            }
        }
        _ => {}
    }
    aim_camera_8006d7dc(m, entity, state);
    m.pop_frame(FRAME);
    0
}

/// One angle step of 80075084: `-difference >> 6` (arithmetic); a zero
/// step becomes -1 for a positive and +1 for a negative difference.
fn h_angle_step(difference: i32) -> i32 {
    let mut step = difference.wrapping_neg() >> 6;
    if step == 0 && difference > 0 {
        step = -1;
    }
    if step == 0 && difference < 0 {
        step = 1;
    }
    step
}

/// 80074E64 (JP; PAL 800737E0, near, `del:2`): `entity` follows the entity `[[gp+0x2E8] + 4 * [target]]`
/// (800729DC mode 0x200; `controls` = `[gp+0x51C]`, `target` =
/// `[0x800BCE1C]`). Controls reset as in 80074BA8. The offset at `target`
/// +4..+0xC (z - 0x64000), >> 6, rotated by the leader's orientation
/// (80030778 of +0x18, MVMVA sf=1) and << 6 is added to the leader's
/// position (+8); the leader's speed (80083F40 of velocity >> 6, << 6) goes
/// to state +0x48 (JP leaves +0x4C; PAL set it to 0x1000) and, as the
/// forward vector (0, 0,
/// speed >> 5) rotated and << 5, to the velocity +0x38..+0x40; the angles
/// +0x18.. are copied.
pub fn follow_entity_800737e0(
    m: &mut Machine,
    entity: u32,
    state: u32,
    controls: u32,
    target: u32,
) {
    const FRAME: u32 = 0x68;
    let sp = m.push_frame(FRAME);
    let rotated = sp.wrapping_add(0x10);
    let vector = sp.wrapping_add(0x20);
    let matrix = sp.wrapping_add(0x28);
    for (offset, value) in [(0, 0), (2, 0), (4, 0), (6, 0x800), (8, 0), (0xa, 0)] {
        m.sh(controls.wrapping_add(offset), value);
    }
    m.sw(controls.wrapping_add(0xc), 0);
    let leader_of = |m: &mut Machine| {
        let index = m.lw(target);
        let table = m.gw(crate::common::ENTITIES);
        m.lw(index.wrapping_shl(2).wrapping_add(table))
    };
    let leader = leader_of(m); // s2
    crate::shared::f_80030534(m, [leader.wrapping_add(0x18), matrix, 0, 0]);
    let x = m.lw(target.wrapping_add(4));
    m.sh(vector, sra(x, 6) as u16);
    let y = m.lw(target.wrapping_add(8));
    m.sh(vector.wrapping_add(2), sra(y, 6) as u16);
    let z = m.lw(target.wrapping_add(0xc)).wrapping_add(0xfff9_c000);
    m.sh(vector.wrapping_add(4), sra(z, 6) as u16);
    crate::sdk::set_rot_matrix(m, matrix);
    g_rotate(m, vector, rotated);
    for i in 0..3 {
        let offset = m.lw(rotated.wrapping_add(i * 4));
        let base = m.lw(leader.wrapping_add(8 + i * 4));
        m.sw(
            entity.wrapping_add(8 + i * 4),
            base.wrapping_add(offset << 6),
        );
    }
    let leader_now = leader_of(m);
    let vx = sra(m.lw(leader_now.wrapping_add(0x38)), 6);
    let vy = sra(m.lw(leader_now.wrapping_add(0x3c)), 6);
    let vz = sra(m.lw(leader_now.wrapping_add(0x40)), 6);
    let speed = vector_length_800828f8(m, vx, vy, vz) << 6;
    m.sh(vector, 0);
    m.sh(vector.wrapping_add(2), 0);
    m.sh(vector.wrapping_add(4), sra(speed, 5) as u16);
    m.sw(state.wrapping_add(0x48), speed);
    g_rotate(m, vector, rotated);
    for i in 0..3 {
        let value = m.lw(rotated.wrapping_add(i * 4));
        m.sw(entity.wrapping_add(0x38 + i * 4), value << 5);
    }
    for i in 0..3 {
        let angle = m.lhu(leader.wrapping_add(0x18 + i * 2));
        m.sh(entity.wrapping_add(0x18 + i * 2), angle);
    }
    m.pop_frame(FRAME);
}

/// Rotate the SVECTOR at `vector` by the current rotation matrix (lwc2
/// VXY0/VZ0, MVMVA sf=1 mx=0 v=0 cv=3) and store MAC1..MAC3 at `out`.
fn g_rotate(m: &mut Machine, vector: u32, out: u32) {
    let xy = m.lw(vector);
    m.gte.mtc2(reg::VXY0, xy);
    let z = m.lw(vector.wrapping_add(4));
    m.gte.mtc2(reg::VZ0, z);
    m.gte.mvmva(true, false, 0, 0, 3);
    for i in 0..3 {
        let value = m.gte.mfc2(reg::MAC1 + i);
        m.sw(out.wrapping_add(i as u32 * 4), value);
    }
}

/// 80075B04 (JP; PAL 80074488, near, `del:3 imm:3 op:1`): modes 0x4000/0x8000 of 800729DC (take-off run; `replay` is
/// the caller's stack word +0x14, 1 in mode 0x8000; `view` its word
/// +0x10). Phase (state +8):
/// - 0: 8007599C (entity, state, `view`), then velocity, speed, the
///   state's rate/angle halves (+0x30..+0x3C, +0x4C), `out` +4..+0xC and
///   `controls` +0..+4/+8 cleared, throttle `out` +0xE = `controls` +6 =
///   -0x1000; phase 1.
/// - 1: throttle = count * 0x2000 / 90 - 0x1000 (at most 0x1000) from
///   state +0xC, which counts to 0x5B: then phase 2, the voice at state
///   +0x52 released (8002DDA8) and the engine sound 0x5C (PAL 0x60, 0x63 in
///   replay)
///   started (8002DB4C) into +0x52.
/// - 2: speed += 0x11C (0x30C in replay); the vector (0, 0, that step)
///   rotated by the entity's orientation (80030778 of +0x18) is added to
///   the velocity (+0x38..+0x40) and the velocity to the position; above
///   speed 0xF000 phase 3 and the voice released.
/// - 3: returns 1 without aiming.
///
/// Other phases and 0..2 end with 8006EE38 (entity, state) and return 0.
pub fn takeoff_run_80074488(
    m: &mut Machine,
    entity: u32,
    state: u32,
    out: u32,
    controls: u32,
    view: u32,
    replay: u32,
) -> u32 {
    const FRAME: u32 = 0x68;
    let sp = m.push_frame(FRAME);
    match m.lw(state.wrapping_add(8)) {
        0 => {
            place_on_ground_80074320(m, entity, state, view);
            m.sw(state.wrapping_add(0x48), 0);
            for offset in [0x38, 0x3c, 0x40] {
                m.sw(entity.wrapping_add(offset), 0);
            }
            for offset in [0x30, 0x32, 0x34, 0x38, 0x3a, 0x3c, 0x4c] {
                m.sh(state.wrapping_add(offset), 0);
            }
            for offset in [4, 6, 8, 0xa, 0xc] {
                m.sh(out.wrapping_add(offset), 0);
            }
            m.sh(out.wrapping_add(0xe), 0xf000);
            for offset in [0, 2, 4] {
                m.sh(controls.wrapping_add(offset), 0);
            }
            m.sh(controls.wrapping_add(6), 0xf000);
            m.sh(controls.wrapping_add(8), 0);
            m.sw(state.wrapping_add(0xc), 0);
            m.sw(state.wrapping_add(8), 1);
        }
        1 => {
            // (count << 13) / 90: the reciprocal 0xB60B60B7, HI + dividend,
            // >> 6, minus the dividend's sign.
            let scaled = m.lw(state.wrapping_add(0xc)).wrapping_shl(13);
            let high = (i64::from(scaled as i32) * i64::from(0xb60b_60b7u32 as i32)) >> 32;
            let quotient =
                ((high as u32).wrapping_add(scaled) as i32 >> 6).wrapping_sub(scaled as i32 >> 31);
            let throttle = quotient.wrapping_sub(0x1000);
            m.sh(out.wrapping_add(0xe), throttle as u16);
            if throttle as i16 >= 0x1001 {
                m.sh(out.wrapping_add(0xe), 0x1000);
            }
            let throttle = m.lhu(out.wrapping_add(0xe));
            m.sh(controls.wrapping_add(6), throttle);
            let count = m.lw(state.wrapping_add(0xc)).wrapping_add(1);
            m.sw(state.wrapping_add(0xc), count);
            if count as i32 >= 0x5b {
                m.sw(state.wrapping_add(8), 2);
                m.sw(state.wrapping_add(0xc), 0);
                h_release_voice(m);
                let sound = 0x5c;
                let voice = external(m, 0x8002_db4c, [sound, 0, 0, 0]);
                let current = m.gw(0x520);
                m.sh(current.wrapping_add(0x52), voice as u16);
            }
        }
        2 => {
            let step: u32 = if replay != 0 { 0x30c } else { 0x11c };
            let (vector, matrix, rotated) = (
                sp.wrapping_add(0x10),
                sp.wrapping_add(0x18),
                sp.wrapping_add(0x38),
            );
            let speed = m.lw(state.wrapping_add(0x48)).wrapping_add(step);
            m.sw(state.wrapping_add(0x48), speed);
            m.sh(vector, 0);
            m.sh(vector.wrapping_add(2), 0);
            m.sh(vector.wrapping_add(4), step as u16);
            crate::shared::f_80030534(m, [entity.wrapping_add(0x18), matrix, 0, 0]);
            crate::sdk::set_rot_matrix(m, matrix);
            fm_rotate(m, vector, rotated);
            for i in 0..3 {
                let velocity = entity.wrapping_add(0x38 + i * 4);
                let sum = m
                    .lw(velocity)
                    .wrapping_add(m.lw(rotated.wrapping_add(i * 4)));
                m.sw(velocity, sum);
            }
            for i in 0..3 {
                let position = entity.wrapping_add(8 + i * 4);
                let sum = m
                    .lw(position)
                    .wrapping_add(m.lw(entity.wrapping_add(0x38 + i * 4)));
                m.sw(position, sum);
            }
            if 0xf000 < m.lw(state.wrapping_add(0x48)) as i32 {
                m.sw(state.wrapping_add(8), 3);
                m.sw(state.wrapping_add(0xc), 0);
                h_release_voice(m);
            }
        }
        3 => {
            m.pop_frame(FRAME);
            return 1;
        }
        _ => {}
    }
    aim_camera_8006d7dc(m, entity, state);
    m.pop_frame(FRAME);
    0
}

/// 8002DDA8 (release a sound channel) for the voice in `[gp+0x520]` +0x52
/// when it is not negative, as 80075B04 and 80076DEC do.
fn h_release_voice(m: &mut Machine) {
    let state = m.gw(0x520);
    let voice = m.lh(state.wrapping_add(0x52));
    if voice >= 0 {
        external(m, 0x8002_dda8, [i32::from(voice) as u32, 0, 0, 0]);
    }
}

/// 80052FA8 (JP; PAL 80051974, near, `del:3 imm:2 ins:4`): attack step of ground unit `unit` with attack parameters
/// (record +0x10: byte 0 kind, 0x10 = missiles; word +4 range): on mission 0xC
/// (context +0x38C) kind-0x36 units (PAL 0x46) only outside four x bands; a unit without a target
/// (+0x5B = 0xFF) picks one (80046124; flag 0x80 falls back to the player).
/// The distance is the unit's sight-record distance for the player, else
/// the Manhattan distance (>> 12) to the target (a target out of play is
/// dropped to 0xFF). Out of range the target is released (800464B0);
/// otherwise the gun (80053214) or missile (800535D4) step runs and a
/// result of 1 (target out of the firing cone or busy) releases it too.
/// Returns that result (0 when nothing fired).
pub fn unit_attack_80051974(m: &mut Machine, unit: u32) -> u32 {
    let record = a2_unit_record(m, unit);
    let units = m.gw(UNITS_2C8);
    let attack = m.lw(record.wrapping_add(0x10));
    let entity = m.lw(units.wrapping_add(unit.wrapping_mul(4)));
    if m.lbu(attack) == 0 {
        return 0;
    }
    if mission(m) == 0xc && m.lh(entity.wrapping_add(0x48)) == 0x36 {
        let x = m.lw(entity.wrapping_add(8));
        let outside = 0x200_0000 < x.wrapping_add(0x1b00_0000)
            && 0x400_0000 < x.wrapping_add(0x1400_0000)
            && 0x200_0000 < x.wrapping_add(0x700_0000)
            && 0x300_0000 < x.wrapping_add(0xff80_0000);
        if !outside {
            return 0;
        }
    }
    let slot = unit.wrapping_add(0x20);
    let target = entity.wrapping_add(0x5b);
    if m.lbu(target) == 0xff {
        let found = choose_target_800454ec(m, slot);
        m.sb(target, found as u8);
        if found & 0xff == 0xff {
            if m.lw(entity) & 0x80 != 0 {
                m.sb(target, 0);
            }
            if m.lbu(target) == 0xff {
                return 0;
            }
        }
    }
    let distance = if m.lbu(target) == 0 {
        let sight = m.lw(SIGHT_4C98.wrapping_add(slot.wrapping_mul(4)));
        m.lw(sight.wrapping_add(4))
    } else {
        let index = u32::from(m.lbu(target));
        let table = m.gw(crate::common::ENTITIES);
        let other_state = m.lw(a2_state(index));
        let other = m.lw(table.wrapping_add(index << 2));
        if other_state & 1 == 0 {
            m.sb(target, 0xff);
        }
        let mut sum = 0i32;
        for offset in [8, 0xc, 0x10] {
            let own = m.lw(entity.wrapping_add(offset));
            let theirs = m.lw(other.wrapping_add(offset));
            sum = sum.wrapping_add(a2_axis_distance(own, theirs));
        }
        sum as u32
    };
    if m.lw(attack.wrapping_add(4)) < distance {
        let released = external(m, 0x8004_64b0, [slot, 0, 0, 0]);
        m.sb(target, released as u8);
        return 0;
    }
    let result = if m.lbu(attack) & 0x10 != 0 {
        missile_attack_80051f9c(m, unit)
    } else {
        gun_attack_80051bdc(m, unit, distance)
    };
    if result == 0 {
        return 0;
    }
    let released = external(m, 0x8004_64b0, [slot, 0, 0, 0]);
    m.sb(target, released as u8);
    result
}

/// 80048758 (JP; PAL 80047A0C, near, `del:2 imm:9 ins:2 op:4 reg:6`: the log
/// at context +0x3B8 / +0x748 for PAL +0x3BC / +0x74C, the mission halfword
/// context +0x38C for PAL byte 800BAEB4, kinds 0x2D / 0x30 / 0x31 / 0x1B for
/// PAL 0x3F / 0x40 / 0x41 / 0x15): kill log entry for entity `slot` (8 bytes
/// at context +0x748, count at context +0x3C2): kind byte (+0x48); with
/// state bit 0x800 (kind 0x2D only after 4 such kills on missions
/// 0x11/0x1F/0x26/0x2E/0x34) a label (0x74 unflagged, 0x72/0x73 for kinds
/// 0x30/0x31, 0x70/0x71 by state bit 0x400 for kinds above; class 1 for kind
/// 0x1B on mission 0x21), the class (+1) with its statistics counter, the
/// slot (+6), map bytes of x and z (+4/+5) and the flight-log call count / 30
/// (+2); the count then increments.
pub fn record_kill_80047a0c(m: &mut Machine, slot: u32) {
    let context = m.gw(crate::common::CONTEXT);
    let table = m.gw(crate::common::ENTITIES);
    let log = context.wrapping_add(0x3b8);
    let count = u32::from(m.lbu(log.wrapping_add(0xa)));
    let entity = m.lw(table.wrapping_add(slot.wrapping_mul(4)));
    let entry = context.wrapping_add((count << 3) + 0x748);
    let kind = m.lbu(entity.wrapping_add(0x48));
    m.sb(entry, kind);
    let state = a2_state(slot);
    if m.lw(state) & 0x800 == 0 {
        return;
    }
    if m.lh(entity.wrapping_add(0x48)) == 0x2d {
        let stats = m.gw(A2_STATS_330);
        let kills = m.lbu(stats.wrapping_add(0x3c));
        let stage = mission(m);
        m.sb(stats.wrapping_add(0x3c), kills.wrapping_add(1));
        let stats = m.gw(A2_STATS_330);
        if m.lbu(stats.wrapping_add(0x3c)) < 4 {
            return;
        }
        if ![0x11, 0x1f, 0x26, 0x2e, 0x34].contains(&stage) {
            return;
        }
    }
    let label = if m.lw(entity) & 0x80 == 0 {
        Some(0x74)
    } else {
        match m.lbu(entry) {
            0x30 => Some(0x72),
            0x31 => Some(0x73),
            kind if kind < 0x31 => None,
            _ if m.lw(state) & 0x400 != 0 => Some(0x70),
            _ => Some(0x71),
        }
    };
    if let Some(label) = label {
        m.sb(entry, label);
    }
    let class = entry.wrapping_add(1);
    let special = mission(m) == 0x21 && m.lh(entity.wrapping_add(0x48)) == 0x1b;
    let attacker = |m: &mut Machine| m.lw(entity.wrapping_add(0x50)) & 0x3f;
    if special {
        m.sb(class, 1);
        a2_count(m, 0x2d);
    } else if m.lw(state) & 0x100 != 0 {
        if attacker(m) != 0 {
            m.sb(class, 1);
            a2_count(m, 0x2d);
        } else {
            m.sb(class, 4);
            a2_count(m, 0x2e);
        }
    } else {
        let flags = m.lw(entity);
        if flags & 0x100 != 0 {
            m.sb(class, 3);
            if attacker(m) == 0 {
                a2_count(m, 0xb);
            }
        } else if attacker(m) == 0 {
            m.sb(class, 0);
            a2_count(m, 0x2e);
        } else if flags & 0x40 != 0 {
            m.sb(class, 2);
        } else {
            m.sb(class, 1);
            a2_count(m, 0x2d);
        }
    }
    m.sb(entry.wrapping_add(6), slot as u8);
    let x = m.lw(entity.wrapping_add(8));
    m.sb(entry.wrapping_add(4), a2_map_byte(x));
    let z = m.lw(entity.wrapping_add(0x10));
    m.sb(entry.wrapping_add(5), a2_map_byte(z));
    let calls = u64::from(m.lhu(log.wrapping_add(2)));
    // `multu` by 0x88888889, HI >> 4: the call count / 30.
    m.sh(entry.wrapping_add(2), ((calls * 0x8888_8889) >> 36) as u16);
    let count = m.lbu(log.wrapping_add(0xa));
    m.sb(log.wrapping_add(0xa), count.wrapping_add(1));
}

/// a2: map coordinate byte `clamp(((v >> 12) + 0x40000) >> 11, 0, 255)`.
fn a2_map_byte(value: u32) -> u8 {
    let scaled = ((value as i32) >> 12).wrapping_add(0x40000) >> 11;
    scaled.clamp(0, 0xff) as u8
}

/// a2: increment the statistics byte `[gp+0x340] + offset`.
fn a2_count(m: &mut Machine, offset: u32) {
    let stats = m.gw(A2_STATS_330).wrapping_add(offset);
    let value = m.lbu(stats).wrapping_add(1);
    m.sb(stats, value);
}

/// a2: `[gp+0x340]`: mission statistics bytes (kills by class, ...).
const A2_STATS_330: u32 = 0x340;

/// 800520C8 (JP; PAL 80050A98, near, `del:4 imm:7 ins:3 op:4`): destruction of ground unit `unit` once it is in play and has no
/// hit points left: entity flag 0x200 cleared; statistics +0x20/+0x21 for
/// state bits 0x100/0x200; with state bit 0x800 and no attacker (+0x50) an
/// unflagged (0x40) unit focuses the kill camera (800797FC, context +0x39D
/// bit 0; PAL +0x3A1) and shows message 0x2E (kind-0x46 objects on mission
/// 0x16; PAL stage 0x16 kinds 0x56/0x57) or 7 (flag 0x80) through 8004CEB0
/// (PAL 8004B760); then the kill log (80048758), state = top byte | 2,
/// the destroyed state (80051E34) and the destruction script (record +0x1C,
/// 80054EBC).
pub fn destroy_unit_80050a98(m: &mut Machine, unit: u32) {
    let slot = unit.wrapping_add(0x20);
    let state = a2_state(slot);
    let entity = a2_unit_entity(m, unit);
    if m.lw(state) & 1 == 0 {
        return;
    }
    if m.lh(entity.wrapping_add(0x4e)) > 0 {
        return;
    }
    let flags = m.lw(entity) & !0x200;
    m.sw(entity, flags);
    if m.lw(state) & 0x100 != 0 {
        a2_count(m, 0x20);
    }
    if m.lw(state) & 0x200 != 0 {
        a2_count(m, 0x21);
    }
    if m.lw(state) & 0x800 != 0 && m.lw(entity.wrapping_add(0x50)) & 0x3f == 0 {
        let flags = m.lw(entity);
        if flags & 0x40 == 0 {
            let context = m.gw(crate::common::CONTEXT);
            if m.lbu(context.wrapping_add(0x39d)) & 1 != 0 {
                focus_kill_camera_8007818c(m, flags);
            }
            let message = if mission_16_object(m, entity) {
                Some(0x2e)
            } else if m.lw(entity) & 0x80 != 0 {
                Some(7)
            } else {
                None
            };
            if let Some(id) = message {
                external(m, SHOW_MESSAGE, [id, 0, 0, 0]);
            }
        }
    }
    record_kill_80047a0c(m, slot);
    let bits = (m.lw(state) & 0xff00_0000) | 2;
    m.sw(state, bits);
    unit_destroyed_8005080c(m, unit);
    let record = a2_unit_record(m, unit);
    let script = m.lw(record.wrapping_add(0x1c));
    if script != 0 {
        external(m, 0x8005_4ebc, [script, 0, 0, 0]);
    }
}

/// a2: the entity of ground unit `unit` (`[[gp+0x2D8] + unit * 4]`).
fn a2_unit_entity(m: &mut Machine, unit: u32) -> u32 {
    let units = m.gw(UNITS_2C8);
    m.lw(units.wrapping_add(unit.wrapping_mul(4)))
}

/// 800522B0 (JP; PAL 80050C84, near, `del:9 imm:13 ins:11 op:3`: mission
/// 0x2B from context +0x38C, PAL stage byte 800BAEB4; the kind-9 overlay
/// level is not reset at gameplay frame 0): motion of ground unit `unit` (a0). Motion record P =
/// `[[gp+0x388] + unit*0x28 + 0x14]`: byte 0 kind, +2 heading, +4 vertical
/// speed, +8 speed, +0x0C/+0x18/+0x24 waypoints (VECTOR). Unit entity E =
/// `[[gp+0x2D8] + unit*4]`: word 0 flags, +4 model byte, +5 flag byte, +6
/// angle, +8 position, +0x38 velocity, +0x58 phase byte, +0x5C/+0x60
/// counters, +0x68 angle step. Nothing happens while the unit's slot state
/// has 0x400000 or for kinds 10 and above. Kinds (jump table 800241D4):
/// - 0: velocity 0.
/// - 1: horizontal velocity along the heading (sine table 800104AC).
/// - 2: vertical velocity P+4 only.
/// - 3: vertical velocity -/+0x100 per frame by phase (0 and 3 down), the
///   phase advancing every 64 frames through 0..3.
/// - 4: waypoint run (80052D70) through the three waypoints by phase; on
///   mission 0x2B the arrival in phase 3 runs event scripts 0x148/0x14C once.
/// - 5: patrol between waypoints 1 and 2 (phase 0/1).
/// - 6: vertical approach to P+0x10 then angle stages and a run to
///   waypoint 1 (script 0x148 once on arrival, mission flag 0x40).
/// - 7/8: slot state bit 0x20000 when the player is inside a fixed box /
///   when the unit is less than P+0x0C above the player, unless E+5 has 0x4.
/// - 9: placed at the anchor (800B2078) nearest to the player; unit 0 also
///   runs the anchor timers and the 8008FC9C overlay.
///
/// The result (`v0`) is not used by the caller.
pub fn unit_motion_80050c84(m: &mut Machine, unit: u32) {
    let sp = m.push_frame(J_MOTION_FRAME);
    let records = m.gw(J_UNIT_RECORDS_378);
    let units = m.gw(UNITS_2C8);
    let record = records.wrapping_add(unit.wrapping_mul(0x28));
    let state = m.lw(j_unit_state(unit));
    let entity = m.lw(units.wrapping_add(unit << 2));
    let path = m.lw(record.wrapping_add(0x14));
    if state & 0x40_0000 == 0 {
        match m.lbu(path) {
            0 => {
                for offset in [0x38, 0x3c, 0x40] {
                    m.sw(entity.wrapping_add(offset), 0);
                }
            }
            1 => j_heading_velocity(m, entity, path),
            2 => {
                m.sw(entity.wrapping_add(0x38), 0);
                let vertical = m.lw(path.wrapping_add(4));
                m.sw(entity.wrapping_add(0x40), 0);
                m.sw(entity.wrapping_add(0x3c), vertical);
            }
            3 => j_bob(m, entity),
            4 => j_waypoint_run(m, entity, path, sp),
            5 => j_patrol(m, entity, path, sp),
            6 => j_approach(m, entity, path, sp),
            7 => {
                if j_player_in_box(m, sp) {
                    j_mark_unit(m, unit, entity);
                }
            }
            8 => {
                let player = slot_entity(m, 0);
                let height = m.lw(entity.wrapping_add(0x10));
                let player_height = m.lw(player.wrapping_add(0x10));
                let limit = m.lw(path.wrapping_add(0xc));
                if (height.wrapping_sub(player_height) as i32) < limit as i32 {
                    j_mark_unit(m, unit, entity);
                }
            }
            9 => j_anchor(m, unit, entity),
            _ => {}
        }
    }
    m.pop_frame(J_MOTION_FRAME);
}

/// Stack frame of 800522B0: the VECTORs at +0x10..+0x78 are guest memory
/// because their addresses go to 80052D70.
const J_MOTION_FRAME: u32 = 0xb0;

/// Kind 9 (80052AC4). Unit 0 first finds the anchor nearest to the player
/// (80083DFC; index to `gp+0x494`) while counting each positive anchor timer
/// down on odd gameplay frames (+0xC bit 0); the nearest timer loses 0x10
/// more within 0x4000 and stops at 0. In gameplay mode 4 (+4) the overlay
/// level `[gp+0x484]` (JP: not reset at frame 0, PAL: 0) rises by 0x10 (below 0x400) within
/// 0x3000, with mission bit 0x8, 8008FC9C(gameplay +0x10, +0x14, level)
/// into gameplay +0x14 and 80076FA4 every 32 frames; farther away it falls
/// by 0x40 (not below 0), bit 0x8 clears and 8008FC9C runs while it stays
/// positive. Every unit then gets E+5 |= 4, model byte E+4 = definition
/// byte +2 + (3 - timer >> 12) * 2, angle E+6 = timer & 0xFFF and the
/// anchor position offset by +-0x800000 in x (unit bit 0) and z (unit >> 1).
fn j_anchor(m: &mut Machine, unit: u32, entity: u32) {
    let player = slot_entity(m, 0);
    if unit == 0 {
        let mut nearest = i32::MAX;
        for index in 0..3u32 {
            let anchor = J_ANCHORS_01B4 + index * 0x10;
            let distance =
                crate::shared::f_800827b4(m, [anchor, player.wrapping_add(8), 0, 0]) as i32;
            if distance < nearest {
                nearest = distance;
                m.sh(crate::GP + J_NEAREST_48C, index as u16);
            }
            let timer = crate::GP + J_TIMERS_480 + index * 2;
            let value = m.lh(timer);
            let raw = m.lhu(timer);
            if value > 0 {
                let gameplay = m.gw(crate::common::GAMEPLAY);
                if m.lw(gameplay.wrapping_add(0xc)) & 1 != 0 {
                    m.sh(timer, raw.wrapping_sub(1));
                }
            }
        }
        let timer = j_nearest_timer(m);
        let value = m.lh(timer);
        let raw = m.lhu(timer);
        if value > 0 && nearest < 0x4000 {
            m.sh(timer, raw.wrapping_sub(0x10));
        }
        let timer = j_nearest_timer(m);
        if m.lh(timer) < 0 {
            m.sh(timer, 0);
        }
        let gameplay = m.gw(crate::common::GAMEPLAY);
        if m.lw(gameplay.wrapping_add(4)) == 4 {
            j_overlay(m, gameplay, nearest);
        }
    }
    let flags = m.lbu(entity.wrapping_add(5));
    m.sb(entity.wrapping_add(5), flags | 4);
    let timer = j_nearest_timer(m);
    let records = m.gw(J_UNIT_RECORDS_378);
    let value = m.lhu(timer);
    let definition = m.lw(records
        .wrapping_add(unit.wrapping_mul(0x28))
        .wrapping_add(8));
    let level = (value as i16 >> 12) as u8;
    let base = m.lbu(definition.wrapping_add(2));
    m.sb(
        entity.wrapping_add(4),
        base.wrapping_add(3u8.wrapping_sub(level).wrapping_mul(2)),
    );
    let index = m.lh(crate::GP + J_NEAREST_48C) as i32 as u32;
    let value = m.lhu(
        crate::GP
            .wrapping_add(J_TIMERS_480)
            .wrapping_add(index << 1),
    );
    m.sh(entity.wrapping_add(6), value & 0xfff);
    let anchor = J_ANCHORS_01B4.wrapping_add(index << 4);
    let x = m
        .lw(anchor)
        .wrapping_add((unit & 1) << 24)
        .wrapping_add(0xff80_0000);
    m.sw(entity.wrapping_add(8), x);
    let y = m.lw(anchor.wrapping_add(4));
    m.sw(entity.wrapping_add(0xc), y);
    let row = ((unit as i32) >> 1) as u32;
    let z = m
        .lw(anchor.wrapping_add(8))
        .wrapping_sub(row << 24)
        .wrapping_add(0x80_0000);
    m.sw(entity.wrapping_add(0x10), z);
}

/// 800B2078: three 16-byte anchor positions (VECTOR).
const J_ANCHORS_01B4: u32 = 0x800b_2078;

/// `gp+0x488`: three halfword timers, one per anchor (0x4000 at mission
/// start, 80053B5C).
const J_TIMERS_480: u32 = 0x488;

/// `gp+0x494`: halfword index of the anchor nearest to the player.
const J_NEAREST_48C: u32 = 0x494;

/// `[gp+0x388]`: 0x28-byte records of entities 32..63 (+0x08 definition,
/// +0x14 motion record).
const J_UNIT_RECORDS_378: u32 = 0x388;

/// Address of the anchor timer `gp+0x488 + [gp+0x494] * 2` (signed index).
fn j_nearest_timer(m: &mut Machine) -> u32 {
    let index = m.lh(crate::GP + J_NEAREST_48C) as i32 as u32;
    (index << 1).wrapping_add(crate::GP + J_TIMERS_480)
}

/// Kind 9 overlay in gameplay mode 4 (80052BA8..80052C84): see
/// [`j_anchor`]. 8008FC9C (HUD, not owned here) takes the original
/// `a0..a3` (a3 still holds the gameplay pointer); 80076FA4 takes no
/// arguments.
fn j_overlay(m: &mut Machine, gameplay: u32, nearest: i32) {
    if nearest < 0x3000 {
        let mission = m.gw(crate::script::MISSION);
        let word = m.lw(mission);
        let level = m.gw(J_OVERLAY_47C);
        m.sw(mission, word | 8);
        let a0 = m.lw(gameplay.wrapping_add(0x10));
        let a1 = m.lw(gameplay.wrapping_add(0x14));
        let next = external(m, 0x8008_fc9c, [a0, a1, level, gameplay]);
        let gameplay = m.gw(crate::common::GAMEPLAY);
        let level = m.gw(J_OVERLAY_47C);
        m.sw(gameplay.wrapping_add(0x14), next);
        if (level as i32) < 0x400 {
            m.set_gw(J_OVERLAY_47C, level.wrapping_add(0x10));
        }
        if m.lw(gameplay.wrapping_add(0xc)) & 0x1f == 0 {
            external(m, 0x8007_6fa4, [gameplay, 0, 0, 0]);
        }
    } else {
        let mission = m.gw(crate::script::MISSION);
        let level = m.gw(J_OVERLAY_47C).wrapping_sub(0x40);
        let word = m.lw(mission);
        m.set_gw(J_OVERLAY_47C, level);
        m.sw(mission, word & !8);
        if (level as i32) < 0 {
            m.set_gw(J_OVERLAY_47C, 0);
        }
        let level = m.gw(J_OVERLAY_47C);
        if level as i32 > 0 {
            let a0 = m.lw(gameplay.wrapping_add(0x10));
            let a1 = m.lw(gameplay.wrapping_add(0x14));
            let next = external(m, 0x8008_fc9c, [a0, a1, level, gameplay]);
            let gameplay = m.gw(crate::common::GAMEPLAY);
            m.sw(gameplay.wrapping_add(0x14), next);
        }
    }
}

/// `[gp+0x484]`: overlay level passed to 8008FC9C.
const J_OVERLAY_47C: u32 = 0x484;

/// 80052A88: slot state bit 0x20000 for the unit unless E+5 has bit 0x4.
fn j_mark_unit(m: &mut Machine, unit: u32, entity: u32) {
    if m.lbu(entity.wrapping_add(5)) & 4 == 0 {
        let state = j_unit_state(unit);
        let word = m.lw(state);
        m.sw(state, word | 0x2_0000);
    }
}

/// Slot state word of ground unit `unit` (800C6FD0 slot 0x20 + unit).
fn j_unit_state(unit: u32) -> u32 {
    SLOT_STATE_4D98.wrapping_add(unit.wrapping_add(0x20) << 2)
}

/// Kind 7 (800529D0): whether the player position >> 12 (spilled to
/// sp+0x80..0x88) lies in -0x4FFF <= x < -0x4800, y >= -0x3FF and
/// -0x57FF <= z < -0x1800.
fn j_player_in_box(m: &mut Machine, sp: u32) -> bool {
    let mut position = [0i32; 3];
    for (i, value) in (0u32..).zip(position.iter_mut()) {
        let player = slot_entity(m, 0);
        *value = m.lw(player.wrapping_add(8 + i * 4)) as i32 >> 12;
        m.sw(sp + 0x80 + i * 4, *value as u32);
    }
    let [x, y, z] = position;
    (-0x4fff..-0x4800).contains(&x) && y >= -0x3ff && (-0x57ff..-0x1800).contains(&z)
}

/// Kind 6 (800527C4): E+5 |= 4, then by phase:
/// - 0: velocity (0, P+4, 0); angle 0x3000 once E.y - P+0x10 >= -0x100000;
///   at E.y >= P+0x10: angle 0x1000, E.y = P+0x10, phase 1, counter 0.
/// - 1: angle 0x2000 below 0x20 counts, 0x3000 below 0x40, then phase 2
///   with step E+0x68 = 0x200; the counter E+0x60 advances.
/// - 2: E.y is limited to P+0x10, direction to waypoint 1 (sp+0x60) with
///   script +0x148 once at distance 0 (mission word bit 0x40), velocity
///   along it, step = -0x200 at angle 0x4000 and 0x200 at 0x2000, angle +=
///   step.
fn j_approach(m: &mut Machine, entity: u32, path: u32, sp: u32) {
    let at = |offset: u32| entity.wrapping_add(offset);
    let flags = m.lbu(at(5));
    let phase = m.lb(at(0x58));
    m.sb(at(5), flags | 4);
    match phase {
        0 => {
            m.sw(at(0x38), 0);
            let vertical = m.lw(path.wrapping_add(4));
            m.sw(at(0x40), 0);
            m.sw(at(0x3c), vertical);
            let height = m.lw(at(0xc));
            let target = m.lw(path.wrapping_add(0x10));
            if height.wrapping_sub(target) as i32 >= -0x10_0000 {
                m.sh(at(6), 0x3000);
            }
            let height = m.lw(at(0xc));
            let target = m.lw(path.wrapping_add(0x10));
            if (height as i32) >= target as i32 {
                m.sh(at(6), 0x1000);
                m.sw(at(0xc), target);
                m.sb(at(0x58), 1);
                m.sw(at(0x60), 0);
            }
        }
        1 => {
            let count = m.lw(at(0x60)) as i32;
            if count < 0x20 {
                m.sh(at(6), 0x2000);
            } else if count < 0x40 {
                m.sh(at(6), 0x3000);
            } else {
                m.sb(at(0x58), 2);
                m.sw(at(0x68), 0x200);
            }
            let count = m.lw(at(0x60));
            m.sw(at(0x60), count.wrapping_add(1));
        }
        2 => {
            let target = m.lw(path.wrapping_add(0x10));
            if (target as i32) < m.lw(at(0xc)) as i32 {
                m.sw(at(0xc), target);
            }
            let (to, out) = (sp + 0x60, sp + 0x70);
            j_copy_vector(m, path.wrapping_add(0xc), to);
            let distance = direction_8005173c(m, at(8), to, out);
            let mission = m.gw(crate::script::MISSION);
            if !m.lw(mission) & 0x40 != 0 && distance == 0 {
                let scripts = m.gw(J_SCRIPTS_328);
                let script = m.lw(scripts.wrapping_add(0x148));
                crate::script::interpret(m, script);
                let mission = m.gw(crate::script::MISSION);
                let word = m.lw(mission);
                m.sw(mission, word | 0x40);
            }
            j_set_velocity(m, entity, path, out);
            match m.lh(at(6)) {
                0x4000 => m.sw(at(0x68), -0x200i32 as u32),
                0x2000 => m.sw(at(0x68), 0x200),
                _ => {}
            }
            let angle = m.lhu(at(6));
            let step = m.lhu(at(0x68));
            m.sh(at(6), angle.wrapping_add(step));
        }
        _ => {}
    }
}

/// Entity velocity +0x38/+0x3C/+0x40 = motion speed (`path` +8, loaded
/// again for every component) times the direction VECTOR at `direction`,
/// `>> 14` (80052684, 80052770, 80052940).
fn j_set_velocity(m: &mut Machine, entity: u32, path: u32, direction: u32) {
    for i in 0..3 {
        let speed = m.lw(path.wrapping_add(8));
        let component = m.lw(direction.wrapping_add(i * 4));
        m.sw(entity.wrapping_add(0x38 + i * 4), j_scale(speed, component));
    }
}

/// `(speed * component) >> 14`: `mult` low word, arithmetic shift.
fn j_scale(speed: u32, component: u32) -> u32 {
    ((speed as i32).wrapping_mul(component as i32) >> 14) as u32
}

/// `[gp+0x338]`: mission script pointer table (+0x148, +0x14C event scripts).
const J_SCRIPTS_328: u32 = 0x338;

/// Copy the three words of the VECTOR at `src` to `dst` (load, store).
fn j_copy_vector(m: &mut Machine, src: u32, dst: u32) {
    for i in 0..3 {
        let word = m.lw(src.wrapping_add(i * 4));
        m.sw(dst.wrapping_add(i * 4), word);
    }
}

/// Kind 5 (800526CC): direction from the unit position to waypoint 1
/// (phase 0) or 2 (phase 1); other phases reuse the never-written frame
/// words at sp+0x40 as the original does. A distance under the speed
/// toggles the phase (nonzero -> 0, 0 -> 1). Velocity follows the
/// direction.
fn j_patrol(m: &mut Machine, entity: u32, path: u32, sp: u32) {
    let (to, out) = (sp + 0x40, sp + 0x50);
    match m.lb(entity.wrapping_add(0x58)) {
        0 => j_copy_vector(m, path.wrapping_add(0xc), to),
        1 => j_copy_vector(m, path.wrapping_add(0x18), to),
        _ => {}
    }
    let distance = direction_8005173c(m, entity.wrapping_add(8), to, out);
    if (distance as i32) < m.lw(path.wrapping_add(8)) as i32 {
        let phase = u8::from(m.lb(entity.wrapping_add(0x58)) == 0);
        m.sb(entity.wrapping_add(0x58), phase);
    }
    j_set_velocity(m, entity, path, out);
}

/// Kind 4 (80052454): direction (80052D70) from the unit position (four
/// words copied to sp+0x10) to waypoint 1/2/3 for phase 0/1/other; on mission
/// 0x2B phases other than 0..2 start at waypoint 2 instead. Below phase 3,
/// a distance under the speed advances the phase. On mission 0x2B in phase 3
/// at distance 0, script +0x148 runs once (E flag 0x40, `[gp+0x490]` bit
/// 0x40), else script +0x14C once (E flag 0x80, bit 0x80). Then the
/// velocity follows the direction.
fn j_waypoint_run(m: &mut Machine, entity: u32, path: u32, sp: u32) {
    let (from, to, out) = (sp + 0x10, sp + 0x20, sp + 0x30);
    let phase = m.lb(entity.wrapping_add(0x58));
    match phase {
        0 | 1 => {
            copy_words(m, entity.wrapping_add(8), from, 4);
            let waypoint = if phase == 0 { 0xc } else { 0x18 };
            j_copy_vector(m, path.wrapping_add(waypoint), to);
        }
        _ => {
            if phase != 2 && mission(m) == 0x2b {
                j_copy_vector(m, path.wrapping_add(0x18), from);
            } else {
                copy_words(m, entity.wrapping_add(8), from, 4);
            }
            j_copy_vector(m, path.wrapping_add(0x24), to);
        }
    }
    let distance = direction_8005173c(m, from, to, out);
    let phase = m.lb(entity.wrapping_add(0x58));
    let raw = m.lbu(entity.wrapping_add(0x58));
    if phase < 3 && (distance as i32) < m.lw(path.wrapping_add(8)) as i32 {
        m.sb(entity.wrapping_add(0x58), raw.wrapping_add(1));
    }
    if mission(m) == 0x2b && m.lb(entity.wrapping_add(0x58)) == 3 && distance == 0 {
        if m.lw(entity) & 0x40 != 0 && !m.gw(J_EVENTS_488) & 0x40 != 0 {
            j_event_script(m, 0x148, 0x40);
        } else if m.lw(entity) & 0x80 != 0 && !m.gw(J_EVENTS_488) & 0x80 != 0 {
            j_event_script(m, 0x14c, 0x80);
        }
    }
    j_set_velocity(m, entity, path, out);
}

/// Mission script `[[gp+0x338] + offset]` through 80054EBC, then `bit` set
/// in `[gp+0x490]`.
fn j_event_script(m: &mut Machine, offset: u32, bit: u32) {
    let scripts = m.gw(J_SCRIPTS_328);
    let script = m.lw(scripts.wrapping_add(offset));
    crate::script::interpret(m, script);
    let events = m.gw(J_EVENTS_488);
    m.set_gw(J_EVENTS_488, events | bit);
}

/// `[gp+0x490]`: one-shot bits 0x40/0x80 of the two event scripts.
const J_EVENTS_488: u32 = 0x490;

/// Kind 3 (800523D4): E+0x3C -= 0x100 in phases 0 and 3, else += 0x100;
/// counter E+0x5C counts to 0x40, then restarts and advances the phase
/// byte (4 wraps to 0). E+0x38 = E+0x40 = 0.
fn j_bob(m: &mut Machine, entity: u32) {
    let phase = m.lb(entity.wrapping_add(0x58));
    let vertical = m.lw(entity.wrapping_add(0x3c));
    let vertical = if phase == 0 || phase == 3 {
        vertical.wrapping_sub(0x100)
    } else {
        vertical.wrapping_add(0x100)
    };
    m.sw(entity.wrapping_add(0x3c), vertical);
    let count = m.lw(entity.wrapping_add(0x5c)).wrapping_add(1);
    m.sw(entity.wrapping_add(0x5c), count);
    if count == 0x40 {
        let next = m.lbu(entity.wrapping_add(0x58)).wrapping_add(1);
        m.sb(entity.wrapping_add(0x58), next);
        if next == 4 {
            m.sb(entity.wrapping_add(0x58), 0);
        }
        m.sw(entity.wrapping_add(0x5c), 0);
    }
    m.sw(entity.wrapping_add(0x38), 0);
    m.sw(entity.wrapping_add(0x40), 0);
}

/// Kind 1 (80052360): E+0x3C = 0, E+0x38 = speed * sin(heading) >> 14,
/// E+0x40 = speed * cos(heading) >> 14 (heading = P+2 & 0x3FFF, cosine =
/// entry + 0x1000).
fn j_heading_velocity(m: &mut Machine, entity: u32, path: u32) {
    let heading = u32::from(m.lhu(path.wrapping_add(2))) & 0x3fff;
    let sine = m.lh(SINE_0498.wrapping_add(heading << 1));
    let speed = m.lw(path.wrapping_add(8));
    m.sw(entity.wrapping_add(0x3c), 0);
    m.sw(
        entity.wrapping_add(0x38),
        j_scale(speed, sine as i32 as u32),
    );
    let heading = (u32::from(m.lhu(path.wrapping_add(2))) & 0x3fff) + 0x1000;
    let cosine = m.lh(SINE_0498.wrapping_add(heading << 1));
    let speed = m.lw(path.wrapping_add(8));
    m.sw(
        entity.wrapping_add(0x40),
        j_scale(cosine as i32 as u32, speed),
    );
}

/// 800104AC: sine table, 0x4000 entries per turn (+0x1000 entries of cosine
/// overlap), 1.0 = 0x4000.
const SINE_0498: u32 = 0x8001_04ac;

/// 8006DD5C (JP; PAL 8006C58C, changed: the roll target has one stick turn
/// term (PAL two, divisor 0x400 before and 0x420 after the levelling), the
/// levelling below |roll| 0x800 and the push-back beyond |bank| 0x800 (PAL
/// 0xA00 for both), the turn added after the pitch-input scaling):
/// analog-pad control step, called by 80072938 when context +0x120 bit 2 is
/// set, as (`input` = context +0x120, `rates` `[gp+0x514]`, `out`
/// `[gp+0x518]`, `controls` `[gp+0x51C]`, stack words +0x10 `entity`, +0x14
/// `motion` (the caller's s1), +0x18 `state` `[gp+0x520]`).
///
/// Inputs as 8006D0DC (+0x08 buttons, +0x18 axis mask, axes +0x1C pitch,
/// +0x20 roll, +0x24 yaw, +0x28 throttle; buttons give +-0x1000). The
/// targets are coordinated with the entity attitude (+0x18 pitch, +0x1A
/// heading, +0x1C bank):
/// - roll: while |roll| < 0x800 the bank-levelling term `-bank * (0x800 -
///   |roll|) / 0x800`, beyond |bank| = 0x800 a push back of `|bank| - 0x800`
///   (both scaled by `0x1000 - |pitch angle|`), all scaled by `0x1000 -
///   |pitch input|`; then the turn term ([`f_turn`], divisor 0x400, which
///   also turns the heading) and a pitch-input correction beyond |bank| =
///   0x800;
/// - pitch: the pitch input faded out from |bank| = 0x800 (negated from
///   |bank| = 0x1000); the pitch angle (+0x18) changes by `motion+4 *
///   [state+0x4C] * |pitch angle| * |pitch input|` (negated beyond |bank| =
///   0x1000);
/// - yaw: axis or buttons, clamped to +-0x1000 (its rate limit too).
///
/// Rates and controls then follow [`f_control_tail`].
#[allow(clippy::too_many_arguments)]
pub fn analog_control_8006c58c(
    m: &mut Machine,
    input: u32,
    rates: u32,
    out: u32,
    controls: u32,
    entity: u32,
    motion: u32,
    state: u32,
) {
    let angles = entity.wrapping_add(0x18); // t5
    let buttons = m.lw(input.wrapping_add(8)); // t7
    let roll_input = if m.lw(input.wrapping_add(0x18)) & 2 != 0 {
        m.lw(input.wrapping_add(0x20)) as i32
    } else {
        let mut value = 0i32;
        if buttons & 0x0200_0000 != 0 {
            value = -0x1000;
        }
        if buttons & 0x0400_0000 != 0 {
            value = value.wrapping_add(0x1000);
        }
        value
    }; // t2
    let pitch_input = if m.lw(input.wrapping_add(0x18)) & 1 != 0 {
        m.lw(input.wrapping_add(0x1c)) as i32
    } else {
        let mut value = 0i32;
        if buttons & 0x0080_0000 != 0 {
            value = 0x1000;
        }
        if buttons & 0x0100_0000 != 0 {
            value = value.wrapping_sub(0x1000);
        }
        value
    }; // t4
    let roll_magnitude = magnitude(roll_input); // t0
    let bank = i32::from(m.lh(angles.wrapping_add(4))); // t1
    let bank_magnitude = magnitude(bank); // a0
    let level = 0x1000 - magnitude(i32::from(m.lh(angles))); // v1

    // Roll target (t3).
    let mut roll = 0i32;
    if roll_magnitude < 0x800 {
        let product = bank
            .wrapping_neg()
            .wrapping_mul(0x800_i32.wrapping_sub(roll_magnitude));
        roll = fm_mul12(f_round11(product), level);
    }
    if i32::from(m.lh(angles.wrapping_add(4))) >= 0x801 {
        roll = roll.wrapping_sub(fm_mul12(bank_magnitude.wrapping_sub(0x800), level));
    }
    if i32::from(m.lh(angles.wrapping_add(4))) < -0x800 {
        roll = roll.wrapping_add(fm_mul12(bank_magnitude.wrapping_sub(0x800), level));
    }
    roll = fm_mul12(roll, 0x1000_i32.wrapping_sub(magnitude(pitch_input)));
    roll = roll.wrapping_add(f_turn(m, angles, motion, state, roll_input, 0x400));

    // Pitch target (s1) and the pitch-angle change.
    let bank = i32::from(m.lh(angles.wrapping_add(4)));
    let bank_magnitude = magnitude(bank); // t1
    let excess = if bank_magnitude < 0x800 {
        0
    } else {
        (bank_magnitude.wrapping_sub(0x800) << 12) / 0x800
    };
    let lift = fm_mul12(0x1000 - excess, pitch_input);
    let pitch = if bank_magnitude < 0x1000 {
        lift
    } else {
        lift.wrapping_neg()
    };
    let pitch_magnitude = magnitude(pitch_input);
    let signed_excess = if i32::from(m.lh(angles.wrapping_add(4))) >= 0 {
        excess.wrapping_neg()
    } else {
        excess
    };
    let correction = fm_mul12(signed_excess, pitch_magnitude);
    let level = 0x1000 - magnitude(i32::from(m.lh(angles)));
    if bank_magnitude < 0x1000 {
        roll = roll.wrapping_add(fm_mul12(correction, level));
    }
    let scale = fm_mul12(magnitude(i32::from(m.lh(angles))), pitch_magnitude);
    let attitude = m.lhu(angles);
    let mut rate = m.lw(motion.wrapping_add(4)) as i32;
    if bank_magnitude >= 0x1001 {
        rate = rate.wrapping_neg();
    }
    let factor = i32::from(m.lh(state.wrapping_add(0x4c)));
    let delta = fm_mul12(fm_mul12(rate, factor), scale);
    m.sh(angles, attitude.wrapping_add(delta as u16));

    // Yaw target (t6) and its rate limit (s0).
    let mut yaw_limit: i32 = 0x1000;
    let mut yaw: i32 = 0;
    if m.lw(input.wrapping_add(0x18)) & 4 != 0 {
        yaw = m.lw(input.wrapping_add(0x24)) as i32;
        yaw_limit = magnitude(yaw);
    } else {
        if buttons & 0x2000_0000 != 0 {
            yaw = yaw.wrapping_sub(yaw_limit);
        }
        if buttons & 0x4000_0000 != 0 {
            yaw = yaw.wrapping_add(yaw_limit);
        }
    }
    let yaw = yaw.clamp(-0x1000, 0x1000);
    let yaw_limit = yaw_limit.clamp(-0x1000, 0x1000);

    // Throttle target (a2).
    let throttle = if m.lw(input.wrapping_add(0x18)) & 8 != 0 {
        m.lw(input.wrapping_add(0x28)) as i32
    } else {
        let mut value = 0i32;
        if buttons & 0x0800_0000 != 0 {
            value = 0x1000;
        }
        if buttons & 0x1000_0000 != 0 {
            value = value.wrapping_sub(0x1000);
        }
        value
    };
    f_control_tail(
        m,
        rates,
        out,
        controls,
        [pitch, yaw, roll, throttle],
        [magnitude(pitch), yaw_limit, magnitude(roll)],
    );
}

/// `value / 2048` rounded toward zero (`bgez; addiu 0x7FF; sra 11`).
fn f_round11(value: i32) -> i32 {
    if value < 0 {
        value.wrapping_add(0x7ff) >> 11
    } else {
        value >> 11
    }
}

/// The tail shared by 8006DD5C and 8007542C (the 8006D0DC form with the roll
/// rate integrating `roll >> 5`). `targets` are pitch, yaw, roll and
/// throttle; `limits` the pitch/yaw/roll rate clamps. Rates whose magnitude
/// exceeds their target's decay (to 64%, the throttle by the 0xEA0EA0EB
/// reciprocal); the controls (+0 roll, +2 pitch, +4 yaw) approach their
/// targets by `|delta| / 4 + 2` and snap when closer; the throttle lever (+8)
/// and power (+6) follow; the rates integrate pitch >> 3, yaw >> 2, roll >> 5
/// and throttle >> 5, are clamped and copied to `out` +4/+6/+8/+0xE.
///
/// 8007542C has no yaw-rate update (its yaw target is 0): the `+ (0 >> 2)`
/// store here writes the loaded value back unchanged.
fn f_control_tail(
    m: &mut Machine,
    rates: u32,
    out: u32,
    controls: u32,
    targets: [i32; 4],
    limits: [i32; 3],
) {
    let [pitch, yaw, roll, throttle] = targets;
    for (offset, target) in [(0, pitch), (2, yaw), (4, roll)] {
        let rate = m.lh(rates.wrapping_add(offset));
        if magnitude(target) < magnitude(i32::from(rate)) {
            m.sh(rates.wrapping_add(offset), decay_64_percent(rate));
        }
    }
    let rate = m.lh(rates.wrapping_add(6));
    if magnitude(throttle) < magnitude(i32::from(rate)) {
        let scaled = i32::from(rate) << 7;
        let high = ((i64::from(scaled) * i64::from(0xea0e_a0ebu32 as i32)) >> 32) as i32;
        let value = (high.wrapping_add(scaled) >> 7).wrapping_sub(scaled >> 31);
        m.sh(rates.wrapping_add(6), value as u16);
    }

    let step = |current: i16, target: i32| -> i32 {
        let delta = i32::from(current).wrapping_sub(target);
        if delta >= 0 {
            (delta >> 2).wrapping_add(2)
        } else {
            (target.wrapping_sub(i32::from(current)) >> 2).wrapping_add(2)
        }
    };
    let roll_step = step(m.lh(controls), roll);
    let pitch_step = step(m.lh(controls.wrapping_add(2)), pitch);
    let yaw_step = step(m.lh(controls.wrapping_add(4)), yaw);
    let approach = [
        (0, roll, roll_step),
        (2, pitch, pitch_step),
        (4, yaw, yaw_step),
    ];
    for (offset, target, amount) in approach {
        let at = controls.wrapping_add(offset);
        if target < i32::from(m.lh(at)) {
            let value = m.lhu(at);
            m.sh(at, value.wrapping_sub(amount as u16));
        }
        if i32::from(m.lh(at)) < target {
            let value = m.lhu(at);
            m.sh(at, value.wrapping_add(amount as u16));
        }
    }
    for (offset, target, amount) in approach {
        let at = controls.wrapping_add(offset);
        let current = i32::from(m.lh(at));
        let delta = current.wrapping_sub(target);
        let distance = if delta < 0 {
            target.wrapping_sub(current)
        } else {
            delta
        };
        if distance < amount {
            m.sh(at, target as u16);
        }
    }
    let lever = if throttle < -0x800 {
        throttle.wrapping_add(0x800).wrapping_neg() << 1
    } else {
        0
    };
    let at = controls.wrapping_add(8);
    if i32::from(m.lh(at)) < lever {
        let value = m.lhu(at);
        m.sh(at, value.wrapping_add(0x80));
    }
    if lever < i32::from(m.lh(at)) {
        let value = m.lhu(at);
        m.sh(at, value.wrapping_sub(0x80));
    }
    let power = (i32::from(m.lh(rates.wrapping_add(6))) + 0x1000) >> 1;
    m.sh(controls.wrapping_add(6), power as u16);

    for (offset, increment) in [
        (0, pitch >> 3),
        (2, yaw >> 2),
        (4, roll >> 5),
        (6, throttle >> 5),
    ] {
        let at = rates.wrapping_add(offset);
        let value = m.lhu(at);
        m.sh(at, value.wrapping_add(increment as u16));
    }
    let [pitch_limit, yaw_limit, roll_limit] = limits;
    for (offset, limit) in [
        (0, pitch_limit),
        (2, yaw_limit),
        (4, roll_limit),
        (6, 0x1000),
    ] {
        let at = rates.wrapping_add(offset);
        let low = limit.wrapping_neg();
        if i32::from(m.lh(at)) < low {
            m.sh(at, low as u16);
        }
        if limit < i32::from(m.lh(at)) {
            m.sh(at, limit as u16);
        }
    }
    for (from, to) in [(0, 4), (2, 6), (4, 8), (6, 0xe)] {
        let value = m.lhu(rates.wrapping_add(from));
        m.sh(out.wrapping_add(to), value);
    }
}

/// One roll-stick turn term of 8006DD5C (PAL 8006C67C.. with `divisor` 0x400,
/// PAL 8006C8E0.. with 0x420). `angles` is entity +0x18 (+0 pitch, +2 heading,
/// +4 bank). When the bank and `roll_input` have the same strict sign, the
/// gain is 0x1000, from |bank| = 0x400 reduced by
/// `((|bank| - 0x400) << 12) / divisor` and clamped to 0..=0x1000, and the
/// turn rate is motion word +0; otherwise the gain is 0x1000 and the rate
/// motion word +4. The heading turns by `sine(bank) / 4 * rate / 4096 *
/// [state+0x4C] / 4096`. Returns `gain * roll_input / 4096`.
fn f_turn(
    m: &mut Machine,
    angles: u32,
    motion: u32,
    state: u32,
    roll_input: i32,
    divisor: i32,
) -> i32 {
    let bank = i32::from(m.lh(angles.wrapping_add(4)));
    let (gain, rate) = if (bank > 0 && roll_input > 0) || (bank < 0 && roll_input < 0) {
        let bank_magnitude = magnitude(bank);
        let mut gain = 0x1000;
        if bank_magnitude >= 0x400 {
            gain = 0x1000 - ((bank_magnitude - 0x400) << 12) / divisor;
        }
        (gain.clamp(0, 0x1000), m.lw(motion))
    } else {
        (0x1000, m.lw(motion.wrapping_add(4)))
    };
    let turn = fm_mul12(gain, roll_input);
    let angle = m.lhu(angles.wrapping_add(4));
    let sine = f_quarter_sine(m, angle);
    let factor = i32::from(m.lh(state.wrapping_add(0x4c)));
    let delta = fm_mul12(fm_mul12(sine, rate as i32), factor);
    let heading = m.lhu(angles.wrapping_add(2));
    m.sh(angles.wrapping_add(2), heading.wrapping_add(delta as u16));
    turn
}

/// Sine-table entry (800104AC) for `angle & 0x3FFF` as `sll 16; sra 18`
/// (the signed halfword / 4), clamped to +-0x1000.
fn f_quarter_sine(m: &mut Machine, angle: u16) -> i32 {
    let entry = m.lhu(SINE_TABLE.wrapping_add(u32::from(angle & 0x3fff) << 1));
    (i32::from(entry as i16) >> 2).clamp(-0x1000, 0x1000)
}

/// 80070A80: contacts of `entity` (the player) with the 64 entities of the
/// table at `[gp+0x2E8]`. With the speed `v` = (|vx|+|vy|+|vz|) >> 12 and
/// the next position (position + velocity) >> 12, an entity whose word 0
/// has 0x4400 and radius `r` (+0x4C, halfword) is close when 3r + v is at
/// least the Manhattan distance and r² + v²/9 at least the squared distance
/// (32-bit wrapping products). A close entity flagged 0x1020 without +5 bit
/// 1 is tested exactly with 8009B4EC(position, velocity, other, state +0x1C;
/// stack word 0x1F800000); a hit sets state +0x2C = `[800BD0C4]`. Any other
/// close entity is a contact at the current position (state +0x1C..+0x24,
/// +0x2C = 0). Either way state +0x10 gets bit 0x20.
///
/// JP (exact; patched for the JP model ray): the speed `v` stays in s4,
/// which 8009B4EC leaves untouched, so it is the carried vertex register of
/// 8009A944 ([`object_ray_jp`], [`object_hit_jp`]).
pub fn collide_entities_8006f3fc(m: &mut Machine, entity: u32, state: u32) {
    let e = |offset: u32| entity.wrapping_add(offset);
    let s = |offset: u32| state.wrapping_add(offset);
    let sp = m.push_frame(0x40);
    let vx = m.lw(e(0x38));
    let vy = m.lw(e(0x3c));
    let vz = m.lw(e(0x40));
    let speed = (vx as i32)
        .wrapping_abs()
        .wrapping_add((vy as i32).wrapping_abs())
        .wrapping_add((vz as i32).wrapping_abs())
        >> 12;
    // mult/mflo, then the compiler's multiply-high by 0x38E38E39: v² / 9.
    let slack = speed.wrapping_mul(speed) / 9;
    let y = (m.lw(e(0xc)).wrapping_add(vy) as i32) >> 12;
    let x = (m.lw(e(8)).wrapping_add(vx) as i32) >> 12;
    let z = (m.lw(e(0x10)).wrapping_add(vz) as i32) >> 12;
    for slot in 0..0x40u32 {
        let table = m.gw(0x2e8);
        let other = m.lw(table.wrapping_add(slot * 4));
        let o = |offset: u32| other.wrapping_add(offset);
        let kind = m.lw(other);
        if kind & 0x4400 != 0x4400 {
            continue;
        }
        let radius = i32::from(m.lh(o(0x4c)));
        let dx = ((m.lw(o(8)) as i32) >> 12).wrapping_sub(x);
        let dy = ((m.lw(o(0xc)) as i32) >> 12).wrapping_sub(y);
        let dz = ((m.lw(o(0x10)) as i32) >> 12).wrapping_sub(z);
        let reach = (radius * 3).wrapping_add(speed);
        let manhattan = dx
            .wrapping_abs()
            .wrapping_add(dy.wrapping_abs())
            .wrapping_add(dz.wrapping_abs());
        if reach < manhattan {
            continue;
        }
        let distance = dx
            .wrapping_mul(dx)
            .wrapping_add(dy.wrapping_mul(dy))
            .wrapping_add(dz.wrapping_mul(dz));
        if (radius * radius).wrapping_add(slack) < distance {
            continue;
        }
        if kind & 0x1020 == 0x1020 && m.lbu(o(5)) & 2 == 0 {
            m.sw(sp + 0x10, 0x1f80_0000);
            let hit = object_ray_jp(m, e(8), e(0x38), other, s(0x1c), 0x1f80_0000, speed as u32);
            if hit == GROUND_RAY_MISS {
                continue;
            }
            let status = m.lw(s(0x10));
            let surface = m.lw(CONTACT_SURFACE);
            m.sw(s(0x2c), surface);
            m.sw(s(0x10), status | 0x20);
        } else {
            let status = m.lw(s(0x10));
            m.sw(s(0x10), status | 0x20);
            let px = m.lw(e(8));
            m.sw(s(0x1c), px);
            let py = m.lw(e(0xc));
            m.sw(s(0x20), py);
            let pz = m.lw(e(0x10));
            m.sw(s(0x2c), 0);
            m.sw(s(0x24), pz);
        }
    }
    m.pop_frame(0x40);
}

const CONTACT_SURFACE: u32 = 0x800b_d0c4;

/// Return value of the ground rays (8009A5D0, 8009A800, 8009B4EC) when
/// nothing was hit; the callers test `0x7FFFFFFE < v0` (signed).
const GROUND_RAY_MISS: u32 = 0x7fff_ffff;

/// 8009B4EC (JP; exact, REVIEW §3b): ray of the moving entity at VECTOR `from` with velocity
/// VECTOR `velocity` (the ray is velocity >> 12) against the model of the
/// entity `object` (model table `[[gp+0x2E0]+0x258]`, entry by byte +4;
/// relative position (object +8 - from) >> 12 as the SVECTOR offset).
/// With flag +5 bit 2 the entry holds five key frames: halfword +6 bits
/// 12..15 (signed, clamped to 0..4) pick the key, bits 0..11 the blend
/// toward the next key (80066A9C interpolates both vertex lists into
/// `work`, the caller's stack word +0x10); otherwise the first model is
/// used with its own vertices. 8009A944 does the search; `out` = its hit
/// point + `from`. Returns the hit distance (0x7FFFFFFF when none).
///
/// The generated `object_ray_800999fc` with `carry`, the caller's s4 (which
/// 8009B4EC does not touch), passed on to [`object_hit_jp`]: the JP model
/// ray's carried vertex register. Callers that know their s4 call this.
pub fn object_ray_jp(
    m: &mut Machine,
    from: u32,
    velocity: u32,
    object: u32,
    out: u32,
    work: u32,
    carry: u32,
) -> u32 {
    const SIZE: u32 = 0x48;
    let sp = m.push_frame(SIZE);
    let delta = sp.wrapping_add(0x18);
    let offset = sp.wrapping_add(0x28);
    for i in 0..3 {
        let value = m.lw(velocity.wrapping_add(i * 4));
        m.sw(delta.wrapping_add(i * 4), sra(value, 12));
    }
    for i in 0..3 {
        let position = m.lw(object.wrapping_add(8 + i * 4));
        let origin = m.lw(from.wrapping_add(i * 4));
        m.sh(
            offset.wrapping_add(i * 2),
            sra(position.wrapping_sub(origin), 12) as u16,
        );
    }
    let kind = u32::from(m.lbu(object.wrapping_add(4)));
    let gameplay = m.gw(crate::common::GAMEPLAY);
    let models = m.lw(gameplay.wrapping_add(0x258));
    let entry = (kind << 2).wrapping_add(models);
    let (model, vertices, rotated) = if m.lbu(object.wrapping_add(5)) & 4 == 0 {
        let model = m.lw(entry);
        (model, m.lw(model.wrapping_add(4)), work)
    } else {
        let frame = m.lhu(object.wrapping_add(6));
        let key = ((u32::from(frame) << 16) as i32 >> 28).clamp(0, 4) as u32;
        let blend = u32::from(frame) & 0xfff;
        let model = m.lw(entry.wrapping_add(key * 4));
        if blend == 0 {
            (model, m.lw(model.wrapping_add(4)), work)
        } else {
            let next = if key < 4 { key + 1 } else { 4 };
            let other = m.lw(entry.wrapping_add(next * 4));
            let first = m.lw(model.wrapping_add(4));
            let second = m.lw(other.wrapping_add(4));
            let rotated = external(m, 0x8006_6a9c, [blend, first, second, work]);
            (model, work, rotated)
        }
    };
    m.sw(sp.wrapping_add(0x10), vertices);
    m.sw(sp.wrapping_add(0x14), offset);
    let distance = object_hit_jp(m, delta, out, rotated, model, vertices, offset, carry);
    c2_add_vector(m, out, from);
    m.pop_frame(SIZE);
    distance
}

/// `point` += `base` (VECTORs, word by word, wrapping).
fn c2_add_vector(m: &mut Machine, point: u32, base: u32) {
    for i in 0..3 {
        let value = m.lw(point.wrapping_add(i * 4));
        let offset = m.lw(base.wrapping_add(i * 4));
        m.sw(point.wrapping_add(i * 4), value.wrapping_add(offset));
    }
}
