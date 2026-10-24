//! Player flight update: the four routines the resident flight update
//! 80032F3C calls first and their dynamic subtrees, including the paths that
//! pad input reaches (pitch/roll/yaw/throttle buttons, gun and missile
//! fire, vibration, camera shake). See docs/FLIGHT_MACHINE.md for the
//! translation rules.
//!
//! Top-level entry points (no arguments, called by address from 80032F3C):
//! - 800729DC [`update_player_80071358`]: player state modes, controls
//!   (80072938, 8006D0DC), flight model (8006EF30), effects/weapons
//!   (80072178) and the flight camera block.
//! - 80045898 [`update_damage_80044d50`]: hit timers, damage, link damage,
//!   lock slots.
//! - 800517F4 [`update_ground_units_800501c8`]: ground-unit marks, sounds
//!   and motion.
//! - 8004658C [`update_sensors_80045950`]: linked entities, sight/home
//!   records, flight-path log.
//!
//! Sections: shared leaves; controls and 800729DC; 80072178 and the engine
//! sound; weapons (gun/missile handler tables 800B3B44/800B3B60, projectile
//! allocation, vibration, screen effects); group A (80045898, 800517F4,
//! 8004658C subtrees); group B (80072178 subtree); group C (terrain ray,
//! orientation, camera aim); group D (flight model 8006EF30).
//!
//! Conventions in this module:
//! - Arguments and return values are the original registers (`a0..a3` as
//!   `u32`, `v0` as the returned `u32`); a fifth or later argument is the
//!   caller's stack word +0x10.., passed as an extra parameter. Callers that
//!   call a routine owned elsewhere with stack arguments store them in their
//!   own pushed frame at the original offsets; [`dispatch`] reads such
//!   arguments from `m.sp + 0x10..`.
//! - Calls to routines owned by other modules (or not yet translated) go
//!   through [`crate::dispatch::call`] by original address; shared routines
//!   through [`crate::shared`], SDK routines through [`crate::sdk`].
//! - Frames are pushed (`Machine::push_frame`, original size) only where a
//!   local's address is passed on or stack arguments are stored.
//! - Every translated routine is claimed in [`dispatch`].
use crate::{
    dispatch as calls,
    machine::{Machine, SmallData},
};
use ac3_gte::reg;

/// A routine owned elsewhere, called by original address (`jal`), with the
/// original argument registers; returns its `v0`.
fn external(m: &mut Machine, address: u32, args: [u32; 4]) -> u32 {
    calls::call(m, address, args)
}

// ---- small leaves used by several subtrees --------------------------------------

/// 80083F40: length of the vector (`x`, `y`, `z`) through the GTE: SQR
/// (sf=0, lm=1) of IR1..IR3, the sum of MAC1..MAC3 normalized through
/// LZCS/LZCR, and the square-root table at 800B6DD4. A sum <= 0 gives 0.
pub fn vector_length_800828f8(m: &mut Machine, x: u32, y: u32, z: u32) -> u32 {
    m.gte.mtc2(reg::IR1, x);
    m.gte.mtc2(reg::IR2, y);
    m.gte.mtc2(reg::IR3, z);
    m.gte.sqr(false, true);
    let a = m.gte.mfc2(reg::MAC1);
    let b = m.gte.mfc2(reg::MAC2);
    let c = m.gte.mfc2(reg::MAC3);
    let sum = a.wrapping_add(b).wrapping_add(c);
    m.gte.mtc2(reg::LZCS, sum);
    if sum as i32 <= 0 {
        return 0;
    }
    let zeros = m.gte.mfc2(reg::LZCR) & 0xfe;
    let mantissa = if zeros < 0x18 {
        ((sum as i32) >> (0x18 - zeros)) as u32
    } else {
        sum << (zeros - 0x18)
    };
    let root = m.lh(0x800b_6dd4u32.wrapping_add(mantissa.wrapping_sub(0x40).wrapping_mul(2)));
    let shift = (0x1f - zeros) >> 1;
    ((i32::from(root) << shift) >> 12) as u32
}

/// 8002939C: claim one of the six 4-byte vibration slots at `[gp+0x30]`
/// for `priority`: the first slot that is idle (byte 0 = 0) or whose
/// priority (byte 1) is not above `priority` (signed) receives byte 1 =
/// `priority`. Returns the slot address, or 0 when none qualifies.
pub fn claim_vibration_80028f7c(m: &mut Machine, priority: u32) -> u32 {
    let mut slot = m.gw(0x30);
    for _ in 0..6 {
        if m.lbu(slot) == 0 || priority as i32 >= i32::from(m.lbu(slot.wrapping_add(1))) {
            m.sb(slot.wrapping_add(1), priority as u8);
            return slot;
        }
        slot = slot.wrapping_add(4);
    }
    0
}

/// 800293F0: vibration of `strength` (signed). From 0x20 upwards a slot is
/// claimed with priority 5 (8002939C); it gets byte 0 = 1 and byte 2 =
/// `strength >> 5`, where 0x6D and above become 0x69.
pub fn vibrate_80028fd0(m: &mut Machine, strength: u32) {
    if (strength as i32) < 0x20 {
        return;
    }
    let slot = claim_vibration_80028f7c(m, 5);
    if slot == 0 {
        return;
    }
    m.sb(slot, 1);
    let mut level = (strength as i32) >> 5;
    if level >= 0x6d {
        level = 0x69;
    }
    m.sb(slot.wrapping_add(2), level as u8);
}

// ---- player controls (800729DC group) ----------------------------------------------

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

// JP-REVIEW(R0262): near function: PAL 8006B90C -> JP 8006D0DC, reg:1, 1 instructions differ (whole routine)
/// 8006D0DC: digital/analog control step. `input` is the pad block
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
// JP-PATCH: replaced by `crate::jp::res_p1_player::control_step_8006b90c` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn control_step_8006b90c(m: &mut Machine, input: u32, rates: u32, out: u32, controls: u32) {
    crate::jp::res_p1_player::control_step_8006b90c(m, input, rates, out, controls)
}

/// 8006E874: scale the SVECTOR at `vector` by `speed / 21` (4.12 fixed
/// point; speeds above 0x14FFF scale by 0x1000): each component becomes
/// `component * scale / 4096`, rounded toward zero.
pub fn scale_by_speed_8006d218(m: &mut Machine, speed: u32, vector: u32) {
    let speed = speed as i32;
    let scale = if speed > 0x14fff {
        0x1000
    } else {
        let high = ((i64::from(speed) * 0x30c3_0c31) >> 32) as i32;
        (high >> 2).wrapping_sub(speed >> 31)
    };
    let fixed = |component: i16| {
        let product = (i64::from(component) * i64::from(scale)) as i32;
        let product = if product < 0 {
            product.wrapping_add(0xfff)
        } else {
            product
        };
        (product >> 12) as u16
    };
    let y = m.lh(vector.wrapping_add(2));
    let z = m.lh(vector.wrapping_add(4));
    let x = m.lh(vector);
    m.sh(vector.wrapping_add(2), fixed(y));
    m.sh(vector.wrapping_add(4), fixed(z));
    m.sh(vector, fixed(x));
}

/// 80072938: player control input for `entity` (`flags` is the caller's
/// s1, forwarded to 8006DD5C). Context +0x128 negative: 80073B18 (entity,
/// [gp+0x514], [gp+0x518], [gp+0x51C], 0); else with context +0x120 bit 2:
/// 8006DD5C (context+0x120, the same three, entity, flags, [gp+0x520]);
/// otherwise the pad step 8006D0DC.
pub fn control_input_800712b4(m: &mut Machine, entity: u32, flags: u32) {
    const FRAME: u32 = 0x28;
    let context = m.gw(crate::common::CONTEXT);
    let rates = m.gw(0x514);
    let out = m.gw(0x518);
    let controls = m.gw(0x51c);
    if (m.lw(context.wrapping_add(0x128)) as i32) < 0 {
        // Stack arguments are stored at their original frame offsets.
        let sp = m.push_frame(FRAME);
        m.sw(sp.wrapping_add(0x10), 0);
        auto_level_80072494(m, entity, rates, out, controls, 0);
        m.pop_frame(FRAME);
    } else if m.lw(context.wrapping_add(0x120)) & 4 != 0 {
        let sp = m.push_frame(FRAME);
        let state = m.gw(0x520);
        m.sw(sp.wrapping_add(0x14), flags);
        m.sw(sp.wrapping_add(0x10), entity);
        m.sw(sp.wrapping_add(0x18), state);
        analog_control_8006c58c(
            m,
            context.wrapping_add(0x120),
            rates,
            out,
            controls,
            entity,
            flags,
            state,
        );
        m.pop_frame(FRAME);
    } else {
        control_step_8006b90c(m, context.wrapping_add(0x120), rates, out, controls);
    }
}

/// `[0x800BCE18]`: camera/view block (angles at +0x10, position at +0).
const VIEW: u32 = 0x800b_ce18;

// JP-REVIEW(R0267): near function: PAL 80071358 -> JP 800729DC, imm:4, 4 instructions differ (whole routine)
/// 800729DC: player flight update (called by 80032F3C with no arguments).
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
// JP-PATCH: replaced by `crate::jp::res_p1_player::update_player_80071358` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn update_player_80071358(m: &mut Machine) {
    crate::jp::res_p1_player::update_player_80071358(m)
}

// ---- player effects 80072178 and engine sound 80072114 --------------------------

// JP-REVIEW(R0271): near function: PAL 80070AF4 -> JP 80072178, imm:4, 4 instructions differ (whole routine)
/// 80072178: per-frame player effects selected by `flags` (the
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
///   subtracts 3 / 5 / 100 hit points by the context's level (+0x398),
///   raises event 4, plays sound 0x27 (8002DBE0) unless context +0x28 has
///   bits 0..2, and reacts through 80082BAC.
/// - unless state word 0 has bits 0x7000: event vibrations (800295CC,
///   80029604, 80029588, each restarting the shake counter state +0x54),
///   8006CB44(0) below altitude 0x400, and the engine sound (80072114).
/// - shake counter: 8006CB44(1) while it runs; above speed 0x8C000 (state
///   +0x48) 8006CB44(2) and a 0x800 vibration.
/// - weapons: gun = context +0x3AD, special weapon and its reload slots from
///   context +0x3AE and state +0x58 (or +0x3AF and +0x64 when 8003A5C0 is
///   set), both overridable (8003A624, 8003A62C); control +4 = the
///   aircraft's count (+0x7C / +0x7E) capped by the table at 800B3B28; the
///   reload timers tick (800718E0); bit 3 runs the fire handlers (gun on
///   context +0x128 bit 0x800, weapon on +0x12C bit 0x1000) and counts down
///   state +0x56; control +0x20 = slots in use (800718A0 marks free ones).
// JP-PATCH: replaced by `crate::jp::res_p1_player::player_effects_80070af4` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn player_effects_80070af4(m: &mut Machine, flags: u32) {
    crate::jp::res_p1_player::player_effects_80070af4(m, flags)
}
/// 80072114: engine sound. Fills the parameter block at 800C7BE8
/// (+0 speed = state +0x48, +4 = control +0x2C (height above ground), +8 =
/// `[gp+0x518]` +0xE, +0xA = state +0x34, +0xC = state +0x30) and runs
/// 800301F0 on it.
pub fn engine_sound_80070a90(m: &mut Machine) {
    let control = m.gw(0x524);
    let state = m.gw(0x520);
    let view = m.gw(0x518);
    let height = m.lw(control.wrapping_add(0x2c));
    m.sw(ENGINE_PARAMS + 4, height);
    let speed = m.lw(state.wrapping_add(0x48));
    m.sw(ENGINE_PARAMS, speed);
    let value = m.lhu(view.wrapping_add(0xe));
    m.sh(ENGINE_PARAMS + 8, value);
    let value = m.lhu(state.wrapping_add(0x30));
    m.sh(ENGINE_PARAMS + 0xc, value);
    let value = m.lhu(state.wrapping_add(0x34));
    m.sh(ENGINE_PARAMS + 0xa, value);
    engine_voices_8002ffac(m, ENGINE_PARAMS);
}

// ---- weapons (handlers reached from 80072178) ------------------------------------------

/// 80072094: run gun handler `index` from the code-pointer table at
/// 800B3B44 (entry 0 = none). The handler receives its own address in a0.
pub fn fire_gun_80070a10(m: &mut Machine, index: u32) {
    let handler = m.lw(0x800b_3b44u32.wrapping_add(index.wrapping_shl(2)));
    if handler != 0 {
        calls::call(m, handler, [handler, 0, 0, 0]);
    }
}

/// 800720D4: run weapon handler `index` from the code-pointer table at
/// 800B3B60 (entry 0 = none) with `arg` in a0 (and still in a1).
pub fn fire_weapon_80070a50(m: &mut Machine, index: u32, arg: u32) {
    let handler = m.lw(0x800b_3b60u32.wrapping_add(index.wrapping_shl(2)));
    if handler != 0 {
        calls::call(m, handler, [arg, arg, 0, 0]);
    }
}

/// Shared body of the gun handlers in the 800B3B44 table: when the gun
/// timer (state +0x56) is 0, state +0x10 |= 0x2000, a bullet of `kind`
/// (800713B0), sound `sound` (8002DB4C), gun vibration `vibration`
/// (800294C4), screen effect `effect` (80090110), timer = `timer`.
fn gun_burst(m: &mut Machine, kind: u32, sound: u32, vibration: u32, effect: u32, timer: u16) {
    let state = m.gw(0x520);
    if m.lh(state.wrapping_add(0x56)) != 0 {
        return;
    }
    let flags = m.lw(state.wrapping_add(0x10));
    m.sw(state.wrapping_add(0x10), flags | 0x2000);
    fire_bullet_8006fd2c(m, kind);
    external(m, 0x8002_db4c, [sound, 0, 0, 0]);
    vibrate_gun_800290a4(m, vibration);
    screen_effect_8008e8a0(m, effect);
    let state = m.gw(0x520);
    m.sh(state.wrapping_add(0x56), timer);
}

/// 800714E0: gun handler 1 (bullet 0, sound 0x11, vibration 1, effect
/// 0x20000, timer 2).
pub fn gun_8006fe5c(m: &mut Machine) {
    gun_burst(m, 0, 0x11, 1, 0x2_0000, 2);
}
/// 80071544: gun handler 2 (bullet 1, sound 0x12, vibration 2, effect
/// 0x30001, timer 3).
pub fn gun_8006fec0(m: &mut Machine) {
    gun_burst(m, 1, 0x12, 2, 0x3_0001, 3);
}
/// 800715AC: gun handler 3 (bullet 2, sound 0x13, vibration 3, effect
/// 0x30002, timer 6).
pub fn gun_8006ff28(m: &mut Machine) {
    gun_burst(m, 2, 0x13, 3, 0x3_0002, 6);
}
/// 80071614: gun handler 4 (bullet 3, sound 0x14, vibration 4, effect
/// 0x40003, timer 2).
pub fn gun_8006ff90(m: &mut Machine) {
    gun_burst(m, 3, 0x14, 4, 0x4_0003, 2);
}
/// 80071708: gun handler 6 (bullet 5, sound 0x15, vibration 6, effect
/// 0x40004, timer 3).
pub fn gun_80070084(m: &mut Machine) {
    gun_burst(m, 5, 0x15, 6, 0x4_0004, 3);
}

/// 8007167C: gun handler 5, a burst: with the timer at 0 it sets state
/// +0x10 |= 0x2000, effect 0x100005, a bullet of kind 4, timer 0x20, sound
/// 0x15 and vibration 5; while the timer is above 0x10 every even value
/// fires another bullet of kind 4.
pub fn gun_8006fff8(m: &mut Machine) {
    let state = m.gw(0x520);
    let timer = m.lh(state.wrapping_add(0x56));
    if timer == 0 {
        let flags = m.lw(state.wrapping_add(0x10));
        m.sw(state.wrapping_add(0x10), flags | 0x2000);
        screen_effect_8008e8a0(m, 0x10_0005);
        fire_bullet_8006fd2c(m, 4);
        let state = m.gw(0x520);
        m.sh(state.wrapping_add(0x56), 0x20);
        external(m, 0x8002_db4c, [0x15, 0, 0, 0]);
        vibrate_gun_800290a4(m, 5);
    } else if timer >= 0x11 && timer & 1 == 0 {
        fire_bullet_8006fd2c(m, 4);
    }
}

/// A special-weapon spawner (800689E8 ...): owner, origin, velocity,
/// target -> projectile or 0.
type Spawner = fn(&mut Machine, u32, u32, u32, u32) -> u32;

/// How a missile handler of the 800B3B60 table creates its projectile.
#[derive(Clone, Copy)]
enum Launch {
    /// 80068800 with flags (stack word +0x10); frame 0x38, origin at +0x18.
    Spawn(u32),
    /// Another spawner (owner, origin, velocity, target); frame 0x30,
    /// origin at +0x10.
    Other(Spawner),
}

/// Shared body of the missile handlers: with missiles left (motion block
/// +0x1C) and a launcher armed by 800718A0 (`timers`, motion block +4
/// bytes, `reload`), state +0x10 |= 0x4000, one missile less, sound
/// `sound`, launch vibration `vibration` (80029448), the launch point
/// (80071770) and the projectile `launch` with target `[motion block
/// +0x18]`.
fn launch_missile(
    m: &mut Machine,
    timers: u32,
    reload: u32,
    sound: u32,
    vibration: u32,
    launch: Launch,
) {
    let (frame, origin_offset) = match launch {
        Launch::Spawn(_) => (0x38, 0x18),
        Launch::Other(_) => (0x30, 0x10),
    };
    let sp = m.push_frame(frame);
    if let Some(entity) = arm_launcher(m, timers, reload) {
        external(m, 0x8002_db4c, [sound, 0, 0, 0]);
        vibrate_weapon_80029028(m, vibration);
        let origin = sp.wrapping_add(origin_offset);
        weapon_origin_800700ec(m, entity, origin);
        if let Launch::Spawn(flags) = launch {
            m.sw(sp.wrapping_add(0x10), flags);
        }
        let motion_block = m.gw(0x524);
        let owner = m.lw(entity);
        let target = m.lw(motion_block.wrapping_add(0x18));
        let velocity = entity.wrapping_add(0x38);
        match launch {
            Launch::Spawn(flags) => {
                spawn_80067030(m, owner, origin, velocity, target, flags);
            }
            Launch::Other(spawner) => {
                spawner(m, owner, origin, velocity, target);
            }
        }
    }
    m.pop_frame(frame);
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

/// 80071950: standard missile (800B3B60 entry 1): reload 0x96, sound 0xE,
/// vibration 1, 80068800 flags 0x10102. `timers` is the launcher reload
/// byte array.
pub fn launch_missile_800702cc(m: &mut Machine, timers: u32) {
    launch_missile(m, timers, 0x96, 0xe, 1, Launch::Spawn(0x0001_0102));
}
/// 80071A00: missile entry 10: reload 0x96, sound 0x1A, vibration 0xA,
/// 80068800 flags 0x7010102.
pub fn launch_missile_8007037c(m: &mut Machine, timers: u32) {
    launch_missile(m, timers, 0x96, 0x1a, 0xa, Launch::Spawn(0x0701_0102));
}
/// 80071AB0: entry 2: reload 0x4B, sound 0xE, vibration 2, spawner 80068C0C.
pub fn launch_missile_8007042c(m: &mut Machine, timers: u32) {
    launch_missile(m, timers, 0x4b, 0xe, 2, Launch::Other(spawn_8006743c));
}
/// 80071B54: entry 3: reload 0x96, sound 0x10, vibration 3, spawner 80068C80.
pub fn launch_missile_800704d0(m: &mut Machine, timers: u32) {
    launch_missile(m, timers, 0x96, 0x10, 3, Launch::Other(spawn_800674b0));
}
/// 80071BF8: entry 4: reload 0xFA, sound 0xE, vibration 4, spawner 80068D0C.
pub fn launch_missile_80070574(m: &mut Machine, timers: u32) {
    launch_missile(m, timers, 0xfa, 0xe, 4, Launch::Other(spawn_8006753c));
}
/// 80071C9C: entry 5: reload 0xB4, sound 0x10, vibration 5, spawner 80068A58.
pub fn launch_missile_80070618(m: &mut Machine, timers: u32) {
    launch_missile(m, timers, 0xb4, 0x10, 5, Launch::Other(spawn_80067288));
}
/// 80071D40: entry 6: reload 0xB4, sound 0xF, vibration 6, spawner 80068AFC.
pub fn launch_missile_800706bc(m: &mut Machine, timers: u32) {
    launch_missile(m, timers, 0xb4, 0xf, 6, Launch::Other(spawn_8006732c));
}
/// 80071EBC: entry 8: reload 0x5A, sound 0xE, vibration 8, spawner 80068BA8.
pub fn launch_missile_80070838(m: &mut Machine, timers: u32) {
    launch_missile(m, timers, 0x5a, 0xe, 8, Launch::Other(spawn_800673d8));
}
/// 80071F60: entry 9: reload 0xB4, sound 0x10, vibration 9, spawner 800689E8.
pub fn launch_missile_800708dc(m: &mut Machine, timers: u32) {
    launch_missile(m, timers, 0xb4, 0x10, 9, Launch::Other(spawn_80067218));
}

/// 80071DE4: entry 7, needs a target (`[motion block +0x18]` not -1):
/// reload 0xB4, sound 0x19, vibration 7 (none), spawner 80068F34 from the
/// entity position, then 8008350C (target position, target velocity).
pub fn launch_missile_80070760(m: &mut Machine, timers: u32) {
    let motion_block = m.gw(0x524);
    if m.lw(motion_block.wrapping_add(0x1c)) == 0
        || m.lw(motion_block.wrapping_add(0x18)) == u32::MAX
    {
        return;
    }
    let Some(entity) = arm_launcher(m, timers, 0xb4) else {
        return;
    };
    external(m, 0x8002_db4c, [0x19, 0, 0, 0]);
    vibrate_weapon_80029028(m, 7);
    let motion_block = m.gw(0x524);
    let owner = m.lw(entity);
    let target = m.lw(motion_block.wrapping_add(0x18));
    spawn_80067764(
        m,
        owner,
        entity.wrapping_add(8),
        entity.wrapping_add(0x38),
        target,
    );
    let motion_block = m.gw(0x524);
    let index = m.lw(motion_block.wrapping_add(0x18)) & 0x3f;
    let table = m.gw(crate::common::ENTITIES);
    let other = m.lw(index.wrapping_shl(2).wrapping_add(table));
    crate::effects::beam_80081ec4(m, other.wrapping_add(8), other.wrapping_add(0x38));
}

// JP-REVIEW(R0272): near function: PAL 80070980 -> JP 80072004, imm:1, 1 instructions differ (whole routine)
/// 80072004: entry 11: reload 0xB4, sound 0x5E, no vibration, spawner
/// 80068FA8 from the entity position.
// JP-PATCH: replaced by `crate::jp::res_p1_player::launch_missile_80070980` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn launch_missile_80070980(m: &mut Machine, timers: u32) {
    crate::jp::res_p1_player::launch_missile_80070980(m, timers)
}

/// 80090110: request screen effect `effect` for the frame: the effect block
/// `[gp+0x41C]` gets +0x394C = `effect` and +0x3948 = 1, then 8007EFBC
/// (`effect` & 0xF, `effect` >> 16). Returns 8007EFBC's command address.
pub fn screen_effect_8008e8a0(m: &mut Machine, effect: u32) -> u32 {
    let block = m.gw(0x41c);
    m.sw(block.wrapping_add(0x394c), effect);
    m.sw(block.wrapping_add(0x3948), 1);
    effect_command_8007d940(m, effect & 0xf, ((effect as i32) >> 16) as u32)
}

/// 8007EFBC: append a one-word effect command (8007DF28 reserves it in the
/// command ring `[gp+0x544]`): low nibble 1, bits 4..6 = 3, bits 7..8 = 0,
/// bits 9..16 = `kind` & 0xFF, bits 17..28 = `value` & 0xFFF, the other
/// bits kept. Returns the command address.
pub fn effect_command_8007d940(m: &mut Machine, kind: u32, value: u32) -> u32 {
    let command = external(m, 0x8007_df28, [1, 0, 0, 0]);
    let word = m.lw(command);
    let word = (word & 0xffff_fff0) | 1;
    let word = (word & 0xffff_ff8f) | 0x30;
    let word = word & 0xffff_fe7f & 0xfffe_01ff | (kind & 0xff) << 9;
    let word = word & 0xe001_ffff | (value & 0xfff) << 17;
    m.sw(command, word);
    command
}

/// RTV0 (sf=1, no translation) of the SVECTOR at `vector` by the matrix at
/// `matrix` (set as the rotation, inline SetRotMatrix), MAC1..MAC3 stored
/// to `out` (swc2), as inlined in 800713B0/80071770.
fn rotate_offset(m: &mut Machine, matrix: u32, vector: u32, out: u32) {
    crate::sdk::set_rot_matrix(m, matrix);
    let xy = m.lw(vector);
    let z = m.lw(vector.wrapping_add(4));
    m.gte.mtc2(reg::VXY0, xy);
    m.gte.mtc2(reg::VZ0, z);
    m.gte.mvmva(true, false, 0, 0, 3);
    for i in 0..3 {
        let value = m.gte.mfc2(reg::MAC1 + i);
        m.sw(out.wrapping_add(i as u32 * 4), value);
    }
}

/// 800713B0: queue a player bullet of `kind`: the muzzle offset (resource
/// block +0xC, +0x90) rotated by the player's orientation (80030778 of
/// entity +0x18), times 16, plus the entity position, with the entity
/// angles, into the shared launch block 0x800C7BC8; then 8006B034 (owner
/// = entity word 0, that block, entity velocity +0x38, `kind`).
pub fn fire_bullet_8006fd2c(m: &mut Machine, kind: u32) {
    const FRAME: u32 = 0x40;
    const LAUNCH: u32 = 0x800c_7bc8;
    let sp = m.push_frame(FRAME);
    let table = m.gw(crate::common::ENTITIES);
    let resources = m.gw(crate::common::RESOURCES);
    let entity = m.lw(table);
    let muzzle = m.lw(resources.wrapping_add(0xc)).wrapping_add(0x90);
    let matrix = sp.wrapping_add(0x10);
    crate::shared::f_80030534(m, [entity.wrapping_add(0x18), matrix, 0, 0]);
    rotate_offset(m, matrix, muzzle, LAUNCH);
    let x = m.lw(LAUNCH) << 4;
    m.sw(LAUNCH, x);
    let y = m.lw(LAUNCH + 4) << 4;
    let z = m.lw(LAUNCH + 8) << 4;
    m.sw(LAUNCH + 4, y);
    m.sw(LAUNCH + 8, z);
    let x = x.wrapping_add(m.lw(entity.wrapping_add(8)));
    m.sw(LAUNCH, x);
    let y = y.wrapping_add(m.lw(entity.wrapping_add(0xc)));
    m.sw(LAUNCH + 4, y);
    let z = z.wrapping_add(m.lw(entity.wrapping_add(0x10)));
    m.sw(LAUNCH + 8, z);
    for i in 0..3 {
        let angle = m.lhu(entity.wrapping_add(0x18 + i * 2));
        m.sh(LAUNCH + 0x10 + i * 2, angle);
    }
    let owner = m.lw(entity);
    queue_bullet_80069864(m, owner, LAUNCH, entity.wrapping_add(0x38), kind);
    m.pop_frame(FRAME);
}

/// 8006B034: queue a bullet request in the projectile block `[gp+0x50C]`
/// (at most 8 pending, count +0x154C, 16-byte entries from +0x1550): claims
/// the first free bit of the 128-bit slot mask at +0x33D0 and records
/// (slot, `kind`, `owner`, `origin`, `velocity`). Returns 1, or 0 when the
/// queue or the slots are full.
pub fn queue_bullet_80069864(
    m: &mut Machine,
    owner: u32,
    origin: u32,
    velocity: u32,
    kind: u32,
) -> u32 {
    let block = m.gw(0x50c);
    if m.lw(block.wrapping_add(0x154c)) as i32 >= 8 {
        return 0;
    }
    let mut words = block.wrapping_add(0x33d0);
    let mut word = m.lw(words);
    let mut base: u32 = 0;
    if word == u32::MAX {
        loop {
            base += 0x20;
            if base >= 0x80 {
                return 0;
            }
            words = words.wrapping_add(4);
            word = m.lw(words);
            if word != u32::MAX {
                break;
            }
        }
    }
    let bit = first_clear_bit(word);
    let value = m.lw(words) | 1u32.wrapping_shl(bit);
    m.sw(words, value);
    let block = m.gw(0x50c);
    let count = m.lw(block.wrapping_add(0x154c));
    m.sw(block.wrapping_add(0x154c), count.wrapping_add(1));
    let entry = block.wrapping_add(count.wrapping_shl(4).wrapping_add(0x1550));
    m.sh(entry, bit.wrapping_add(base) as u16);
    m.sh(entry.wrapping_add(2), kind as u16);
    m.sw(entry.wrapping_add(4), owner);
    m.sw(entry.wrapping_add(8), origin);
    m.sw(entry.wrapping_add(0xc), velocity);
    1
}

/// The original's free-bit scan: 0 if bit 0 is clear, else the first clear
/// bit from 1 upwards (shift counts wrap at 32 like `sllv`).
fn first_clear_bit(mask: u32) -> u32 {
    if mask & 1 == 0 {
        return 0;
    }
    let mut bit = 1u32;
    while mask & 1u32.wrapping_shl(bit) != 0 {
        bit = bit.wrapping_add(1);
    }
    bit
}

/// 800294C4: gun vibration of `kind` (1..6) when the pad supports it
/// (context +0x120 & 3 == 3): claims a slot with priority 3 (8002939C) and
/// sets byte 2 / byte 0 from the 6-pair table at 0x80010380 and byte 1 = 1
/// (kept at 3 for kind 3); other kinds clear byte 0.
pub fn vibrate_gun_800290a4(m: &mut Machine, kind: u32) {
    let context = m.gw(crate::common::CONTEXT);
    if m.lw(context.wrapping_add(0x120)) & 3 != 3 {
        return;
    }
    let slot = claim_vibration_80028f7c(m, 3);
    if slot == 0 {
        return;
    }
    // The original copies the 12-byte table to its stack frame first.
    let table = m.read_bytes(0x8001_0380, 12);
    let index = kind.wrapping_sub(1);
    if index >= 6 {
        m.sb(slot, 0);
        return;
    }
    let pair = index as usize * 2;
    m.sb(slot.wrapping_add(2), table[pair]);
    m.sb(slot, table[pair + 1]);
    if index != 2 {
        m.sb(slot.wrapping_add(1), 1);
    }
}

/// 8006CB44: camera shake impulse `kind` (0 low altitude, 1 hit shake,
/// 2 overspeed) when the stage enables it (`[[gp+0x2E0] +0x28] +0x50` &
/// 3): the four shake accumulators at gameplay +0x28C..+0x292 (as two
/// pairs) gain the halfword pair for the context level (`[gp+0x2EC]` +0x28
/// & 7) from table 800B2CDC (kind 0) or 800B2CFC (other kinds; kind 1 first
/// also adds 800B2CBC); each is then capped: 7 and above become 6.
pub fn camera_shake_8006b374(m: &mut Machine, kind: u32) {
    let gameplay = m.gw(crate::common::GAMEPLAY);
    let stage = m.lw(gameplay.wrapping_add(0x28));
    if m.lw(stage.wrapping_add(0x50)) & 3 == 0 {
        return;
    }
    let context = m.gw(crate::common::CONTEXT);
    let level = m.lw(context.wrapping_add(0x28)) & 7;
    let add_pairs = |m: &mut Machine, gameplay: u32, table: u32| {
        let pair = table.wrapping_add(level << 2);
        for (offset, half) in [(0x28c, 0), (0x28e, 2), (0x290, 0), (0x292, 2)] {
            let value = m
                .lhu(gameplay.wrapping_add(offset))
                .wrapping_add(m.lhu(pair.wrapping_add(half)));
            m.sh(gameplay.wrapping_add(offset), value);
        }
    };
    if kind == 1 {
        add_pairs(m, gameplay, 0x800b_2cbc);
    }
    let table = if kind == 0 { 0x800b_2cdc } else { 0x800b_2cfc };
    let gameplay = m.gw(crate::common::GAMEPLAY);
    add_pairs(m, gameplay, table);
    for offset in [0x28c, 0x28e, 0x290, 0x292] {
        let gameplay = m.gw(crate::common::GAMEPLAY);
        if m.lh(gameplay.wrapping_add(offset)) >= 7 {
            m.sh(gameplay.wrapping_add(offset), 6);
        }
    }
}

/// A vibration preset routine (80029588, 800295CC, 80029604).
type Vibration = fn(&mut Machine);

/// 80029588: random vibration for hit event 4: a slot with priority 0x10
/// (8002939C) gets byte 0 = 0xA and byte 2 = 0xA8 + (rand() & 0x3F).
pub fn vibrate_random_80029168(m: &mut Machine) {
    let slot = claim_vibration_80028f7c(m, 0x10);
    if slot == 0 {
        return;
    }
    m.sb(slot, 0xa);
    let random = crate::sdk::rand(m);
    m.sb(slot.wrapping_add(2), ((random & 0x3f) + 0xa8) as u8);
}

/// 800295CC: vibration for hit event 1: priority 0x14, byte 0 = 3, byte 2
/// = 0xE6.
pub fn vibrate_preset_800291ac(m: &mut Machine) {
    let slot = claim_vibration_80028f7c(m, 0x14);
    if slot != 0 {
        m.sb(slot, 3);
        m.sb(slot.wrapping_add(2), 0xe6);
    }
}

/// 80029604: vibration for hit event 2: priority 0x1E, byte 0 = 0xF, byte
/// 2 = 0xFA.
pub fn vibrate_preset_800291e4(m: &mut Machine) {
    let slot = claim_vibration_80028f7c(m, 0x1e);
    if slot != 0 {
        m.sb(slot, 0xf);
        m.sb(slot.wrapping_add(2), 0xfa);
    }
}

/// 80029448: weapon-launch vibration of `kind` when the pad supports it
/// (context +0x120 & 3 == 3; kinds 7 and 0xB none): a slot with priority 9
/// (8002939C), byte 2 = 0xFF, byte 0 = 2 for kinds 6 and 0xA, else 1.
pub fn vibrate_weapon_80029028(m: &mut Machine, kind: u32) {
    let context = m.gw(crate::common::CONTEXT);
    if m.lw(context.wrapping_add(0x120)) & 3 != 3 || kind == 7 || kind == 0xb {
        return;
    }
    let slot = claim_vibration_80028f7c(m, 9);
    if slot == 0 {
        return;
    }
    m.sb(slot.wrapping_add(2), 0xff);
    m.sb(slot, if kind == 6 || kind == 0xa { 2 } else { 1 });
}

/// 80071770: weapon launch point of `entity` into `out` (VECTOR + angles):
/// the launcher offset (resource block +0xC, +0x98, +8 when motion block
/// +0x1C is odd: alternating sides) rotated by the entity orientation
/// (80030778 of +0x18), times 16, plus the entity position (+8); the
/// entity angles (+0x18..) at `out` +0x10.
pub fn weapon_origin_800700ec(m: &mut Machine, entity: u32, out: u32) {
    const FRAME: u32 = 0x40;
    let sp = m.push_frame(FRAME);
    let motion_block = m.gw(0x524);
    let resources = m.gw(crate::common::RESOURCES);
    let side = m.lw(motion_block.wrapping_add(0x1c)) & 1;
    let offset = m
        .lw(resources.wrapping_add(0xc))
        .wrapping_add((side << 3) + 0x98);
    let matrix = sp.wrapping_add(0x10);
    crate::shared::f_80030534(m, [entity.wrapping_add(0x18), matrix, 0, 0]);
    rotate_offset(m, matrix, offset, out);
    let x = m.lw(out);
    let z = m.lw(out.wrapping_add(8));
    m.sw(out, x << 4);
    let y = m.lw(out.wrapping_add(4));
    m.sw(out.wrapping_add(8), z << 4);
    m.sw(out.wrapping_add(4), y << 4);
    for i in 0..3 {
        let value = m
            .lw(out.wrapping_add(i * 4))
            .wrapping_add(m.lw(entity.wrapping_add(8 + i * 4)));
        m.sw(out.wrapping_add(i * 4), value);
    }
    for i in 0..3 {
        let angle = m.lhu(entity.wrapping_add(0x18 + i * 2));
        m.sh(out.wrapping_add(0x10 + i * 2), angle);
    }
    m.pop_frame(FRAME);
}

/// 800685C0: allocate a projectile in the block `[gp+0x50C]`: the first
/// clear bit of the 32-slot mask +0x1548 (none: returns 0); per-owner count
/// +0x48[`owner` & 0x3F]; the 0xA4-byte slot at +0xC8 is initialized from
/// `origin` (position, angles), `velocity` (+0x38; speed = length of
/// velocity >> 12, << 12, at +0x50/+0x64), lifetime 0x1D4C0 (+0x68) and
/// `target` (+0x70; -1 = none: aim point = own position and flag 1). A
/// target entity i gets its lock count +0x88[i], the slot +8[i] and bit i
/// of the lock mask (+0 / +4). Returns the slot address.
pub fn allocate_projectile_80066df0(
    m: &mut Machine,
    owner: u32,
    origin: u32,
    velocity: u32,
    target: u32,
) -> u32 {
    let block = m.gw(0x50c);
    let mask = m.lw(block.wrapping_add(0x1548));
    if mask == u32::MAX {
        return 0;
    }
    let slot = first_clear_bit(mask);
    let mask = mask | 1u32.wrapping_shl(slot);
    let block = m.gw(0x50c);
    m.sw(block.wrapping_add(0x1548), mask);
    let counter = block.wrapping_add(owner & 0x3f).wrapping_add(0x48);
    let count = m.lbu(counter).wrapping_add(1);
    m.sb(counter, count);
    let block = m.gw(0x50c);
    let p = block.wrapping_add(slot.wrapping_mul(0xa4).wrapping_add(0xc8));
    m.sb(p.wrapping_add(4), 0);
    m.sh(p.wrapping_add(8), 0);
    m.sh(p.wrapping_add(0xa), 0);
    m.sw(p, 0);
    m.sb(p.wrapping_add(0x34), 1);
    m.sb(p.wrapping_add(0x35), 0);
    m.sh(p.wrapping_add(0x1a), 0x40);
    m.sw(p.wrapping_add(0x6c), owner);
    for i in 0..3 {
        let value = m.lw(origin.wrapping_add(i * 4));
        m.sw(p.wrapping_add(0x1c + i * 4), value);
    }
    for i in 0..3 {
        let value = m.lhu(origin.wrapping_add(0x10 + i * 2));
        m.sh(p.wrapping_add(0x2c + i * 2), value);
    }
    let vx = m.lw(velocity);
    m.sw(p.wrapping_add(0x38), vx);
    let vy = m.lw(velocity.wrapping_add(4));
    let vx = m.lw(p.wrapping_add(0x38));
    m.sw(p.wrapping_add(0x3c), vy);
    let vz = m.lw(velocity.wrapping_add(8));
    m.sw(p.wrapping_add(0x40), vz);
    let shift = |value: u32| ((value as i32) >> 12) as u32;
    let speed = vector_length_800828f8(m, shift(vx), shift(vy), shift(vz)) << 12;
    m.sw(p.wrapping_add(0x64), speed);
    m.sw(p.wrapping_add(0x50), speed);
    m.sw(p.wrapping_add(0x60), 0);
    m.sw(p.wrapping_add(0x48), 0);
    m.sw(p.wrapping_add(0x4c), 0);
    m.sh(p.wrapping_add(0x58), 0);
    m.sh(p.wrapping_add(0x5a), 0);
    m.sh(p.wrapping_add(0x5c), 0);
    m.sw(p.wrapping_add(0x68), 0x1_d4c0);
    m.sw(p.wrapping_add(0x70), target);
    let (aim, aim_velocity) = if target == u32::MAX {
        let own = p.wrapping_add(0x1c);
        let flags = m.lw(p) | 1;
        m.sw(p, flags);
        (own, own)
    } else {
        let index = target & 0x3f;
        let table = m.gw(crate::common::ENTITIES);
        let block = m.gw(0x50c);
        let entity = m.lw(index.wrapping_shl(2).wrapping_add(table));
        let locks = block.wrapping_add(index).wrapping_add(0x88);
        let count = m.lbu(locks).wrapping_add(1);
        m.sb(locks, count);
        let block = m.gw(0x50c);
        m.sb(block.wrapping_add(index).wrapping_add(8), slot as u8);
        let block = m.gw(0x50c);
        if target & 0x20 != 0 {
            let bits = m.lw(block.wrapping_add(4)) | 1u32 << (target & 0x1f);
            m.sw(block.wrapping_add(4), bits);
        } else {
            let bits = m.lw(block) | 1u32.wrapping_shl(index);
            m.sw(block, bits);
        }
        (entity.wrapping_add(8), entity.wrapping_add(0x38))
    };
    m.sw(p.wrapping_add(0x74), aim);
    m.sw(p.wrapping_add(0x78), aim_velocity);
    p
}

/// 80068800: spawn a projectile (800685C0 with `owner`, `origin`,
/// `velocity`, `target`) and apply `flags` (the caller's stack word +0x10):
/// bit 0x01000000 clear: +0x35 |= 8; 0x02000000: +0x35 |= 0x20, else
/// 0x10; 0x04000000 / 0x08000000: word +0 |= 0x80 / 0x100; word +0 |= 8.
/// The low byte selects the motion class (2 = by distance to the target:
/// class 1 from 5000 up), byte 1 the model (gp tables +0x264 / +0x27C) and
/// byte 2 the parameters (+0x254/+0x25C or +0x26C/+0x274). Returns the
/// slot or 0.
pub fn spawn_80067030(
    m: &mut Machine,
    owner: u32,
    origin: u32,
    velocity: u32,
    target: u32,
    flags: u32,
) -> u32 {
    let p = allocate_projectile_80066df0(m, owner, origin, velocity, target);
    if p == 0 {
        return 0;
    }
    if !flags & 0x0100_0000 != 0 {
        let bits = m.lbu(p.wrapping_add(0x35)) | 8;
        m.sb(p.wrapping_add(0x35), bits);
    }
    let side = if flags & 0x0200_0000 != 0 { 0x20 } else { 0x10 };
    let bits = m.lbu(p.wrapping_add(0x35)) | side;
    m.sb(p.wrapping_add(0x35), bits);
    if flags & 0x0400_0000 != 0 {
        let word = m.lw(p) | 0x80;
        m.sw(p, word);
    }
    if flags & 0x0800_0000 != 0 {
        let word = m.lw(p) | 0x100;
        m.sw(p, word);
    }
    let word = m.lw(p);
    m.sb(p.wrapping_add(6), 0);
    m.sb(p.wrapping_add(7), 0);
    m.sw(p, word | 8);
    let class = flags & 0xff;
    let model = (flags >> 8) & 0xff;
    let parameters = (flags >> 16) & 0xff;
    let class = if class == 2 {
        let aim = m.lw(p.wrapping_add(0x74));
        let distance = crate::shared::f_800827b4(m, [origin, aim, 0, 0]);
        u32::from(distance as i32 >= 0x1388)
    } else {
        class
    };
    let table = |m: &mut Machine, offset: u32, index: u32| {
        m.lhu(crate::GP.wrapping_add(offset).wrapping_add(index << 1))
    };
    match class {
        0 => {
            m.sb(p.wrapping_add(5), 0);
            let shape = table(m, 0x278, model);
            m.sh(p.wrapping_add(0x18), shape);
            let first = table(m, 0x268, parameters);
            m.sh(p.wrapping_add(0x7c), first);
            let second = table(m, 0x270, parameters);
            m.sw(p.wrapping_add(0x80), 0x5_0000);
            m.sw(p.wrapping_add(0x84), 0x1e00);
            m.sw(p.wrapping_add(0x88), 0xe00);
            m.sh(p.wrapping_add(0x7e), second);
        }
        1 => {
            m.sb(p.wrapping_add(5), 1);
            let shape = table(m, 0x290, model);
            m.sh(p.wrapping_add(0x18), shape);
            let first = table(m, 0x280, parameters);
            m.sh(p.wrapping_add(0x7c), first);
            let second = table(m, 0x288, parameters);
            m.sw(p.wrapping_add(0x80), 0x7_8000);
            m.sw(p.wrapping_add(0x84), 0x1400);
            m.sw(p.wrapping_add(0x88), 0x1400);
            m.sh(p.wrapping_add(0x7e), second);
        }
        _ => {}
    }
    p
}

// ---- special-weapon spawners (missile handler table 800B3B60) --------------------
// Each allocates a projectile with 800685C0 (owner, origin, velocity, target)
// and sets its kind fields; it returns the projectile or 0.

/// 800689E8: spawner of handler 9: +0x1A = 0x2000, +0x34 = 4, +5 = 2,
/// model 0x12C, +0x9C = 0x190, +6/+7 = 0, +0xA0 = 0xC0, word 0 |= 0x10.
pub fn spawn_80067218(m: &mut Machine, owner: u32, origin: u32, velocity: u32, target: u32) -> u32 {
    let p = allocate_projectile_80066df0(m, owner, origin, velocity, target);
    if p == 0 {
        return 0;
    }
    m.sh(p.wrapping_add(0x1a), 0x2000);
    m.sb(p.wrapping_add(0x34), 4);
    m.sb(p.wrapping_add(5), 2);
    m.sh(p.wrapping_add(0x18), 0x12c);
    m.sw(p.wrapping_add(0x9c), 0x190);
    let word = m.lw(p);
    m.sb(p.wrapping_add(6), 0);
    m.sb(p.wrapping_add(7), 0);
    m.sw(p.wrapping_add(0xa0), 0xc0);
    m.sw(p, word | 0x10);
    p
}

/// 80068A58: spawner of handler 5: +0x1A = 0x200, +0x34 = 4, +5 = 2, +7 =
/// 3, model 0xF0, +0x9C = 0x400, +0xA0 = 0xC0, +6 = 0, velocity x1.5, word
/// 0 |= 0x10.
pub fn spawn_80067288(m: &mut Machine, owner: u32, origin: u32, velocity: u32, target: u32) -> u32 {
    let p = allocate_projectile_80066df0(m, owner, origin, velocity, target);
    if p == 0 {
        return 0;
    }
    let vx = m.lw(p.wrapping_add(0x38));
    m.sh(p.wrapping_add(0x1a), 0x200);
    m.sb(p.wrapping_add(0x34), 4);
    m.sb(p.wrapping_add(5), 2);
    m.sb(p.wrapping_add(7), 3);
    m.sh(p.wrapping_add(0x18), 0xf0);
    m.sw(p.wrapping_add(0x9c), 0x400);
    m.sw(p.wrapping_add(0xa0), 0xc0);
    let vy = m.lw(p.wrapping_add(0x3c));
    m.sb(p.wrapping_add(6), 0);
    m.sw(
        p.wrapping_add(0x38),
        vx.wrapping_add(((vx as i32) >> 1) as u32),
    );
    let vz = m.lw(p.wrapping_add(0x40));
    m.sw(
        p.wrapping_add(0x3c),
        vy.wrapping_add(((vy as i32) >> 1) as u32),
    );
    let word = m.lw(p);
    m.sw(
        p.wrapping_add(0x40),
        vz.wrapping_add(((vz as i32) >> 1) as u32),
    );
    m.sw(p, word | 0x10);
    p
}

/// 80068AFC: spawner of handler 6: +0x1A = 0x400, +0x34 = 4, +5 = 2, +7 =
/// 1, model 0xF0, +0x9C = 0x4EC, +0xA0 = 0xC0, +6 = 0, velocity x1.5,
/// word 0 |= 0x50.
pub fn spawn_8006732c(m: &mut Machine, owner: u32, origin: u32, velocity: u32, target: u32) -> u32 {
    let p = allocate_projectile_80066df0(m, owner, origin, velocity, target);
    if p == 0 {
        return 0;
    }
    let vx = m.lw(p.wrapping_add(0x38));
    m.sh(p.wrapping_add(0x1a), 0x400);
    m.sb(p.wrapping_add(0x34), 4);
    m.sb(p.wrapping_add(5), 2);
    m.sb(p.wrapping_add(7), 1);
    m.sh(p.wrapping_add(0x18), 0xf0);
    m.sw(p.wrapping_add(0x9c), 0x4ec);
    m.sw(p.wrapping_add(0xa0), 0xc0);
    let vy = m.lw(p.wrapping_add(0x3c));
    m.sb(p.wrapping_add(6), 0);
    m.sw(
        p.wrapping_add(0x38),
        vx.wrapping_add(((vx as i32) >> 1) as u32),
    );
    m.sw(
        p.wrapping_add(0x3c),
        vy.wrapping_add(((vy as i32) >> 1) as u32),
    );
    let word = m.lw(p);
    let vz = m.lw(p.wrapping_add(0x40));
    let word = word | 0x10;
    m.sw(p, word);
    m.sw(
        p.wrapping_add(0x40),
        vz.wrapping_add(((vz as i32) >> 1) as u32),
    );
    m.sw(p, word | 0x40);
    p
}

/// 80068BA8: spawner of handler 8: +0x1A = 0x1000, +5 = 3, +0x84/+0x88 =
/// 0x1400, +6/+7 = 0, +0x80 = 0x12C000, model 0x5A.
pub fn spawn_800673d8(m: &mut Machine, owner: u32, origin: u32, velocity: u32, target: u32) -> u32 {
    let p = allocate_projectile_80066df0(m, owner, origin, velocity, target);
    if p == 0 {
        return 0;
    }
    m.sh(p.wrapping_add(0x1a), 0x1000);
    m.sb(p.wrapping_add(5), 3);
    m.sw(p.wrapping_add(0x84), 0x1400);
    m.sw(p.wrapping_add(0x88), 0x1400);
    m.sb(p.wrapping_add(6), 0);
    m.sb(p.wrapping_add(7), 0);
    m.sw(p.wrapping_add(0x80), 0x12_c000);
    m.sh(p.wrapping_add(0x18), 0x5a);
    p
}

/// 80068C0C: spawner of handler 2: model 0x53, +0x7C = 0x14, +0x7E = 1,
/// +0x80 = 0xA0000, +0x30 = 0, +5/+6/+7 = 0, +0x84/+0x88 = 0x5A00, +0x35
/// |= 0x18.
pub fn spawn_8006743c(m: &mut Machine, owner: u32, origin: u32, velocity: u32, target: u32) -> u32 {
    let p = allocate_projectile_80066df0(m, owner, origin, velocity, target);
    if p == 0 {
        return 0;
    }
    m.sh(p.wrapping_add(0x18), 0x53);
    m.sh(p.wrapping_add(0x7c), 0x14);
    m.sh(p.wrapping_add(0x7e), 1);
    m.sw(p.wrapping_add(0x80), 0xa_0000);
    let flags = m.lbu(p.wrapping_add(0x35));
    m.sh(p.wrapping_add(0x30), 0);
    m.sb(p.wrapping_add(6), 0);
    m.sb(p.wrapping_add(7), 0);
    m.sb(p.wrapping_add(5), 0);
    m.sw(p.wrapping_add(0x84), 0x5a00);
    m.sw(p.wrapping_add(0x88), 0x5a00);
    m.sb(p.wrapping_add(0x35), flags | 0x18);
    p
}

/// 80068C80: spawner of handler 3: +0x1A = 0x80, +0x34 = 3, +6 = 2, +5 =
/// 1, model 0x96, +0x7C = 0x12, +0x7E = 7, +0x80 = 0x3E000, +7 = 0,
/// +0x84/+0x88 = 0x5A00, +0x35 |= 0x18.
pub fn spawn_800674b0(m: &mut Machine, owner: u32, origin: u32, velocity: u32, target: u32) -> u32 {
    let p = allocate_projectile_80066df0(m, owner, origin, velocity, target);
    if p == 0 {
        return 0;
    }
    m.sh(p.wrapping_add(0x1a), 0x80);
    m.sb(p.wrapping_add(0x34), 3);
    m.sb(p.wrapping_add(6), 2);
    m.sb(p.wrapping_add(5), 1);
    m.sh(p.wrapping_add(0x18), 0x96);
    m.sh(p.wrapping_add(0x7c), 0x12);
    m.sh(p.wrapping_add(0x7e), 7);
    let flags = m.lbu(p.wrapping_add(0x35));
    m.sw(p.wrapping_add(0x80), 0x3_e000);
    m.sb(p.wrapping_add(7), 0);
    m.sw(p.wrapping_add(0x84), 0x5a00);
    m.sw(p.wrapping_add(0x88), 0x5a00);
    m.sb(p.wrapping_add(0x35), flags | 0x18);
    p
}

/// 80068D0C: spawner of handler 4: +0x1A = 0x100, +0x34 = 2, +6 = 3, +5 =
/// 1, model 0x96, +0x7C = 0x1E, +0x7E = 7, +0x84 = 0x1200, +0x80 =
/// 0x48000, +7 = 0, +0x88 = 0xA00, +0x35 |= 0x18; with a target, +0x68 =
/// the distance from `origin` to the target (80083DFC) and +0x98 = 3/4 of
/// it (upper target bank, 0x20) or 243/256 of it.
pub fn spawn_8006753c(m: &mut Machine, owner: u32, origin: u32, velocity: u32, target: u32) -> u32 {
    let p = allocate_projectile_80066df0(m, owner, origin, velocity, target);
    if p == 0 {
        return 0;
    }
    m.sh(p.wrapping_add(0x1a), 0x100);
    m.sb(p.wrapping_add(0x34), 2);
    m.sb(p.wrapping_add(6), 3);
    m.sb(p.wrapping_add(5), 1);
    m.sh(p.wrapping_add(0x18), 0x96);
    m.sh(p.wrapping_add(0x7c), 0x1e);
    m.sh(p.wrapping_add(0x7e), 7);
    m.sw(p.wrapping_add(0x84), 0x1200);
    let flags = m.lbu(p.wrapping_add(0x35));
    m.sw(p.wrapping_add(0x80), 0x4_8000);
    m.sb(p.wrapping_add(7), 0);
    m.sw(p.wrapping_add(0x88), 0xa00);
    m.sb(p.wrapping_add(0x35), flags | 0x18);
    if target != u32::MAX {
        let table = m.gw(crate::common::ENTITIES);
        let other = m.lw((target & 0x3f).wrapping_shl(2).wrapping_add(table));
        let distance = crate::shared::f_800827b4(m, [origin, other.wrapping_add(8), 0, 0]);
        let d = distance as i32;
        let lead = if target & 0x20 != 0 {
            (d >> 2).wrapping_add(d >> 1)
        } else {
            d.wrapping_mul(972) >> 10
        };
        m.sw(p.wrapping_add(0x98), lead as u32);
        m.sw(p.wrapping_add(0x68), distance);
    }
    p
}

/// 80068F34: spawner of handler 7 (beam): +0x1A = 0x800, +5 = 4, +7 = 2,
/// +0x9C = 0x10, +0x34 = 0, model 0, velocity 0, +0xA0 = 1, word 0 |=
/// 0x20.
pub fn spawn_80067764(m: &mut Machine, owner: u32, origin: u32, velocity: u32, target: u32) -> u32 {
    let p = allocate_projectile_80066df0(m, owner, origin, velocity, target);
    if p == 0 {
        return 0;
    }
    m.sh(p.wrapping_add(0x1a), 0x800);
    m.sb(p.wrapping_add(5), 4);
    m.sb(p.wrapping_add(7), 2);
    m.sw(p.wrapping_add(0x9c), 0x10);
    let word = m.lw(p);
    m.sb(p.wrapping_add(0x34), 0);
    m.sh(p.wrapping_add(0x18), 0);
    m.sw(p.wrapping_add(0x38), 0);
    m.sw(p.wrapping_add(0x3c), 0);
    m.sw(p.wrapping_add(0x40), 0);
    m.sw(p.wrapping_add(0xa0), 1);
    m.sw(p, word | 0x20);
    p
}

/// 80068FA8: spawner of handler 11: +0x1A = 0x4000, +5 = 4, +7 = 3, +0x34
/// = 0, model 0, velocity 0, +0x9C/+0xA0 = 1, word 0 |= 0x20.
pub fn spawn_800677d8(m: &mut Machine, owner: u32, origin: u32, velocity: u32, target: u32) -> u32 {
    let p = allocate_projectile_80066df0(m, owner, origin, velocity, target);
    if p == 0 {
        return 0;
    }
    m.sh(p.wrapping_add(0x1a), 0x4000);
    m.sb(p.wrapping_add(5), 4);
    m.sb(p.wrapping_add(7), 3);
    let word = m.lw(p);
    m.sb(p.wrapping_add(0x34), 0);
    m.sh(p.wrapping_add(0x18), 0);
    m.sw(p.wrapping_add(0x38), 0);
    m.sw(p.wrapping_add(0x3c), 0);
    m.sw(p.wrapping_add(0x40), 0);
    m.sw(p.wrapping_add(0x9c), 1);
    m.sw(p.wrapping_add(0xa0), 1);
    m.sw(p, word | 0x20);
    p
}

// ==== group A: 80045898, 800517F4, 8004658C and their subtrees ====
//
// Entity slots index the 64-entry pointer table `[gp+0x2E8]`; slots 0x20..0x3F
// are also reached through the 32-entry ground-unit table `[gp+0x2D8]`.

/// 800C6FD0: 64 per-slot state words (bit 0: slot in play; 0x2: unit
/// destroyed; 0x4: hit timer running; 0x10: unit wrecked; 0x20: home record
/// due; 0x400000: frozen).
const SLOT_STATE_4D98: u32 = 0x800c_6fd0;
/// 800C6ED0: 64 per-slot pointers to sight records (word 0 flags, word +4
/// distance to the player, +8 relative vector, +0x18 off-axis angle,
/// halfword timers +0x1A/+0x1C/+0x1E).
const SIGHT_4C98: u32 = 0x800c_6ed0;
/// 800C6EA0: player view block: rotation matrix (800308F0) and range limits
/// at +0x20 (angle), +0x24 and +0x28 (distances).
const VIEW_4C68: u32 = 0x800c_6ea0;
/// 800C70D0: 64 per-slot hit timer bytes.
const HIT_TIMER_4E98: u32 = 0x800c_70d0;
/// 800C6D80: 64 four-byte lock slots (byte 0 per slot; bytes 1..3 of slot 0
/// are the current lock targets).
const LOCK_SLOTS_4B48: u32 = 0x800c_6d80;
/// 800C6D60: weapon target block (byte +0x0E target slot, +0x12 0xFF = none).
const TARGET_4B28: u32 = 0x800c_6d60;
/// 800BD094: pointer to the block whose word +0x10 bit 0x4000 enables
/// link effects.
const EFFECTS_AF1C: u32 = 0x800b_d094;
/// 800104AC: sine table, 0x4000 entries per turn (+0x1000 entries of cosine
/// overlap), 1.0 = 0x4000.
const SINE_0498: u32 = 0x8001_04ac;
/// `[gp+0x2D8]`: pointer to the 32 ground-unit entity pointers (slots 0x20..).
const UNITS_2C8: u32 = 0x2d8;
/// `[gp+0x37D]`: number of entity links; `[gp+0x398]` 0x14-byte link records
/// (halfword kind, byte +2 parent slot, byte +3 child slot, +4 offset);
/// `[gp+0x3A4]` per-link flag words (bit 0 active, 0x10 flipped).
const LINK_COUNT_36D: u32 = 0x37d;
const LINKS_388: u32 = 0x398;
const LINK_FLAGS_394: u32 = 0x3a4;

/// Entity pointer of `slot` from the table at `[gp+0x2E8]`.
fn slot_entity(m: &mut Machine, slot: u32) -> u32 {
    let table = m.gw(crate::common::ENTITIES);
    m.lw(table.wrapping_add(slot.wrapping_mul(4)))
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

/// `lwl/lwr` + `swl/swr` copy of the two (possibly unaligned) words at
/// `src + 0x18` to `dst + 0x18` (the angle SVECTOR).
fn copy_angles(m: &mut Machine, src: u32, dst: u32) {
    let bytes = m.read_bytes(src.wrapping_add(0x18), 8);
    m.write_bytes(dst.wrapping_add(0x18), &bytes);
}

/// 80045898: per-frame damage pass over entity slots: hit timers (800458D0),
/// damage application (80045A40), link damage (8004C3C4) and lock slots
/// (80046030). No arguments, no result.
pub fn update_damage_80044d50(m: &mut Machine) {
    tick_hit_timers_80044d88(m);
    apply_damage_80044ef8(m);
    propagate_link_damage_8004ac74(m);
    update_lock_slots_800453f8(m);
}

/// 800458D0: for slots 2..63 whose state word has bit 0x4, count the hit
/// timer byte (800C70D0) down; once it is or reaches zero, clear state bit
/// 0x4 and the entity's flag 0x4000.
pub fn tick_hit_timers_80044d88(m: &mut Machine) {
    for slot in 2..0x40u32 {
        let state_address = SLOT_STATE_4D98 + slot * 4;
        let state = m.lw(state_address);
        let entity = slot_entity(m, slot);
        if state & 4 == 0 {
            continue;
        }
        let timer_address = HIT_TIMER_4E98 + slot;
        let timer = m.lbu(timer_address);
        if timer != 0 {
            let left = timer.wrapping_sub(1);
            m.sb(timer_address, left);
            if left != 0 {
                continue;
            }
        }
        let state = m.lw(state_address);
        m.sw(state_address, state & !4);
        let flags = m.lw(entity);
        m.sw(entity, flags & !0x4000);
    }
}

// JP-REVIEW(R0276): changed function: PAL 80044EF8 -> JP 80045A40, call+:3 del:23 imm:21 ins:83 op:20 reg:2, 149 instructions differ (whole routine)
/// 80045A40: apply this frame's hits to entity slots 2..63 (entity flag
/// 0x200, state bit 0, hit points `+0x4E` below 10000). Word `+0x50` bits 0..5
/// name the attacker, word `+0x54` holds hit bits: 0..5 (direct, damage by
/// bit or by the attacker's record +0x20), 6..14 (splash, by bit or record
/// +0x1C); damage doubles when context word +0x398 is 0. Effects via
/// 8002F3C0, stage messages via 8004CEB0; the hit bits are then cleared.
// JP-PATCH: replaced by `crate::jp::res_p1_player::apply_damage_80044ef8` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn apply_damage_80044ef8(m: &mut Machine) {
    crate::jp::res_p1_player::apply_damage_80044ef8(m)
}

/// 8004C3C4: for each active link (`[gp+0x3A4]` bit 0) whose parent has no
/// hit points left while the child has: a child out of play (state bit 0
/// clear) with entity flag 0x10000 is wrecked (80038320 for slots < 0x20,
/// 8004E94C for units), its
/// hit points are zeroed and kind-5 links call 8004C338(link). With gameplay
/// bits 0xE and a child slot < 0x20: a hit-timer parent starts the child's
/// (80038320), and a parent without flag 0x4000 clears the child's.
pub fn propagate_link_damage_8004ac74(m: &mut Machine) {
    let mut index = 0u32;
    while index < u32::from(m.lbu(crate::GP + LINK_COUNT_36D)) {
        let link_flags = m.gw(LINK_FLAGS_394).wrapping_add(index * 4);
        if m.lw(link_flags) & 1 != 0 {
            let link = m.gw(LINKS_388).wrapping_add(index * 0x14);
            let parent_slot = u32::from(m.lbu(link.wrapping_add(2)));
            let child_slot = u32::from(m.lbu(link.wrapping_add(3)));
            let parent = slot_entity(m, parent_slot);
            let child = slot_entity(m, child_slot);
            if m.lh(parent.wrapping_add(0x4e)) <= 0 && m.lh(child.wrapping_add(0x4e)) > 0 {
                if m.lw(SLOT_STATE_4D98 + child_slot * 4) & 1 == 0 && m.lw(child) & 0x1_0000 != 0 {
                    if child_slot < 0x20 {
                        wreck_slot_80038010(m, child_slot);
                    } else {
                        external(m, 0x8004_e94c, [child_slot - 0x20, 0, 0, 0]);
                    }
                }
                let link = m.gw(LINKS_388).wrapping_add(index * 0x14);
                m.sh(child.wrapping_add(0x4e), 0);
                if m.lhu(link) == 5 {
                    external(m, 0x8004_c338, [index, 0, 0, 0]);
                }
            }
            let gameplay = m.gw(crate::common::GAMEPLAY);
            if m.lw(gameplay.wrapping_add(4)) & 0xe != 0 && child_slot < 0x20 {
                if m.lw(SLOT_STATE_4D98 + parent_slot * 4) & 4 != 0
                    && m.lw(SLOT_STATE_4D98 + child_slot * 4) & 4 == 0
                {
                    wreck_slot_80038010(m, child_slot);
                }
                if m.lw(parent) & 0x4000 == 0 {
                    let flags = m.lw(child);
                    if flags & 0x4000 != 0 {
                        m.sw(child, flags & !0x4000);
                    }
                }
            }
        }
        index += 1;
    }
}

/// 80046030: lock slots for entity slots 2..63: an empty lock byte of a slot
/// in play becomes 0xFF; a set byte of a slot out of play is released
/// (8004651C(slot)). Then each current lock target (bytes 2 and 3 of slot 0)
/// equal to byte 1 is dropped through 800464B0(target).
pub fn update_lock_slots_800453f8(m: &mut Machine) {
    for slot in 2..0x40u32 {
        let lock = LOCK_SLOTS_4B48 + slot * 4;
        let state = SLOT_STATE_4D98 + slot * 4;
        if m.lbu(lock) == 0 {
            if m.lw(state) & 1 != 0 {
                m.sb(lock, 0xff);
            }
        } else if m.lw(state) & 1 == 0 {
            release_lock_800458e0(m, slot);
        }
    }
    let mut current = m.lbu(LOCK_SLOTS_4B48 + 1);
    if current == 0xff {
        return;
    }
    let second = m.lbu(LOCK_SLOTS_4B48 + 2);
    if current == second {
        external(m, 0x8004_64b0, [u32::from(second), 0, 0, 0]);
        current = m.lbu(LOCK_SLOTS_4B48 + 1);
    }
    let third = m.lbu(LOCK_SLOTS_4B48 + 3);
    if current == third {
        external(m, 0x8004_64b0, [u32::from(third), 0, 0, 0]);
    }
}

/// 800517F4: per-frame ground-unit pass: unit states (80051498), per-unit
/// updates and the unit sound (80051414), unit motion and stage effects
/// (8005157C). No arguments, no result.
pub fn update_ground_units_800501c8(m: &mut Machine) {
    mark_units_8004fe70(m);
    update_units_8004fdf0(m);
    move_units_8004ff54(m);
}

/// 80051498: for the 32 ground units: clear entity flag 0x8000, then for
/// units with flag 0x4000 set it again when the slot is in play and wrecked
/// (state 0x10, sight record flag 0x80 or no hit points left), or out of play
/// and destroyed (state 0x2).
pub fn mark_units_8004fe70(m: &mut Machine) {
    let mut units = m.gw(UNITS_2C8);
    for unit in 0..0x20u32 {
        let slot4 = 0x80 + unit * 4;
        let entity = m.lw(units);
        let flags = m.lw(entity) & 0xffff_7fff;
        m.sw(entity, flags);
        if flags & 0x4000 != 0 {
            let state = m.lw(SLOT_STATE_4D98 + slot4);
            let marked = if state & 1 != 0 {
                state & 0x10 != 0
                    || {
                        let record = m.lw(SIGHT_4C98 + slot4);
                        m.lw(record) & 0x80 != 0
                    }
                    || {
                        let table = m.gw(crate::common::ENTITIES);
                        let unit_entity = m.lw(table.wrapping_add(slot4));
                        m.lh(unit_entity.wrapping_add(0x4e)) <= 0
                    }
            } else {
                state & 2 != 0
            };
            if marked {
                m.sw(entity, flags | 0x8000);
            }
        }
        units = units.wrapping_add(4);
    }
}

// JP-REVIEW(R0277): near function: PAL 8004FDF0 -> JP 80051414, del:3 ins:4, 7 instructions differ (whole routine)
/// 80051414: 80051A54(unit) for each ground unit with entity flags 0xC000,
/// then the unit sound (80051840) unless the stage is 0x29.
// JP-PATCH: replaced by `crate::jp::res_p1_player::update_units_8004fdf0` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn update_units_8004fdf0(m: &mut Machine) {
    crate::jp::res_p1_player::update_units_8004fdf0(m)
}

// JP-REVIEW(R0278): near function: PAL 80050214 -> JP 80051840, imm:3, 3 instructions differ (whole routine)
/// 80051840: sound of the nearest ground unit of kind 0x32/0x33/0x3D (entity
/// flags 0xC000, sight record distance). The voice `[gp+0x47C]` stops
/// (8002DDA8) when gameplay bit 0x4 is clear, or when no unit is within
/// 0x2000; otherwise it starts (8002DCB0(0x5B)) and is positioned
/// (8002E6DC/8002E67C/8002E84C/8002E8DC/8002EC48 with 800C77E0).
// JP-PATCH: replaced by `crate::jp::res_p1_player::update_unit_sound_80050214` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn update_unit_sound_80050214(m: &mut Machine) {
    crate::jp::res_p1_player::update_unit_sound_80050214(m)
}

// JP-REVIEW(R0279): near function: PAL 8004FF54 -> JP 8005157C, imm:2 ins:1 op:2, 5 instructions differ (whole routine)
/// 8005157C: ground units with entity flags 0xC000 and no state bit 0x400000
/// move by their velocity (`+0x38..+0x40` added to `+0x08..+0x10`) when
/// `+0x54` bit 31 is set, and keep only the top byte of `+0x54`. On stage 8
/// (kind 0x51) and stage 0x30 (kind 0x4E) units in play within 0x3000 of the
/// player call 80083CA4(position, context[+0x28] + unit * 8, 0 or 1).
// JP-PATCH: replaced by `crate::jp::res_p1_player::move_units_8004ff54` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn move_units_8004ff54(m: &mut Machine) {
    crate::jp::res_p1_player::move_units_8004ff54(m)
}

// JP-REVIEW(R0281): near function: PAL 80045950 -> JP 8004658C, imm:1 ins:1 op:2, 4 instructions differ (whole routine)
/// 8004658C: per-frame sensor pass: linked entities (8004BED4), sight records
/// (80045240), and, with gameplay bit 0x4 (except stage 9 while gameplay
/// word +8 is positive), the flight path log (80048658). No arguments, no
/// result.
// JP-PATCH: replaced by `crate::jp::res_p1_player::update_sensors_80045950` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn update_sensors_80045950(m: &mut Machine) {
    crate::jp::res_p1_player::update_sensors_80045950(m)
}

/// 8004BED4: place each active link's child entity from its parent by kind:
/// 3 mirror image below y = -0x42000 (angles reflected, flag 0x10000 follows
/// the parent's); 4 offset through 800451AC with the parent's angles (+0x1FFF
/// roll) and velocity; 5 orbit on the sine table at the parent's angles
/// (flip bit 0x10 when the child faces away), optionally spawning an effect
/// (80068800, a4 = 0x102); 6 offset with roll from the frame counter; other
/// kinds offset with the parent's angles.
pub fn update_linked_entities_8004a784(m: &mut Machine) {
    let mut index = 0u32;
    while index < u32::from(m.lbu(crate::GP + LINK_COUNT_36D)) {
        let flags_address = m.gw(LINK_FLAGS_394).wrapping_add(index * 4);
        let flags = m.lw(flags_address);
        if flags & 1 != 0 {
            let link = m.gw(LINKS_388).wrapping_add(index * 0x14);
            let table = m.gw(crate::common::ENTITIES);
            let parent_slot = u32::from(m.lbu(link.wrapping_add(2)));
            let child_slot = u32::from(m.lbu(link.wrapping_add(3)));
            let parent = m.lw(table.wrapping_add(parent_slot * 4));
            let kind = m.lhu(link);
            let child = m.lw(table.wrapping_add(child_slot * 4));
            match kind {
                3 => mirror_link_8004a784(m, parent_slot, parent, child),
                4 => {
                    place_offset_80044664(
                        m,
                        parent.wrapping_add(8),
                        link.wrapping_add(4),
                        child.wrapping_add(8),
                    );
                    let pitch = m.lhu(parent.wrapping_add(0x18));
                    m.sh(child.wrapping_add(0x18), pitch);
                    let yaw = m.lhu(parent.wrapping_add(0x1a));
                    m.sh(child.wrapping_add(0x1a), yaw);
                    let roll = m.lhu(parent.wrapping_add(0x1c));
                    m.sh(child.wrapping_add(0x1c), roll.wrapping_add(0x1fff));
                    copy_words(m, parent.wrapping_add(0x38), child.wrapping_add(0x38), 4);
                    let bits = m.lw(child.wrapping_add(0x54));
                    m.sw(child.wrapping_add(0x54), bits & 0x7fff_ffff);
                }
                5 => orbit_link_8004a784(m, index, flags_address, flags, parent, child),
                6 => {
                    place_offset_80044664(
                        m,
                        parent.wrapping_add(8),
                        link.wrapping_add(4),
                        child.wrapping_add(8),
                    );
                    let gameplay = m.gw(crate::common::GAMEPLAY);
                    copy_angles(m, parent, child);
                    let frame = m.lhu(gameplay.wrapping_add(0xc));
                    m.sh(child.wrapping_add(0x1c), (frame & 0x3f) << 8);
                }
                _ => {
                    place_offset_80044664(
                        m,
                        parent.wrapping_add(8),
                        link.wrapping_add(4),
                        child.wrapping_add(8),
                    );
                    copy_angles(m, parent, child);
                }
            }
        }
        index += 1;
    }
}

/// 8004BED4 kind 3: mirror image of the parent (x, -0x42000 - y, z; angles
/// -pitch, yaw, 0x2000 - roll). A player parent (slot 0) clears the child's
/// byte +4 and sets bit 0 of byte +5; a parent without flag 0x10000 clears
/// the child's.
fn mirror_link_8004a784(m: &mut Machine, parent_slot: u32, parent: u32, child: u32) {
    if parent_slot == 0 {
        let bits = m.lbu(child.wrapping_add(5));
        m.sb(child.wrapping_add(4), 0);
        m.sb(child.wrapping_add(5), bits | 1);
    }
    let x = m.lw(parent.wrapping_add(8));
    m.sw(child.wrapping_add(8), x);
    let y = m.lw(parent.wrapping_add(0xc));
    m.sw(child.wrapping_add(0xc), 0xfffb_e000u32.wrapping_sub(y));
    let z = m.lw(parent.wrapping_add(0x10));
    m.sw(child.wrapping_add(0x10), z);
    let pitch = m.lhu(parent.wrapping_add(0x18));
    m.sh(child.wrapping_add(0x18), 0u16.wrapping_sub(pitch));
    let yaw = m.lhu(parent.wrapping_add(0x1a));
    m.sh(child.wrapping_add(0x1a), yaw);
    let roll = m.lhu(parent.wrapping_add(0x1c));
    m.sh(child.wrapping_add(0x1c), 0x2000u16.wrapping_sub(roll));
    if m.lw(parent) & 0x1_0000 == 0 {
        let flags = m.lw(child);
        m.sw(child, flags & 0xfffe_ffff);
    }
}

/// 8004BED4 kind 5: toggle link flag 0x10 when the child's yaw is within
/// 0x1C01..0x23FF of the parent's; place the child on the parent's
/// (yaw + 0x1000, or + 0x3000 when flipped) and pitch direction: x/z offsets
/// `sin/cos(yaw) * cos(pitch) >> 9`, y offset `sin(pitch) << 5`; copy the
/// angles and byte +5 bit 0x8; with effects enabled spawn 80068800(parent
/// word 0, child position, parent +0x38, target word or -1, 0x102).
fn orbit_link_8004a784(
    m: &mut Machine,
    index: u32,
    flags_address: u32,
    flags: u32,
    parent: u32,
    child: u32,
) {
    let child_yaw = i32::from(m.lh(child.wrapping_add(0x1a)));
    let parent_yaw = i32::from(m.lh(parent.wrapping_add(0x1a)));
    let facing = child_yaw.wrapping_sub(parent_yaw) as u32 & 0x3fff;
    if facing.wrapping_sub(0x1c01) < 0x7ff {
        m.sw(flags_address, flags ^ 0x10);
    }
    let parent_yaw = i32::from(m.lh(parent.wrapping_add(0x1a)));
    let flipped = {
        let table = m.gw(LINK_FLAGS_394);
        m.lw(table.wrapping_add(index * 4)) & 0x10 != 0
    };
    let mut yaw = parent_yaw.wrapping_add(0x1000) as u32 & 0x3fff;
    if flipped {
        yaw = (yaw + 0x2000) & 0x3fff;
    }
    let sine = |m: &mut Machine, angle: u32| i32::from(m.lh(SINE_0498 + angle * 2));
    let sin_yaw = sine(m, yaw);
    let pitch = i32::from(m.lh(parent.wrapping_add(0x18))) as u32 & 0x3fff;
    let cos_pitch = sine(m, pitch + 0x1000);
    let x_offset = sin_yaw.wrapping_mul(cos_pitch);
    let cos_yaw = sine(m, yaw + 0x1000);
    let z_offset = cos_yaw.wrapping_mul(cos_pitch);
    let x = m.lw(parent.wrapping_add(8));
    m.sw(
        child.wrapping_add(8),
        x.wrapping_add((x_offset >> 9) as u32),
    );
    let sin_pitch = sine(m, pitch);
    let y = m.lw(parent.wrapping_add(0xc));
    m.sw(
        child.wrapping_add(0xc),
        y.wrapping_add((sin_pitch as u32) << 5),
    );
    let z = m.lw(parent.wrapping_add(0x10));
    m.sw(
        child.wrapping_add(0x10),
        z.wrapping_add((z_offset >> 9) as u32),
    );
    copy_angles(m, parent, child);
    let bits = if m.lbu(parent.wrapping_add(5)) & 8 != 0 {
        m.lbu(child.wrapping_add(5)) | 8
    } else {
        m.lbu(child.wrapping_add(5)) & 0xf7
    };
    m.sb(child.wrapping_add(5), bits);
    let effects = m.lw(EFFECTS_AF1C);
    if m.lw(effects.wrapping_add(0x10)) & 0x4000 == 0 {
        return;
    }
    let target = if m.lbu(TARGET_4B28 + 0x12) != 0xff {
        let slot = u32::from(m.lbu(TARGET_4B28 + 0xe));
        let entity = slot_entity(m, slot);
        m.lw(entity)
    } else {
        0xffff_ffff
    };
    let kind = m.lw(parent);
    spawn_80067030(
        m,
        kind,
        child.wrapping_add(8),
        parent.wrapping_add(0x38),
        target,
        0x102,
    );
}

/// 80044F94: `out` = the VECTOR from position `from` to position `to`
/// (components `(to - from) >> 12`, arithmetic) rotated by the matrix that
/// 800308F0 builds from the angles at `from + 0x10` (ApplyMatrixLV). Frame
/// 0x50: the matrix at sp+0x10, the vector at sp+0x30. Returns
/// ApplyMatrixLV's `v0` (`out`).
pub fn relative_vector_8004444c(m: &mut Machine, from: u32, to: u32, out: u32) -> u32 {
    // One implementation: hud.rs owns 80044F94.
    crate::dispatch::call(m, 0x8004_4f94, [from, to, out, 0])
}

/// 80044F94/80045024 helper: VECTOR `(to[i] - from[i]) >> 12` (wrapping
/// `subu`, arithmetic shift) stored at `vector`.
fn difference_8004444c(m: &mut Machine, from: u32, to: u32, vector: u32) {
    for i in 0..3 {
        let a = m.lw(to.wrapping_add(i * 4));
        let b = m.lw(from.wrapping_add(i * 4));
        m.sw(vector + i * 4, ((a.wrapping_sub(b) as i32) >> 12) as u32);
    }
}

/// 80045024: `out` = ApplyMatrixLV(`matrix`, `(to - from) >> 12`) for the
/// positions `from` (a0) and `to` (a2). Frame 0x28: the vector at sp+0x10.
/// Returns ApplyMatrixLV's `v0` (`out`).
pub fn view_vector_800444dc(m: &mut Machine, from: u32, matrix: u32, to: u32, out: u32) -> u32 {
    let sp = m.push_frame(0x28);
    let vector = sp + 0x10;
    difference_8004444c(m, from, to, vector);
    let v0 = crate::sdk::apply_matrix_lv(m, matrix, vector, out);
    m.pop_frame(0x28);
    v0
}

/// 80045090: off-axis angle of the VECTOR at `vector`: the horizontal length
/// h = SquareRoot0(x*x + y*y) (components pre-shifted right by 3 and the
/// root shifted back when |x| + |y| > 0xAFFF; h = 0x100000 when |x| + |y| >
/// 0x57FFF), then the absolute value of 80030D54(h, z).
pub fn off_axis_angle_80044548(m: &mut Machine, vector: u32) -> u32 {
    let x = m.lw(vector) as i32;
    let y = m.lw(vector.wrapping_add(4)) as i32;
    let sum = x.wrapping_abs().wrapping_add(y.wrapping_abs());
    let horizontal = if sum > 0x5_7fff {
        0x10_0000
    } else {
        let shift = if sum > 0xafff { 3 } else { 0 };
        let (xs, ys) = (x >> shift, y >> shift);
        let squares = xs.wrapping_mul(xs).wrapping_add(ys.wrapping_mul(ys));
        crate::sdk::square_root0(m, squares as u32) << shift
    };
    let z = m.lw(vector.wrapping_add(8));
    let angle = crate::shared::f_80030b10(m, [horizontal, z, 0, 0]) as i32;
    angle.wrapping_abs() as u32
}

/// 80045240: sight records: player-relative records (80045278), linked
/// records (8004C5B8), home records (800455B8), record timers (80045768).
/// Returns 80045768's `v0` (0), which it leaves in `v0`.
pub fn update_sight_800446f8(m: &mut Machine) -> u32 {
    update_sight_records_80044730(m);
    copy_link_records_8004ae68(m);
    update_home_records_80044a70(m);
    tick_record_timers_80044c20(m)
}

/// 80045278: player view matrix (800308F0(player angles, 800C6EA0)), then for
/// slots 2..63: clear entity flag 0x2000; for entities with flag 0x4000 and
/// no state bit 0x400000 clear the sight record and refresh its distance
/// (80083DFC; unknown distance 0x100000 every 64 frames, beyond 0x84D0
/// every 16, else every frame). Flags: 0x80 near (<= 0x84CF, units < 0x3000);
/// within 0x7FFF: 0x1 and entity flag 0x2000, relative vector (80045024),
/// off-axis angle +0x18 (80045090), 0x1000000, 0x2 angle < 0x600, 0x40/0x4
/// within 0x3000/0x2000 and angle, 0x100/0x8/0x10 within view +0x24
/// (angle < view +0x20 / < 0x600), 0x20 within view +0x28 with 0x2.
/// Returns 0.
pub fn update_sight_records_80044730(m: &mut Machine) -> u32 {
    let player = slot_entity(m, 0);
    crate::shared::f_800306ac(m, [player.wrapping_add(0x18), VIEW_4C68, 0, 0]);
    for slot in 2..0x40u32 {
        let offset = slot * 4;
        let entity = slot_entity(m, slot);
        let flags = m.lw(entity) & !0x2000;
        let record = m.lw(SIGHT_4C98 + offset);
        m.sw(entity, flags);
        if flags & 0x4000 == 0 {
            continue;
        }
        if m.lw(SLOT_STATE_4D98 + offset) & 0x40_0000 != 0 {
            continue;
        }
        let distance_address = record.wrapping_add(4);
        let previous = m.lw(distance_address);
        m.sw(record, 0);
        let gameplay = m.gw(crate::common::GAMEPLAY);
        if previous == 0x10_0000 {
            if slot != m.lw(gameplay.wrapping_add(0xc)) & 0x3f {
                continue;
            }
        } else if previous as i32 > 0x84d0 && slot & 0xf != m.lw(gameplay.wrapping_add(0xc)) & 0xf {
            continue;
        }
        let distance =
            crate::shared::f_800827b4(m, [entity.wrapping_add(8), player.wrapping_add(8), 0, 0]);
        m.sw(distance_address, distance);
        let near = if slot < 0x20 {
            distance as i32 <= 0x84cf
        } else {
            (distance as i32) < 0x3000
        };
        let set = |m: &mut Machine, bits: u32| {
            let value = m.lw(record);
            m.sw(record, value | bits);
        };
        if near {
            set(m, 0x80);
        }
        if m.lw(distance_address) as i32 > 0x7fff {
            continue;
        }
        set(m, 1);
        let value = m.lw(entity);
        m.sw(entity, value | 0x2000);
        if m.lw(distance_address) as i32 > 0x7fff {
            continue;
        }
        set(m, 0x100_0000);
        let relative = record.wrapping_add(8);
        view_vector_800444dc(
            m,
            player.wrapping_add(8),
            VIEW_4C68,
            entity.wrapping_add(8),
            relative,
        );
        let angle_address = record.wrapping_add(0x18);
        let angle = off_axis_angle_80044548(m, relative);
        m.sh(angle_address, angle as u16);
        if (angle as u16 as i16) < 0x600 {
            set(m, 2);
        }
        if (m.lw(distance_address) as i32) < 0x3000 && m.lh(angle_address) < 0x600 {
            set(m, 0x40);
        }
        if (m.lw(distance_address) as i32) < 0x2000 && m.lh(angle_address) < 0x600 {
            set(m, 4);
        }
        if (m.lw(distance_address) as i32) < m.lw(VIEW_4C68 + 0x24) as i32 {
            let value = m.lw(record);
            m.sw(record, value | 0x100);
            if i32::from(m.lh(angle_address)) < m.lw(VIEW_4C68 + 0x20) as i32 {
                m.sw(record, value | 0x108);
            }
            if m.lh(angle_address) < 0x600 {
                set(m, 0x10);
            }
        }
        if (m.lw(distance_address) as i32) < m.lw(VIEW_4C68 + 0x28) as i32 {
            let value = m.lw(record);
            if value & 2 != 0 {
                m.sw(record, value | 0x20);
            }
        }
    }
    0
}

/// 8004C5B8: for each active link: the child's sight record gets distance 1
/// when the parent is the player (slot 0), else a copy of the parent's first
/// 0x28 bytes; a child with flag 0x4000 whose parent has 0x4000 and 0x2000
/// gains flag 0x2000.
pub fn copy_link_records_8004ae68(m: &mut Machine) {
    let mut index = 0u32;
    while index < u32::from(m.lbu(crate::GP + LINK_COUNT_36D)) {
        let flags = m.gw(LINK_FLAGS_394).wrapping_add(index * 4);
        if m.lw(flags) & 1 != 0 {
            let link = m.gw(LINKS_388).wrapping_add(index * 0x14);
            let child_slot = u32::from(m.lbu(link.wrapping_add(3)));
            let parent_slot = u32::from(m.lbu(link.wrapping_add(2)));
            let child_record = m.lw(SIGHT_4C98 + child_slot * 4);
            if parent_slot == 0 {
                m.sw(child_record.wrapping_add(4), 1);
            } else {
                let parent_record = m.lw(SIGHT_4C98 + parent_slot * 4);
                copy_words(m, parent_record, child_record, 10);
            }
            let parent_flags = {
                let parent = slot_entity(m, parent_slot);
                m.lw(parent)
            };
            if parent_flags & 0x4000 != 0 {
                let child = slot_entity(m, child_slot);
                let child_flags = m.lw(child);
                if child_flags & 0x4000 != 0 && parent_flags & 0x2000 != 0 {
                    m.sw(child, child_flags | 0x2000);
                }
            }
        }
        index += 1;
    }
}

/// 800455B8: home records (0xB8 bytes per slot at `[gp+0x34C]`) of slots
/// 2..31 with entity flags 0xC000. When state bit 0x20 is set, refresh the
/// home distance (+0x88, 80083DFC to +0x68), vector (+0x8C, 80044F94) and
/// angle (+0x9C, 80045090) and clear the bit. With a target slot (+0x60 not
/// 0xFF) refresh the target distance/vector/angle (+0xA0/+0xA4/+0xB4) and
/// add 0x10 to +0xB6 within 0x2000; otherwise copy the home values and clear
/// +0xB6. Returns 0.
pub fn update_home_records_80044a70(m: &mut Machine) -> u32 {
    for slot in 2..0x20u32 {
        let state = SLOT_STATE_4D98 + slot * 4;
        let entity = slot_entity(m, slot);
        let record = m.gw(0x34c).wrapping_add(slot * 0xb8);
        if !m.lw(entity) & 0xc000 != 0 {
            continue;
        }
        let position = entity.wrapping_add(8);
        if m.lw(state) & 0x20 != 0 {
            let home = record.wrapping_add(0x68);
            let distance = crate::shared::f_800827b4(m, [position, home, 0, 0]);
            m.sw(record.wrapping_add(0x88), distance);
            let vector = record.wrapping_add(0x8c);
            relative_vector_8004444c(m, position, home, vector);
            let angle = off_axis_angle_80044548(m, vector);
            m.sh(record.wrapping_add(0x9c), angle as u16);
            let bits = m.lw(state);
            m.sw(state, bits & !0x20);
        }
        let target = m.lbu(record.wrapping_add(0x60));
        if target != 0xff {
            let other = slot_entity(m, u32::from(target)).wrapping_add(8);
            let distance = crate::shared::f_800827b4(m, [position, other, 0, 0]);
            m.sw(record.wrapping_add(0xa0), distance);
            let vector = record.wrapping_add(0xa4);
            relative_vector_8004444c(m, position, other, vector);
            let angle = off_axis_angle_80044548(m, vector);
            let distance = m.lw(record.wrapping_add(0xa0));
            m.sh(record.wrapping_add(0xb4), angle as u16);
            if (distance as i32) < 0x2000 {
                let value = m.lhu(record.wrapping_add(0xb6));
                m.sh(record.wrapping_add(0xb6), value.wrapping_add(0x10));
            }
        } else {
            let distance = m.lw(record.wrapping_add(0x88));
            m.sw(record.wrapping_add(0xa0), distance);
            copy_words(m, record.wrapping_add(0x8c), record.wrapping_add(0xa4), 4);
            let angle = m.lhu(record.wrapping_add(0x9c));
            m.sh(record.wrapping_add(0xb6), 0);
            m.sh(record.wrapping_add(0xb4), angle);
        }
    }
    0
}

/// 80045768: sight record timers of slots 2..63 with entity flags 0xC000:
/// +0x1E counts while flag 0x40 holds and +0x1C while 0x8 holds (saturating
/// at 0x7FFF, else 0); for slots < 0x20, +0x1A counts while flag 0x4 holds
/// (else 0) and sets flag 0x2000000 once it reaches the halfword +0xC of the
/// slot's `[gp+0x384]` entry pointer (+0x18). Returns 0.
pub fn tick_record_timers_80044c20(m: &mut Machine) -> u32 {
    let entries = m.gw(0x384);
    let table = m.gw(crate::common::ENTITIES);
    for slot in 2..0x40u32 {
        let entity = m.lw(table.wrapping_add(slot * 4));
        let flags = m.lw(entity);
        let record = m.lw(SIGHT_4C98 + slot * 4);
        if !flags & 0xc000 != 0 || record == 0 {
            continue;
        }
        for (bit, offset) in [(0x40, 0x1e), (8, 0x1c)] {
            let counter = record.wrapping_add(offset);
            if m.lw(record) & bit != 0 {
                if m.lh(counter) < 0x7fff {
                    let value = m.lhu(counter);
                    m.sh(counter, value.wrapping_add(1));
                }
            } else {
                m.sh(counter, 0);
            }
        }
        if slot < 0x20 {
            let counter = record.wrapping_add(0x1a);
            if m.lw(record) & 4 != 0 {
                let value = m.lhu(counter).wrapping_add(1);
                let entry = m.lw(entries.wrapping_add(slot * 0x40).wrapping_add(0x18));
                m.sh(counter, value);
                if (value as i16) >= m.lh(entry.wrapping_add(0xc)) {
                    let bits = m.lw(record);
                    m.sw(record, bits | 0x200_0000);
                }
            } else {
                m.sh(counter, 0);
            }
        }
    }
    0
}

// JP-REVIEW(R0282): near function: PAL 8004790C -> JP 80048658, imm:1, 1 instructions differ (whole routine)
/// 80048658: flight path log at context +0x3BC: every 120 frames (gameplay
/// word +0xC; the original divides by reciprocal multiplication), while the
/// count (+8) is below 450, append the player's x and z as bytes
/// `clamp(((v >> 12) + 0x40000) >> 11, 0, 255)` at +0xC + count * 2; the
/// halfword +2 counts every call.
// JP-PATCH: replaced by `crate::jp::res_p1_player::record_flight_path_8004790c` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn record_flight_path_8004790c(m: &mut Machine) {
    crate::jp::res_p1_player::record_flight_path_8004790c(m)
}

// ==== end of group A ====

// ---- group B: player effects (80072178 subtree) ---------------------------------

/// Return value of the ground rays (8009A5D0, 8009A800, 8009B4EC) when
/// nothing was hit; the callers test `0x7FFFFFFE < v0` (signed).
const GROUND_RAY_MISS: u32 = 0x7fff_ffff;
/// Contact normal pointer and surface word left by the last ground-ray hit.
const CONTACT_NORMAL: u32 = 0x800b_d0c0;
const CONTACT_SURFACE: u32 = 0x800b_d0c4;
/// Terrain tests are skipped above this height (y < 0xFF448000; y grows
/// downwards); ground rays start no higher.
const GROUND_CEILING: u32 = 0xff44_8000;
/// Normals of the six world-bound faces (8-byte SVECTORs, index 1..6).
const WORLD_BOUND_NORMALS: u32 = 0x8002_4630;
/// The SVECTOR (0, 0, 0x4000) rotated by 8007084C.
const ATTITUDE_AXIS: u32 = 0x800b_ce0c;
/// Engine-sound parameter block filled by 80072114.
const ENGINE_PARAMS: u32 = 0x800c_7be8;
/// Engine-sound state at `$gp+0x32C`: +0 started flag, +1..+3 the voices of
/// the three engine sounds (0xFF = none), +4..+6 their sound ids.
const ENGINE_SOUND: u32 = crate::machine::GP + 0x32c;
/// SPU voice table (24 entries of 20 bytes): +1 flags (0x10 = update), +4/+6
/// volumes, +0xC pitch.
const VOICE_TABLE: u32 = 0x800c_5830;
/// Sound table (20-byte entries): +4/+6 volumes, +8 base pitch.
const SOUND_TABLE: u32 = 0x800c_5a10;

/// 8007084C: attitude updates requested by entity +0x54 bits 16..23
/// (`state` is the flight state), then those bits are cleared (bits 24..31
/// kept). Nothing happens unless entity word 0 has both 0x4000 and 0x8000.
/// - 0x400000: 80030F64(entity +0x18, entity +0x30);
/// - 0x100000: 80030F64(state +0x40, entity +0x30), then (bit rechecked)
///   800313A8(entity +0x18, entity +0x30, state +0x40);
/// - 0x20000 / 0x10000: 80030EF8(entity +0x20, entity +0x18 / state +0x40,
///   entity +0x38);
/// - any of 0xF80000: the matrices of entity +0x18 (800308F0) and state +0x40
///   (80030778) are multiplied (MulMatrix), the axis (0, 0, 0x4000) rotated
///   by the product (MVMVA sf=1, no translation) and its angles stored at
///   state +0x38 (80030D54(-y, z)), +0x3A (80030D54(x, z)) and +0x3C = 0.
pub fn apply_attitude_8006f1c8(m: &mut Machine, entity: u32, state: u32) {
    let e = |offset: u32| entity.wrapping_add(offset);
    let s = |offset: u32| state.wrapping_add(offset);
    if !m.lw(entity) & 0xc000 != 0 {
        return;
    }
    let sp = m.push_frame(0x80);
    if m.lw(e(0x54)) & 0x40_0000 != 0 {
        orientation_80030d20(m, e(0x18), e(0x30));
    }
    if m.lw(e(0x54)) & 0x10_0000 != 0 {
        orientation_80030d20(m, s(0x40), e(0x30));
        if m.lw(e(0x54)) & 0x10_0000 != 0 {
            turn_orientation_80031164(m, e(0x18), e(0x30), s(0x40));
        }
    }
    if m.lw(e(0x54)) & 0x2_0000 != 0 {
        external(m, 0x8003_0ef8, [e(0x20), e(0x18), e(0x38), 0]);
    }
    if m.lw(e(0x54)) & 0x1_0000 != 0 {
        external(m, 0x8003_0ef8, [e(0x20), s(0x40), e(0x38), 0]);
    }
    if m.lw(e(0x54)) & 0xf8_0000 != 0 {
        // lwl/lwr + swl/swr pairs on word-aligned addresses: a plain copy.
        let axis_xy = m.lw(ATTITUDE_AXIS);
        let axis_z = m.lw(ATTITUDE_AXIS + 4);
        m.sw(sp + 0x60, axis_xy);
        m.sw(sp + 0x64, axis_z);
        crate::shared::f_800306ac(m, [e(0x18), sp + 0x10, 0, 0]);
        crate::shared::f_80030534(m, [s(0x40), sp + 0x30, 0, 0]);
        crate::sdk::mul_matrix(m, sp + 0x10, sp + 0x30);
        // Inline SetRotMatrix(sp+0x10), ldv0(sp+0x60), two NOPs (no hazard).
        crate::sdk::set_rot_matrix(m, sp + 0x10);
        let xy = m.lw(sp + 0x60);
        m.gte.mtc2(reg::VXY0, xy);
        let z = m.lw(sp + 0x64);
        m.gte.mtc2(reg::VZ0, z);
        m.gte.mvmva(true, false, 0, 0, 3);
        for i in 0..3 {
            let value = m.gte.mfc2(reg::MAC1 + i);
            m.sw(sp + 0x50 + 4 * i as u32, value);
        }
        let y = m.lw(sp + 0x54);
        let z = m.lw(sp + 0x58);
        let pitch = crate::shared::f_80030b10(m, [0u32.wrapping_sub(y), z, 0, 0]);
        m.sh(s(0x38), pitch as u16);
        let x = m.lw(sp + 0x50);
        let z = m.lw(sp + 0x58);
        let yaw = crate::shared::f_80030b10(m, [x, z, 0, 0]);
        m.sh(s(0x3a), yaw as u16);
        m.sh(s(0x3c), 0);
    }
    let requests = m.lw(e(0x54));
    m.sw(e(0x54), requests & 0xff00_0000);
    m.pop_frame(0x80);
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
// JP-PATCH: replaced by `crate::jp::res_p1_player::collide_entities_8006f3fc` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn collide_entities_8006f3fc(m: &mut Machine, entity: u32, state: u32) {
    crate::jp::res_p1_player::collide_entities_8006f3fc(m, entity, state)
}

/// Terrain grid cell of a world coordinate: (coordinate >> 23) + 0x80, with
/// negative cells mapped to 0..7 and cells from 0x100 to 0xF8..0xFF.
fn ground_cell(coordinate: u32) -> u32 {
    let mut cell = ((coordinate as i32) >> 23) + 0x80;
    if cell < 0 {
        cell &= 7;
    }
    if cell >= 0x100 {
        cell = (cell & 7) + 0xf8;
    }
    cell as u32
}

/// The contact of the last ground-ray hit: normal `[800BD0C0]` into
/// `state` +0x14..+0x18, surface `[800BD0C4]` into +0x2C, `bit` into +0x10.
fn store_ground_contact(m: &mut Machine, state: u32, bit: u32) {
    let s = |offset: u32| state.wrapping_add(offset);
    let normal = m.lw(CONTACT_NORMAL);
    let surface = m.lw(CONTACT_SURFACE);
    let nx = m.lhu(normal);
    m.sh(s(0x14), nx);
    let ny = m.lhu(normal.wrapping_add(2));
    m.sh(s(0x16), ny);
    let nz = m.lhu(normal.wrapping_add(4));
    let status = m.lw(s(0x10));
    m.sw(s(0x2c), surface);
    m.sw(s(0x10), status | bit);
    m.sh(s(0x18), nz);
}

/// 80070CF4: world bounds and terrain contacts of `entity` (the player)
/// for its next position (position + velocity); `state` is the flight state.
/// - Bounds `[gp+0x2E0]` +0x2A0..+0x2B4 (min/max x, y, z) with a 0x84
///   margin: a clamped next position is stored as the position, the face
///   normal (80024630 + 8 * face) into state +0x14..+0x18, the surface (4,
///   or 3 / 1 for the y faces) into 800BD0C4 and state +0x2C, bit 2 into
///   state +0x10.
/// - Terrain (heights in units of 0x1000, y grows downwards), skipped above
///   0xFF448000: the grid cell of the position (map `[gp+0x2E0]` +0x248,
///   9-bit block numbers, 0 = none; block pointers from +0x24C) selects a
///   block with levels +0x16 and +0x14. Unless the current and next heights
///   are both more than 0x84 above +0x16 within one cell, the move is
///   ray-tested (8009A5D0 within a cell, 8009A800 across cells; the hit
///   point goes to state +0x1C) and a hit stored with bit 1. When the next
///   height is lower than +0x16 - 0x63, a vertical ray over +/-0x44000
///   around the next height (downwards when falling) stores a hit with bit 2
///   and puts the entity 0x42000 above the hit (falling) or below it. When
///   the next height is more than 0x3C below +0x14, the entity is put 0x3C
///   above +0x14 with an upward normal, bit 4, and the position as the
///   contact point (state +0x2C = 0).
pub fn world_collision_8006f670(m: &mut Machine, entity: u32, state: u32) {
    let e = |offset: u32| entity.wrapping_add(offset);
    let s = |offset: u32| state.wrapping_add(offset);
    let sp = m.push_frame(0x48);
    let mut x = m.lw(e(0x38)).wrapping_add(m.lw(e(8))) as i32;
    let mut y = m.lw(e(0x3c)).wrapping_add(m.lw(e(0xc))) as i32;
    let mut z = m.lw(e(0x40)).wrapping_add(m.lw(e(0x10))) as i32;
    let bounds = m.gw(0x2e0).wrapping_add(0x2a0);
    let mut face = 0u32;
    let mut surface = 0u32;
    let bound = |m: &mut Machine, offset: u32, margin: i32| {
        (m.lw(bounds.wrapping_add(offset)) as i32).wrapping_add(margin)
    };
    let low = bound(m, 0, 0x84);
    if x < low {
        (surface, face, x) = (4, 1, low);
    }
    let high = bound(m, 4, -0x84);
    if high < x {
        (surface, face, x) = (4, 2, high);
    }
    let low = bound(m, 8, 0x84);
    if y < low {
        (surface, face, y) = (3, 3, low);
    }
    let high = bound(m, 0xc, -0x84);
    if high < y {
        (surface, face, y) = (1, 4, high);
    }
    let low = bound(m, 0x10, 0x84);
    if z < low {
        (surface, face, z) = (4, 5, low);
    }
    let high = bound(m, 0x14, -0x84);
    if high < z {
        (surface, face, z) = (4, 6, high);
    }
    if face != 0 {
        let normal = WORLD_BOUND_NORMALS + face * 8;
        m.sw(e(8), x as u32);
        m.sw(e(0xc), y as u32);
        m.sw(e(0x10), z as u32);
        let nx = m.lhu(normal);
        m.sh(s(0x14), nx);
        let ny = m.lhu(normal + 2);
        m.sh(s(0x16), ny);
        let nz = m.lhu(normal + 4);
        let status = m.lw(s(0x10));
        m.sw(CONTACT_SURFACE, surface);
        m.sw(s(0x2c), surface);
        m.sw(s(0x10), status | 2);
        m.sh(s(0x18), nz);
    }
    if (m.lw(e(0xc)) as i32) < GROUND_CEILING as i32 {
        m.pop_frame(0x48);
        return;
    }
    let terrain = m.gw(0x2e0).wrapping_add(0x244);
    let tiles = m.lw(terrain.wrapping_add(4));
    let next_x = m.lw(e(8)).wrapping_add(m.lw(e(0x38)));
    m.sw(sp + 0x10, next_x);
    let next_y = m.lw(e(0xc)).wrapping_add(m.lw(e(0x3c)));
    m.sw(sp + 0x14, next_y);
    let next_z = m.lw(e(0x10)).wrapping_add(m.lw(e(0x40)));
    m.sw(sp + 0x18, next_z);
    let next_cell = ground_cell(next_x) + (ground_cell(next_z) << 8);
    let cell = ground_cell(m.lw(e(8))) + (ground_cell(m.lw(e(0x10))) << 8);
    let tile = u32::from(m.lhu(tiles.wrapping_add(cell * 2)) & 0x1ff);
    if tile == 0 {
        m.pop_frame(0x48);
        return;
    }
    let blocks = m.lw(terrain.wrapping_add(8));
    let block = m.lw(blocks.wrapping_add(tile * 4));
    let height = ((m.lw(e(0xc)) as i32) >> 12) + 0x84;
    let level = i32::from(m.lh(block.wrapping_add(0x16)));
    let next_height = (next_y as i32) >> 12;
    if height < level && next_height + 0x84 < level && cell == next_cell {
        m.pop_frame(0x48);
        return;
    }
    let hit = if cell == next_cell {
        ground_ray_80098b80(m, e(8), sp + 0x10, s(0x1c), 0x1f80_0000)
    } else {
        ground_ray_cells_80098db0(m, e(8), sp + 0x10, s(0x1c), 0x1f80_0000)
    };
    if hit != GROUND_RAY_MISS {
        store_ground_contact(m, state, 1);
    }
    if i32::from(m.lh(block.wrapping_add(0x16))) < next_height + 0x63 {
        let x = m.lw(e(8));
        m.sw(sp + 0x10, x);
        let y = m.lw(e(0xc)).wrapping_add(m.lw(e(0x3c)));
        m.sw(sp + 0x14, y.wrapping_add(0x4_4000));
        let z = m.lw(e(0x10));
        m.sw(sp + 0x18, z);
        let x = m.lw(e(8));
        m.sw(sp + 0x20, x);
        let y = m.lw(e(0xc)).wrapping_add(m.lw(e(0x3c)));
        m.sw(sp + 0x24, y.wrapping_sub(0x4_4000));
        let z = m.lw(e(0x10));
        m.sw(sp + 0x28, z);
        let (from, to) = if m.lw(e(0x3c)) as i32 > 0 {
            (sp + 0x20, sp + 0x10)
        } else {
            (sp + 0x10, sp + 0x20)
        };
        let hit = ground_ray_80098b80(m, from, to, s(0x1c), 0x1f80_0000);
        if hit != GROUND_RAY_MISS {
            store_ground_contact(m, state, 2);
            let lift = if m.lw(e(0x3c)) as i32 > 0 {
                0xfffb_e000
            } else {
                0x4_2000
            };
            let ground = m.lw(s(0x20));
            m.sw(e(0xc), ground.wrapping_add(lift));
        }
    }
    let limit = i32::from(m.lh(block.wrapping_add(0x14)));
    if limit < next_height - 0x3c {
        m.sw(e(0xc), ((limit - 0x3c) as u32) << 12);
        let status = m.lw(s(0x10));
        m.sh(s(0x14), 0);
        m.sh(s(0x16), 0xf000);
        m.sh(s(0x18), 0);
        m.sw(s(0x10), status | 4);
        let px = m.lw(e(8));
        m.sw(s(0x1c), px);
        let py = m.lw(e(0xc));
        m.sw(s(0x20), py);
        let pz = m.lw(e(0x10));
        m.sw(s(0x2c), 0);
        m.sw(s(0x24), pz);
    }
    m.pop_frame(0x48);
}

/// 80070A34: when entity +0x54 bit 31 is set, add the velocity (+0x38..+0x40)
/// to the position (+0x8..+0x10).
pub fn integrate_position_8006f3b0(m: &mut Machine, entity: u32) {
    let e = |offset: u32| entity.wrapping_add(offset);
    if (m.lw(e(0x54)) as i32) >= 0 {
        return;
    }
    let x = m.lw(e(8));
    let vx = m.lw(e(0x38));
    let vy = m.lw(e(0x3c));
    let vz = m.lw(e(0x40));
    m.sw(e(8), x.wrapping_add(vx));
    let y = m.lw(e(0xc));
    let z = m.lw(e(0x10));
    m.sw(e(0xc), y.wrapping_add(vy));
    m.sw(e(0x10), z.wrapping_add(vz));
}

/// 800711FC: ground height under the VECTOR at `position`. Without a terrain
/// block in its grid cell (map `[gp+0x2E0]` +0x248) the normal is up and the
/// surface 1, result 0. Otherwise a vertical ray (8009A5D0, scratchpad work
/// area) runs from (x, max(y, 0xFF448000), z) to (x, 0, z): a hit returns
/// its height (ray output +4) and copies the hit normal `[800BD0C0]` and
/// surface `[800BD0C4]`; a miss gives the up normal, surface 0 and 0.
/// `normal` (SVECTOR) and `surface` (word) are optional (0 = not stored).
pub fn ground_height_8006fb78(m: &mut Machine, position: u32, normal: u32, surface: u32) -> u32 {
    let up = |m: &mut Machine| {
        if normal != 0 {
            m.sh(normal, 0);
            m.sh(normal.wrapping_add(2), 0xf000);
            m.sh(normal.wrapping_add(4), 0);
        }
    };
    let x = m.lw(position);
    let z = m.lw(position.wrapping_add(8));
    let map = m.gw(0x2e0);
    let tiles = m.lw(map.wrapping_add(0x248));
    let cell = ground_cell(x) + (ground_cell(z) << 8);
    if m.lhu(tiles.wrapping_add(cell * 2)) & 0x1ff == 0 {
        up(m);
        if surface != 0 {
            m.sw(surface, 1);
        }
        return 0;
    }
    let sp = m.push_frame(0x50);
    let y = m.lw(position.wrapping_add(4));
    let top = if (GROUND_CEILING as i32) < y as i32 {
        y
    } else {
        GROUND_CEILING
    };
    m.sw(sp + 0x10, x);
    m.sw(sp + 0x14, top);
    let z = m.lw(position.wrapping_add(8));
    m.sw(sp + 0x18, z);
    let x = m.lw(position);
    m.sw(sp + 0x24, 0);
    m.sw(sp + 0x20, x);
    let z = m.lw(position.wrapping_add(8));
    m.sw(sp + 0x28, z);
    let hit = ground_ray_80098b80(m, sp + 0x10, sp + 0x20, sp + 0x30, 0x1f80_0000);
    let height = if hit == GROUND_RAY_MISS {
        up(m);
        if surface != 0 {
            m.sw(surface, 0);
        }
        0
    } else {
        if normal != 0 {
            let source = m.lw(CONTACT_NORMAL);
            for offset in [0, 2, 4] {
                let value = m.lhu(source.wrapping_add(offset));
                m.sh(normal.wrapping_add(offset), value);
            }
        }
        if surface != 0 {
            let value = m.lw(CONTACT_SURFACE);
            m.sw(surface, value);
        }
        m.lw(sp + 0x34)
    };
    m.pop_frame(0x50);
    height
}

/// 800301F0: engine sound voices from the parameter block `params` (see
/// 80072114). The first call starts the three engine sounds (8002DB4C on
/// the ids at `$gp+0x330..0x332`, voices into `$gp+0x32D..0x32F`). Each
/// started voice gets its sound's base pitch (sound table +8) shifted by
/// 8002E64C (8002DE24) and its volumes (+4/+6) offset through 8002DEC8:
/// - voice 3: pitch + (+8 >> 4), volumes + (+8 >> 2) (both signed halfword);
/// - voice 2: pitch + ((+0 - 0x24000) << 9 >> 16); left - 4 * (+0xA), right
///   + 4 * (+0xA);
/// - voice 1: pitch + (|+0xC| << 18 >> 16); volumes + 8 * |+0xC| + 0x1000 -
///   (clamp(+4 - 0x200000, 0, 0x400000) >> 10).
pub fn engine_voices_8002ffac(m: &mut Machine, params: u32) {
    let q = |offset: u32| params.wrapping_add(offset);
    if m.lbu(ENGINE_SOUND) == 0 {
        for i in 0..3 {
            let id = m.lbu(ENGINE_SOUND + 4 + i);
            let voice = external(m, 0x8002_db4c, [u32::from(id), 0, 0, 0]);
            m.sb(ENGINE_SOUND + 1 + i, voice as u8);
        }
        let started = m.lbu(ENGINE_SOUND);
        m.sb(ENGINE_SOUND, started.wrapping_add(1));
    }
    let sext16 = |value: u32| value as u16 as i16 as i32 as u32;
    let sound = |m: &mut Machine, slot: u32| {
        let id = u32::from(m.lbu(ENGINE_SOUND + 4 + slot));
        SOUND_TABLE + id * 20
    };
    let voice = u32::from(m.lbu(ENGINE_SOUND + 3));
    if voice != 0xff {
        let entry = sound(m, 2);
        let raw = i32::from(m.lhu(q(8)) as i16);
        let base = i32::from(m.lh(entry + 8)) as u32;
        let pitch = pitch_add_8002e230(base, (raw >> 4) as u32);
        set_voice_pitch_8002d9fc(m, voice, pitch & 0xffff);
        let offset = (raw >> 2) as u32;
        let left = sext16(u32::from(m.lhu(entry + 4)).wrapping_add(offset));
        let right = sext16(u32::from(m.lhu(entry + 6)).wrapping_add(offset));
        crate::shared::f_8002daa0(m, [voice, left, right, offset]);
    }
    let voice = u32::from(m.lbu(ENGINE_SOUND + 2));
    if voice != 0xff {
        let entry = sound(m, 1);
        let speed = m.lw(q(0)).wrapping_add(0xfffd_c000);
        let base = i32::from(m.lh(entry + 8)) as u32;
        let pitch = pitch_add_8002e230(base, ((speed << 9) as i32 >> 16) as u32);
        set_voice_pitch_8002d9fc(m, voice, pitch & 0xffff);
        let offset = (i32::from(m.lh(q(0xa))) as u32) << 2;
        let left = sext16(u32::from(m.lhu(entry + 4)).wrapping_sub(offset));
        let right = sext16(u32::from(m.lhu(entry + 6)).wrapping_add(offset));
        crate::shared::f_8002daa0(m, [voice, left, right, offset]);
    }
    let voice = u32::from(m.lbu(ENGINE_SOUND + 1));
    if voice != 0xff {
        let entry = sound(m, 0);
        let turn = i32::from(m.lh(q(0xc))).unsigned_abs();
        let base = i32::from(m.lh(entry + 8)) as u32;
        let pitch = pitch_add_8002e230(base, ((turn << 18) as i32 >> 16) as u32);
        set_voice_pitch_8002d9fc(m, voice, pitch & 0xffff);
        let turn = i32::from(m.lh(q(0xc))).unsigned_abs();
        let height = (m.lw(q(4)).wrapping_add(0xffe0_0000) as i32).clamp(0, 0x40_0000);
        let offset = (turn << 3)
            .wrapping_add(0x1000)
            .wrapping_sub((height >> 10) as u32);
        let left = sext16(u32::from(m.lhu(entry + 4)).wrapping_add(offset));
        let right = sext16(u32::from(m.lhu(entry + 6)).wrapping_add(offset));
        crate::shared::f_8002daa0(m, [voice, left, right, offset]);
    }
}

/// 8002E64C: `pitch` in note/fine form (note in bits 8..14, fine 0..127 in
/// bits 0..6) plus `delta` fine steps, carried into the note; returns the
/// result sign-extended from 16 bits.
pub fn pitch_add_8002e230(pitch: u32, delta: u32) -> u32 {
    let linear = ((pitch & 0xff00) >> 1) | (pitch & 0x7f);
    let sum = delta.wrapping_add(linear);
    let packed = ((sum & 0x7f80) << 1) | (sum & 0x7f);
    packed as u16 as i16 as i32 as u32
}

/// 8002DE24: pitch of SPU voice `voice` (voice table 800C5830, +0xC) with
/// the update flag 0x10 in +1. Returns `voice`, or 0xFF (nothing stored)
/// when `voice` >= 24 (unsigned).
pub fn set_voice_pitch_8002d9fc(m: &mut Machine, voice: u32, pitch: u32) -> u32 {
    if voice >= 0x18 {
        return 0xff;
    }
    let entry = VOICE_TABLE + voice * 20;
    let flags = m.lbu(entry + 1);
    m.sh(entry + 0xc, pitch as u16);
    m.sb(entry + 1, flags | 0x10);
    voice
}

/// 8003A624: gun override for 80072178; always -1 (none).
pub fn gun_override_8003a0f0() -> u32 {
    0xffff_ffff
}

/// 800718E0: count down the twelve reload timers (bytes) at state +0x58 and
/// the twelve at +0x64 (`[gp+0x520]` reloaded for each byte).
pub fn tick_reload_timers_8007025c(m: &mut Machine) {
    for base in [0x58, 0x64] {
        for i in 0..12 {
            let state = m.gw(0x520);
            let timer = state.wrapping_add(i).wrapping_add(base);
            let value = m.lbu(timer);
            if value != 0 {
                m.sb(timer, value - 1);
            }
        }
    }
}

/// 800718A0: over the `count` (signed; <= 0: none) bytes at `buffer`, store
/// `value` into the first zero byte and 0 into the later zero bytes.
/// Returns the number of zero bytes.
pub fn fill_free_slots_8007021c(m: &mut Machine, buffer: u32, count: u32, value: u32) -> u32 {
    let mut value = value;
    let mut free = 0;
    if count as i32 <= 0 {
        return 0;
    }
    for i in 0..count {
        let slot = buffer.wrapping_add(i);
        if m.lbu(slot) == 0 {
            m.sb(slot, value as u8);
            value = 0;
            free += 1;
        }
    }
    free
}

// ---- end of group B ------------------------------------------------------------

// ---- group C: terrain ray, orientation and camera aim ----------------------------
// 8009A5D0 -> 80099894 -> 80099428 / 80099750; 80030F64, 80031BB4, 8006EE38.

/// `[gp+0x54C]`/`[gp+0x550]`: normal address and flags word of the terrain
/// face last hit by 80099894.
const GROUND_HIT_NORMAL: u32 = 0x54c;
const GROUND_HIT_FLAGS: u32 = 0x550;

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

/// Inline `MulMatrix0` column step (80099428, 80099894): IR1..IR3 = the
/// column halfwords zero-extended (`lhu`), two NOPs, MVMVA sf=1 lm=0
/// rotation x IR (0x049E012), then IR1..IR3 read back (`mfc2`).
fn rotate_column(m: &mut Machine, column: [u16; 3]) -> [u32; 3] {
    for (i, value) in column.into_iter().enumerate() {
        m.gte.mtc2(reg::IR1 + i, u32::from(value));
    }
    m.gte.mvmva(true, false, 0, 3, 3);
    std::array::from_fn(|i| m.gte.mfc2(reg::IR1 + i))
}

/// 80099428's product step: `matrix` (MATRIX rotation halfwords) =
/// `matrix` x `factor` (row-major). The rotation registers are loaded from
/// `matrix` (inline SetRotMatrix group), then each column of `factor` goes
/// through [`rotate_column`] into the same column of `matrix`.
fn multiply_columns(m: &mut Machine, matrix: u32, factor: [u16; 9]) {
    crate::sdk::set_rot_matrix(m, matrix);
    for column in 0..3 {
        let ir = rotate_column(m, [factor[column], factor[column + 3], factor[column + 6]]);
        let at = matrix.wrapping_add(column as u32 * 2);
        for (row, value) in ir.into_iter().enumerate() {
            m.sh(at.wrapping_add(row as u32 * 6), value as u16);
        }
    }
}

/// 80099428: rotation part of `matrix` (MATRIX) for the unit SVECTOR
/// `direction`: identity, times a rotation about X from the direction's y
/// and its horizontal length l = |(x, 0, z)| (80083F40), then, when l != 0,
/// times a rotation about Y from (x << 12) / l and (z << 12) / l. The
/// factor matrices are locals of the original (sp+0x10, used only by inline
/// GTE code); the products are formed on the GTE (MVMVA sf=1). `v0` is
/// unused by both callers.
pub fn direction_matrix_80097a70(m: &mut Machine, matrix: u32, direction: u32) {
    const ONE: u16 = 0x1000;
    for (i, value) in [ONE, 0, 0, 0, ONE, 0, 0, 0, ONE].into_iter().enumerate() {
        m.sh(matrix.wrapping_add(i as u32 * 2), value);
    }
    let x = i32::from(m.lh(direction)) as u32;
    let z = i32::from(m.lh(direction.wrapping_add(4))) as u32;
    let horizontal = vector_length_800828f8(m, x, 0, z);
    let y = i32::from(m.lh(direction.wrapping_add(2))) as u32;
    let (l, minus_y, y) = (horizontal as u16, 0u32.wrapping_sub(y) as u16, y as u16);
    multiply_columns(m, matrix, [ONE, 0, 0, 0, l, minus_y, 0, y, l]);
    if horizontal == 0 {
        return;
    }
    let x = i32::from(m.lh(direction)) as u32;
    let sin = div_quotient(x << 12, horizontal);
    let z = i32::from(m.lh(direction.wrapping_add(4))) as u32;
    let cos = div_quotient(z << 12, horizontal);
    let (sin, minus_sin, cos) = (sin as u16, 0u32.wrapping_sub(sin) as u16, cos as u16);
    multiply_columns(m, matrix, [cos, 0, minus_sin, 0, ONE, 0, sin, 0, cos]);
}

/// 80099750's vertex load: the halfword pair at `xy` and the halfword at `z`
/// plus the offset `delta`, packed as the VXY/VZ words (`addu`, `andi`,
/// `sll`, `or`; VZ keeps the full 32-bit sum).
fn offset_vertex(m: &mut Machine, xy: u32, z: u32, delta: [u32; 3]) -> (u32, u32) {
    let x = (i32::from(m.lh(xy)) as u32).wrapping_add(delta[0]) & 0xffff;
    let y = (i32::from(m.lh(xy.wrapping_add(2))) as u32).wrapping_add(delta[1]);
    let z = (i32::from(m.lh(z)) as u32).wrapping_add(delta[2]);
    (x | y << 16, z)
}

/// 80099750's store: IR1..IR3 (`mfc2`) as halfwords at `out` +0/+2/+4.
fn store_ir(m: &mut Machine, out: u32) {
    let ir: [u32; 3] = std::array::from_fn(|i| m.gte.mfc2(reg::IR1 + i));
    for (i, value) in ir.into_iter().enumerate() {
        m.sh(out.wrapping_add(i as u32 * 2), value as u16);
    }
}

/// 80099750: rotate a terrain block's vertices into `out`. `vertices` is a
/// count word followed by groups of three vertices (0x14 bytes: x/y
/// halfword pairs at +0/+4/+8, z halfwords at +0xC/+0xE/+0x10). Each vertex
/// plus the halfword offset at `offset` (+0/+2/+4) goes through MVMVA sf=1
/// (current rotation, no translation, V0/V1/V2 in turn) and IR1..IR3 are
/// stored as halfwords into consecutive 8-byte entries. The loop is a
/// do-while on the count minus 3 per group (signed), and always loads the
/// following group's first vertex into V0. `v0` is unused by the callers.
pub fn transform_vertices_80097d98(m: &mut Machine, vertices: u32, offset: u32, out: u32) {
    let delta: [u32; 3] =
        std::array::from_fn(|i| i32::from(m.lh(offset.wrapping_add(i as u32 * 2))) as u32);
    let mut count = m.lw(vertices);
    let mut group = vertices.wrapping_add(4);
    let mut out = out;
    let (xy, z) = offset_vertex(m, group, group.wrapping_add(0xc), delta);
    m.gte.mtc2(reg::VXY0, xy);
    m.gte.mtc2(reg::VZ0, z);
    loop {
        m.gte.mvmva(true, false, 0, 0, 3);
        let (xy, z) = offset_vertex(m, group.wrapping_add(4), group.wrapping_add(0xe), delta);
        m.gte.mtc2(reg::VXY1, xy);
        m.gte.mtc2(reg::VZ1, z);
        store_ir(m, out);
        out = out.wrapping_add(8);
        m.gte.mvmva(true, false, 0, 1, 3);
        let (xy, z) = offset_vertex(m, group.wrapping_add(8), group.wrapping_add(0x10), delta);
        m.gte.mtc2(reg::VXY2, xy);
        m.gte.mtc2(reg::VZ2, z);
        store_ir(m, out);
        out = out.wrapping_add(8);
        m.gte.mvmva(true, false, 0, 2, 3);
        group = group.wrapping_add(0x14);
        count = count.wrapping_sub(3);
        let (xy, z) = offset_vertex(m, group, group.wrapping_add(0xc), delta);
        m.gte.mtc2(reg::VXY0, xy);
        m.gte.mtc2(reg::VZ0, z);
        store_ir(m, out);
        out = out.wrapping_add(8);
        if count as i32 <= 0 {
            break;
        }
    }
}

/// Search state of 80099894: nearest distance (s1), ray length (s4), and
/// the nearest face's normal address (s6) and flags (s5).
struct NearestFace {
    distance: u32,
    limit: u32,
    normal: u32,
    flags: u32,
}

impl NearestFace {}

/// NCLIP and its MAC0 (signed): the side of the ray (SXY2 = 0) relative to
/// the edge SXY0-SXY1.
fn nclip_side(m: &mut Machine) -> i32 {
    m.gte.nclip();
    m.gte.mfc2(reg::MAC0) as i32
}

// JP-REVIEW(R0295): changed function: PAL 80097EDC -> JP 80099894, del:317 imm:8 ins:355 op:9 reg:67, 756 instructions differ (whole routine)
/// 80099894: nearest terrain face hit by the ray from the origin along the
/// VECTOR `delta` (its low halfwords; caller units >> 12), not beyond its
/// length. `list` (the caller's stack word +0x10, a fifth argument) holds
/// 12-byte entries {x, y, z offset halfwords; +8 word: block index in bits
/// 0..8, 0x200000 ends the list}; `blocks` is the block pointer table. For
/// each entry the block's vertices (block +4) are rotated into ray space
/// into `work` (80099750), then the faces of the lists at block +0x10
/// (0x30-byte faces) and +0xC (0x24-byte faces) are intersected: the ray
/// is the z axis (80099428 matrix) and the screen origin SXY2 = 0 is
/// edge-tested with NCLIP. Returns the hit distance (0x7FFFFFFF when none);
/// `out` (VECTOR) = the normalized direction x distance, or zero when the
/// distance exceeds 0x7FFF. A hit stores the face's normal address and
/// flags at `[gp+0x54C]`/`[gp+0x550]`; otherwise `[gp+0x54C]` = 0.
// JP-PATCH: replaced by `crate::jp::res_p1_player::ground_hit_80097edc` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn ground_hit_80097edc(
    m: &mut Machine,
    delta: u32,
    out: u32,
    work: u32,
    blocks: u32,
    list: u32,
) -> u32 {
    crate::jp::res_p1_player::ground_hit_80097edc(m, delta, out, work, blocks, list)
}

/// 8009A5D0: terrain ray from VECTOR `from` to VECTOR `to` (world units,
/// 1/4096). The terrain cell under `from` (256 x 256 halfword map at
/// `[[gp+0x2E0]+0x248]`, cells of 0x800000; indices wrap into 0..7 when
/// negative and 0xF8..0xFF when >= 0x100) gives a block index (bits 0..8).
/// Index 0: `out` = `from`, returns 0x7FFFFFFF. Otherwise a one-entry list
/// {0x400 - (from.x >> 12 & 0x7FF), -from.y >> 12, 0x400 - (from.z >> 12 &
/// 0x7FF), index} goes to 80099894 with the ray (to - from) >> 12, the
/// block table `[[gp+0x2E0]+0x24C]` and `work`; `out` = its hit point +
/// `from`, and its result (the hit distance) is returned.
pub fn ground_ray_80098b80(m: &mut Machine, from: u32, to: u32, out: u32, work: u32) -> u32 {
    const SIZE: u32 = 0x70;
    let sp = m.push_frame(SIZE);
    let terrain = m.gw(crate::common::GAMEPLAY).wrapping_add(0x244);
    let x = m.lw(from) as i32;
    let cells = m.lw(terrain.wrapping_add(4));
    m.sw(sp.wrapping_add(0x18), (x >> 12) as u32);
    let y = m.lw(from.wrapping_add(4)) as i32;
    m.sw(sp.wrapping_add(0x1c), (y >> 12) as u32);
    let z = m.lw(from.wrapping_add(8)) as i32;
    m.sw(sp.wrapping_add(0x20), (z >> 12) as u32);
    // Dead stores of the original: from >> 12 at +0x28, to >> 12 at +0x38.
    for (base, offset) in [(from, 0x28), (to, 0x38)] {
        for i in 0..3 {
            let value = m.lw(base.wrapping_add(i * 4)) as i32 >> 12;
            m.sw(sp.wrapping_add(offset + i * 4), value as u32);
        }
    }
    let fine_x = (x >> 12) as u32 & 0x7ff;
    let fine_z = (z >> 12) as u32 & 0x7ff;
    let mut cell_x = (x >> 23).wrapping_add(0x80);
    let mut cell_z = (z >> 23).wrapping_add(0x80);
    if cell_x < 0 {
        cell_x &= 7;
    }
    if cell_z < 0 {
        cell_z &= 7;
    }
    if cell_x as i16 >= 0x100 {
        cell_x = (cell_x & 7) | 0xf8;
    }
    if cell_z as i16 >= 0x100 {
        cell_z = (cell_z & 7) | 0xf8;
    }
    let row = (i32::from(cell_z as i16) << 9) as u32;
    let column = (i32::from(cell_x as i16) << 1) as u32;
    let index = m.lh(cells.wrapping_add(column).wrapping_add(row)) as u32 & 0x1ff;
    let result = if index == 0 {
        for i in 0..3 {
            let value = m.lw(from.wrapping_add(i * 4));
            m.sw(out.wrapping_add(i * 4), value);
        }
        0x7fff_ffff
    } else {
        let list = sp.wrapping_add(0x48);
        m.sh(list, 0x400u32.wrapping_sub(fine_x) as u16);
        let y = m.lw(from.wrapping_add(4));
        m.sh(list.wrapping_add(4), 0x400u32.wrapping_sub(fine_z) as u16);
        m.sw(list.wrapping_add(8), index);
        m.sh(
            list.wrapping_add(2),
            (0u32.wrapping_sub(y) as i32 >> 12) as u16,
        );
        m.sw(sp.wrapping_add(0x5c), 0x20_0000);
        for i in 0..3 {
            let target = m.lw(to.wrapping_add(i * 4));
            let origin = m.lw(from.wrapping_add(i * 4));
            let delta = target.wrapping_sub(origin) as i32 >> 12;
            m.sw(sp.wrapping_add(0x18 + i * 4), delta as u32);
        }
        m.sw(sp.wrapping_add(0x10), list);
        let blocks = m.lw(terrain.wrapping_add(8));
        let distance = ground_hit_80097edc(m, sp.wrapping_add(0x18), out, work, blocks, list);
        for i in 0..3 {
            let hit = m.lw(out.wrapping_add(i * 4));
            let origin = m.lw(from.wrapping_add(i * 4));
            m.sw(out.wrapping_add(i * 4), hit.wrapping_add(origin));
        }
        distance
    };
    m.pop_frame(SIZE);
    result
}

/// 80030F64/80031BB4 angle wrap: a pitch beyond +-0x1000 is mirrored
/// (0x2000 - p or -0x2000 - p) with 0x2000 added to the other angles, then
/// each other angle is wrapped into -0x2000..0x1FFF (signed compares,
/// wrapping adds on the full registers).
fn wrap_angles(mut pitch: u32, others: &mut [u32]) -> u32 {
    if pitch as i32 >= 0x1001 {
        pitch = 0x2000u32.wrapping_sub(pitch);
        others.iter_mut().for_each(|a| *a = a.wrapping_add(0x2000));
    }
    if (pitch as i32) < -0x1000 {
        pitch = 0xffff_e000u32.wrapping_sub(pitch);
        others.iter_mut().for_each(|a| *a = a.wrapping_add(0x2000));
    }
    for angle in others {
        if *angle as i32 >= 0x2000 {
            *angle = angle.wrapping_sub(0x4000);
        }
        if (*angle as i32) < -0x2000 {
            *angle = angle.wrapping_add(0x4000);
        }
    }
    pitch
}

/// Arithmetic shift of a guest word.
fn sra(value: u32, shift: u32) -> u32 {
    ((value as i32) >> shift) as u32
}

/// 80030F64: turn the orientation SVECTOR `angles` (in/out) by the angle
/// delta SVECTOR `delta`. Near vertical (|angles.x| >= 0xF89..0xFE1) small
/// delta x/y components are doubled (up to three times). The two VECTORs at
/// 800234D8/800234E8 are rotated by the delta matrix (80030A68) and then by
/// the current orientation matrix (80030A68 of `angles`, ApplyRotMatrixLV);
/// the new angles are x = 80030EB8(-a.y >> 4), y = -80030D54(-a.x, a.z) and
/// z = 80030D54(b.x, -b.y), where b is scaled by 1/16, rotated by the
/// 80030BDC matrix of (-x, 80030D54(-a.x, a.z), 0) and scaled by 1/4; then
/// [`wrap_angles`]. The second parameter was named `out` in the skeleton;
/// it is an input.
pub fn orientation_80030d20(m: &mut Machine, angles: u32, delta: u32) {
    const SIZE: u32 = 0x98;
    let sp = m.push_frame(SIZE);
    let matrix = sp.wrapping_add(0x10);
    let turn = sp.wrapping_add(0x30);
    let axes = [sp.wrapping_add(0x38), sp.wrapping_add(0x48)];
    let rotated = [sp.wrapping_add(0x58), sp.wrapping_add(0x68)];
    for (source, target) in [0x8002_34d8u32, 0x8002_34e8].into_iter().zip(axes) {
        for i in 0..4 {
            let word = m.lw(source + i * 4);
            m.sw(target.wrapping_add(i * 4), word);
        }
    }
    for i in 0..3 {
        let half = m.lhu(delta.wrapping_add(i * 2));
        m.sh(turn.wrapping_add(i * 2), half);
    }
    for (threshold, slot, limit) in [
        (0xf91, 0, 8),
        (0xf89, 2, 8),
        (0xfc9, 0, 0xc),
        (0xfc1, 2, 0xc),
        (0xfe1, 0, 0x10),
        (0xfdb, 2, 0x10),
    ] {
        if i32::from(m.lh(angles)).abs() >= threshold {
            let value = m.lh(turn.wrapping_add(slot));
            if i32::from(value).abs() < limit {
                m.sh(turn.wrapping_add(slot), (value as u16) << 1);
            }
        }
    }
    crate::shared::f_80030824(m, [turn, matrix, 0, 0]);
    crate::sdk::set_rot_matrix(m, matrix);
    crate::sdk::apply_rot_matrix_lv(m, axes[0], rotated[0]);
    crate::sdk::apply_rot_matrix_lv(m, axes[1], rotated[1]);
    crate::shared::f_80030824(m, [angles, matrix, 0, 0]);
    crate::sdk::set_rot_matrix(m, matrix);
    crate::sdk::apply_rot_matrix_lv(m, rotated[0], rotated[0]);
    crate::sdk::apply_rot_matrix_lv(m, rotated[1], rotated[1]);
    let [a, b] = rotated;
    let a_y = m.lw(a.wrapping_add(4));
    let b_x = m.lw(b);
    let b_z = m.lw(b.wrapping_add(8));
    m.sw(b, sra(b_x, 4));
    let b_y = m.lw(b.wrapping_add(4));
    m.sw(b.wrapping_add(8), sra(b_z, 4));
    m.sw(b.wrapping_add(4), sra(b_y, 4));
    let pitch = crate::shared::f_80030c74(m, [sra(0u32.wrapping_sub(a_y), 4), 0, 0, 0]);
    let a_x = m.lw(a);
    let a_z = m.lw(a.wrapping_add(8));
    let heading = crate::shared::f_80030b10(m, [0u32.wrapping_sub(a_x), a_z, 0, 0]);
    let yaw = 0u32.wrapping_sub(heading);
    m.sh(turn, 0u32.wrapping_sub(pitch) as u16);
    m.sh(turn.wrapping_add(2), heading as u16);
    m.sh(turn.wrapping_add(4), 0);
    crate::shared::f_80030998(m, [turn, matrix, 0, 0]);
    crate::sdk::set_rot_matrix(m, matrix);
    crate::sdk::apply_rot_matrix_lv(m, b, b);
    let b_x = sra(m.lw(b), 2);
    let b_y = sra(m.lw(b.wrapping_add(4)), 2);
    let b_z = sra(m.lw(b.wrapping_add(8)), 2);
    m.sw(b, b_x);
    m.sw(b.wrapping_add(4), b_y);
    m.sw(b.wrapping_add(8), b_z);
    let roll = crate::shared::f_80030b10(m, [b_x, 0u32.wrapping_sub(b_y), 0, 0]);
    let mut others = [yaw, roll];
    let pitch = wrap_angles(pitch, &mut others);
    m.sh(angles, pitch as u16);
    m.sh(angles.wrapping_add(2), others[0] as u16);
    m.sh(angles.wrapping_add(4), others[1] as u16);
    m.pop_frame(SIZE);
}

/// 80031BB4: angles SVECTOR `out` = {pitch, yaw, 0} of the VECTOR `vector`
/// (x, y, z words): pitch = 80030D54(-y, h) with h = 0x100000 when |x| +
/// |z| > 0xAFFFF, otherwise the horizontal length (80083F40 of x, z;
/// computed on x >> 4, z >> 4 and shifted back when |x| + |z| > 0xB000);
/// yaw = 80030D54(x, z); wrapped by [`wrap_angles`].
pub fn vector_angles_80031970(m: &mut Machine, vector: u32, out: u32) {
    let x = m.lw(vector);
    let z = m.lw(vector.wrapping_add(8));
    let y = m.lw(vector.wrapping_add(4));
    let magnitude = |v: u32| {
        if (v as i32) < 0 {
            0u32.wrapping_sub(v)
        } else {
            v
        }
    };
    let spread = magnitude(x).wrapping_add(magnitude(z)) as i32;
    let horizontal = if spread > 0xa_ffff {
        0x10_0000
    } else {
        let (a0, a2, shift) = if spread > 0xb000 {
            (sra(x, 4), sra(z, 4), 4)
        } else {
            (x, z, 0)
        };
        vector_length_800828f8(m, a0, 0, a2) << shift
    };
    let pitch = crate::shared::f_80030b10(m, [0u32.wrapping_sub(y), horizontal, 0, 0]);
    let yaw = crate::shared::f_80030b10(m, [x, z, 0, 0]);
    let mut others = [yaw];
    let pitch = wrap_angles(pitch, &mut others);
    m.sh(out, pitch as u16);
    m.sh(out.wrapping_add(2), others[0] as u16);
    m.sh(out.wrapping_add(4), 0);
}

/// 8006EE38: aim angles of `view` (+0x38 pitch, +0x3A yaw, +0x3C = 0) from
/// the SVECTOR at 800BCE0C rotated by matrix(800308F0, `camera` +0x18) x
/// matrix(80030778, `view` +0x40) (MulMatrix, then MVMVA sf=1 with the
/// product as rotation, no translation): pitch = 80030D54(-r.y, r.z), yaw =
/// 80030D54(r.x, r.z). Callers ignore `v0` (the yaw); tail callers that
/// return it are themselves called without using `v0`.
pub fn aim_camera_8006d7dc(m: &mut Machine, camera: u32, view: u32) {
    // One implementation: shared.rs owns 8006EE38 (it also returns the yaw).
    crate::dispatch::call(m, 0x8006_ee38, [camera, view, 0, 0]);
}

// ---- end of group C -------------------------------------------------------------

// ---- group D: player flight model (8006EF30, 8006EA88, 8006E908) ----------------

/// 800104AC: resident sine table (halfwords, 1.0 = 0x1000). Entry
/// `angle & 0x3FFF` is the sine, entry `(angle & 0x3FFF) + 0x1000` the cosine.
const SINE_TABLE: u32 = 0x8001_04ac;

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

/// `value / 4096` rounded toward zero (`bgez; addiu 0xFFF; sra 12`).
fn fm_round12(value: i32) -> i32 {
    if value < 0 {
        value.wrapping_add(0xfff) >> 12
    } else {
        value >> 12
    }
}

/// Low word of `a * b` (`mult; mflo`) divided by 4096 toward zero.
fn fm_mul12(a: i32, b: i32) -> i32 {
    fm_round12(a.wrapping_mul(b))
}

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

/// Add the VECTOR at `delta` to the VECTOR at `sum` (wrapping words).
fn fm_accumulate(m: &mut Machine, sum: u32, delta: u32) {
    for i in 0..3u32 {
        let a = m.lw(sum.wrapping_add(i * 4));
        let b = m.lw(delta.wrapping_add(i * 4));
        m.sw(sum.wrapping_add(i * 4), a.wrapping_add(b));
    }
}

/// 8006E908: speed change of one flight step. `speed` is clamped to
/// `0..=max_speed` and mapped to a fraction: `(speed << 12) / cruise_speed / 2`
/// up to `cruise_speed`, `0x800 + ((speed - cruise) << 12) / (max - cruise) / 2`
/// above it, clamped to +-0x1000. The throttle target
/// `(throttle + 0x1000) >> 1` minus that fraction is scaled by `gain_up`
/// (positive difference) or `gain_down` (>> 12 toward zero), then by the
/// altitude factor `0x1000 + 2867 * max(altitude >> 12, -54822) / 54822`
/// (a reciprocal multiply by 0x4C81F747 >> 46). Arguments 5..7 are the
/// caller's stack words +0x10/+0x14/+0x18. Returns the change (`v0`).
pub fn speed_change_8006d2ac(
    speed: u32,
    max_speed: u32,
    cruise_speed: u32,
    throttle: u32,
    altitude: u32,
    gain_up: u32,
    gain_down: u32,
) -> u32 {
    let max_speed = max_speed as i32;
    let cruise = cruise_speed as i32;
    let mut speed = speed as i32;
    if speed < 0 {
        speed = 0;
    }
    if max_speed < speed {
        speed = max_speed;
    }
    let mut fraction = if cruise < speed {
        let above = fm_div(
            speed.wrapping_sub(cruise) << 12,
            max_speed.wrapping_sub(cruise),
        );
        (above >> 1).wrapping_add(0x800)
    } else {
        fm_div(speed << 12, cruise) >> 1
    };
    if fraction >= 0x1001 {
        fraction = 0x1000;
    }
    if fraction < -0x1000 {
        fraction = -0x1000;
    }
    let difference = ((throttle as i32).wrapping_add(0x1000) >> 1).wrapping_sub(fraction);
    let gain = if difference > 0 {
        gain_up as i32
    } else {
        gain_down as i32
    };
    let change = fm_mul12(gain, difference);
    let mut height = (altitude as i32) >> 12;
    if height < -0xd626 {
        height = -0xd626;
    }
    // 2867 * height (built from shifts in the original), then the signed
    // quotient by 54822: HI of the product with 0x4C81F747, >> 14, minus the
    // sign word.
    let scaled = height.wrapping_mul(2867);
    let high = ((i64::from(scaled) * 0x4c81_f747) >> 32) as i32;
    let quotient = (high >> 14).wrapping_sub(scaled >> 31);
    fm_mul12(change, 0x1000_i32.wrapping_add(quotient)) as u32
}

/// 8006EA88: altitude-dependent flight limits from the aircraft table
/// `params` (+0x2C ceiling, negative, in altitude >> 12 units; +0x30..+0x44
/// speeds). `altitude` (entity word +0xC) >> 12 is clamped to
/// `[ceiling, 0]`; two sine-table curves over the clamped height give the
/// lift reference speed (stored at `lift_out`, a3), the cruise speed
/// (`cruise_out`, caller's stack word +0x10) and the top speed (`max_out`,
/// stack word +0x14; capped by the lift speed + `[+0x40] - [+0x38]`).
/// Returns the air density factor `clamp(0x1000 - q, 0, 0x1000)` where `q`
/// rises from 0 at three quarters of the ceiling to 0x1000 at the ceiling.
/// `speed` (a1) is clamped to >= 0 and then overwritten unused.
pub fn altitude_limits_8006d42c(
    m: &mut Machine,
    altitude: u32,
    speed: u32,
    params: u32,
    lift_out: u32,
    cruise_out: u32,
    max_out: u32,
) -> u32 {
    let _ = speed;
    let ceiling = m.lw(params.wrapping_add(0x2c)) as i32;
    let p30 = m.lw(params.wrapping_add(0x30)) as i32;
    let p34 = m.lw(params.wrapping_add(0x34)) as i32;
    let p38 = m.lw(params.wrapping_add(0x38)) as i32;
    let p3c = m.lw(params.wrapping_add(0x3c)) as i32;
    let p40 = m.lw(params.wrapping_add(0x40)) as i32;
    let p44 = m.lw(params.wrapping_add(0x44)) as i32;
    let mut height = (altitude as i32) >> 12;
    if height > 0 {
        height = 0;
    }
    if height < ceiling {
        height = ceiling;
    }
    let low_curve = fm_div((height.wrapping_sub(ceiling) << 14).wrapping_neg(), ceiling);
    let span = p3c.wrapping_sub(p34);
    let span_part = span.wrapping_mul(low_curve);
    let high_curve = fm_div(height << 14, ceiling);
    let low_index = (low_curve.wrapping_mul(5) >> 2) & 0x7ffe;
    let low_sine = i32::from(m.lh(SINE_TABLE.wrapping_add(low_index as u32)));
    let low_wave = low_sine.wrapping_mul(span >> 2);
    let high_sine = i32::from(m.lh(SINE_TABLE.wrapping_add((high_curve & 0x7ffe) as u32)));
    let quarter = p30.wrapping_sub(p38) >> 2;
    let high_wave = high_sine.wrapping_mul(quarter);
    let high_part = quarter.wrapping_mul(high_curve);
    let mut top = (span_part >> 14)
        .wrapping_add(p34)
        .wrapping_add(low_wave >> 13);
    let lift = (high_part >> 12)
        .wrapping_add(p38)
        .wrapping_sub(high_wave >> 14);
    let cap = lift.wrapping_add(p40.wrapping_sub(p38));
    if cap < top {
        top = cap;
    }
    let quarter_ceiling = ceiling >> 2;
    let thin = fm_div(
        height.wrapping_sub(ceiling.wrapping_sub(quarter_ceiling)) << 12,
        quarter_ceiling,
    );
    let middle = (p30.wrapping_add(p34) >> 1).wrapping_sub(p44);
    let cruise = ((middle >> 2).wrapping_mul(high_curve) >> 12).wrapping_add(p44);
    m.sw(lift_out, lift as u32);
    m.sw(max_out, top as u32);
    m.sw(cruise_out, cruise as u32);
    let mut density = 0x1000_i32.wrapping_sub(thin);
    if density >= 0x1001 {
        density = 0x1000;
    }
    if density < 0 {
        density = 0;
    }
    density as u32
}

// JP-REVIEW(R0297): near function: PAL 8006D8D4 -> JP 8006EF30, del:8 ins:18 op:10 reg:26, 62 instructions differ (whole routine)
/// 8006EF30: player flight model, one 25 Hz step (called by 800729DC).
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
// JP-PATCH: replaced by `crate::jp::res_p1_player::flight_model_8006d8d4` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn flight_model_8006d8d4(m: &mut Machine, a0: u32, a1: u32, a2: u32, a3: u32) {
    crate::jp::res_p1_player::flight_model_8006d8d4(m, a0, a1, a2, a3)
}
// ---- end of group D ----------------------------------------------------------

// ==== d2: free flight model 800702D8 (800729DC modes 0x1000000..0x10000000) ====

/// 800702D8: free flight model, the variant of [`flight_model_8006d8d4`] that
/// 800729DC runs in modes 0x1000000, 0x2000000, 0x4000000, 0x8000000 and
/// 0x10000000: no lift, gravity, power, stall or altitude limits. Same
/// arguments: `entity` (a0), `control` (a1, `[gp+0x518]`: sticks +4 pitch,
/// +6 yaw, +8 roll, +0xE throttle), `params` (a2, `[[gp+0x524]]`) and
/// `state` (a3, `[gp+0x520]`).
/// - Rates: target = stick x `[params+0/+8/+0xC]` >> 12 (toward zero); the
///   state rates +0x30..+0x34 plus the targets, clamped to +-those limits,
///   become the entity's angular velocity +0x30 (80030F64 turns the angles
///   +0x18 by it); the state keeps them decayed by 0xF7F/0x1000, but not
///   below the target, where they exceed it.
/// - `state+0x48` = 80083F40 length of (-velocity >> 6), << 6.
/// - Thrust (throttle x `[params+0x48]` >> 12, floor) along the body z axis
///   (80030778 matrix): none backward while the body-frame z speed
///   (800308F0 matrix, velocity >> 6) is negative, none forward above
///   `[params+0x3C]`. Velocity +0x38 += thrust + drag (-velocity >> 8), each
///   component clamped to +-0xB4000.
/// - Then 80031BB4 (velocity angles into `state+0x40`), 8006EE38 (camera)
///   and `state+0x4C` = 0x1000. Frame 0xC8 bytes (vector locals in RAM).
pub fn flight_model_free_8006ec54(m: &mut Machine, a0: u32, a1: u32, a2: u32, a3: u32) {
    const FRAME: u32 = 0xc8;
    let (entity, control, params, state) = (a0, a1, a2, a3);
    let at = |base: u32, offset: u32| base.wrapping_add(offset);
    let sp = m.push_frame(FRAME);
    let angles = at(entity, 0x18);
    let velocity = at(entity, 0x38);
    // sp+0x18: velocity change of the step.
    let accel = at(sp, 0x18);
    crate::sdk::memset(m, accel, 0, 0x10);

    let pitch_limit = m.lw(params) as i32;
    let pitch_stick = i32::from(m.lh(at(control, 4)));
    let yaw_stick = i32::from(m.lh(at(control, 6)));
    let roll_stick = i32::from(m.lh(at(control, 8)));
    let target_x = fm_mul12(pitch_stick, pitch_limit);
    let target_y = fm_mul12(yaw_stick, m.lw(at(params, 8)) as i32);
    let target_z = fm_mul12(roll_stick, m.lw(at(params, 0xc)) as i32);
    let rate_x = i32::from(m.lh(at(state, 0x30))).wrapping_add(target_x);
    let rate_z = i32::from(m.lh(at(state, 0x34))).wrapping_add(target_z);
    let rate_y = i32::from(m.lh(at(state, 0x32))).wrapping_add(target_y);
    let mut rate_x = fm_clamp_either(rate_x, pitch_limit);
    let yaw_limit = m.lw(at(params, 8)) as i32;
    let mut rate_y = fm_clamp_either(rate_y, yaw_limit);
    let roll_limit = m.lw(at(params, 0xc)) as i32;
    let mut rate_z = fm_clamp_either(rate_z, roll_limit);
    // sp+0x10..0x14 in the original: the clamped rates for the entity.
    let entity_rates = [rate_x as u16, rate_y as u16, rate_z as u16];
    for (rate, target) in [
        (&mut rate_x, target_x),
        (&mut rate_y, target_y),
        (&mut rate_z, target_z),
    ] {
        if fm_abs(target) < fm_abs(*rate) {
            *rate = fm_mul12(*rate, 0xf7f);
            if fm_abs(*rate) < fm_abs(target) {
                *rate = target;
            }
        }
    }
    m.sh(at(state, 0x30), rate_x as u16);
    m.sh(at(state, 0x32), rate_y as u16);
    m.sh(at(state, 0x34), rate_z as u16);
    for (i, rate) in entity_rates.into_iter().enumerate() {
        m.sh(at(entity, 0x30 + i as u32 * 2), rate);
    }
    orientation_80030d20(m, angles, at(entity, 0x30));

    let reversed: [u32; 3] =
        std::array::from_fn(|i| m.lw(at(velocity, i as u32 * 4)).wrapping_neg());
    let [x, y, z] = reversed.map(|v| ((v as i32) >> 6) as u32);
    let speed = vector_length_800828f8(m, x, y, z) << 6;

    // Body-frame velocity (velocity >> 6, not negated) for the thrust gates.
    crate::shared::f_800306ac(m, [angles, at(sp, 0x30), 0, 0]);
    crate::sdk::set_rot_matrix(m, at(sp, 0x30));
    for i in 0..3u32 {
        let v = m.lw(at(velocity, i * 4)) as i32;
        m.sh(at(sp, 0x28 + i * 2), (v >> 6) as u16);
    }
    fm_rotate(m, at(sp, 0x28), at(sp, 0x50));
    let throttle = i32::from(m.lh(at(control, 0xe)));
    let product = throttle.wrapping_mul(m.lw(at(params, 0x48)) as i32);
    let forward = (m.lw(at(sp, 0x58)) << 6) as i32;
    let mut thrust = product >> 12;
    if forward < 0 && thrust < 0 {
        thrust = 0;
    }
    if (m.lw(at(params, 0x3c)) as i32) < forward && thrust > 0 {
        thrust = 0;
    }

    // Thrust along the body z axis.
    m.sh(at(sp, 0x60), 0);
    m.sh(at(sp, 0x62), 0);
    m.sh(at(sp, 0x64), thrust as u16);
    crate::shared::f_80030534(m, [angles, at(sp, 0x68), 0, 0]);
    crate::sdk::set_rot_matrix(m, at(sp, 0x68));
    fm_rotate(m, at(sp, 0x60), at(sp, 0x88));
    fm_accumulate(m, accel, at(sp, 0x88));
    // Drag -velocity >> 8 (sp+0x98..0xA0 in the original).
    for i in 0..3u32 {
        let drag = (m.lw(at(velocity, i * 4)) as i32).wrapping_neg() >> 8;
        let sum = m.lw(at(accel, i * 4)).wrapping_add(drag as u32);
        m.sw(at(accel, i * 4), sum);
    }
    fm_accumulate(m, velocity, accel);
    for i in 0..3u32 {
        let address = at(velocity, i * 4);
        if 0xb_4000 < m.lw(address) as i32 {
            m.sw(address, 0xb_4000);
        }
        if (m.lw(address) as i32) < -0xb_4000 {
            m.sw(address, (-0xb_4000_i32) as u32);
        }
    }

    m.sw(at(state, 0x48), speed);
    vector_angles_80031970(m, velocity, at(state, 0x40));
    aim_camera_8006d7dc(m, entity, state);
    m.sh(at(state, 0x4c), 0x1000);
    m.pop_frame(FRAME);
}
// ==== end of d2 ====
// ==== e: autopilot control steps 8006D678, 80073B18, 8007407C and 8006E7E0 ====
//
// Siblings of 8006D0DC over the same blocks: `rates` (`[gp+0x514]`) +0
// pitch, +2 yaw, +4 roll, +6 throttle; `controls` (`[gp+0x51C]`) +0 roll,
// +2 pitch, +4 yaw, +6 power, +8 throttle lever; `out` (`[gp+0x518]`)
// +4/+6/+8/+0xE. Entity angles: +0x18 pitch, +0x1C roll (0x4000 per turn).

/// `value * 128 / 220` with the reciprocal multiply of 8006D678
/// (0x094F2095, shift 3, rounded toward zero).
fn e_decay_58_percent(value: i16) -> u16 {
    let scaled = i32::from(value) << 7;
    let high = ((i64::from(scaled) * 0x094f_2095) >> 32) as i32;
    ((high >> 3).wrapping_sub(scaled >> 31)) as u16
}

/// The throttle-rate decay of the control steps: `value * 128 / 140` with
/// the reciprocal 0xEA0EA0EB (high word plus the multiplicand, shift 7,
/// rounded toward zero).
fn e_decay_throttle(value: i16) -> u16 {
    let scaled = i32::from(value) << 7;
    let high = ((i64::from(scaled) * i64::from(0xea0e_a0ebu32 as i32)) >> 32) as i32;
    ((high.wrapping_add(scaled) >> 7).wrapping_sub(scaled >> 31)) as u16
}

/// Control targets and rate limits from the pad block (as 8006D0DC).
struct EPadTargets {
    pitch: i32,
    yaw: i32,
    roll: i32,
    throttle: i32,
    pitch_limit: i32,
    yaw_limit: i32,
    roll_limit: i32,
}

/// The target block of 8006D678 (identical to 8006D0DC): an analog axis
/// (mask +0x18 bits 1/4/2/8, values +0x1C/+0x24/+0x20/+0x28) gives the
/// target and its magnitude as the rate limit (pitch, yaw, roll); otherwise
/// the buttons (+0x08) give +-0x1000 with limit 0x1000.
fn e_pad_targets(m: &mut Machine, input: u32) -> EPadTargets {
    let mut t = EPadTargets {
        pitch: 0,
        yaw: 0,
        roll: 0,
        throttle: 0,
        pitch_limit: 0x1000,
        yaw_limit: 0x1000,
        roll_limit: 0x1000,
    };
    let buttons = m.lw(input.wrapping_add(8));
    if m.lw(input.wrapping_add(0x18)) & 1 != 0 {
        t.pitch = m.lw(input.wrapping_add(0x1c)) as i32;
        t.pitch_limit = magnitude(t.pitch);
    } else {
        if buttons & 0x0080_0000 != 0 {
            t.pitch = 0x1000;
        }
        if buttons & 0x0100_0000 != 0 {
            t.pitch = t.pitch.wrapping_sub(0x1000);
        }
    }
    if m.lw(input.wrapping_add(0x18)) & 4 != 0 {
        t.yaw = m.lw(input.wrapping_add(0x24)) as i32;
        t.yaw_limit = magnitude(t.yaw);
    } else {
        if buttons & 0x2000_0000 != 0 {
            t.yaw = t.yaw.wrapping_sub(t.yaw_limit);
        }
        if buttons & 0x4000_0000 != 0 {
            t.yaw = t.yaw.wrapping_add(t.yaw_limit);
        }
    }
    if m.lw(input.wrapping_add(0x18)) & 2 != 0 {
        t.roll = m.lw(input.wrapping_add(0x20)) as i32;
        t.roll_limit = magnitude(t.roll);
    } else {
        if buttons & 0x0200_0000 != 0 {
            t.roll = t.roll.wrapping_sub(t.roll_limit);
        }
        if buttons & 0x0400_0000 != 0 {
            t.roll = t.roll.wrapping_add(t.roll_limit);
        }
    }
    if m.lw(input.wrapping_add(0x18)) & 8 != 0 {
        t.throttle = m.lw(input.wrapping_add(0x28)) as i32;
    } else {
        if buttons & 0x0800_0000 != 0 {
            t.throttle = t.throttle.wrapping_add(0x1000);
        }
        if buttons & 0x1000_0000 != 0 {
            t.throttle = t.throttle.wrapping_sub(0x1000);
        }
    }
    t
}

/// The controls (+0 roll, +2 pitch, +4 yaw) approach `targets` (same
/// order) by `|delta| / 4 + 2`, then snap to them when closer than that.
fn e_approach_controls(m: &mut Machine, controls: u32, targets: [i32; 3]) {
    let mut steps = [0i32; 3];
    for (i, (&target, step)) in targets.iter().zip(steps.iter_mut()).enumerate() {
        let current = i32::from(m.lh(controls.wrapping_add(i as u32 * 2)));
        let delta = current.wrapping_sub(target);
        *step = if delta >= 0 {
            (delta >> 2).wrapping_add(2)
        } else {
            (target.wrapping_sub(current) >> 2).wrapping_add(2)
        };
    }
    for (i, (&target, &step)) in targets.iter().zip(steps.iter()).enumerate() {
        let at = controls.wrapping_add(i as u32 * 2);
        if target < i32::from(m.lh(at)) {
            let value = m.lhu(at);
            m.sh(at, value.wrapping_sub(step as u16));
        }
        if i32::from(m.lh(at)) < target {
            let value = m.lhu(at);
            m.sh(at, value.wrapping_add(step as u16));
        }
    }
    for (i, (&target, &step)) in targets.iter().zip(steps.iter()).enumerate() {
        let at = controls.wrapping_add(i as u32 * 2);
        let current = i32::from(m.lh(at));
        let delta = current.wrapping_sub(target);
        let distance = if delta < 0 {
            target.wrapping_sub(current)
        } else {
            delta
        };
        if distance < step {
            m.sh(at, target as u16);
        }
    }
}

/// The throttle lever (controls +8) moves by 0x80 towards `lever`.
fn e_move_lever(m: &mut Machine, controls: u32, lever: i32) {
    let at = controls.wrapping_add(8);
    if i32::from(m.lh(at)) < lever {
        let value = m.lhu(at);
        m.sh(at, value.wrapping_add(0x80));
    }
    if lever < i32::from(m.lh(at)) {
        let value = m.lhu(at);
        m.sh(at, value.wrapping_sub(0x80));
    }
}

/// Clamp the rates +0/+2/+4 to `+-limits` and the throttle rate +6 to
/// +-0x1000 (lower bound first, re-reading each halfword).
fn e_clamp_rates(m: &mut Machine, rates: u32, limits: [i32; 3]) {
    for (i, limit) in limits.into_iter().chain([0x1000]).enumerate() {
        let at = rates.wrapping_add(i as u32 * 2);
        let low = limit.wrapping_neg();
        if i32::from(m.lh(at)) < low {
            m.sh(at, low as u16);
        }
        if limit < i32::from(m.lh(at)) {
            m.sh(at, limit as u16);
        }
    }
}

/// Shared tail of 80073B18 and 8007407C for the targets `pitch`, `roll`
/// and `throttle` (yaw target 0): rates above `|target|` decay (64%,
/// throttle 91%; the yaw rate whenever it is not 0), the controls approach
/// the targets, the lever returns to 0, power = (throttle rate + 0x1000) /
/// 2, the rates integrate `pitch >> 3`, `roll >> 5`, `throttle >> 5` (yaw
/// not at all) and are clamped to `|pitch|`, 0, `|roll|`, 0x1000, then
/// copied to `out` +4/+6/+8/+0xE.
fn e_steer(m: &mut Machine, rates: u32, out: u32, controls: u32, targets: [i32; 3]) {
    let [pitch, roll, throttle] = targets;
    let yaw = 0; // t4
    let limits = [magnitude(pitch), magnitude(yaw), magnitude(roll)];
    for (i, &limit) in limits.iter().enumerate() {
        let at = rates.wrapping_add(i as u32 * 2);
        let rate = m.lh(at);
        if limit < magnitude(i32::from(rate)) {
            m.sh(at, decay_64_percent(rate));
        }
    }
    let rate = m.lh(rates.wrapping_add(6));
    if throttle < magnitude(i32::from(rate)) {
        m.sh(rates.wrapping_add(6), e_decay_throttle(rate));
    }
    e_approach_controls(m, controls, [roll, pitch, yaw]);
    e_move_lever(m, controls, 0);
    let power = (i32::from(m.lh(rates.wrapping_add(6))) + 0x1000) >> 1;
    m.sh(controls.wrapping_add(6), power as u16);
    // The original also loads rates +2 here (unused).
    for (offset, increment) in [(0, pitch >> 3), (4, roll >> 5), (6, throttle >> 5)] {
        let at = rates.wrapping_add(offset);
        let value = m.lhu(at);
        m.sh(at, value.wrapping_add(increment as u16));
    }
    e_clamp_rates(m, rates, limits);
    for (from, to) in [(0, 4), (2, 6), (4, 8), (6, 0xe)] {
        let value = m.lhu(rates.wrapping_add(from));
        m.sh(out.wrapping_add(to), value);
    }
}

/// Roll target that levels the wings: `-roll * 4` (entity +0x1C), halved
/// once for each of `|roll| < 0x1C7`, `< 0x111` and `< 0x88`.
fn e_level_roll(m: &mut Machine, entity: u32, roll: i32) -> i32 {
    let mut target = roll;
    let bank = magnitude(i32::from(m.lh(entity.wrapping_add(0x1c))));
    for threshold in [0x1c7, 0x111, 0x88] {
        if bank < threshold {
            target >>= 1;
        }
    }
    target
}

/// 80073B18: autopilot for `entity` over the control blocks (`rates`,
/// `out`, `controls` as 8006D0DC); `climb` is the caller's stack word
/// +0x10. Above the ceiling (entity +0xC < 0xFF448000) or without `climb`
/// it levels out: pitch target `-pitch * 16` while `|roll| < 0x800`
/// (halved when `|pitch| < 0xB6`), no throttle. Otherwise, while `|roll| <
/// 0x800`, it climbs: pitch target `(0x800 - pitch) * 16` below pitch
/// 0x800, throttle 0x1000. Roll target: wings level. Targets clamp to
/// +-0x1000; the rest is the shared steering tail (`e_steer`).
pub fn auto_level_80072494(
    m: &mut Machine,
    entity: u32,
    rates: u32,
    out: u32,
    controls: u32,
    climb: u32,
) {
    let mut pitch: i32 = 0; // t2
    let mut throttle: i32 = 0; // t6
    let roll_angle = i32::from(m.lh(entity.wrapping_add(0x1c))); // t0
    let roll = roll_angle.wrapping_neg() << 2; // t1
    let above_ceiling = (m.lw(entity.wrapping_add(0xc)) as i32) < GROUND_CEILING as i32;
    if above_ceiling || climb == 0 {
        if magnitude(roll_angle) < 0x800 {
            pitch = i32::from(m.lh(entity.wrapping_add(0x18))).wrapping_neg() << 4;
        }
        if magnitude(i32::from(m.lh(entity.wrapping_add(0x18)))) < 0xb6 {
            pitch >>= 1;
        }
    } else if magnitude(roll_angle) < 0x800 {
        let pitch_angle = i32::from(m.lh(entity.wrapping_add(0x18)));
        if pitch_angle < 0x800 {
            pitch = (0x800 - pitch_angle) << 4;
        }
        throttle = 0x1000;
    }
    let roll = e_level_roll(m, entity, roll).clamp(-0x1000, 0x1000);
    let pitch = pitch.clamp(-0x1000, 0x1000);
    e_steer(m, rates, out, controls, [pitch, roll, throttle]);
}

/// 8007407C: climbing autopilot for `entity` (control blocks as
/// 8006D0DC): while `|roll| < 0x800` the pitch target is `(0x800 - pitch)
/// * 16` (0 at pitch 0x800) with throttle 0x1000; the roll target levels
/// the wings. Targets clamp to +-0x1000; the rest is the steering tail
/// shared with 80073B18 (`e_steer`).
pub fn auto_climb_800729f8(m: &mut Machine, entity: u32, rates: u32, out: u32, controls: u32) {
    let mut pitch: i32 = 0; // t1
    let mut throttle: i32 = 0; // t5
    let roll_angle = i32::from(m.lh(entity.wrapping_add(0x1c)));
    let roll = roll_angle.wrapping_neg() << 2; // t0
    if magnitude(roll_angle) < 0x800 {
        let pitch_angle = i32::from(m.lh(entity.wrapping_add(0x18)));
        if pitch_angle != 0x800 {
            pitch = (0x800 - pitch_angle) << 4;
        }
        throttle = 0x1000;
    }
    let roll = e_level_roll(m, entity, roll).clamp(-0x1000, 0x1000);
    let pitch = pitch.clamp(-0x1000, 0x1000);
    e_steer(m, rates, out, controls, [pitch, roll, throttle]);
}

/// 8006D678: control step of 800729DC modes 0x1000000/0x2000000/0x4000000
/// (`input` = context +0x120; `rates`, `out`, `controls` as 8006D0DC).
///
/// With input +0x08 bit 31 set and either context +4 nonzero or the
/// player's `|roll|` (entity +0x1C) >= 0x223, the autopilot steers instead:
/// 80073B18 (stack word 0) in mode 0x1000000, else 8007407C, with the
/// player entity `[[gp+0x2E8]]`.
///
/// Otherwise the pad step of 8006D0DC with slower response: rates above
/// their targets decay to 58% (128/220; throttle as 8006D0DC), all four
/// rates integrate `target >> 5`. Outputs: +0xE throttle, +4 pitch; with
/// context +0x120 bit 2, +6 = roll and +8 = yaw + roll / 32 (clamped to
/// +-0x1000) while the player's roll angle returns towards 0 by 0xC per
/// step (beyond +-0xC); without it +6 = yaw, +8 = roll.
pub fn control_step_8006bea8(m: &mut Machine, input: u32, rates: u32, out: u32, controls: u32) {
    const FRAME: u32 = 0x20;
    let entities = m.gw(crate::common::ENTITIES);
    let flags = m.lw(input.wrapping_add(8));
    let entity = m.lw(entities);
    if (flags as i32) < 0 {
        let context = m.gw(crate::common::CONTEXT);
        let autopilot = m.lw(context.wrapping_add(4)) != 0
            || magnitude(i32::from(m.lh(entity.wrapping_add(0x1c)))) >= 0x223;
        if autopilot {
            let state = m.gw(0x520);
            if m.lw(state.wrapping_add(4)) == 0x0100_0000 {
                // Stack argument stored at its original frame offset.
                let sp = m.push_frame(FRAME);
                m.sw(sp.wrapping_add(0x10), 0);
                auto_level_80072494(m, entity, rates, out, controls, 0);
                m.pop_frame(FRAME);
            } else {
                auto_climb_800729f8(m, entity, rates, out, controls);
            }
            return;
        }
    }

    let t = e_pad_targets(m, input);
    for (offset, target) in [(0, t.pitch), (2, t.yaw), (4, t.roll)] {
        let at = rates.wrapping_add(offset);
        let rate = m.lh(at);
        if magnitude(target) < magnitude(i32::from(rate)) {
            m.sh(at, e_decay_58_percent(rate));
        }
    }
    let rate = m.lh(rates.wrapping_add(6));
    if magnitude(t.throttle) < magnitude(i32::from(rate)) {
        m.sh(rates.wrapping_add(6), e_decay_throttle(rate));
    }
    e_approach_controls(m, controls, [t.roll, t.pitch, t.yaw]);
    // Lever target: -2 * (throttle + 0x800) when throttle < -0x800, else 0.
    let lever = if t.throttle < -0x800 {
        t.throttle.wrapping_add(0x800).wrapping_neg() << 1
    } else {
        0
    };
    e_move_lever(m, controls, lever);
    let power = (i32::from(m.lh(rates.wrapping_add(6))) + 0x1000) >> 1;
    m.sh(controls.wrapping_add(6), power as u16);
    for (offset, target) in [(0, t.pitch), (2, t.yaw), (4, t.roll), (6, t.throttle)] {
        let at = rates.wrapping_add(offset);
        let value = m.lhu(at);
        m.sh(at, value.wrapping_add((target >> 5) as u16));
    }
    e_clamp_rates(m, rates, [t.pitch_limit, t.yaw_limit, t.roll_limit]);

    let throttle_rate = m.lhu(rates.wrapping_add(6));
    let context = m.gw(crate::common::CONTEXT);
    m.sh(out.wrapping_add(0xe), throttle_rate);
    let pitch_rate = m.lhu(rates);
    m.sh(out.wrapping_add(4), pitch_rate);
    if m.lw(context.wrapping_add(0x120)) & 4 != 0 {
        let roll_rate = m.lhu(rates.wrapping_add(4));
        m.sh(out.wrapping_add(6), roll_rate);
        let roll_rate = m.lh(rates.wrapping_add(4));
        let yaw_rate = m.lhu(rates.wrapping_add(2));
        let yaw = u32::from(yaw_rate).wrapping_add((i32::from(roll_rate) >> 5) as u32) as u16;
        m.sh(out.wrapping_add(8), yaw);
        if (yaw as i16) > 0x1000 {
            m.sh(out.wrapping_add(8), 0x1000);
        }
        if m.lh(out.wrapping_add(8)) < -0x1000 {
            m.sh(out.wrapping_add(8), 0xf000);
        }
        let entities = m.gw(crate::common::ENTITIES);
        let player = m.lw(entities);
        let angle = player.wrapping_add(0x1c);
        if m.lh(angle) >= 0xd {
            let value = m.lhu(angle);
            m.sh(angle, value.wrapping_sub(0xc));
        }
        if m.lh(angle) < -0xc {
            let value = m.lhu(angle);
            m.sh(angle, value.wrapping_add(0xc));
        }
    } else {
        let yaw_rate = m.lhu(rates.wrapping_add(2));
        m.sh(out.wrapping_add(6), yaw_rate);
        let roll_rate = m.lhu(rates.wrapping_add(4));
        m.sh(out.wrapping_add(8), roll_rate);
    }
}

/// `speed / 21` in 4.12 fixed point as 8006E7E0/8006E874 compute it
/// (reciprocal 0x30C30C31, shift 2, rounded toward zero); 0x1000 above
/// 0x14FFF.
fn e_speed_scale(speed: u32) -> i32 {
    let speed = speed as i32;
    if speed > 0x14fff {
        0x1000
    } else {
        let high = ((i64::from(speed) * 0x30c3_0c31) >> 32) as i32;
        (high >> 2).wrapping_sub(speed >> 31)
    }
}

/// `component * scale / 4096` (low product word, rounded toward zero).
fn e_scale_fixed(component: i16, scale: i32) -> u16 {
    let product = (i64::from(component) * i64::from(scale)) as i32;
    let product = if product < 0 {
        product.wrapping_add(0xfff)
    } else {
        product
    };
    (product >> 12) as u16
}

/// 8006E7E0: scale the rate outputs `out` +4, +6, +8 (`[gp+0x518]`) by
/// `speed / 21` (4.12 fixed point; 0x1000 above speed 0x14FFF), like
/// 8006E874 does for the controls.
pub fn scale_outputs_by_speed_8006d184(m: &mut Machine, speed: u32, out: u32) {
    let scale = e_speed_scale(speed);
    let pitch = m.lh(out.wrapping_add(4));
    let yaw = m.lh(out.wrapping_add(6));
    let roll = m.lh(out.wrapping_add(8));
    m.sh(out.wrapping_add(4), e_scale_fixed(pitch, scale));
    m.sh(out.wrapping_add(6), e_scale_fixed(yaw, scale));
    m.sh(out.wrapping_add(8), e_scale_fixed(roll, scale));
}
// ==== end of e ====
// ==== f: analog control 8006DD5C and scripted control 8007542C ====

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

// JP-REVIEW(R0301): changed function: PAL 8006C58C -> JP 8006DD5C, del:155 imm:4 ins:62 op:14 reg:158, 393 instructions differ (whole routine)
/// 8006DD5C: analog-pad control step, called by 80072938 when context
/// +0x120 bit 2 is set, as (`input` = context +0x120, `rates` `[gp+0x514]`,
/// `out` `[gp+0x518]`, `controls` `[gp+0x51C]`, stack words +0x10 `entity`,
/// +0x14 `motion` (the caller's s1), +0x18 `state` `[gp+0x520]`).
///
/// Inputs as 8006D0DC (+0x08 buttons, +0x18 axis mask, axes +0x1C pitch,
/// +0x20 roll, +0x24 yaw, +0x28 throttle; buttons give +-0x1000). The
/// targets are coordinated with the entity attitude (+0x18 pitch, +0x1A
/// heading, +0x1C bank):
/// - roll: two turn terms ([`f_turn`]; each also turns the heading), a
///   bank-levelling term `-bank * (0xA00 - |roll|) / 0xA00` while
///   |roll| < 0xA00, a push back beyond |bank| = 0xA00 (both scaled by
///   `0x1000 - |pitch angle|`), the first three scaled by `0x1000 - |pitch
///   input|`, and a pitch-input correction beyond |bank| = 0x800;
/// - pitch: the pitch input faded out from |bank| = 0x800 (negated from
///   |bank| = 0x1000); the pitch angle (+0x18) changes by `motion+4 *
///   [state+0x4C] * |pitch angle| * |pitch input|` (negated beyond |bank| =
///   0x1000);
/// - yaw: axis or buttons, clamped to +-0x1000 (its rate limit too).
///
/// Rates and controls then follow [`f_control_tail`].
// JP-PATCH: replaced by `crate::jp::res_p1_player::analog_control_8006c58c` (src/jp/mod.rs REPLACES, PATCHES.md)
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
    crate::jp::res_p1_player::analog_control_8006c58c(
        m, input, rates, out, controls, entity, motion, state,
    )
}

/// 8007542C: control step of the 800729DC mode 0x800 phase, called as
/// (`entity`, `state` `[gp+0x520]`, `rates` `[gp+0x514]`, `out`
/// `[gp+0x518]`, stack word +0x10 `controls` `[gp+0x51C]`). The phase word
/// state +8 (frame count +0xC) sets the targets:
/// - 0: the four rates cleared, phase 1;
/// - 1: throttle -0x800 for 0x4C frames, then phase 2;
/// - 2: throttle -0x800, roll `(0x71C - bank) * 2` (bank = entity +0x1C)
///   for 0x79 frames, then phase 3;
/// - 3: roll `-bank * 2`, pitch `-pitch angle * 16` (entity +0x18).
///
/// Pitch and roll are clamped to +-0x1000 (also their rate limits), yaw is
/// 0; rates and controls follow [`f_control_tail`].
pub fn scripted_control_80073db0(
    m: &mut Machine,
    entity: u32,
    state: u32,
    rates: u32,
    out: u32,
    controls: u32,
) {
    let mut pitch: i32 = 0; // t4
    let mut roll: i32 = 0; // t0
    let mut throttle: i32 = 0; // t6
    match m.lw(state.wrapping_add(8)) {
        0 => {
            m.sw(state.wrapping_add(8), 1);
            m.sw(state.wrapping_add(0xc), 0);
            for offset in [0, 2, 4, 6] {
                m.sh(rates.wrapping_add(offset), 0);
            }
        }
        1 => {
            throttle = -0x800;
            let count = m.lw(state.wrapping_add(0xc));
            if (count as i32) < 0x4c {
                m.sw(state.wrapping_add(0xc), count.wrapping_add(1));
            } else {
                m.sw(state.wrapping_add(8), 2);
                m.sw(state.wrapping_add(0xc), 0);
            }
        }
        2 => {
            throttle = -0x800;
            let bank = i32::from(m.lh(entity.wrapping_add(0x1c)));
            let count = m.lw(state.wrapping_add(0xc));
            roll = (0x71c - bank) << 1;
            if (count as i32) < 0x79 {
                m.sw(state.wrapping_add(0xc), count.wrapping_add(1));
            } else {
                m.sw(state.wrapping_add(8), 3);
                m.sw(state.wrapping_add(0xc), 0);
            }
        }
        3 => {
            let bank = i32::from(m.lh(entity.wrapping_add(0x1c)));
            let attitude = i32::from(m.lh(entity.wrapping_add(0x18)));
            roll = bank.wrapping_neg() << 1;
            pitch = attitude.wrapping_neg() << 4;
        }
        _ => {}
    }
    let pitch = pitch.clamp(-0x1000, 0x1000);
    let roll = roll.clamp(-0x1000, 0x1000);
    f_control_tail(
        m,
        rates,
        out,
        controls,
        [pitch, 0, roll, throttle],
        [magnitude(pitch), 0, magnitude(roll)],
    );
}
// ==== end of f ====
// ==== b2: positioned sound voices (8002DCB0..8002F3C0) and 800313A8 ====

/// Pitch offsets by Doppler index (512 halfwords), used by 8002EC48.
const B2_DOPPLER_PITCH: u32 = 0x800b_0500;

/// Voice table entry of `voice` (20 bytes each from 800C5830; the original
/// does not range-check here).
fn b2_voice(voice: u32) -> u32 {
    VOICE_TABLE.wrapping_add(voice.wrapping_mul(20))
}

/// Set the update flag 0x10 in byte +1 of the voice entry.
fn b2_mark(m: &mut Machine, entry: u32) {
    let flags = m.lbu(entry.wrapping_add(1));
    m.sb(entry.wrapping_add(1), flags | 0x10);
}

/// 8002DCB0: start sound `id` on a newly claimed channel: 8002DD38 claims
/// one (0xFF = none), then 8002E514(`id`, channel). Returns the channel or
/// 0xFF.
pub fn start_sound_8002d888(m: &mut Machine, id: u32) -> u32 {
    let channel = external(m, 0x8002_dd38, [0; 4]);
    if channel != 0xff {
        external(m, 0x8002_e514, [id, channel, 0, 0]);
    }
    channel
}

/// 8002E67C: reapply the stored levels of `voice`: update flag, volumes
/// (+4/+6) = the word at +8, pitch (+0xC) = +0xE. Returns `voice`; 0xFF
/// (no voice) returns 0xFF without effect.
pub fn restore_voice_8002e260(m: &mut Machine, voice: u32) -> u32 {
    if voice == 0xff {
        return 0xff;
    }
    let entry = b2_voice(voice);
    b2_mark(m, entry);
    // lwl/lwr + swl/swr pairs on word-aligned addresses (the table and the
    // 20-byte stride are word-aligned): a plain word copy.
    let levels = m.lw(entry.wrapping_add(8));
    m.sw(entry.wrapping_add(4), levels);
    let pitch = m.lhu(entry.wrapping_add(0xe));
    m.sh(entry.wrapping_add(0xc), pitch);
    voice
}

/// 8002E6DC: positioned-sound record `sound` for a source at the VECTOR
/// `source` heard from `listener` (VECTOR +0, orientation SVECTOR +0x10):
/// - +0 distance (80083DFC); +4 its change since the previous distance (0
///   when that was 0 or 0x100000, or when the change is 0x12D or more);
/// - +8 the bearing 80030D54(z, x) & 0x3FFF of (source - listener) >> 12
///   rotated into the listener frame (800308F0 matrix, ApplyMatrixLV);
/// - +0xA a pan that moves 0x100 towards the side (sign of z) per call,
///   limited to +/-0x400.
pub fn position_sound_8002e2c0(m: &mut Machine, sound: u32, listener: u32, source: u32) {
    const SIZE: u32 = 0x60;
    let sp = m.push_frame(SIZE);
    let r = |offset: u32| sound.wrapping_add(offset);
    let distance = crate::shared::f_800827b4(m, [listener, source, 0, 0]);
    let previous = m.lw(sound);
    if previous == 0 || previous == 0x10_0000 {
        m.sw(r(4), 0);
    } else {
        m.sw(r(4), previous.wrapping_sub(distance));
    }
    if (m.lw(r(4)) as i32).wrapping_abs() >= 0x12d {
        m.sw(r(4), 0);
    }
    m.sw(sound, distance);
    let matrix = sp + 0x10;
    let offset = sp + 0x30;
    let local = sp + 0x40;
    crate::shared::f_800306ac(m, [listener.wrapping_add(0x10), matrix, 0, 0]);
    for i in 0..3 {
        let to = m.lw(source.wrapping_add(i * 4));
        let from = m.lw(listener.wrapping_add(i * 4));
        m.sw(offset + i * 4, sra(to.wrapping_sub(from), 12));
    }
    crate::sdk::apply_matrix_lv(m, matrix, offset, local);
    let z = m.lw(local + 8);
    let x = m.lw(local);
    let bearing = crate::shared::f_80030b10(m, [z, x, 0, 0]);
    m.sh(r(8), (bearing & 0x3fff) as u16);
    let pan = m.lhu(r(0xa));
    let pan = if m.lw(local + 8) as i32 >= 0 {
        pan.wrapping_add(0x100)
    } else {
        pan.wrapping_sub(0x100)
    };
    m.sh(r(0xa), pan);
    let pan = m.lh(r(0xa));
    if pan >= 0x401 {
        m.sh(r(0xa), 0x400);
    } else if pan < -0x400 {
        m.sh(r(0xa), 0xfc00);
    }
    m.pop_frame(SIZE);
}

/// 8002E84C: distance attenuation of `voice` from the record `sound`:
/// level = clamp(0x3FFF - (distance << `shift`), 0, 0x4000) (signed), both
/// volumes = volume(+4) * (level * level >> 14) >> 14. Returns `voice`
/// (0xFF: no effect).
pub fn distance_volume_8002e430(m: &mut Machine, voice: u32, sound: u32, shift: u32) -> u32 {
    if voice == 0xff {
        return 0xff;
    }
    let entry = b2_voice(voice);
    b2_mark(m, entry);
    let distance = m.lw(sound);
    let mut level = 0x3fff_u32.wrapping_sub(distance << (shift & 31)) as i32;
    if level < 0 {
        level = 0;
    }
    if level >= 0x4001 {
        level = 0x4000;
    }
    let fade = level.wrapping_mul(level) >> 14;
    let volume = i32::from(m.lh(entry.wrapping_add(4))).wrapping_mul(fade) >> 14;
    m.sh(entry.wrapping_add(6), volume as u16);
    m.sh(entry.wrapping_add(4), volume as u16);
    voice
}

/// 8002E8DC: stereo split of `voice` by the bearing of `sound` (+8): left
/// (+4) = |volume * sin(b / 2) >> 14|, right (+6) = |volume * cos(b / 2) >>
/// 14| (sine table 800104AC). Each side is raised to at least the near
/// floor volume * max(0x4000 minus (distance << 6), 0) >> 14 and limited
/// to 0x3FFF (signed halfword compares). Returns `voice` (0xFF: no effect).
pub fn pan_volume_8002e4c0(m: &mut Machine, voice: u32, sound: u32) -> u32 {
    if voice == 0xff {
        return 0xff;
    }
    let entry = b2_voice(voice);
    let e = |offset: u32| entry.wrapping_add(offset);
    b2_mark(m, entry);
    let angle = (u32::from(m.lhu(sound.wrapping_add(8))) >> 1) & 0x3fff;
    let volume = i32::from(m.lh(e(4)));
    let sine = i32::from(m.lh(SINE_TABLE + angle * 2));
    let cosine = i32::from(m.lh(SINE_TABLE + (angle + 0x1000) * 2));
    m.sh(e(4), (volume.wrapping_mul(sine) >> 14) as u16);
    m.sh(e(6), (volume.wrapping_mul(cosine) >> 14) as u16);
    for side in [4, 6] {
        if m.lh(e(side)) < 0 {
            let value = m.lhu(e(side));
            m.sh(e(side), 0u16.wrapping_sub(value));
        }
    }
    let distance = m.lw(sound);
    let mut near = 0x4000_u32.wrapping_sub(distance << 6) as i32;
    if near < 0 {
        near = 0;
    }
    let floor = volume.wrapping_mul(near) >> 14;
    for side in [4, 6] {
        if i32::from(m.lh(e(side))) < floor {
            m.sh(e(side), floor as u16);
        }
    }
    for side in [4, 6] {
        if m.lh(e(side)) >= 0x4000 {
            m.sh(e(side), 0x3fff);
        }
    }
    voice
}

/// 8002EA24: for a source on the left (`side` < 0, signed) negate the left
/// volume (+4) of `voice` (inverted phase); the update flag is set either
/// way. Returns `voice` (0xFF: no effect).
pub fn surround_side_8002e608(m: &mut Machine, voice: u32, side: u32) -> u32 {
    if voice == 0xff {
        return 0xff;
    }
    let entry = b2_voice(voice);
    b2_mark(m, entry);
    if (side as i32) < 0 {
        let left = m.lhu(entry.wrapping_add(4));
        m.sh(entry.wrapping_add(4), 0u16.wrapping_sub(left));
    }
    voice
}

/// 8002EC48: Doppler pitch of `voice`: index = ((distance change (`sound`
/// +4) + 0x470) << 9) / 0x8E0 (signed, truncating; the original multiplies
/// by 0xE6C2B449 and shifts), limited to 0..0x1FF; pitch (+0xC) = base
/// pitch (+0xE) shifted by the table entry at 800B0500 (8002E64C). Returns
/// `voice` (0xFF: no effect).
pub fn doppler_pitch_8002e82c(m: &mut Machine, voice: u32, sound: u32) -> u32 {
    if voice == 0xff {
        return 0xff;
    }
    let entry = b2_voice(voice);
    b2_mark(m, entry);
    let scaled = (m.lw(sound.wrapping_add(4)).wrapping_add(0x470) << 9) as i32;
    let index = (scaled / 0x8e0).clamp(0, 0x1ff) as u32;
    let base = i32::from(m.lh(entry.wrapping_add(0xe))) as u32;
    let offset = i32::from(m.lh(B2_DOPPLER_PITCH + index * 2)) as u32;
    let pitch = pitch_add_8002e230(base, offset);
    m.sh(entry.wrapping_add(0xc), pitch as u16);
    voice
}

/// 8002F0D4: detune `voice`: pitch (+0xC) shifted by the signed halfword
/// `delta` (8002E64C), update flag set. Returns `voice` (0xFF: no effect).
pub fn detune_8002ecb8(m: &mut Machine, voice: u32, delta: u32) -> u32 {
    if voice == 0xff {
        return 0xff;
    }
    let entry = b2_voice(voice);
    let delta = delta as u16 as i16 as i32 as u32;
    let flags = m.lbu(entry.wrapping_add(1));
    let pitch = i32::from(m.lh(entry.wrapping_add(0xc))) as u32;
    m.sb(entry.wrapping_add(1), flags | 0x10);
    let pitch = pitch_add_8002e230(pitch, delta);
    m.sh(entry.wrapping_add(0xc), pitch as u16);
    voice
}

/// 8002F3C0: play sound `id` (8002DB4C) at the VECTOR `position` heard from
/// the camera (`[gp+0x2E0]` +0x28, +4): a record in the frame at sp+0x10
/// is positioned (8002E6DC), the voice attenuated with `shift` & 0xFF
/// (8002E84C), split (8002E8DC), phase-inverted on the left (8002EA24 with
/// the pan of the record) and detuned by (rand() & 0x3FF) - 0x200
/// (8002F0D4). Returns the voice, or 0xFF when none started. The record is
/// not initialized: its previous distance (+0) and pan (+0xA) are whatever
/// the stack held, as in the original.
pub fn play_positioned_sound_8002efa4(m: &mut Machine, id: u32, position: u32, shift: u32) -> u32 {
    const SIZE: u32 = 0x30;
    let sp = m.push_frame(SIZE);
    let voice = external(m, 0x8002_db4c, [id, 0, 0, 0]);
    if voice == 0xff {
        m.pop_frame(SIZE);
        return 0xff;
    }
    let sound = sp + 0x10;
    let gameplay = m.gw(crate::common::GAMEPLAY);
    let listener = m.lw(gameplay.wrapping_add(0x28)).wrapping_add(4);
    position_sound_8002e2c0(m, sound, listener, position);
    distance_volume_8002e430(m, voice, sound, shift & 0xff);
    pan_volume_8002e4c0(m, voice, sound);
    let pan = i32::from(m.lh(sp + 0x1a)) as u32;
    surround_side_8002e608(m, voice, pan);
    let random = crate::sdk::rand(m);
    let result = detune_8002ecb8(m, voice, (random & 0x3ff).wrapping_sub(0x200));
    m.pop_frame(SIZE);
    result
}

/// 800313A8: [`orientation_80030d20`] with a separate output: the
/// orientation SVECTOR `angles` (only read) turned by the delta SVECTOR
/// `delta`, stored at `out`. The instructions are those of 80030F64 with
/// the input in s4 and the output in s5.
pub fn turn_orientation_80031164(m: &mut Machine, angles: u32, delta: u32, out: u32) {
    // One implementation: render.rs owns 800313A8.
    crate::dispatch::call(m, 0x8003_13a8, [angles, delta, out, 0]);
}

// ==== end of b2 ====
// ==== g: crash 80074560, ground roll 80074878, sequences 80074BA8/80074E64 ====

/// 8002963C: crash-impact vibration: a slot with priority 0xFA (8002939C)
/// gets byte 0 = 0x3C and byte 2 = 0xFF.
pub fn vibrate_preset_8002921c(m: &mut Machine) {
    let slot = claim_vibration_80028f7c(m, 0xfa);
    if slot != 0 {
        m.sb(slot, 0x3c);
        m.sb(slot.wrapping_add(2), 0xff);
    }
}

/// 80029674: falling vibration: a slot with priority 0xA0 (8002939C) gets
/// byte 0 = 1 (stored before the `rand` call) and byte 2 = 0x41 + (rand()
/// & 0x3F).
pub fn vibrate_random_80029254(m: &mut Machine) {
    let slot = claim_vibration_80028f7c(m, 0xa0);
    if slot == 0 {
        return;
    }
    m.sb(slot, 1);
    let random = crate::sdk::rand(m);
    m.sb(slot.wrapping_add(2), ((random & 0x3f) + 0x41) as u8);
}

/// 800296B8: crash-start vibration: priority 0xB4, byte 0 = 9, byte 2 =
/// 0xFF.
pub fn vibrate_preset_80029298(m: &mut Machine) {
    let slot = claim_vibration_80028f7c(m, 0xb4);
    if slot != 0 {
        m.sb(slot, 9);
        m.sb(slot.wrapping_add(2), 0xff);
    }
}

/// `x / 8` toward zero (`bgez; addiu 7; sra 3`).
fn g_div8(value: i32) -> i32 {
    if value < 0 {
        value.wrapping_add(7) >> 3
    } else {
        value >> 3
    }
}

/// Crash fall: entity +0x3C (vertical speed) grows by 669 * (`[state+0xC]`
/// >> 5), the original's shift/subtract chain (7, 223, 669).
fn g_fall(m: &mut Machine, entity: u32, state: u32) {
    let count = ((m.lw(state.wrapping_add(0xc)) as i32) >> 5) as u32;
    let speed = m.lw(entity.wrapping_add(0x3c));
    m.sw(
        entity.wrapping_add(0x3c),
        speed.wrapping_add(count.wrapping_mul(669)),
    );
}

/// Crash pitch: entity +0x18 moves towards -0x1000 (nose down) by
/// `(-0x1000 - pitch) * factor / 4096` (toward zero).
fn g_dive(m: &mut Machine, entity: u32, factor: i32) {
    let pitch = i32::from(m.lh(entity.wrapping_add(0x18)));
    let raw = m.lhu(entity.wrapping_add(0x18));
    let step = fm_round12((-0x1000i32).wrapping_sub(pitch).wrapping_mul(factor));
    m.sh(entity.wrapping_add(0x18), raw.wrapping_add(step as u16));
}

/// 80074560: player crash (800729DC mode 0x100000) for `entity` and the
/// player state `state` (phase +8, counter +0xC, ground-contact bits +0x10
/// & 0xF, roll rate +0x34):
/// - phase 0: counter 0, phase 1 when context +0x24 & 4 else 2; a zero
///   roll rate becomes (context +0x24 & 0x7F) - 0x40, one below 0x20 in
///   magnitude is multiplied by 4; explosion 80081F94 (+8, +0x38),
///   vibration 800296B8, sound 0x20 (8002DB4C).
/// - phase 1 (spin): airborne fall [`g_fall`], pitch towards -0x1000 by
///   30/4096 of the distance, roll rate += rate / 8 clamped to +-0x200 and
///   added to the roll +0x1C.
/// - phase 2 (tumble): fall, pitch by 15/4096, roll += sine table entry
///   (counter << 8 & 0x3F00) >> 6.
/// - phase 0x10 (wreck): 8008276C (+8, counter), entity +0 bit 0x10000
///   cleared.
///
/// Then the counter increments; in phases 1 and 2 the smoke trail 800821EC
/// (+8, +0x38, counter) and vibration 80029674 run, and ground contact
/// switches to phase 0x10 (counter 0): explosion 80081F94, vibration
/// 8002963C, sound 0x22, velocity +0x38..+0x40 zeroed. Callers ignore `v0`.
pub fn crash_80072edc(m: &mut Machine, entity: u32, state: u32) {
    let position = entity.wrapping_add(8);
    let velocity = entity.wrapping_add(0x38);
    match m.lw(state.wrapping_add(8)) {
        0 => {
            let context = m.gw(crate::common::CONTEXT);
            m.sw(state.wrapping_add(0xc), 0);
            let phase = if m.lw(context.wrapping_add(0x24)) & 4 != 0 {
                1
            } else {
                2
            };
            m.sw(state.wrapping_add(8), phase);
            let rate = m.lh(state.wrapping_add(0x34));
            let raw = m.lhu(state.wrapping_add(0x34));
            if rate == 0 {
                let context = m.gw(crate::common::CONTEXT);
                let random = m.lhu(context.wrapping_add(0x24)) & 0x7f;
                m.sh(
                    state.wrapping_add(0x34),
                    raw.wrapping_sub(0x40).wrapping_add(random),
                );
            } else if i32::from(rate).abs() < 0x20 {
                m.sh(state.wrapping_add(0x34), raw.wrapping_shl(2));
            }
            crate::effects::crash_explosion_8008094c(m, position, velocity);
            vibrate_preset_80029298(m);
            external(m, 0x8002_db4c, [0x20, 0, 0, 0]);
        }
        1 => {
            if m.lw(state.wrapping_add(0x10)) & 0xf == 0 {
                g_fall(m, entity, state);
            }
            g_dive(m, entity, 30);
            let rate = i32::from(m.lh(state.wrapping_add(0x34)));
            let rate = rate.wrapping_add(g_div8(rate)).clamp(-0x200, 0x200);
            m.sh(state.wrapping_add(0x34), rate as u16);
            let roll = m.lhu(entity.wrapping_add(0x1c));
            m.sh(entity.wrapping_add(0x1c), roll.wrapping_add(rate as u16));
        }
        2 => {
            if m.lw(state.wrapping_add(0x10)) & 0xf == 0 {
                g_fall(m, entity, state);
            }
            g_dive(m, entity, 15);
            let count = m.lw(state.wrapping_add(0xc));
            let entry = SINE_TABLE.wrapping_add(count.wrapping_shl(9) & 0x7e00);
            let sway = i32::from(m.lh(entry)) >> 6;
            let roll = m.lhu(entity.wrapping_add(0x1c));
            m.sh(entity.wrapping_add(0x1c), roll.wrapping_add(sway as u16));
        }
        0x10 => {
            let count = m.lw(state.wrapping_add(0xc));
            crate::effects::burning_80081124(m, position, count);
            let flags = m.lw(entity) & 0xfffe_ffff;
            m.sw(entity, flags);
        }
        _ => {}
    }
    let count = m.lw(state.wrapping_add(0xc)).wrapping_add(1);
    let phase = m.lw(state.wrapping_add(8));
    m.sw(state.wrapping_add(0xc), count);
    if phase & 3 == 0 {
        return;
    }
    crate::effects::wreck_debris_80080ba4(m, position, velocity, count);
    vibrate_random_80029254(m);
    if m.lw(state.wrapping_add(0x10)) & 0xf == 0 {
        return;
    }
    m.sw(state.wrapping_add(0xc), 0);
    m.sw(state.wrapping_add(8), 0x10);
    crate::effects::crash_explosion_8008094c(m, position, velocity);
    vibrate_preset_8002921c(m);
    external(m, 0x8002_db4c, [0x22, 0, 0, 0]);
    for offset in [0x38, 0x3c, 0x40] {
        m.sw(entity.wrapping_add(offset), 0);
    }
}

/// 800795C8: player camera after the ground roll: `stopped` != 0 gives
/// 80035740(0) and camera mode 0x100, else 80035740(1) and mode 0x80; then
/// 800793C4 (`[[gp+0x2E0] +0x28]`, mode, the player entity `[[gp+0x2E8]]`).
/// Returns the `v0` of 800793C4 (the only caller ignores it).
pub fn player_camera_80077f58(m: &mut Machine, stopped: u32) -> u32 {
    let (mode, request) = if stopped != 0 { (0x100, 0) } else { (0x80, 1) };
    external(m, 0x8003_5740, [request, 0, 0, 0]);
    let gameplay = m.gw(crate::common::GAMEPLAY);
    let table = m.gw(crate::common::ENTITIES);
    let camera = m.lw(gameplay.wrapping_add(0x28));
    let player = m.lw(table);
    external(m, 0x8007_93c4, [camera, mode, player, 0])
}

/// 80074878: ground roll of `entity` (800729DC modes 0x20 and 0x40, with
/// `braking` 0 / 1): the horizontal velocity (+0x38, +0x40) is multiplied
/// by 0xFF4 (braking: 0xF60) / 4096 (toward zero). Airborne (state +0x10 &
/// 0xF clear) the vertical speed +0x3C grows by 0x29D. On the ground the
/// roll +0x1C is damped by 0xF00 / 4096 (0xEC0 + lift / 8 for |roll| <
/// 0x600, lift = +0x3C clamped to 0..0x400), and the pitch +0x18 settles
/// through the phases at state +8:
/// - 0: pitch < 0: phase 1; pitch >= 0xB6: phase 2; else phase 3 with
///   state +0xC = (pitch << 10) / 182 + 0x1000;
/// - 1: pitch += max((0xB6 - pitch) / 2, 2); from 0xB6 on: pitch 0xB6,
///   phase 3 with +0xC = 0x1400;
/// - 2: pitch -= max((pitch - 0xB6) * 2 * lift / 4096, 8); below 0xB7: as
///   phase 1's end;
/// - 3: pitch = rsin(+0xC) * 0xB6 / 4096 * clamp(+0xC, 0, 0x1000) / 4096
///   (800A60F8), +0xC -= 0x40 (braking 0x80); below -0x200: 800795C8(1)
///   and phase 4.
///
/// The velocity is stored back and 8006EE38 (entity, state) aims the view.
pub fn ground_roll_800731f4(m: &mut Machine, entity: u32, state: u32, braking: u32) {
    let mut vx = m.lw(entity.wrapping_add(0x38)) as i32; // s5
    let mut vy = m.lw(entity.wrapping_add(0x3c)) as i32; // s3
    let mut vz = m.lw(entity.wrapping_add(0x40)) as i32; // s4
    let friction = if braking == 0 { 0xff4 } else { 0xf60 };
    vx = fm_mul12(vx, friction);
    vz = fm_mul12(vz, friction);
    if m.lw(state.wrapping_add(0x10)) & 0xf == 0 {
        vy = vy.wrapping_add(0x29d);
    } else {
        let lift = vy.clamp(0, 0x400); // a1
        let roll = i32::from(m.lh(entity.wrapping_add(0x1c)));
        let damping = if fm_abs(roll) < 0x600 {
            (lift >> 3) + 0xec0
        } else {
            0xf00
        };
        m.sh(entity.wrapping_add(0x1c), fm_mul12(roll, damping) as u16);
        let pitch = i32::from(m.lh(entity.wrapping_add(0x18))); // a0
        if m.lw(state.wrapping_add(8)) == 0 {
            if pitch < 0 {
                m.sw(state.wrapping_add(8), 1);
            } else if pitch < 0xb6 {
                // (pitch << 10) / 182 through the 0xB40B40B5 reciprocal.
                let scaled = pitch << 10;
                let high = ((i64::from(scaled) * i64::from(0xb40b_40b5u32 as i32)) >> 32) as i32;
                m.sw(state.wrapping_add(8), 3);
                let start = (high.wrapping_add(scaled) >> 7).wrapping_sub(scaled >> 31);
                m.sw(state.wrapping_add(0xc), start.wrapping_add(0x1000) as u32);
            } else {
                m.sw(state.wrapping_add(8), 2);
            }
        }
        let settle = |m: &mut Machine| {
            m.sh(entity.wrapping_add(0x18), 0xb6);
            m.sw(state.wrapping_add(0xc), 0x1400);
            m.sw(state.wrapping_add(8), 3);
        };
        match m.lw(state.wrapping_add(8)) {
            1 => {
                let step = (0xb6 - pitch) / 2;
                let step = step.max(2);
                let raw = m.lhu(entity.wrapping_add(0x18));
                m.sh(entity.wrapping_add(0x18), raw.wrapping_add(step as u16));
                if pitch >= 0xb6 {
                    settle(m);
                }
            }
            2 => {
                let step = fm_mul12(pitch - 0xb6, lift << 1).max(8);
                let raw = m.lhu(entity.wrapping_add(0x18));
                m.sh(entity.wrapping_add(0x18), raw.wrapping_sub(step as u16));
                if pitch < 0xb7 {
                    settle(m);
                }
            }
            3 => {
                let envelope = (m.lw(state.wrapping_add(0xc)) as i32).clamp(0, 0x1000);
                let angle = m.lw(state.wrapping_add(0xc));
                let sine = external(m, 0x800a_60f8, [angle, 0, 0, 0]) as i32;
                let swing = fm_round12(sine.wrapping_mul(0xb6));
                m.sh(entity.wrapping_add(0x18), fm_mul12(swing, envelope) as u16);
                let step = if braking == 0 { 0x40 } else { 0x80 };
                let angle = m.lw(state.wrapping_add(0xc)).wrapping_sub(step);
                m.sw(state.wrapping_add(0xc), angle);
                if (m.lw(state.wrapping_add(0xc)) as i32) < -0x200 {
                    player_camera_80077f58(m, 1);
                    m.sw(state.wrapping_add(8), 4);
                }
            }
            _ => {}
        }
    }
    m.sw(entity.wrapping_add(0x38), vx as u32);
    m.sw(entity.wrapping_add(0x3c), vy as u32);
    m.sw(entity.wrapping_add(0x40), vz as u32);
    aim_camera_8006d7dc(m, entity, state);
}

/// `((-value) << 16) >> 12`: a negated 16-bit sine as a 4.12 speed << 4
/// (`subu zero; sll 16; sra 12`, or `mult 0x10000; mflo; sra 12`).
fn g_negated_lift(value: u32) -> u32 {
    ((value.wrapping_neg() << 16) as i32 >> 12) as u32
}

/// 80074BA8: scripted fly-in of `entity` (800729DC mode 0x10; `controls`
/// = `[gp+0x51C]`, `view` = `[0x800BCE18]`). The controls are reset (+0
/// .. +0xA zero, throttle +6 = 0x800, word +0xC zero), then by phase
/// (state +8):
/// - 0: position = view position + (sin, height, cos) of the view yaw
///   (+0x12; sine table >> 2, times -0x780), height -0x2DA000 (`[gp+0x2A0]`
///   = 0: -0x33E000); velocity +0x38/+0x40 = (sin, cos) >> 2 << 4, +0x3C =
///   -rsin(-0x13E) << 4; angles (-0x111, view yaw, 0); speed state +0x48
///   = 0x10000; counter 0, phase 1.
/// - 1: after 0x1F frames (counter), phase 2 with counter 0.
/// - 2: k = rsin((min(counter << 12 / 75, 0x1000) >> 1) + 0x400) / 2 +
///   0x800; pitch = -409k / 4096 + 0x88; +0x3C = -rsin(-182k / 4096 -
///   0x88) << 4. Returns 1 (no aim update) from counter 0x79 on or on
///   ground contact (state +0x10 & 9); otherwise the counter increments.
///
/// Phases 0..2 end with 80031BB4 (+0x38 into state +0x40) and 8006EE38
/// (entity, state) and return 0.
pub fn scripted_fly_in_80073524(
    m: &mut Machine,
    entity: u32,
    state: u32,
    controls: u32,
    view: u32,
) -> u32 {
    for (offset, value) in [(0, 0), (2, 0), (4, 0), (6, 0x800), (8, 0), (0xa, 0)] {
        m.sh(controls.wrapping_add(offset), value);
    }
    m.sw(controls.wrapping_add(0xc), 0);
    match m.lw(state.wrapping_add(8)) {
        0 => {
            let height = if m.gw(0x2a0) != 0 {
                0xffd2_6000u32
            } else {
                0xffcc_2000
            };
            let yaw = m.lhu(view.wrapping_add(0x12)); // s0
            let index = u32::from(yaw) & 0x3fff;
            let sine = i32::from(m.lh(SINE_TABLE.wrapping_add(index << 1))) >> 2; // a1
            let cosine = i32::from(m.lh(SINE_TABLE.wrapping_add((index + 0x1000) << 1))) >> 2; // v1
            let x = m.lw(view);
            let z = m.lw(view.wrapping_add(8));
            let y = m.lw(view.wrapping_add(4));
            let x_offset = sine.wrapping_mul(-0x780) as u32;
            m.sw(entity.wrapping_add(8), x_offset.wrapping_add(x));
            m.sw(entity.wrapping_add(0xc), height.wrapping_add(y));
            let z_offset = cosine.wrapping_mul(-0x780) as u32;
            m.sw(entity.wrapping_add(0x10), z_offset.wrapping_add(z));
            let climb = external(m, 0x800a_60f8, [-0x13ei32 as u32, 0, 0, 0]);
            m.sw(entity.wrapping_add(0x3c), g_negated_lift(climb));
            m.sh(entity.wrapping_add(0x18), -0x111i16 as u16);
            m.sw(entity.wrapping_add(0x38), (sine << 4) as u32);
            m.sw(entity.wrapping_add(0x40), (cosine << 4) as u32);
            m.sh(entity.wrapping_add(0x1a), yaw);
            m.sh(entity.wrapping_add(0x1c), 0);
            m.sw(state.wrapping_add(0x48), 0x1_0000);
            m.sw(state.wrapping_add(0xc), 0);
            m.sw(state.wrapping_add(8), 1);
        }
        1 => {
            let count = m.lw(state.wrapping_add(0xc)).wrapping_add(1);
            m.sw(state.wrapping_add(0xc), count);
            if count as i32 >= 0x1f {
                m.sw(state.wrapping_add(0xc), 0);
                m.sw(state.wrapping_add(8), 2);
            }
        }
        2 => {
            let count = m.lw(state.wrapping_add(0xc)) as i32;
            let progress = if count < 0x4c {
                // (count << 12) / 75 through the 0x1B4E81B5 reciprocal.
                let scaled = count.wrapping_shl(12);
                let high = ((i64::from(scaled) * 0x1b4e_81b5) >> 32) as i32;
                (high >> 3).wrapping_sub(scaled >> 31)
            } else {
                0x1000
            };
            let sine = external(m, 0x800a_60f8, [((progress >> 1) + 0x400) as u32, 0, 0, 0]);
            let k = ((sine as i32) >> 1).wrapping_add(0x800);
            let angle = fm_round12(k.wrapping_mul(-182)).wrapping_sub(0x88);
            let pitch = fm_round12(k.wrapping_mul(-409)).wrapping_add(0x88);
            let climb = external(m, 0x800a_60f8, [angle as u32, 0, 0, 0]);
            m.sh(entity.wrapping_add(0x18), pitch as u16);
            m.sw(entity.wrapping_add(0x3c), g_negated_lift(climb));
            let count = m.lw(state.wrapping_add(0xc));
            if count as i32 >= 0x79 || m.lw(state.wrapping_add(0x10)) & 9 != 0 {
                return 1;
            }
            m.sw(state.wrapping_add(0xc), count.wrapping_add(1));
        }
        _ => {}
    }
    vector_angles_80031970(m, entity.wrapping_add(0x38), state.wrapping_add(0x40));
    aim_camera_8006d7dc(m, entity, state);
    0
}

// JP-REVIEW(R0303): near function: PAL 800737E0 -> JP 80074E64, del:2, 2 instructions differ (whole routine)
/// 80074E64: `entity` follows the entity `[[gp+0x2E8] + 4 * [target]]`
/// (800729DC mode 0x200; `controls` = `[gp+0x51C]`, `target` =
/// `[0x800BCE1C]`). Controls reset as in 80074BA8. The offset at `target`
/// +4..+0xC (z - 0x64000), >> 6, rotated by the leader's orientation
/// (80030778 of +0x18, MVMVA sf=1) and << 6 is added to the leader's
/// position (+8); the leader's speed (80083F40 of velocity >> 6, << 6) goes
/// to state +0x48 (+0x4C = 0x1000) and, as the forward vector (0, 0,
/// speed >> 5) rotated and << 5, to the velocity +0x38..+0x40; the angles
/// +0x18.. are copied.
// JP-PATCH: replaced by `crate::jp::res_p1_player::follow_entity_800737e0` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn follow_entity_800737e0(
    m: &mut Machine,
    entity: u32,
    state: u32,
    controls: u32,
    target: u32,
) {
    crate::jp::res_p1_player::follow_entity_800737e0(m, entity, state, controls, target)
}

// ==== end of g ====
// ==== h: 800729DC mode handlers (take-off, landing approach, formation, exit, crash) ====

/// `[0x800246E0]`: the six mode words of 80076B18 (landing approach),
/// indexed by `kind * 2 + variant`.
const H_APPROACH_MODES: u32 = 0x8002_46e0;
/// `[0x800246F8]`: the eight mode words of 80076C54 (target view).
const H_TARGET_MODES: u32 = 0x8002_46f8;
/// `[0x80024718]`: the six mode words of 80076DEC (take-off / start).
const H_START_MODES: u32 = 0x8002_4718;
/// `[0x8002474C]`: the three camera modes of 80079564.
const H_CAMERA_MODES: u32 = 0x8002_474c;
/// `[gp+0x52C]`: the block 80076B18 / 80076C54 fill from the view record.
const H_VIEW_COPY: u32 = 0x52c;

/// The original's stack copy of a rodata table: `words` words of `table`
/// stored at `local` (in the caller's pushed frame), then entry `index` read
/// back at `local + index * 4`, as the original indexes its frame.
fn h_table_entry(m: &mut Machine, table: u32, words: u32, local: u32, index: u32) -> u32 {
    for i in 0..words {
        let word = m.lw(table.wrapping_add(i * 4));
        m.sw(local.wrapping_add(i * 4), word);
    }
    m.lw(local.wrapping_add(index.wrapping_shl(2)))
}

/// The angle unwrap of 80076428 and 800765B0 (signed, full registers):
/// `angle` loses 0x4000 when `reference - angle` is below 0x2000, then
/// gains 0x4000 when the new difference is above 0x2000.
fn h_unwrap(reference: i32, angle: i32) -> i32 {
    let mut angle = angle;
    if reference.wrapping_sub(angle) < 0x2000 {
        angle = angle.wrapping_sub(0x4000);
    }
    if reference.wrapping_sub(angle) >= 0x2001 {
        angle = angle.wrapping_add(0x4000);
    }
    angle
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

// JP-REVIEW(R0305): near function: PAL 80073A08 -> JP 80075084, imm:2, 2 instructions differ (whole routine)
/// 80075084: mode 0x400 of 800729DC, joining the target's formation.
/// `offset` is the 80076924 output +0x24 (relative position << 6 at
/// +0..+8, angle differences at +0x10..+0x14), `target` the target
/// entity's velocity (+0x38). The entity angles +0x18/+0x1A/+0x1C move by
/// [`h_angle_step`] of the differences. Phase (state +8) 0: the velocity
/// (entity +0x38..+0x40) steps by 0x100 toward `target`; when every axis is
/// within 0x100 the phase becomes 1 (state +0xC = 0). Phase 1: velocity =
/// `target` - (`offset` >> 5, clamped to +-0x1000); while the summed
/// magnitude of that correction is below 0x3000 the counter state +0xC
/// counts (sound 0x37, or 0x61 with `[gp+0x2A0]` = 2, at 0x96), and from
/// 0x12D the routine returns 1 without aiming. Otherwise 8006EE38 (entity,
/// state) and 0.
// JP-PATCH: replaced by `crate::jp::res_p1_player::join_formation_80073a08` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn join_formation_80073a08(
    m: &mut Machine,
    entity: u32,
    state: u32,
    offset: u32,
    target: u32,
) -> u32 {
    crate::jp::res_p1_player::join_formation_80073a08(m, entity, state, offset, target)
}

/// 8007599C: put the player on the ground below the view record `view`
/// (position +0..+8, yaw +0x12). State +0x48 (speed) = 0; a ground ray
/// (8009A5D0, work area 0x1F800000) from `view` - 0x64000 to `view` +
/// 0x64000 in y writes the hit point to state +0x1C..+0x24. On a hit
/// (result below 0x7FFFFFFF) state +0x14..+0x18 = the hit normal
/// (`[gp+0x54C]`) and state +0x2C = the hit flags (`[gp+0x550]`); otherwise
/// state +0x1C..+0x24 = the view position. The entity goes to that point
/// with y - 0x42000, pitch/roll 0 and the view's yaw, copied to state
/// +0x40..+0x44; then 8006EE38 (entity, state).
pub fn place_on_ground_80074320(m: &mut Machine, entity: u32, state: u32, view: u32) {
    const FRAME: u32 = 0x40;
    let sp = m.push_frame(FRAME);
    let (from, to) = (sp.wrapping_add(0x10), sp.wrapping_add(0x20));
    m.sw(state.wrapping_add(0x48), 0);
    for (point, height) in [(from, 0xfff9_c000u32), (to, 0x6_4000)] {
        let x = m.lw(view);
        m.sw(point, x);
        let y = m.lw(view.wrapping_add(4));
        m.sw(point.wrapping_add(4), y.wrapping_add(height));
        let z = m.lw(view.wrapping_add(8));
        m.sw(point.wrapping_add(8), z);
    }
    let hit = state.wrapping_add(0x1c);
    let distance = ground_ray_80098b80(m, from, to, hit, 0x1f80_0000);
    if distance as i32 <= 0x7fff_fffe {
        let normal = m.gw(GROUND_HIT_NORMAL);
        let x = m.lhu(normal);
        m.sh(state.wrapping_add(0x14), x);
        let y = m.lhu(normal.wrapping_add(2));
        m.sh(state.wrapping_add(0x16), y);
        let flags = m.gw(GROUND_HIT_FLAGS);
        let z = m.lhu(normal.wrapping_add(4));
        m.sw(state.wrapping_add(0x2c), flags);
        m.sh(state.wrapping_add(0x18), z);
    } else {
        for i in 0..3 {
            let value = m.lw(view.wrapping_add(i * 4));
            m.sw(hit.wrapping_add(i * 4), value);
        }
    }
    let y = m.lw(state.wrapping_add(0x20));
    m.sw(entity.wrapping_add(0xc), y.wrapping_add(0xfffb_e000));
    let x = m.lw(state.wrapping_add(0x1c));
    m.sw(entity.wrapping_add(8), x);
    let z = m.lw(state.wrapping_add(0x24));
    m.sh(entity.wrapping_add(0x18), 0);
    let pitch = m.lhu(entity.wrapping_add(0x18));
    m.sw(entity.wrapping_add(0x10), z);
    let yaw = m.lhu(view.wrapping_add(0x12));
    m.sh(entity.wrapping_add(0x1c), 0);
    m.sh(entity.wrapping_add(0x1a), yaw);
    m.sh(state.wrapping_add(0x40), pitch);
    let yaw = m.lhu(entity.wrapping_add(0x1a));
    m.sh(state.wrapping_add(0x42), yaw);
    let roll = m.lhu(entity.wrapping_add(0x1c));
    m.sh(state.wrapping_add(0x44), roll);
    aim_camera_8006d7dc(m, entity, state);
    m.pop_frame(FRAME);
}

// JP-REVIEW(R0306): near function: PAL 80074488 -> JP 80075B04, del:3 imm:3 op:1, 7 instructions differ (whole routine)
/// 80075B04: modes 0x4000/0x8000 of 800729DC (take-off run; `replay` is
/// the caller's stack word +0x14, 1 in mode 0x8000; `view` its word
/// +0x10). Phase (state +8):
/// - 0: 8007599C (entity, state, `view`), then velocity, speed, the
///   state's rate/angle halves (+0x30..+0x3C, +0x4C), `out` +4..+0xC and
///   `controls` +0..+4/+8 cleared, throttle `out` +0xE = `controls` +6 =
///   -0x1000; phase 1.
/// - 1: throttle = count * 0x2000 / 90 - 0x1000 (at most 0x1000) from
///   state +0xC, which counts to 0x5B: then phase 2, the voice at state
///   +0x52 released (8002DDA8) and the engine sound 0x60 (0x63 in replay)
///   started (8002DB4C) into +0x52.
/// - 2: speed += 0x11C (0x30C in replay); the vector (0, 0, that step)
///   rotated by the entity's orientation (80030778 of +0x18) is added to
///   the velocity (+0x38..+0x40) and the velocity to the position; above
///   speed 0xF000 phase 3 and the voice released.
/// - 3: returns 1 without aiming.
///
/// Other phases and 0..2 end with 8006EE38 (entity, state) and return 0.
// JP-PATCH: replaced by `crate::jp::res_p1_player::takeoff_run_80074488` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn takeoff_run_80074488(
    m: &mut Machine,
    entity: u32,
    state: u32,
    out: u32,
    controls: u32,
    view: u32,
    replay: u32,
) -> u32 {
    crate::jp::res_p1_player::takeoff_run_80074488(m, entity, state, out, controls, view, replay)
}

/// 80075E08: mode 0x10000 of 800729DC (airborne start; `view` and
/// `close` are the caller's stack words +0x10/+0x14). 8007599C (entity,
/// state, `view`); the state's rate/angle halves +0x30..+0x3C cleared and
/// +0x4C = 0x1000; `out` +4..+0xC = 0 and throttle +0xE = 0x1000;
/// `controls` +6 = 0x1000 and +0/+2/+4/+8 = 0; speed (state +0x48) =
/// 0x20000 and state +0xC = 0. With the entity's orientation (80030778 of
/// +0x18) as rotation, the velocity (+0x38..+0x40) = rotated (0, 0, 0x1000)
/// << 5 and the position moves by rotated (0, -0x7D0, 0x1388) << 12 when
/// `close` is set, else (0, -0xDAC, 0x32C8) << 12; then 8006EE38.
pub fn airborne_start_80074798(
    m: &mut Machine,
    entity: u32,
    state: u32,
    out: u32,
    controls: u32,
    view: u32,
    close: u32,
) {
    const FRAME: u32 = 0x68;
    let sp = m.push_frame(FRAME);
    let (vector, matrix, rotated) = (
        sp.wrapping_add(0x10),
        sp.wrapping_add(0x18),
        sp.wrapping_add(0x38),
    );
    place_on_ground_80074320(m, entity, state, view);
    for offset in [0x30, 0x32, 0x34, 0x38, 0x3a, 0x3c] {
        m.sh(state.wrapping_add(offset), 0);
    }
    m.sh(state.wrapping_add(0x4c), 0x1000);
    for offset in [4, 6, 8, 0xa, 0xc] {
        m.sh(out.wrapping_add(offset), 0);
    }
    m.sh(out.wrapping_add(0xe), 0x1000);
    m.sh(controls.wrapping_add(6), 0x1000);
    for offset in [0, 2, 4, 8] {
        m.sh(controls.wrapping_add(offset), 0);
    }
    m.sw(state.wrapping_add(0x48), 0x2_0000);
    m.sw(state.wrapping_add(0xc), 0);
    m.sh(vector, 0);
    m.sh(vector.wrapping_add(2), 0);
    m.sh(vector.wrapping_add(4), 0x1000);
    crate::shared::f_80030534(m, [entity.wrapping_add(0x18), matrix, 0, 0]);
    crate::sdk::set_rot_matrix(m, matrix);
    fm_rotate(m, vector, rotated);
    for i in 0..3 {
        let value = m.lw(rotated.wrapping_add(i * 4)) << 5;
        m.sw(rotated.wrapping_add(i * 4), value);
    }
    for i in 0..3 {
        let value = m.lw(rotated.wrapping_add(i * 4));
        m.sw(entity.wrapping_add(0x38 + i * 4), value);
    }
    let (height, distance): (i16, u16) = if close != 0 {
        (-0x7d0, 0x1388)
    } else {
        (-0xdac, 0x32c8)
    };
    m.sh(vector.wrapping_add(2), height as u16);
    m.sh(vector, 0);
    m.sh(vector.wrapping_add(4), distance);
    fm_rotate(m, vector, rotated);
    for i in 0..3 {
        let value = m.lw(rotated.wrapping_add(i * 4)) << 12;
        m.sw(rotated.wrapping_add(i * 4), value);
    }
    for i in 0..3 {
        let position = entity.wrapping_add(8 + i * 4);
        let sum = m
            .lw(position)
            .wrapping_add(m.lw(rotated.wrapping_add(i * 4)));
        m.sw(position, sum);
    }
    aim_camera_8006d7dc(m, entity, state);
    m.pop_frame(FRAME);
}

/// 8007601C: the exit sequence of modes 0x4000000/0x8000000/0x10000000
/// (set by 80076360). Phase (state +8) 0 in mode 0x4000000: phase 1, state
/// +0xC/+0x4E/+0x50 = 0, state word 0 = 0x10. Phase 1, with t = min(state
/// +0xC, 0x12C): the entity's velocity y/z (+0x3C/+0x40) are scaled by
/// (t - 300) / 300; climb = ((t * 0x1400) / 300) << 6; y (+0xC) above
/// 0xEA5FFFF becomes 0xEA60000, else y += climb >> 1 and z (+0x10) += climb
/// << 1. 8006EE38 runs with velocity y = climb, the speed (state +0x48) =
/// 80083F40(-velocity >> 6) << 6, and the velocity words +0x38..+0x44 are
/// restored. State +0xC counts; with y > 0 the timer state +0x4E counts and
/// ends mode 0x4000000 at 0x12D (entity +0x4E = -1), while mode 0x8000000
/// gets state bit 0x200 from 0x1F. The divisions are the reciprocal
/// multiply by 0x1B4E81B5 (HI >> 5, minus the dividend's sign).
pub fn exit_flight_800749ac(m: &mut Machine, entity: u32, state: u32) {
    const FRAME: u32 = 0x30;
    let sp = m.push_frame(FRAME);
    match m.lw(state.wrapping_add(8)) {
        0 => {
            if m.lw(state.wrapping_add(4)) == 0x400_0000 {
                m.sw(state.wrapping_add(8), 1);
                m.sw(state.wrapping_add(0xc), 0);
                m.sh(state.wrapping_add(0x4e), 0);
                m.sh(state.wrapping_add(0x50), 0);
                m.sw(state, 0x10);
            }
        }
        1 => h_exit_climb(m, sp, entity, state),
        _ => {}
    }
    m.pop_frame(FRAME);
}

/// Phase 1 of 8007601C (frame at `sp`; the velocity words are saved at
/// +0x10..+0x1C around 8006EE38/80083F40, as the original does).
fn h_exit_climb(m: &mut Machine, sp: u32, entity: u32, state: u32) {
    let by_300 = |value: u32| {
        let high = (i64::from(value as i32) * i64::from(0x1b4e_81b5)) >> 32;
        ((high as i32) >> 5).wrapping_sub(value as i32 >> 31)
    };
    let mut count = m.lw(state.wrapping_add(0xc));
    if count as i32 >= 0x12d {
        count = 0x12c;
    }
    let climb = by_300(count.wrapping_mul(5).wrapping_shl(10)).wrapping_shl(6);
    let remaining = count.wrapping_sub(0x12c);
    let vy = m.lw(entity.wrapping_add(0x3c)).wrapping_mul(remaining);
    let vz = m.lw(entity.wrapping_add(0x40)).wrapping_mul(remaining);
    let y = m.lw(entity.wrapping_add(0xc));
    m.sw(entity.wrapping_add(0x3c), by_300(vy) as u32);
    m.sw(entity.wrapping_add(0x40), by_300(vz) as u32);
    if 0x0ea5_ffff < y as i32 {
        m.sw(entity.wrapping_add(0xc), 0x0ea6_0000);
    } else {
        m.sw(
            entity.wrapping_add(0xc),
            y.wrapping_add((climb >> 1) as u32),
        );
        let z = m.lw(entity.wrapping_add(0x10));
        m.sw(
            entity.wrapping_add(0x10),
            z.wrapping_add(climb.wrapping_shl(1) as u32),
        );
    }
    for i in 0..4 {
        let value = m.lw(entity.wrapping_add(0x38 + i * 4));
        m.sw(sp.wrapping_add(0x10 + i * 4), value);
    }
    m.sw(entity.wrapping_add(0x3c), climb as u32);
    aim_camera_8006d7dc(m, entity, state);
    let negated: [u32; 3] = std::array::from_fn(|i| {
        (0u32.wrapping_sub(m.lw(entity.wrapping_add(0x38 + i as u32 * 4))) as i32 >> 6) as u32
    });
    let speed = vector_length_800828f8(m, negated[0], negated[1], negated[2]);
    m.sw(state.wrapping_add(0x48), speed << 6);
    for i in 0..4 {
        let value = m.lw(sp.wrapping_add(0x10 + i * 4));
        m.sw(entity.wrapping_add(0x38 + i * 4), value);
    }
    let count = m.lw(state.wrapping_add(0xc)).wrapping_add(1);
    m.sw(state.wrapping_add(0xc), count);
    if m.lw(entity.wrapping_add(0xc)) as i32 <= 0 {
        return;
    }
    let timer = m.lhu(state.wrapping_add(0x4e)).wrapping_add(1);
    m.sh(state.wrapping_add(0x4e), timer);
    if timer as i16 >= 0x12d && m.lw(state.wrapping_add(4)) == 0x400_0000 {
        m.sh(entity.wrapping_add(0x4e), 0xffff);
        return;
    }
    if m.lh(state.wrapping_add(0x4e)) >= 0x1f && m.lw(state.wrapping_add(4)) == 0x800_0000 {
        let flags = m.lw(state);
        m.sw(state, flags | 0x200);
    }
}

/// 80076360: start the exit sequence `kind` in the player state
/// `[gp+0x520]`: 0 = word 0 0x10, mode 0x4000000, phase 0, +0x4E/+0x50
/// cleared; 1 = mode 0x8000000 and word 0 bit 0x40; other = mode
/// 0x10000000.
pub fn set_exit_mode_80074cf0(m: &mut Machine, kind: u32) {
    let state = m.gw(0x520);
    match kind {
        0 => {
            m.sw(state, 0x10);
            m.sw(state.wrapping_add(4), 0x400_0000);
            m.sw(state.wrapping_add(8), 0);
            m.sh(state.wrapping_add(0x4e), 0);
            m.sh(state.wrapping_add(0x50), 0);
        }
        1 => {
            let flags = m.lw(state);
            m.sw(state.wrapping_add(4), 0x800_0000);
            m.sw(state, flags | 0x40);
        }
        _ => m.sw(state.wrapping_add(4), 0x1000_0000),
    }
}

/// 800763CC: crash: the player state `[gp+0x520]` gets mode 0x100000,
/// phase 0 and word 0 bit 0x1000; then the reserved voices are released
/// (800301A8, called with the new word 0 in a0).
pub fn enter_crash_80074d5c(m: &mut Machine) {
    const FRAME: u32 = 0x18;
    m.push_frame(FRAME);
    let state = m.gw(0x520);
    let flags = m.lw(state) | 0x1000;
    m.sw(state.wrapping_add(4), 0x10_0000);
    m.sw(state.wrapping_add(8), 0);
    m.sw(state, flags);
    external(m, 0x8003_01a8, [flags, 0, 0, 0]);
    m.pop_frame(FRAME);
}

/// 80076428: landing approach check of the entity at `position` (entity
/// +8; angles at +0x10..+0x14) against the view record `view`: 0 unless
/// pitch is in -0x16C..0x2AA, |roll| < 0x556, x/z inside the outer box
/// (+0x28/+0x2C origin, +0x30/+0x34 size) and y not below +4 (y <= view
/// y); then 1 when x/z are also inside the inner box (+0x18/+0x1C, sizes
/// +0x20/+0x24) and the yaw, unwrapped against the view's yaw (+0x12),
/// is within 0x2AA of it, else 2.
pub fn approach_area_80074db8(m: &mut Machine, position: u32, view: u32) -> u32 {
    let pitch = i32::from(m.lh(position.wrapping_add(0x10)));
    let roll = i32::from(m.lh(position.wrapping_add(0x14)));
    if !(-0x16c..0x2ab).contains(&pitch) || magnitude(roll) >= 0x556 {
        return 0;
    }
    let x = m.lw(position) as i32;
    let y = m.lw(position.wrapping_add(4)) as i32;
    let z = m.lw(position.wrapping_add(8)) as i32;
    let left = m.lw(view.wrapping_add(0x28)) as i32;
    if x < left || left.wrapping_add(m.lw(view.wrapping_add(0x30)) as i32) < x {
        return 0;
    }
    let near = m.lw(view.wrapping_add(0x2c)) as i32;
    if z < near || near.wrapping_add(m.lw(view.wrapping_add(0x34)) as i32) < z {
        return 0;
    }
    if (m.lw(view.wrapping_add(4)) as i32) < y {
        return 0;
    }
    let mut result = 1;
    let left = m.lw(view.wrapping_add(0x18)) as i32;
    if x < left || left.wrapping_add(m.lw(view.wrapping_add(0x20)) as i32) < x {
        result = 2;
    }
    let near = m.lw(view.wrapping_add(0x1c)) as i32;
    if z < near || near.wrapping_add(m.lw(view.wrapping_add(0x24)) as i32) < z {
        result = 2;
    }
    let yaw = i32::from(m.lh(position.wrapping_add(0x12)));
    let reference = i32::from(m.lh(view.wrapping_add(0x12)));
    let yaw = h_unwrap(reference, yaw);
    if magnitude(reference.wrapping_sub(yaw)) >= 0x2ab {
        result = 2;
    }
    result
}

/// 800765B0: relative geometry of B (`b`: position +0..+8, angles
/// +0x10..+0x14; `b_offset` a VECTOR) seen from A (`a`, `a_offset`), into
/// `out` (the caller's stack word +0x10):
/// - +0x34..+0x38: B's angles unwrapped against A's, minus A's;
/// - +0..+8: A + (A's orientation x (`a_offset` >> 6)) << 6 (stored twice,
///   as the original does);
/// - +0x24..+0x2C: ((B - A) >> 6 + B's rotated offset - A's rotated offset)
///   << 6, with the rotations by 80030778 of each one's angles (MVMVA sf=1);
/// - +0x20: 80083F40 of that difference (>> 6), or 0x10000 when its summed
///   magnitude is above 0xB000. Returns that value.
pub fn relative_geometry_80074f40(
    m: &mut Machine,
    a: u32,
    a_offset: u32,
    b: u32,
    b_offset: u32,
    out: u32,
) -> u32 {
    const FRAME: u32 = 0xa0;
    let sp = m.push_frame(FRAME);
    let (a_matrix, b_matrix) = (sp.wrapping_add(0x10), sp.wrapping_add(0x30));
    let (a_vector, b_vector) = (sp.wrapping_add(0x50), sp.wrapping_add(0x58));
    let (a_rotated, b_rotated) = (sp.wrapping_add(0x60), sp.wrapping_add(0x70));
    let mut angles: [i32; 3] =
        std::array::from_fn(|i| i32::from(m.lh(b.wrapping_add(0x10 + i as u32 * 2))));
    let first = m.lhu(a.wrapping_add(0x10));
    for (i, angle) in angles.iter_mut().enumerate() {
        let reference = i32::from(m.lh(a.wrapping_add(0x10 + i as u32 * 2)));
        *angle = h_unwrap(reference, *angle);
    }
    m.sh(
        out.wrapping_add(0x34),
        (angles[0] as u16).wrapping_sub(first),
    );
    for (i, angle) in angles.iter().enumerate().skip(1) {
        let reference = m.lhu(a.wrapping_add(0x10 + i as u32 * 2));
        m.sh(
            out.wrapping_add(0x34 + i as u32 * 2),
            (*angle as u16).wrapping_sub(reference),
        );
    }
    crate::shared::f_80030534(m, [a.wrapping_add(0x10), a_matrix, 0, 0]);
    crate::shared::f_80030534(m, [b.wrapping_add(0x10), b_matrix, 0, 0]);
    for (source, vector) in [(a_offset, a_vector), (b_offset, b_vector)] {
        for i in 0..3 {
            let value = m.lw(source.wrapping_add(i * 4)) as i32 >> 6;
            m.sh(vector.wrapping_add(i * 2), value as u16);
        }
    }
    crate::sdk::set_rot_matrix(m, a_matrix);
    fm_rotate(m, a_vector, a_rotated);
    crate::sdk::set_rot_matrix(m, b_matrix);
    fm_rotate(m, b_vector, b_rotated);
    let mut delta: [i32; 3] = std::array::from_fn(|i| {
        let i = i as u32 * 4;
        (m.lw(b.wrapping_add(i))
            .wrapping_sub(m.lw(a.wrapping_add(i))) as i32)
            >> 6
    });
    for (i, value) in delta.iter_mut().enumerate() {
        let i = i as u32 * 4;
        let rotated = m
            .lw(b_rotated.wrapping_add(i))
            .wrapping_sub(m.lw(a_rotated.wrapping_add(i)));
        *value = value.wrapping_add(rotated as i32);
    }
    m.sw(out.wrapping_add(0x28), (delta[1] as u32) << 6);
    m.sw(out.wrapping_add(0x24), (delta[0] as u32) << 6);
    m.sw(out.wrapping_add(0x2c), (delta[2] as u32) << 6);
    for _ in 0..2 {
        for i in 0..3 {
            let rotated = m.lw(a_rotated.wrapping_add(i * 4)) << 6;
            let origin = m.lw(a.wrapping_add(i * 4));
            m.sw(out.wrapping_add(i * 4), rotated.wrapping_add(origin));
        }
    }
    let delta = delta.map(|value| value >> 6);
    let size = magnitude(delta[0])
        .wrapping_add(magnitude(delta[1]))
        .wrapping_add(magnitude(delta[2]));
    let distance = if 0xb000 < size {
        0x1_0000
    } else {
        vector_length_800828f8(m, delta[0] as u32, delta[1] as u32, delta[2] as u32)
    };
    m.sw(out.wrapping_add(0x20), distance);
    m.pop_frame(FRAME);
    distance
}

/// 80076924: formation check of `entity` against the target pair:
/// `target` word 0 is the target entity's index (table `[gp+0x2E8]`),
/// `target` +4 its offset VECTOR, `aux` the entity's offset. 800765B0
/// (target +8, `target` +4, entity +8, `aux`, `out`) fills `out`; returns
/// 1 when the three angle differences (+0x34..+0x38) are within 0x16C and
/// the relative position (+0x24/+0x28/+0x2C) within 0x32000/0x50000/0x96000
/// (magnitudes), else 0.
pub fn formation_check_800752b4(
    m: &mut Machine,
    entity: u32,
    target: u32,
    aux: u32,
    out: u32,
) -> u32 {
    const FRAME: u32 = 0x20;
    let sp = m.push_frame(FRAME);
    let index = m.lw(target);
    let table = m.gw(crate::common::ENTITIES);
    let other = m.lw(index.wrapping_shl(2).wrapping_add(table));
    // The fifth argument of 800765B0 at the original stack offset.
    m.sw(sp.wrapping_add(0x10), out);
    relative_geometry_80074f40(
        m,
        other.wrapping_add(8),
        target.wrapping_add(4),
        entity.wrapping_add(8),
        aux,
        out,
    );
    let mut aligned =
        (0..3).all(|i| magnitude(i32::from(m.lh(out.wrapping_add(0x34 + i * 2)))) < 0x16d);
    for (offset, limit) in [(0x24, 0x3_2000), (0x28, 0x5_0000), (0x2c, 0x9_6000)] {
        if limit < magnitude(m.lw(out.wrapping_add(offset)) as i32) {
            aligned = false;
        }
    }
    m.pop_frame(FRAME);
    u32::from(aligned)
}

/// 80076A38: 1 when the entity at `position` (entity +8) is outside the
/// view record's inner box (x/z: origin +0x18/+0x1C, sizes +0x20/+0x24) or
/// more than 0x64000 above the view's y (+4), else 0.
pub fn outside_area_800753c8(m: &mut Machine, position: u32, view: u32) -> u32 {
    let x = m.lw(position) as i32;
    let left = m.lw(view.wrapping_add(0x18)) as i32;
    let width = m.lw(view.wrapping_add(0x20)) as i32;
    let mut outside = x < left || left.wrapping_add(width) < x;
    let z = m.lw(position.wrapping_add(8)) as i32;
    let near = m.lw(view.wrapping_add(0x1c)) as i32;
    if z < near || near.wrapping_add(m.lw(view.wrapping_add(0x24)) as i32) < z {
        outside = true;
    }
    let height = m
        .lw(position.wrapping_add(4))
        .wrapping_sub(m.lw(view.wrapping_add(4)));
    if (height as i32) < -0x6_4000 {
        outside = true;
    }
    u32::from(outside)
}

/// 80076AC0: 1 when the entity at `position` (entity +8) has y >= 0, pitch
/// (+0x10) within 0xE3 of 0x71C and |roll| (+0x14) < 0x1C8, else 0. (The
/// caller's second argument, the view record, is not read.)
pub fn exit_attitude_80075450(m: &mut Machine, position: u32) -> u32 {
    if (m.lw(position.wrapping_add(4)) as i32) < 0 {
        return 0;
    }
    let pitch = i32::from(m.lh(position.wrapping_add(0x10)));
    let roll = i32::from(m.lh(position.wrapping_add(0x14)));
    if magnitude(pitch - 0x71c) >= 0xe4 {
        return 0;
    }
    u32::from(magnitude(roll) < 0x1c8)
}

/// Copy the view record (`[0x800BCE18]`: position +0..+8, angles
/// +0x10..+0x14) into the block `[gp+0x52C]` (80076B18).
fn h_copy_view(m: &mut Machine) {
    let view = m.lw(VIEW);
    let copy = m.gw(H_VIEW_COPY);
    for i in 0..3 {
        let value = m.lw(view.wrapping_add(i * 4));
        m.sw(copy.wrapping_add(i * 4), value);
    }
    for i in 0..3 {
        let value = m.lhu(view.wrapping_add(0x10 + i * 2));
        m.sh(copy.wrapping_add(0x10 + i * 2), value);
    }
}

/// 80076B18: enter the landing-approach mode `kind` * 2 + `variant` of
/// the table at 0x800246E0 (8, 8, 0x10, 0x10, 0x20, 0x40): mode 8 sets
/// state word 0 to 4, 0x10 sets bit 0x40, 0x20/0x40 set bit 0x200 and clear
/// 0x40. The view record goes to `[gp+0x52C]`; state +8 = 0, +4 = the mode.
pub fn set_approach_mode_800754a8(m: &mut Machine, variant: u32, kind: u32) {
    const FRAME: u32 = 0x18;
    let sp = m.push_frame(FRAME);
    let index = kind.wrapping_shl(1).wrapping_add(variant);
    let mode = h_table_entry(m, H_APPROACH_MODES, 6, sp, index);
    let state = m.gw(0x520);
    if mode == 8 {
        m.sw(state, 4);
    }
    if mode == 0x10 {
        let flags = m.lw(state);
        m.sw(state, flags | 0x40);
    } else if mode == 0x20 || mode == 0x40 {
        let flags = m.lw(state);
        m.sw(state, (flags | 0x200) & !0x40);
    }
    h_copy_view(m);
    let state = m.gw(0x520);
    m.sw(state.wrapping_add(8), 0);
    m.sw(state.wrapping_add(4), mode);
    m.pop_frame(FRAME);
}

/// 80076C54: enter the target-view mode `kind` * 2 + `variant` of the
/// table at 0x800246F8 (0x100, 0x100, 0x200, 0x200, 0x400, 0x400, 0x800,
/// 0x1000): 0x100 sets state word 0 to 8; 0x200/0x400/0x1000 set bit 0x40,
/// 0x800 clears it; 0x800/0x1000 set bit 0x200. The block `[gp+0x52C]` is
/// cleared (+0..+8, +0x10..+0x14); state +8 = 0, +4 = the mode.
pub fn set_target_mode_800755e4(m: &mut Machine, variant: u32, kind: u32) {
    const FRAME: u32 = 0x20;
    let sp = m.push_frame(FRAME);
    let index = kind.wrapping_shl(1).wrapping_add(variant);
    let mode = h_table_entry(m, H_TARGET_MODES, 8, sp, index);
    let state = m.gw(0x520);
    if mode == 0x100 {
        m.sw(state, 8);
    }
    if mode == 0x200 || mode == 0x400 || mode == 0x1000 {
        let flags = m.lw(state);
        m.sw(state, flags | 0x40);
    } else if mode == 0x800 {
        let flags = m.lw(state);
        m.sw(state, flags & !0x40);
    }
    if mode == 0x800 || mode == 0x1000 {
        let flags = m.lw(state);
        m.sw(state, flags | 0x200);
    }
    let copy = m.gw(H_VIEW_COPY);
    let state = m.gw(0x520);
    for offset in [0, 4, 8] {
        m.sw(copy.wrapping_add(offset), 0);
    }
    for offset in [0x10, 0x12, 0x14] {
        m.sh(copy.wrapping_add(offset), 0);
    }
    m.sw(state.wrapping_add(8), 0);
    m.sw(state.wrapping_add(4), mode);
    m.pop_frame(FRAME);
}

/// 80076DEC: enter the start mode `kind` * 2 + `variant` of the table at
/// 0x80024718 (0x4000, 0x8000, 0x10000, 0x10000, 4, 4). 0x4000/0x8000 set
/// the voice state +0x52 to -1; state word 0 = 2; 0x10000 makes word 0 0x42
/// and releases the voice (8002DDA8 when +0x52 is not negative); 4 runs
/// 80076274 (word 0 = 1, mode 1), sets bits 0x280 and releases the voice.
/// Then state +4 = the mode, +8 = 0.
pub fn set_start_mode_8007577c(m: &mut Machine, variant: u32, kind: u32) {
    const FRAME: u32 = 0x30;
    let sp = m.push_frame(FRAME);
    let index = kind.wrapping_shl(1).wrapping_add(variant);
    let mode = h_table_entry(m, H_START_MODES, 6, sp.wrapping_add(0x10), index);
    if mode == 0x4000 || mode == 0x8000 {
        let state = m.gw(0x520);
        m.sh(state.wrapping_add(0x52), 0xffff);
    }
    let state = m.gw(0x520);
    m.sw(state, 2);
    if mode == 0x1_0000 {
        m.sw(state, 0x42);
        h_release_voice(m);
    } else if mode == 4 {
        external(m, 0x8007_6274, [mode, 0, 0, 0]);
        let state = m.gw(0x520);
        let flags = m.lw(state);
        m.sw(state, flags | 0x280);
        h_release_voice(m);
    }
    let state = m.gw(0x520);
    m.sw(state.wrapping_add(4), mode);
    m.sw(state.wrapping_add(8), 0);
    m.pop_frame(FRAME);
}

/// 80079564: crash camera: 800793C4 (the camera record `[[gp+0x2E0]+0x28]`,
/// camera mode `which` of the table at 0x8002474C (0x10, 0x20, 0x40), the
/// player entity), then 80035740(0).
pub fn crash_camera_80077ef4(m: &mut Machine, which: u32) {
    const FRAME: u32 = 0x28;
    let sp = m.push_frame(FRAME);
    let mode = h_table_entry(m, H_CAMERA_MODES, 3, sp.wrapping_add(0x10), which);
    let gameplay = m.gw(crate::common::GAMEPLAY);
    let camera = m.lw(gameplay.wrapping_add(0x28));
    let entities = m.gw(crate::common::ENTITIES);
    let player = m.lw(entities);
    external(m, 0x8007_93c4, [camera, mode, player, 0]);
    external(m, 0x8003_5740, [0, 0, 0, 0]);
    m.pop_frame(FRAME);
}
// ==== end of h ====
// ==== j: ground-unit motion 800522B0 and its direction helper 80052D70 ====
//
// 80051A54 calls 800522B0 once per ground unit in play (unit 0..31, entity
// slot 0x20 + unit). The unit's 0x28-byte record at `[gp+0x388]` holds the
// definition pointer at +0x08 and the motion record at +0x14; the unit entity
// is `[[gp+0x2D8] + unit*4]` and its slot state word is 800C6FD0 slot
// 0x20 + unit.

/// MIPS `div` low word behind the compiler's checks: `break 7` at `check`
/// (divisor 0) and `break 6` at `check + 0x18` (`0x80000000 / -1`) record
/// `Fault::Trap` (no continuation is translated) and the routine goes on
/// with the value the division leaves in LO.
fn j_divide(m: &mut Machine, dividend: u32, divisor: u32, check: u32) -> u32 {
    if divisor == 0 {
        m.raise(crate::machine::Fault::Trap {
            address: check,
            code: 7,
        });
    } else if divisor == u32::MAX && dividend == 0x8000_0000 {
        m.raise(crate::machine::Fault::Trap {
            address: check.wrapping_add(0x18),
            code: 6,
        });
    }
    div_quotient(dividend, divisor)
}

/// 80052D70: direction and distance from the VECTOR at `from` to the one at
/// `to`. With d = to - from (wrapping) and |d| per axis (`subu zero`, so
/// -0x80000000 stays negative): when every |d| < 0x1000 (signed), `out` = 0
/// and the result is 0. Otherwise the |d| are halved (arithmetic shifts) `s`
/// times until all are < 0x4000, `len` = SquareRoot0(sum of their squares,
/// `mult` low words) and `out[i]` = `((d[i] >> s) << 14) / len` (`div`;
/// 1.0 = 0x4000); the result is `len << s`. Frame 0x40 holds only locals.
pub fn direction_8005173c(m: &mut Machine, from: u32, to: u32, out: u32) -> u32 {
    let mut delta = [0u32; 3];
    for (i, d) in (0u32..).zip(delta.iter_mut()) {
        let b = m.lw(to.wrapping_add(i * 4));
        let a = m.lw(from.wrapping_add(i * 4));
        *d = b.wrapping_sub(a);
    }
    let mut size = delta.map(|d| (d as i32).wrapping_abs());
    if size.iter().all(|&s| s < 0x1000) {
        for i in 0..3 {
            m.sw(out.wrapping_add(i * 4), 0);
        }
        return 0;
    }
    let mut shift = 0u32;
    while !size.iter().all(|&s| s < 0x4000) {
        shift += 1;
        for s in &mut size {
            *s >>= 1;
        }
    }
    let squares = size
        .iter()
        .fold(0i32, |sum, &s| sum.wrapping_add(s.wrapping_mul(s)));
    let length = crate::sdk::square_root0(m, squares as u32);
    let checks = [0x8005_2ed4u32, 0x8005_2f2c, 0x8005_2f6c];
    for (i, (d, check)) in (0u32..).zip(delta.into_iter().zip(checks)) {
        let scaled = (((d as i32) >> shift) as u32) << 14;
        let quotient = j_divide(m, scaled, length, check);
        m.sw(out.wrapping_add(i * 4), quotient);
    }
    length << shift
}

// JP-REVIEW(R0311): near function: PAL 80050C84 -> JP 800522B0, del:9 imm:13 ins:11 op:3, 36 instructions differ (whole routine)
/// 800522B0: motion of ground unit `unit` (a0). Motion record P =
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
///   stage 0x2B the arrival in phase 3 runs event scripts 0x148/0x14C once.
/// - 5: patrol between waypoints 1 and 2 (phase 0/1).
/// - 6: vertical approach to P+0x10 then angle stages and a run to
///   waypoint 1 (script 0x148 once on arrival, mission flag 0x40).
/// - 7/8: slot state bit 0x20000 when the player is inside a fixed box /
///   when the unit is less than P+0x0C above the player, unless E+5 has 0x4.
/// - 9: placed at the anchor (800B2078) nearest to the player; unit 0 also
///   runs the anchor timers and the 8008FC9C overlay.
///
/// The result (`v0`) is not used by the caller.
// JP-PATCH: replaced by `crate::jp::res_p1_player::unit_motion_80050c84` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn unit_motion_80050c84(m: &mut Machine, unit: u32) {
    crate::jp::res_p1_player::unit_motion_80050c84(m, unit)
}

// ==== end of j ====
// ==== a2: ground units (80051A54 subtree) and the wreck/lock/offset leaves ====
//
// Ground unit `unit` (0..31) is entity slot `unit + 0x20`, its entity pointer
// is `[[gp+0x2D8] + unit*4]` and its 0x28-byte record is at
// `[gp+0x388] + unit*0x28` (+0x10 attack parameters, +0x14 kind block, +0x18
// behaviour descriptor, +0x1C destruction script).

/// a2: the record of ground unit `unit` (`[gp+0x388] + unit * 0x28`).
fn a2_unit_record(m: &mut Machine, unit: u32) -> u32 {
    m.gw(0x388).wrapping_add(unit.wrapping_mul(0x28))
}

/// a2: the entity of ground unit `unit` (`[[gp+0x2D8] + unit * 4]`).
fn a2_unit_entity(m: &mut Machine, unit: u32) -> u32 {
    let units = m.gw(UNITS_2C8);
    m.lw(units.wrapping_add(unit.wrapping_mul(4)))
}

/// a2: address of the state word of entity slot `slot` (800C6FD0).
fn a2_state(slot: u32) -> u32 {
    SLOT_STATE_4D98.wrapping_add(slot.wrapping_mul(4))
}

/// a2: clear state bit 0x20000 (motion script running) of `slot`.
fn a2_stop_script(m: &mut Machine, slot: u32) {
    let state = a2_state(slot);
    let bits = m.lw(state) & 0xfffd_ffff;
    m.sw(state, bits);
}

/// 80038320: wreck entity `slot`: entity flags lose 0x200, 0x400, 0x8000 and
/// 0x10000, the state word keeps only its top byte and gains bit 0x4 (hit
/// timer), and the hit timer byte (800C70D0) becomes 0x96.
pub fn wreck_slot_80038010(m: &mut Machine, slot: u32) {
    let entity = slot_entity(m, slot);
    let flags = m.lw(entity) & !0x200 & !0x400 & 0xffff_7fff;
    m.sw(entity, flags);
    let state = a2_state(slot);
    let bits = m.lw(state) & 0xff00_0000;
    m.sw(state, bits);
    let flags = m.lw(entity) & 0xfffe_ffff;
    m.sw(entity, flags);
    let bits = m.lw(state) | 4;
    m.sw(state, bits);
    m.sb(HIT_TIMER_4E98.wrapping_add(slot), 0x96);
}

/// 8004651C: release the lock slot of entity `slot` (800C6D80): byte 0 = 0,
/// its two attacker bytes (+2, +3) become 0xFF and each attacker's target
/// byte (+1 of its own slot) 0xFF; then 800464B0(`slot`). Returns 0xFF.
pub fn release_lock_800458e0(m: &mut Machine, slot: u32) -> u32 {
    let entry = LOCK_SLOTS_4B48.wrapping_add(slot.wrapping_mul(4));
    let first = m.lbu(entry.wrapping_add(2));
    m.sb(entry, 0);
    m.sb(entry.wrapping_add(2), 0xff);
    if first != 0xff {
        m.sb(LOCK_SLOTS_4B48 + u32::from(first) * 4 + 1, 0xff);
    }
    let second = m.lbu(entry.wrapping_add(3));
    m.sb(entry.wrapping_add(3), 0xff);
    if second != 0xff {
        m.sb(LOCK_SLOTS_4B48 + u32::from(second) * 4 + 1, 0xff);
    }
    external(m, 0x8004_64b0, [slot, 0, 0, 0]);
    0xff
}

/// 800451AC: `out` = `offset` (VECTOR) rotated by the matrix 80030778
/// builds from the angles at `base + 0x10` (ApplyMatrixLV), plus the
/// position at `base`. Frame 0x40: the matrix at sp+0x10.
pub fn place_offset_80044664(m: &mut Machine, base: u32, offset: u32, out: u32) {
    const FRAME: u32 = 0x40;
    let sp = m.push_frame(FRAME);
    let matrix = sp.wrapping_add(0x10);
    crate::shared::f_80030534(m, [base.wrapping_add(0x10), matrix, 0, 0]);
    crate::sdk::apply_matrix_lv(m, matrix, offset, out);
    let x = m.lw(out).wrapping_add(m.lw(base));
    m.sw(out, x);
    let y = m
        .lw(out.wrapping_add(4))
        .wrapping_add(m.lw(base.wrapping_add(4)));
    let z = m.lw(out.wrapping_add(8));
    m.sw(out.wrapping_add(4), y);
    let z = z.wrapping_add(m.lw(base.wrapping_add(8)));
    m.sw(out.wrapping_add(8), z);
    m.pop_frame(FRAME);
}

// JP-REVIEW(R0329): near function: PAL 80050428 -> JP 80051A54, del:1 imm:3 op:2, 6 instructions differ (whole routine)
/// 80051A54: per-frame update of ground unit `unit` (called by 80051414 for
/// units with entity flags 0xC000): the motion script (80051BA0) while state
/// bit 0x20000 is set; a unit out of play only checks for its wreck
/// (80052060). Otherwise a looping descriptor (byte 1 = 1, byte 3 set)
/// re-arms bit 0x20000, destruction is handled (800520C8) and, still in
/// play, the behaviour (800522B0) and in gameplay state 4 with attack
/// parameters the attack (80052FA8) run.
pub fn update_unit_80050428(m: &mut Machine, unit: u32) {
    let record = a2_unit_record(m, unit);
    let slot = unit.wrapping_add(0x20);
    let state = a2_state(slot);
    let bits = m.lw(state);
    let descriptor = m.lw(record.wrapping_add(0x18));
    if bits & 0x2_0000 != 0 {
        step_unit_script_80050578(m, unit);
    }
    let bits = m.lw(state);
    if bits & 1 == 0 {
        check_unit_wreck_80050a30(m, unit);
        return;
    }
    if descriptor != 0
        && m.lbu(descriptor.wrapping_add(1)) == 1
        && m.lbu(descriptor.wrapping_add(3)) != 0
    {
        m.sw(state, bits | 0x2_0000);
    }
    destroy_unit_80050a98(m, unit);
    if m.lw(state) & 1 == 0 {
        return;
    }
    unit_motion_80050c84(m, unit);
    let gameplay = m.gw(crate::common::GAMEPLAY);
    if m.lw(gameplay.wrapping_add(4)) != 4 {
        return;
    }
    if m.lw(record.wrapping_add(0x10)) == 0 {
        return;
    }
    unit_attack_80051974(m, unit);
}

// JP-REVIEW(R0330): near function: PAL 80050578 -> JP 80051BA0, del:2 ins:2 op:1, 5 instructions differ (whole routine)
/// 80051BA0: step the motion script of ground unit `unit` (descriptor at
/// record +0x18: byte 2 end mode, byte 3 step count, halfword durations from
/// +4). The entity's step (+0x59, signed) and time (+0x60) advance by the
/// rate (+0x68, negative runs backwards); +6 = (time << 12) / duration +
/// (step << 12). At the last step the end mode applies: 1 stop, 2 stop and
/// wreck (80051F88), 3 restart, others reverse from the second-to-last step
/// (rate -1); running past the first step: 4 stop, 5 stop and wreck, others
/// restart forward (rate 1). A missing descriptor reads byte 2 of address 0,
/// as the original does.
pub fn step_unit_script_80050578(m: &mut Machine, unit: u32) {
    let units = m.gw(UNITS_2C8);
    let record = a2_unit_record(m, unit);
    let descriptor = m.lw(record.wrapping_add(0x18));
    let entity = m.lw(units.wrapping_add(unit.wrapping_mul(4)));
    let slot = unit.wrapping_add(0x20);
    if descriptor == 0 && m.lbu(descriptor.wrapping_add(2)) == 0 {
        return;
    }
    let duration_of = |m: &mut Machine, step: i8| {
        let at = descriptor.wrapping_add((i32::from(step) as u32).wrapping_mul(2));
        i32::from(m.lh(at.wrapping_add(4)))
    };
    let flags = m.lbu(entity.wrapping_add(5)) | 4;
    let step = m.lb(entity.wrapping_add(0x59));
    let time = m.lw(entity.wrapping_add(0x60));
    m.sb(entity.wrapping_add(5), flags);
    let duration = duration_of(m, step);
    let quotient = fm_div((time << 12) as i32, duration);
    let rate = m.lw(entity.wrapping_add(0x68));
    let time = m.lw(entity.wrapping_add(0x60)).wrapping_add(rate);
    let current = m.lb(entity.wrapping_add(0x59));
    m.sw(entity.wrapping_add(0x60), time);
    let progress = (quotient as u32).wrapping_add((i32::from(step) as u32) << 12);
    m.sh(entity.wrapping_add(6), progress as u16);
    let duration = duration_of(m, current);
    let index = m.lbu(entity.wrapping_add(0x59));
    if duration < time as i32 {
        m.sw(
            entity.wrapping_add(0x60),
            (time as i32).wrapping_sub(duration) as u32,
        );
        m.sb(entity.wrapping_add(0x59), index.wrapping_add(1));
    } else if (time as i32) < 0 {
        let previous = index.wrapping_sub(1);
        m.sb(entity.wrapping_add(0x59), previous);
        if previous as i8 >= 0 {
            let duration = duration_of(m, previous as i8);
            m.sw(entity.wrapping_add(0x60), duration as u32);
        }
    }
    let count = m.lbu(descriptor.wrapping_add(3));
    let step = m.lb(entity.wrapping_add(0x59));
    if i32::from(step) as u32 == u32::from(count).wrapping_sub(1) {
        match m.lbu(descriptor.wrapping_add(2)) {
            1 => a2_stop_script(m, slot),
            2 => {
                a2_stop_script(m, slot);
                wreck_unit_80050958(m, unit);
            }
            3 => {
                m.sb(entity.wrapping_add(0x59), 0);
                m.sw(entity.wrapping_add(0x60), 0);
            }
            _ => {
                let last = u32::from(count).wrapping_sub(2) as u8;
                m.sb(entity.wrapping_add(0x59), last);
                let duration = duration_of(m, last as i8);
                m.sw(entity.wrapping_add(0x68), u32::MAX);
                m.sw(entity.wrapping_add(0x60), duration as u32);
            }
        }
    }
    if m.lb(entity.wrapping_add(0x59)) >= 0 {
        return;
    }
    let mut mode = m.lbu(descriptor.wrapping_add(2));
    if mode == 4 {
        a2_stop_script(m, slot);
        mode = m.lbu(descriptor.wrapping_add(2));
    }
    if mode == 5 {
        a2_stop_script(m, slot);
        wreck_unit_80050958(m, unit);
        return;
    }
    m.sb(entity.wrapping_add(0x59), 0);
    m.sw(entity.wrapping_add(0x60), 0);
    m.sw(entity.wrapping_add(0x68), 1);
}

/// 80051F88: wreck ground unit `unit`: entity flags lose 0x10000, 0x8000 and
/// 0x400; the slot state keeps its top byte and gains bit 0x4 (hit timer);
/// the hit timer byte (800C70D0) becomes 0x96.
pub fn wreck_unit_80050958(m: &mut Machine, unit: u32) {
    let entity = a2_unit_entity(m, unit);
    let slot = unit.wrapping_add(0x20);
    let flags = m.lw(entity) & 0xfffe_ffff & 0xffff_7fff & !0x400;
    m.sw(entity, flags);
    let state = a2_state(slot);
    let bits = (m.lw(state) & 0xff00_0000) | 4;
    m.sw(state, bits);
    m.sb(HIT_TIMER_4E98.wrapping_add(slot), 0x96);
}

/// 80052060: a unit out of play whose kind block (record +0x14) byte 0 is 6
/// and whose entity byte +0x58 is 0 is wrecked (80051F88).
pub fn check_unit_wreck_80050a30(m: &mut Machine, unit: u32) {
    let record = a2_unit_record(m, unit);
    let units = m.gw(UNITS_2C8);
    let kind = m.lw(record.wrapping_add(0x14));
    let kind = m.lbu(kind);
    let entity = m.lw(units.wrapping_add(unit.wrapping_mul(4)));
    if kind == 6 && m.lb(entity.wrapping_add(0x58)) == 0 {
        wreck_unit_80050958(m, unit);
    }
}

// JP-REVIEW(R0331): near function: PAL 800509D0 -> JP 80052000, op:1, 1 instructions differ (whole routine)
/// 80052000: wreck effect of ground unit `unit`: descriptor (record +0x18)
/// byte 0, when set, selects effect `(byte & 0x7F) - 1` at the entity
/// position (8008130C).
pub fn unit_wreck_effect_800509d0(m: &mut Machine, unit: u32) {
    let units = m.gw(UNITS_2C8);
    let record = a2_unit_record(m, unit);
    let descriptor = m.lw(record.wrapping_add(0x18));
    let entity = m.lw(units.wrapping_add(unit.wrapping_mul(4)));
    if descriptor == 0 {
        return;
    }
    let effect = m.lbu(descriptor);
    if effect == 0 {
        return;
    }
    crate::effects::effect_8007fcc4(
        m,
        entity.wrapping_add(8),
        u32::from(effect & 0x7f).wrapping_sub(1),
    );
}

/// 80048AD4: effect list `index`: the 8-byte entries from
/// `[gp+0x36C] + [[gp+0x3A8] + index] * 8` up to a zero halfword each call
/// 8005E460(byte +2, byte +3, halfword +6).
pub fn set_effects_80047dcc(m: &mut Machine, index: u32) {
    let table = m.gw(0x3a8);
    let list = u32::from(m.lbu(table.wrapping_add(index)));
    let mut entry = m.gw(0x36c).wrapping_add(list << 3);
    while m.lh(entry) != 0 {
        let first = u32::from(m.lbu(entry.wrapping_add(2)));
        let second = u32::from(m.lbu(entry.wrapping_add(3)));
        let value = i32::from(m.lh(entry.wrapping_add(6))) as u32;
        external(m, 0x8005_e460, [first, second, value, 0]);
        entry = entry.wrapping_add(8);
    }
}

// JP-REVIEW(R0332): near function: PAL 80050A98 -> JP 800520C8, del:4 imm:7 ins:3 op:4, 18 instructions differ (whole routine)
/// 800520C8: destruction of ground unit `unit` once it is in play and has no
/// hit points left: entity flag 0x200 cleared; statistics +0x20/+0x21 for
/// state bits 0x100/0x200; with state bit 0x800 and no attacker (+0x50) an
/// unflagged (0x40) unit focuses the kill camera (800797FC, context +0x3A1
/// bit 0) and shows message 0x2E (stage 0x16 objects) or 7 (flag 0x80)
/// through 8004CEB0; then the kill log (80048758), state = top byte | 2,
/// the destroyed state (80051E34) and the destruction script (record +0x1C,
/// 80054EBC).
// JP-PATCH: replaced by `crate::jp::res_p1_player::destroy_unit_80050a98` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn destroy_unit_80050a98(m: &mut Machine, unit: u32) {
    crate::jp::res_p1_player::destroy_unit_80050a98(m, unit)
}

// JP-REVIEW(R0333): near function: PAL 8007818C -> JP 800797FC, imm:1, 1 instructions differ (whole routine)
/// 800797FC: kill camera on entity `id` (an entity's first word: bit 31
/// none, low 6 bits the slot) when the effects block (`[800BD094]` word 0
/// bit 0) and context +0x3A1 allow it and the entity has flag 0x4000: the
/// camera (`[gameplay +0x28]`) +0xC0 = slot, 800793C4(camera, 0x800,
/// player entity), 80035740(0).
// JP-PATCH: replaced by `crate::jp::res_p1_player::focus_kill_camera_8007818c` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn focus_kill_camera_8007818c(m: &mut Machine, id: u32) {
    crate::jp::res_p1_player::focus_kill_camera_8007818c(m, id)
}

// JP-REVIEW(R0334): near function: PAL 8005080C -> JP 80051E34, ins:2 op:1, 3 instructions differ (whole routine)
/// 80051E34: destroyed state of ground unit `unit`: state bit 0x20000
/// cleared, entity +6 = 0, byte +5 loses bit 0x4, +0x54 loses bit 31. With a
/// descriptor (record +0x18): byte +0xE 0xFE wrecks the unit (80051F88),
/// another value except 0xFF becomes the entity model (+4, flag 0x10000);
/// byte +0xF selects an effect list (80048AD4); a looping descriptor (byte
/// 1 = 3, byte 3 set) sets state bit 0x20000; then the wreck effect
/// (80052000).
pub fn unit_destroyed_8005080c(m: &mut Machine, unit: u32) {
    let slot = unit.wrapping_add(0x20);
    let state = a2_state(slot);
    let units = m.gw(UNITS_2C8);
    let bits = m.lw(state) & 0xfffd_ffff;
    let entity = m.lw(units.wrapping_add(unit.wrapping_mul(4)));
    m.sw(state, bits);
    let flags = m.lbu(entity.wrapping_add(5));
    let control = m.lw(entity.wrapping_add(0x54));
    m.sh(entity.wrapping_add(6), 0);
    m.sb(entity.wrapping_add(5), flags & 0xfb);
    let record = a2_unit_record(m, unit);
    m.sw(entity.wrapping_add(0x54), control & 0x7fff_ffff);
    let descriptor = m.lw(record.wrapping_add(0x18));
    if descriptor == 0 {
        return;
    }
    let model = m.lbu(descriptor.wrapping_add(0xe));
    if model == 0xfe {
        wreck_unit_80050958(m, unit);
    } else if model != 0xff {
        let flags = m.lw(entity) | 0x1_0000;
        m.sw(entity, flags);
        let model = m.lbu(descriptor.wrapping_add(0xe));
        m.sb(entity.wrapping_add(4), model);
    }
    let effects = m.lbu(descriptor.wrapping_add(0xf));
    if effects != 0xff {
        set_effects_80047dcc(m, u32::from(effects));
    }
    if m.lbu(descriptor.wrapping_add(1)) == 3 && m.lbu(descriptor.wrapping_add(3)) != 0 {
        let bits = m.lw(state) | 0x2_0000;
        m.sw(state, bits);
    }
    unit_wreck_effect_800509d0(m, unit);
}

// JP-REVIEW(R0335): near function: PAL 80047A0C -> JP 80048758, del:2 imm:9 ins:2 op:4 reg:6, 23 instructions differ (whole routine)
/// 80048758: kill log entry for entity `slot` (8 bytes at context +0x74C,
/// count at context +0x3C6): kind byte (+0x48); with state bit 0x800 (kind
/// 0x3F only after 4 such kills on stages 0x11/0x1F/0x26/0x2E/0x34) a label
/// (0x74 unflagged, 0x72/0x73 for kinds 0x40/0x41, 0x70/0x71 by state bit
/// 0x400 for kinds above), the class (+1) with its statistics counter, the
/// slot (+6), map bytes of x and z (+4/+5) and the flight-log call count / 30
/// (+2); the count then increments.
// JP-PATCH: replaced by `crate::jp::res_p1_player::record_kill_80047a0c` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn record_kill_80047a0c(m: &mut Machine, slot: u32) {
    crate::jp::res_p1_player::record_kill_80047a0c(m, slot)
}

// JP-REVIEW(R0336): near function: PAL 80051974 -> JP 80052FA8, del:3 imm:2 ins:4, 9 instructions differ (whole routine)
/// 80052FA8: attack step of ground unit `unit` with attack parameters
/// (record +0x10: byte 0 kind, 0x10 = missiles; word +4 range): stage 0xC
/// kind-0x46 units only outside four x bands; a unit without a target
/// (+0x5B = 0xFF) picks one (80046124; flag 0x80 falls back to the player).
/// The distance is the unit's sight-record distance for the player, else
/// the Manhattan distance (>> 12) to the target (a target out of play is
/// dropped to 0xFF). Out of range the target is released (800464B0);
/// otherwise the gun (80053214) or missile (800535D4) step runs and a
/// result of 1 (target out of the firing cone or busy) releases it too.
/// Returns that result (0 when nothing fired).
// JP-PATCH: replaced by `crate::jp::res_p1_player::unit_attack_80051974` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn unit_attack_80051974(m: &mut Machine, unit: u32) -> u32 {
    crate::jp::res_p1_player::unit_attack_80051974(m, unit)
}

// JP-REVIEW(R0337): near function: PAL 800454EC -> JP 80046124, del:3 imm:15 ins:4, 22 instructions differ (whole routine)
/// 80046124: pick a target for entity `slot` (only on its frame,
/// `gameplay +0xC & 0x3F == slot`, else 0xFF): the player (0) when the
/// entity has flag 0x80, the player's attacker bytes (lock slot 0 +2, +3
/// on harder levels) have room and the player is not targeting it; else the
/// nearest (80083DFC, within 0x100000 and per axis) slot 2..63 in play with
/// flag 0x4000, no state bit 0x800000, another side (flags & 0x1C0, not
/// 0x100), a free attacker byte, not already attacking this entity, and
/// for side 0x40 no state bit 0x100 except on stage 0x1E. Links the choice
/// through 800463B8(`slot`, target) and returns its `v0`.
// JP-PATCH: replaced by `crate::jp::res_p1_player::choose_target_800454ec` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn choose_target_800454ec(m: &mut Machine, slot: u32) -> u32 {
    crate::jp::res_p1_player::choose_target_800454ec(m, slot)
}

// JP-REVIEW(R0338): near function: PAL 80051BDC -> JP 80053214, imm:1, 1 instructions differ (whole routine)
/// 80053214: gun step of ground unit `unit` at `distance` (a1) from its
/// target: aim from the own position (copied to +0x6C) at the target
/// position (copied to +0x84) led by `t = distance / 144` (reciprocal
/// multiply): + velocity * t, y - 669 * t * t / 2; angles into +0x7C
/// (80031BB4, vector at sp+0x10 of the 0x38 frame), jittered by
/// `rand() % (2s) - s` with s = 0x258 (easy level against the player's side,
/// or descriptor byte 1 other than 0/1), 0xC8 (byte 1 = 1) or 0. Out of the
/// firing cone: returns 1. Otherwise a burst: every attack +0x10 frames of
/// the counter (+0x64) a bullet of kind byte 0 - 1 (8006B034) until +0x12,
/// then a pause (+0x5A = 1) until +0x14. Returns 0.
// JP-PATCH: replaced by `crate::jp::res_p1_player::gun_attack_80051bdc` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn gun_attack_80051bdc(m: &mut Machine, unit: u32, distance: u32) -> u32 {
    crate::jp::res_p1_player::gun_attack_80051bdc(m, unit, distance)
}

// JP-REVIEW(R0340): near function: PAL 80051F9C -> JP 800535D4, imm:2, 2 instructions differ (whole routine)
/// 800535D4: missile step of ground unit `unit`: a target that already has
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
// JP-PATCH: replaced by `crate::jp::res_p1_player::missile_attack_80051f9c` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn missile_attack_80051f9c(m: &mut Machine, unit: u32) -> u32 {
    crate::jp::res_p1_player::missile_attack_80051f9c(m, unit)
}

// ==== end of a2 ====
// ==== c2: terrain cell rays and object rays (8009A800, 80098DBC, 80098C68, 8009B4EC, 8009A944) ====

/// End-of-list flag word of 80099894/8009A944 face-search lists (+8).
const C2_LIST_END: u32 = 0x20_0000;
/// 80098DBC: y of the ceiling plane (>> 12 units; y grows downwards).
const C2_CEILING: u32 = 0xffff_f448;

/// `div` quotient behind the compiler's checks: `break 7` (zero divisor)
/// and `break 6` (`i32::MIN / -1`) trap on the console; that continuation
/// is not translated, so those cases record `Fault::Untranslated` at the
/// `break` address (the quotient follows FLIGHT_MACHINE rule 2).
fn c2_div(m: &mut Machine, dividend: u32, divisor: u32, breaks: [u32; 2]) -> u32 {
    let (n, d) = (dividend as i32, divisor as i32);
    if d == 0 {
        m.raise(crate::machine::Fault::Untranslated { address: breaks[0] });
    } else if d == -1 && n == i32::MIN {
        m.raise(crate::machine::Fault::Untranslated { address: breaks[1] });
    }
    div_quotient(dividend, divisor)
}

/// 80098C68: where the segment from the VECTOR `origin` along the SVECTOR
/// `direction` (dx, dy, dz) meets the plane y = `level`. With t = level -
/// origin.y the offset (dx * t / dy, t, dz * t / dy) goes to the VECTOR
/// `out`, or (dx, 0, dz) when dy = 0 and the origin lies in the plane; an
/// x or z offset beyond that component of `direction` is clipped to it and
/// then out.y = dy. Returns 0 when the plane is behind the origin (or, for
/// dy = 0, off it; `out` untouched), otherwise 1 when out.y != dy.
pub fn plane_crossing_800972b0(
    m: &mut Machine,
    origin: u32,
    direction: u32,
    level: u32,
    out: u32,
) -> u32 {
    let origin_y = m.lw(origin.wrapping_add(4));
    let dx = i32::from(m.lh(direction));
    let dy = i32::from(m.lh(direction.wrapping_add(2)));
    let dz = i32::from(m.lh(direction.wrapping_add(4)));
    let rise = level.wrapping_sub(origin_y);
    if dy == 0 {
        if level != origin_y {
            return 0;
        }
        m.sw(out, dx as u32);
        m.sw(out.wrapping_add(8), dz as u32);
        m.sw(out.wrapping_add(4), rise);
    } else {
        let behind = if dy > 0 {
            (level as i32) < origin_y as i32
        } else {
            (origin_y as i32) < level as i32
        };
        if behind {
            return 0;
        }
        let product = dx.wrapping_mul(rise as i32) as u32;
        let x = c2_div(m, product, dy as u32, [0x8009_8cc0, 0x8009_8cd8]);
        let product = dz.wrapping_mul(rise as i32) as u32;
        let z = c2_div(m, product, dy as u32, [0x8009_8d04, 0x8009_8d1c]);
        m.sw(out.wrapping_add(4), rise);
        m.sw(out, x);
        m.sw(out.wrapping_add(8), z);
    }
    for (component, offset) in [(dx, 0), (dz, 8)] {
        let value = m.lw(out.wrapping_add(offset)) as i32;
        let beyond = if component < 0 {
            value < component
        } else {
            component < value
        };
        if beyond {
            m.sw(out.wrapping_add(offset), component as u32);
            m.sw(out.wrapping_add(4), dy as u32);
        }
    }
    u32::from(m.lw(out.wrapping_add(4)) != dy as u32)
}

/// Copy of the VECTOR at `source` into `target` (word by word).
fn c2_copy_vector(m: &mut Machine, target: u32, source: u32) {
    for i in 0..3 {
        let word = m.lw(source.wrapping_add(i * 4));
        m.sw(target.wrapping_add(i * 4), word);
    }
}

/// `point` += `base` (VECTORs, word by word, wrapping).
fn c2_add_vector(m: &mut Machine, point: u32, base: u32) {
    for i in 0..3 {
        let value = m.lw(point.wrapping_add(i * 4));
        let offset = m.lw(base.wrapping_add(i * 4));
        m.sw(point.wrapping_add(i * 4), value.wrapping_add(offset));
    }
}

/// 80098DBC's segment end: `from` + the plane crossing when 80098C68 found
/// one (`hit` != 0), otherwise `to`.
fn c2_segment_end(m: &mut Machine, end: u32, hit: u32, from: u32, to: u32) {
    if hit != 0 {
        c2_add_vector(m, end, from);
    } else {
        c2_copy_vector(m, end, to);
    }
}

/// `value / 0x800` rounded toward zero (`bgez; addiu 0x7FF; sra 11`).
fn c2_cells(value: i32) -> i32 {
    (if value < 0 { value + 0x7ff } else { value }) >> 11
}

/// 80098DBC's screen coordinate half: `value` clamped to -0x8000..0x7FFF.
fn c2_screen_half(value: i32) -> u32 {
    if value > 0x7fff {
        0x7fff
    } else if value < -0x8000 {
        0x8000
    } else {
        value as u32 & 0xffff
    }
}

/// 80098DBC's map lookup: block number (bits 0..8) of terrain cell
/// (`column`, `row`) of the 256 x 256 halfword `map`; an index negative as
/// a halfword becomes `& 7`, one >= 0x100 becomes `(& 7) | 0xF8`.
fn c2_map_block(m: &mut Machine, map: u32, column: i32, row: i32) -> u32 {
    let mut x = column;
    let mut z = row;
    if (column as i16) < 0 {
        x = column & 7;
    }
    if (row as i16) < 0 {
        z = row & 7;
    }
    if x as i16 >= 0x100 {
        x = (x & 7) | 0xf8;
    }
    if z as i16 >= 0x100 {
        z = (z & 7) | 0xf8;
    }
    let column_offset = (i32::from(x as i16) << 1) as u32;
    let row_offset = (i32::from(z as i16) << 9) as u32;
    m.lh(map.wrapping_add(column_offset).wrapping_add(row_offset)) as u32 & 0x1ff
}

/// 80098DBC: the 80099894 search list at `list` for the segment from the
/// VECTOR `from` to the VECTOR `to` (>> 12 world units; terrain cells of
/// 0x800). `map` (the caller's stack word +0x10) is the 256 x 256 halfword
/// cell map; `grid` receives one byte per cell corner (stride 0x20).
/// - Both ends above the ceiling (y < -0xBB8): an empty list.
/// - The segment is clipped to y in -0xBB8..0 with 80098C68 (start: the
///   ceiling crossing unless the start is below the ceiling and rising;
///   end: the ceiling crossing when rising, else the y = 0 crossing).
/// - Corners of the cells around the clipped segment are classified by
///   NCLIP against the segment (SXY0 = start, SXY1 = end in cell-local z/x,
///   SXY2 = corner): 1 left, 0xFF right, 0 on it. A cell whose four corners
///   differ, or are all 0, is crossed (bit `column` of row word `row` in
///   the frame's +0x38 masks).
/// - Each crossed cell with a block gives an entry {x, z: cell origin -
///   `from` + 0x400, y: -from.y; +8: block number}; a 0x200000 word at +8
///   ends the list.
pub fn ray_cell_list_80097404(m: &mut Machine, from: u32, to: u32, grid: u32, list: u32, map: u32) {
    const SIZE: u32 = 0xe0;
    let sp = m.push_frame(SIZE);
    let direction = sp.wrapping_add(0x10);
    let start = sp.wrapping_add(0x18);
    let end = sp.wrapping_add(0x28);
    let masks = sp.wrapping_add(0x38);
    let mut entry = list;
    if (m.lw(from.wrapping_add(4)) as i32) < -0xbb8 && (m.lw(to.wrapping_add(4)) as i32) < -0xbb8 {
        m.sw(entry.wrapping_add(8), C2_LIST_END);
        m.pop_frame(SIZE);
        return;
    }
    let mut dy = 0;
    for i in 0..3 {
        let target = m.lhu(to.wrapping_add(i * 4));
        let origin = m.lhu(from.wrapping_add(i * 4));
        let delta = u32::from(target).wrapping_sub(u32::from(origin));
        m.sh(direction.wrapping_add(i * 2), delta as u16);
        if i == 1 {
            dy = delta;
        }
    }
    let rising = (m.lw(from.wrapping_add(4)) as i32) >= -0xbb7 && (dy as u16 as i16) < 0;
    if rising {
        c2_copy_vector(m, start, from);
        let hit = plane_crossing_800972b0(m, from, direction, C2_CEILING, end);
        c2_segment_end(m, end, hit, from, to);
    } else {
        let hit = plane_crossing_800972b0(m, from, direction, C2_CEILING, start);
        if hit != 0 {
            c2_add_vector(m, start, from);
        } else {
            c2_copy_vector(m, start, from);
        }
        let hit = plane_crossing_800972b0(m, from, direction, 0, end);
        c2_segment_end(m, end, hit, from, to);
    }

    // Grid extent: corner columns x0 + 0x800 * (0..=last_column + 1), rows
    // z0 + 0x800 * (0..=last_row + 1), relative to the start cell origin.
    let end_x = m.lhu(end);
    let start_x = m.lhu(start);
    let end_z = m.lhu(end.wrapping_add(8));
    let start_z = m.lhu(start.wrapping_add(8));
    let dx = u32::from(end_x).wrapping_sub(u32::from(start_x));
    let dx_half = i32::from(dx as u16 as i16);
    let end_y = m.lhu(end.wrapping_add(4));
    let start_y = m.lhu(start.wrapping_add(4));
    let dz = u32::from(end_z).wrapping_sub(u32::from(start_z));
    m.sh(direction, dx as u16);
    m.sh(direction.wrapping_add(4), dz as u16);
    m.sh(
        direction.wrapping_add(2),
        u32::from(end_y).wrapping_sub(u32::from(start_y)) as u16,
    );
    let fine_x = m.lw(start) & 0x7ff;
    let fine_z = m.lw(start.wrapping_add(8)) & 0x7ff;
    let columns = c2_cells(dx_half.abs());
    let rows = c2_cells(i32::from(dz as u16 as i16).abs());
    let (x0, last_column) = if dx_half < 0 {
        (i32::from((dx & 0xf800) as u16 as i16), columns + 1)
    } else if fine_x == 0 {
        (-0x800, columns + 2)
    } else {
        (0, columns + 1)
    };
    let (z0, last_row) = if m.lh(direction.wrapping_add(4)) < 0 {
        let bits = m.lhu(direction.wrapping_add(4)) & 0xf800;
        (i32::from(bits as i16), rows + 1)
    } else if fine_z == 0 {
        (-0x800, rows + 2)
    } else {
        (0, rows + 1)
    };
    for word in (0..32).rev() {
        m.sw(masks.wrapping_add(word * 4), 0);
    }

    // Corner classification (SXY: z in the low half, x in the high half).
    m.gte.mtc2(reg::SXY0, fine_z | fine_x << 16);
    let dz_half = i32::from(m.lh(direction.wrapping_add(4)));
    let far_z = fine_z.wrapping_add(dz_half as u32) & 0xffff;
    let dx_half = i32::from(m.lh(direction));
    let far_x = fine_x.wrapping_add(dx_half as u32) << 16;
    m.gte.mtc2(reg::SXY1, far_z | far_x);
    if last_row + 1 >= 0 {
        let skip_columns = last_column + 1 < 0;
        let (mut row, mut z) = (0i32, z0);
        loop {
            let z_half = c2_screen_half(z);
            if !skip_columns {
                let (mut column, mut x, mut at) = (0i32, x0, row as u32);
                loop {
                    m.gte.mtc2(reg::SXY2, z_half | c2_screen_half(x) << 16);
                    let side = nclip_side(m);
                    let mark = match side.cmp(&0) {
                        std::cmp::Ordering::Greater => 1,
                        std::cmp::Ordering::Less => 0xff,
                        std::cmp::Ordering::Equal => 0,
                    };
                    m.sb(grid.wrapping_add(at), mark);
                    x = x.wrapping_add(0x800);
                    column += 1;
                    at = at.wrapping_add(0x20);
                    if last_column + 1 < column {
                        break;
                    }
                }
            }
            row += 1;
            z = z.wrapping_add(0x800);
            if last_row + 1 < row {
                break;
            }
        }
    }

    // Crossed cells: corners (row, column), (row + 1, column), (row,
    // column + 1), (row + 1, column + 1) of the byte grid.
    for row in 0..=last_row {
        if last_column < 0 {
            continue;
        }
        let mask = masks.wrapping_add(row as u32 * 4);
        for column in 0..=last_column {
            let corner = grid.wrapping_add(row as u32 + column as u32 * 0x20);
            let a = m.lbu(corner);
            let b = m.lbu(corner.wrapping_add(1));
            let crossed = a != b || {
                let c = m.lbu(corner.wrapping_add(0x20));
                a != c || {
                    let d = m.lbu(corner.wrapping_add(0x21));
                    a != d || a & b & c & d == 0
                }
            };
            if crossed {
                let word = m.lw(mask);
                m.sw(mask, word | 1 << (column as u32 & 31));
            }
        }
    }

    // List entries for the crossed cells that have a block.
    let start_x = m.lw(start);
    let start_z = m.lw(start.wrapping_add(8));
    let cell_x = (start_x as i32 >> 11).wrapping_add(0x80);
    let fine_x = start_x & 0x7ff;
    let cell_z = (start_z as i32 >> 11).wrapping_add(0x80);
    let fine_z = start_z & 0x7ff;
    let height = 0u32.wrapping_sub(m.lw(from.wrapping_add(4)));
    let mut z = z0;
    for row in 0..=last_row {
        let mut x = x0;
        if last_column >= 0 {
            let mask = masks.wrapping_add(row as u32 * 4);
            let z_offset = (z as u32).wrapping_sub(fine_z);
            for column in 0..=last_column {
                if m.lw(mask) & 1 << (column as u32 & 31) != 0 {
                    let block = c2_map_block(
                        m,
                        map,
                        cell_x.wrapping_add(c2_cells(x)),
                        cell_z.wrapping_add(c2_cells(z)),
                    );
                    if block != 0 {
                        let base = u32::from(m.lhu(start)).wrapping_add(0x400);
                        let origin = u32::from(m.lhu(from));
                        let offset = (x as u32).wrapping_sub(fine_x).wrapping_add(base);
                        m.sh(entry, offset.wrapping_sub(origin) as u16);
                        let base = u32::from(m.lhu(start.wrapping_add(8))).wrapping_add(0x400);
                        let origin = u32::from(m.lhu(from.wrapping_add(8)));
                        m.sh(entry.wrapping_add(2), height as u16);
                        m.sw(entry.wrapping_add(8), block);
                        let offset = z_offset.wrapping_add(base).wrapping_sub(origin);
                        m.sh(entry.wrapping_add(4), offset as u16);
                        entry = entry.wrapping_add(0xc);
                    }
                }
                x = x.wrapping_add(0x800);
            }
        }
        z = z.wrapping_add(0x800);
    }
    m.sw(entry.wrapping_add(8), C2_LIST_END);
    m.pop_frame(SIZE);
}

/// 8009A800: terrain ray from VECTOR `from` to VECTOR `to` (world units)
/// across cells, the long-range sibling of 8009A5D0: the cells crossed by
/// (from >> 12)..(to >> 12) (80098DBC with the map `[[gp+0x2E0]+0x248]`;
/// list at the frame's +0x18, corner bytes in `work`) are searched by
/// 80099894 along (to >> 12) - (from >> 12) with the block table
/// `[[gp+0x2E0]+0x24C]`; `out` = its hit point + `from`. Returns the hit
/// distance (0x7FFFFFFF when nothing is hit).
// JP-PATCH: replaced by `crate::jp::res_p1_player::ground_ray_cells_80098db0` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn ground_ray_cells_80098db0(m: &mut Machine, from: u32, to: u32, out: u32, work: u32) -> u32 {
    crate::jp::res_p1_player::ground_ray_cells_80098db0(m, from, to, out, work)
}

// JP-REVIEW(R0344): changed function: PAL 80098EF4 -> JP 8009A944, del:322 imm:7 ins:362 op:14 reg:57, 762 instructions differ (whole routine)
/// 8009A944: nearest face of one model hit by the ray from the origin
/// along the VECTOR `delta` (as 80099894 for a single block): `vertices`
/// and `offset` (the caller's stack words +0x10/+0x14: a vertex list and
/// an SVECTOR offset) are rotated into ray space into `work` (80099750),
/// then the face lists of `model` (+0x10, 0x30-byte faces; +0xC, 0x24-byte
/// faces; normals from `[[gp+0x2E0]+0x25C]`) are intersected. Returns the
/// hit distance (0x7FFFFFFF when none); `out` and `[gp+0x54C]`/
/// `[gp+0x550]` as 80099894.
// JP-PATCH: replaced by `crate::jp::res_p1_player::object_hit_80098ef4` (src/jp/mod.rs REPLACES, PATCHES.md)
pub fn object_hit_80098ef4(
    m: &mut Machine,
    delta: u32,
    out: u32,
    work: u32,
    model: u32,
    vertices: u32,
    offset: u32,
) -> u32 {
    crate::jp::res_p1_player::object_hit_80098ef4(m, delta, out, work, model, vertices, offset)
}

/// 8009B4EC: ray of the moving entity at VECTOR `from` with velocity
/// VECTOR `velocity` (the ray is velocity >> 12) against the model of the
/// entity `object` (model table `[[gp+0x2E0]+0x258]`, entry by byte +4;
/// relative position (object +8 - from) >> 12 as the SVECTOR offset).
/// With flag +5 bit 2 the entry holds five key frames: halfword +6 bits
/// 12..15 (signed, clamped to 0..4) pick the key, bits 0..11 the blend
/// toward the next key (80066A9C interpolates both vertex lists into
/// `work`, the caller's stack word +0x10); otherwise the first model is
/// used with its own vertices. 8009A944 does the search; `out` = its hit
/// point + `from`. Returns the hit distance (0x7FFFFFFF when none).
pub fn object_ray_800999fc(
    m: &mut Machine,
    from: u32,
    velocity: u32,
    object: u32,
    out: u32,
    work: u32,
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
    let distance = object_hit_80098ef4(m, delta, out, rotated, model, vertices, offset);
    c2_add_vector(m, out, from);
    m.pop_frame(SIZE);
    distance
}

// ==== end of c2 ====
/// Native targets owned by this module, by original address (direct and
/// indirect calls from other modules): original `a0..a3`, returns `v0`
/// (0 for routines without a result). A fifth argument is read from the
/// caller's stack word `[sp+0x10]` (callers store it in their pushed frame,
/// as the original does).
#[allow(unused_variables)]
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    let [a0, a1, a2, a3] = args;
    Some(match target {
        0x8007_29dc => {
            update_player_80071358(m);
            0
        }
        0x8007_2938 => {
            control_input_800712b4(m, a0, a1);
            0
        }
        0x8006_d0dc => {
            control_step_8006b90c(m, a0, a1, a2, a3);
            0
        }
        0x8006_e874 => {
            scale_by_speed_8006d218(m, a0, a1);
            0
        }
        0x8008_3f40 => vector_length_800828f8(m, a0, a1, a2),
        0x8002_939c => claim_vibration_80028f7c(m, a0),
        0x8002_93f0 => {
            vibrate_80028fd0(m, a0);
            0
        }
        0x8007_2094 => {
            fire_gun_80070a10(m, a0);
            0
        }
        0x8007_20d4 => {
            fire_weapon_80070a50(m, a0, a1);
            0
        }
        0x8007_1950 => {
            launch_missile_800702cc(m, a0);
            0
        }
        0x8007_14e0 => {
            gun_8006fe5c(m);
            0
        }
        0x8007_1544 => {
            gun_8006fec0(m);
            0
        }
        0x8007_15ac => {
            gun_8006ff28(m);
            0
        }
        0x8007_1614 => {
            gun_8006ff90(m);
            0
        }
        0x8007_167c => {
            gun_8006fff8(m);
            0
        }
        0x8007_1708 => {
            gun_80070084(m);
            0
        }
        0x8007_1a00 => {
            launch_missile_8007037c(m, a0);
            0
        }
        0x8007_1ab0 => {
            launch_missile_8007042c(m, a0);
            0
        }
        0x8007_1b54 => {
            launch_missile_800704d0(m, a0);
            0
        }
        0x8007_1bf8 => {
            launch_missile_80070574(m, a0);
            0
        }
        0x8007_1c9c => {
            launch_missile_80070618(m, a0);
            0
        }
        0x8007_1d40 => {
            launch_missile_800706bc(m, a0);
            0
        }
        0x8007_1de4 => {
            launch_missile_80070760(m, a0);
            0
        }
        0x8007_1ebc => {
            launch_missile_80070838(m, a0);
            0
        }
        0x8007_1f60 => {
            launch_missile_800708dc(m, a0);
            0
        }
        0x8007_2004 => {
            launch_missile_80070980(m, a0);
            0
        }
        0x8007_13b0 => {
            fire_bullet_8006fd2c(m, a0);
            0
        }
        0x8006_b034 => queue_bullet_80069864(m, a0, a1, a2, a3),
        0x8002_94c4 => {
            vibrate_gun_800290a4(m, a0);
            0
        }
        0x8006_cb44 => {
            camera_shake_8006b374(m, a0);
            0
        }
        0x8002_9588 => {
            vibrate_random_80029168(m);
            0
        }
        0x8002_95cc => {
            vibrate_preset_800291ac(m);
            0
        }
        0x8002_9604 => {
            vibrate_preset_800291e4(m);
            0
        }
        0x8002_9448 => {
            vibrate_weapon_80029028(m, a0);
            0
        }
        0x8009_0110 => screen_effect_8008e8a0(m, a0),
        0x8007_efbc => effect_command_8007d940(m, a0, a1),
        0x8007_1770 => {
            weapon_origin_800700ec(m, a0, a1);
            0
        }
        0x8006_85c0 => allocate_projectile_80066df0(m, a0, a1, a2, a3),
        0x8006_89e8 => spawn_80067218(m, a0, a1, a2, a3),
        0x8006_8a58 => spawn_80067288(m, a0, a1, a2, a3),
        0x8006_8afc => spawn_8006732c(m, a0, a1, a2, a3),
        0x8006_8ba8 => spawn_800673d8(m, a0, a1, a2, a3),
        0x8006_8c0c => spawn_8006743c(m, a0, a1, a2, a3),
        0x8006_8c80 => spawn_800674b0(m, a0, a1, a2, a3),
        0x8006_8d0c => spawn_8006753c(m, a0, a1, a2, a3),
        0x8006_8f34 => spawn_80067764(m, a0, a1, a2, a3),
        0x8006_8fa8 => spawn_800677d8(m, a0, a1, a2, a3),
        0x8006_8800 => {
            let flags = m.lw(m.sp.wrapping_add(0x10));
            spawn_80067030(m, a0, a1, a2, a3, flags)
        }
        // ---- group A ----
        0x8004_5898 => {
            update_damage_80044d50(m);
            0
        }
        0x8004_58d0 => {
            tick_hit_timers_80044d88(m);
            0
        }
        0x8004_5a40 => {
            apply_damage_80044ef8(m);
            0
        }
        0x8004_c3c4 => {
            propagate_link_damage_8004ac74(m);
            0
        }
        0x8004_6030 => {
            update_lock_slots_800453f8(m);
            0
        }
        0x8005_17f4 => {
            update_ground_units_800501c8(m);
            0
        }
        0x8005_1498 => {
            mark_units_8004fe70(m);
            0
        }
        0x8005_1414 => {
            update_units_8004fdf0(m);
            0
        }
        0x8005_1840 => {
            update_unit_sound_80050214(m);
            0
        }
        0x8005_157c => {
            move_units_8004ff54(m);
            0
        }
        0x8004_658c => {
            update_sensors_80045950(m);
            0
        }
        0x8004_bed4 => {
            update_linked_entities_8004a784(m);
            0
        }
        0x8004_5240 => update_sight_800446f8(m),
        0x8004_5278 => update_sight_records_80044730(m),
        0x8004_c5b8 => {
            copy_link_records_8004ae68(m);
            0
        }
        0x8004_55b8 => update_home_records_80044a70(m),
        0x8004_5768 => tick_record_timers_80044c20(m),
        0x8004_8658 => {
            record_flight_path_8004790c(m);
            0
        }
        0x8004_5024 => view_vector_800444dc(m, a0, a1, a2, a3),
        0x8004_5090 => off_axis_angle_80044548(m, a0),
        // ---- end of group A ----
        // 80072178 subtree
        0x8007_2178 => {
            player_effects_80070af4(m, a0);
            0
        }
        0x8007_2114 => {
            engine_sound_80070a90(m);
            0
        }
        0x8007_084c => {
            apply_attitude_8006f1c8(m, a0, a1);
            0
        }
        0x8007_0a80 => {
            collide_entities_8006f3fc(m, a0, a1);
            0
        }
        0x8007_0cf4 => {
            world_collision_8006f670(m, a0, a1);
            0
        }
        0x8007_0a34 => {
            integrate_position_8006f3b0(m, a0);
            0
        }
        0x8007_11fc => ground_height_8006fb78(m, a0, a1, a2),
        0x8003_01f0 => {
            engine_voices_8002ffac(m, a0);
            0
        }
        0x8002_e64c => pitch_add_8002e230(a0, a1),
        0x8002_de24 => set_voice_pitch_8002d9fc(m, a0, a1),
        0x8003_a624 => gun_override_8003a0f0(),
        0x8007_18e0 => {
            tick_reload_timers_8007025c(m);
            0
        }
        0x8007_18a0 => fill_free_slots_8007021c(m, a0, a1, a2),
        // end of 80072178 subtree
        // ---- group C
        0x8009_a5d0 => ground_ray_80098b80(m, a0, a1, a2, a3),
        0x8009_9428 => {
            direction_matrix_80097a70(m, a0, a1);
            0
        }
        0x8009_9750 => {
            transform_vertices_80097d98(m, a0, a1, a2);
            0
        }
        0x8003_0f64 => {
            orientation_80030d20(m, a0, a1);
            0
        }
        0x8003_1bb4 => {
            vector_angles_80031970(m, a0, a1);
            0
        }
        // ---- end of group C
        // Group D. Stack arguments are the caller's words at `sp+0x10..`.
        0x8006_ef30 => {
            flight_model_8006d8d4(m, a0, a1, a2, a3);
            0
        }
        0x8006_ea88 => {
            let cruise_out = m.lw(m.sp.wrapping_add(0x10));
            let max_out = m.lw(m.sp.wrapping_add(0x14));
            altitude_limits_8006d42c(m, a0, a1, a2, a3, cruise_out, max_out)
        }
        0x8006_e908 => {
            let altitude = m.lw(m.sp.wrapping_add(0x10));
            let gain_up = m.lw(m.sp.wrapping_add(0x14));
            let gain_down = m.lw(m.sp.wrapping_add(0x18));
            speed_change_8006d2ac(a0, a1, a2, a3, altitude, gain_up, gain_down)
        }
        // ---- d2 ----
        0x8007_02d8 => {
            flight_model_free_8006ec54(m, a0, a1, a2, a3);
            0
        }
        // ---- end of d2 ----
        // ---- e ----
        0x8006_d678 => {
            control_step_8006bea8(m, a0, a1, a2, a3);
            0
        }
        0x8007_3b18 => {
            // Fifth argument: the caller's stack word +0x10.
            let climb = m.lw(m.sp.wrapping_add(0x10));
            auto_level_80072494(m, a0, a1, a2, a3, climb);
            0
        }
        0x8007_407c => {
            auto_climb_800729f8(m, a0, a1, a2, a3);
            0
        }
        0x8006_e7e0 => {
            scale_outputs_by_speed_8006d184(m, a0, a1);
            0
        }
        // ---- end of e ----
        // ---- f ----
        // Stack arguments are the caller's words at `sp+0x10..`.
        0x8006_dd5c => {
            let entity = m.lw(m.sp.wrapping_add(0x10));
            let motion = m.lw(m.sp.wrapping_add(0x14));
            let state = m.lw(m.sp.wrapping_add(0x18));
            analog_control_8006c58c(m, a0, a1, a2, a3, entity, motion, state);
            0
        }
        0x8007_542c => {
            let controls = m.lw(m.sp.wrapping_add(0x10));
            scripted_control_80073db0(m, a0, a1, a2, a3, controls);
            0
        }
        // ---- end of f ----
        // ---- b2 ----
        0x8002_dcb0 => start_sound_8002d888(m, a0),
        0x8002_e67c => restore_voice_8002e260(m, a0),
        0x8002_e6dc => {
            position_sound_8002e2c0(m, a0, a1, a2);
            0
        }
        0x8002_e84c => distance_volume_8002e430(m, a0, a1, a2),
        0x8002_e8dc => pan_volume_8002e4c0(m, a0, a1),
        0x8002_ea24 => surround_side_8002e608(m, a0, a1),
        0x8002_ec48 => doppler_pitch_8002e82c(m, a0, a1),
        0x8002_f0d4 => detune_8002ecb8(m, a0, a1),
        0x8002_f3c0 => play_positioned_sound_8002efa4(m, a0, a1, a2),
        // ---- end of b2 ----
        // ---- g ----
        0x8002_963c => {
            vibrate_preset_8002921c(m);
            0
        }
        0x8002_9674 => {
            vibrate_random_80029254(m);
            0
        }
        0x8002_96b8 => {
            vibrate_preset_80029298(m);
            0
        }
        0x8007_4560 => {
            crash_80072edc(m, a0, a1);
            0
        }
        0x8007_95c8 => player_camera_80077f58(m, a0),
        0x8007_4878 => {
            ground_roll_800731f4(m, a0, a1, a2);
            0
        }
        0x8007_4ba8 => scripted_fly_in_80073524(m, a0, a1, a2, a3),
        0x8007_4e64 => {
            follow_entity_800737e0(m, a0, a1, a2, a3);
            0
        }
        // ---- end of g ----
        // ---- h ----
        // 80075B04/80075E08 take the caller's stack words +0x10/+0x14,
        // 800765B0 its word +0x10.
        0x8007_5084 => join_formation_80073a08(m, a0, a1, a2, a3),
        0x8007_599c => {
            place_on_ground_80074320(m, a0, a1, a2);
            0
        }
        0x8007_5b04 => {
            let view = m.lw(m.sp.wrapping_add(0x10));
            let replay = m.lw(m.sp.wrapping_add(0x14));
            takeoff_run_80074488(m, a0, a1, a2, a3, view, replay)
        }
        0x8007_5e08 => {
            let view = m.lw(m.sp.wrapping_add(0x10));
            let close = m.lw(m.sp.wrapping_add(0x14));
            airborne_start_80074798(m, a0, a1, a2, a3, view, close);
            0
        }
        0x8007_601c => {
            exit_flight_800749ac(m, a0, a1);
            0
        }
        0x8007_6360 => {
            set_exit_mode_80074cf0(m, a0);
            0
        }
        0x8007_63cc => {
            enter_crash_80074d5c(m);
            0
        }
        0x8007_6428 => approach_area_80074db8(m, a0, a1),
        0x8007_65b0 => {
            let out = m.lw(m.sp.wrapping_add(0x10));
            relative_geometry_80074f40(m, a0, a1, a2, a3, out)
        }
        0x8007_6924 => formation_check_800752b4(m, a0, a1, a2, a3),
        0x8007_6a38 => outside_area_800753c8(m, a0, a1),
        0x8007_6ac0 => exit_attitude_80075450(m, a0),
        0x8007_6b18 => {
            set_approach_mode_800754a8(m, a0, a1);
            0
        }
        0x8007_6c54 => {
            set_target_mode_800755e4(m, a0, a1);
            0
        }
        0x8007_6dec => {
            set_start_mode_8007577c(m, a0, a1);
            0
        }
        0x8007_9564 => {
            crash_camera_80077ef4(m, a0);
            0
        }
        // ---- end of h ----
        // ---- j ----
        0x8005_22b0 => {
            unit_motion_80050c84(m, a0);
            0
        }
        0x8005_2d70 => direction_8005173c(m, a0, a1, a2),
        // ---- end of j ----
        // ---- a2 ----
        0x8005_1a54 => {
            update_unit_80050428(m, a0);
            0
        }
        0x8005_1ba0 => {
            step_unit_script_80050578(m, a0);
            0
        }
        0x8005_1f88 => {
            wreck_unit_80050958(m, a0);
            0
        }
        0x8005_2060 => {
            check_unit_wreck_80050a30(m, a0);
            0
        }
        0x8005_2000 => {
            unit_wreck_effect_800509d0(m, a0);
            0
        }
        0x8004_8ad4 => {
            set_effects_80047dcc(m, a0);
            0
        }
        0x8005_20c8 => {
            destroy_unit_80050a98(m, a0);
            0
        }
        0x8007_97fc => {
            focus_kill_camera_8007818c(m, a0);
            0
        }
        0x8005_1e34 => {
            unit_destroyed_8005080c(m, a0);
            0
        }
        0x8004_8758 => {
            record_kill_80047a0c(m, a0);
            0
        }
        0x8005_2fa8 => unit_attack_80051974(m, a0),
        0x8004_6124 => choose_target_800454ec(m, a0),
        0x8005_3214 => gun_attack_80051bdc(m, a0, a1),
        0x8005_35d4 => missile_attack_80051f9c(m, a0),
        0x8003_8320 => {
            wreck_slot_80038010(m, a0);
            0
        }
        0x8004_651c => release_lock_800458e0(m, a0),
        0x8004_51ac => {
            place_offset_80044664(m, a0, a1, a2);
            0
        }
        // ---- end of a2 ----
        // ---- c2 ----
        0x8009_a800 => ground_ray_cells_80098db0(m, a0, a1, a2, a3),
        0x8009_8dbc => {
            let map = m.lw(m.sp.wrapping_add(0x10));
            ray_cell_list_80097404(m, a0, a1, a2, a3, map);
            0
        }
        0x8009_8c68 => plane_crossing_800972b0(m, a0, a1, a2, a3),
        0x8009_b4ec => {
            let work = m.lw(m.sp.wrapping_add(0x10));
            object_ray_800999fc(m, a0, a1, a2, a3, work)
        }
        // ---- end of c2 ----
        _ => return None,
    })
}
