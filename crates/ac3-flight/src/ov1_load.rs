//! Overlay 1 (front-end menus): menu states 0 (boot: memory-card probe
//! and resource loading) and 8 (return to the menus), the resource 0x45
//! loader and its texture uploads, and the CD stream control, on the
//! address-preserving machine.
//!
//! Menu state 0 (`[0x800D8744]`: +0 phase, +4 card step, +8 resource step,
//! +0xC language of the loading text, +0x10 slide timer) runs two step
//! sequences side by side each frame: the card probe (`0x800D63D0`,
//! indexed by +4) and the resource loading (`0x800D63F0`, indexed by +8);
//! state 8 (`[0x800D875C]`) runs only the resource steps. The resource
//! state `[0x800D598C]`: +0 card events enabled, +4/+8 card status of
//! slots 0/1, +0xC the relocated resource 0x45, +0x10 the page to show.
//! Step callbacks return bit 0 to advance, bit 1 when the sequence is
//! done, bit 2 (card) to skip to step 7.
use crate::{
    arena,
    common::RESOURCES,
    dispatch,
    machine::{Machine, SmallData},
    ov1::{RESOURCE, context, menu, off},
    ov1_card, ov1_script, ov1_text,
};

const STATE0: u32 = 0x800d_8744;
const STATE0_PHASES: u32 = 0x800d_8748;
const STATE8: u32 = 0x800d_875c;
const STATE8_PHASES: u32 = 0x800d_8760;
const CARD_STEPS: u32 = 0x800d_63d0;
const RESOURCE_STEPS: u32 = 0x800d_63f0;
/// 0x800D6428 / 0x800D6414: the "now loading" strings per language.
const LOADING_TEXTS: u32 = 0x800d_6428;
const RETURN_TEXTS: u32 = 0x800d_6414;
/// 0x800CAF38: the state-8 pulse colour.
const RETURN_PULSE: u32 = 0x800c_af38;
/// 0x800D5990: the built-in script table shown while loading.
const BOOT_SCRIPTS: u32 = 0x800d_5990;
/// `[0x800D873C]`: the CD stream block (+0 flags, +0x1004 stream).
const STREAM: u32 = 0x800d_873c;
/// `[0x800D6410]`: the texture upload state (+0 table, +4 count, +8 arena
/// mark, +0x10 per call, +0x14 next).
const TEXTURES: u32 = 0x800d_6410;
/// 0x80099DD4: the CD read completion routine the resource request uses.
const READ_COMPLETE: u32 = 0x8009_9dd4;
/// "Ulz\x1A": a compressed TIM.
const ULZ: u32 = 0x1a7a_6c55;

/// Resident routines (owned by `loading.rs`, `boot.rs`, `shared.rs`,
/// `script.rs`, `replay.rs`, `sdk.rs`).
const TOTAL_SIZE: u32 = 0x8002_c214;
const QUEUE_FILES: u32 = 0x8002_c404;
const INSTALL_SOUND_BANK: u32 = 0x8002_d544;
const DECOMPRESS: u32 = 0x800a_e118;
const UPLOAD_TIM: u32 = 0x8002_6d00;
const DRAW_SYNC: u32 = 0x800a_6664;
const RESET_QUEUE: u32 = 0x8002_c280;
const STREAM_STATE: u32 = 0x8002_c730;
const WAIT_RESOURCES: u32 = 0x8002_c158;
const QUEUE_STREAM: u32 = 0x8002_c4b0;
const MEMSET: u32 = 0x800a_c148;

fn resource(m: &mut Machine) -> u32 {
    m.lw(RESOURCE)
}

// ---- menu state 0 -----------------------------------------------------------------

/// 800D0914: a fresh game's progress tables: 60 halfwords 45000 at
/// context +0xA08, +0xA80, +0xAF8, +0xB70 and bytes +0xC60.. +0xD14
/// cleared, then +0x974, +0x961, +0xF74, +0xF70, +0xF6C, +0xF78 cleared.
pub fn reset_tables(m: &mut Machine) {
    let context = context(m);
    let base = off(context, 0x960);
    for i in 0..0x3c {
        let half = off(base, 2 * i);
        for offset in [0x210, 0x198, 0x120, 0xa8] {
            m.sh(off(half, offset), 0xafc8);
        }
        let byte = off(base, i);
        for offset in [0x3b4, 0x378, 0x33c, 0x300] {
            m.sb(off(byte, offset), 0);
        }
    }
    m.sw(off(base, 0x14), 0);
    m.sb(off(base, 1), 0);
    for offset in [0x614, 0x610, 0x60c, 0x618] {
        m.sw(off(base, offset), 0);
    }
}

/// 800D097C (state 0 phase 0): slide timer 25, offset slot 1 at x -500.
/// Returns 1.
pub fn boot_start(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let state = m.lw(STATE0);
    m.sw(off(state, 0x10), 0x19);
    ov1_script::set_offset(m, 1, 0xffff_fe0c, 0);
    m.pop_frame(0x18);
    1
}

/// 800D09B4 (state 0 phase 1): slide the logo in (slot 1 at -20 * timer)
/// or, once there, draw the loading text (font 19 at (200, 380)); run one
/// resource step and, after polling both card slots, one card step. Done
/// when the resource sequence is (bit 1): 1 with the language restored
/// from the card (card bit 1), 2 when the card sequence skipped (bit 2),
/// else 0.
pub fn boot_frame(m: &mut Machine, args: [u32; 4]) -> u32 {
    let frame = m.push_frame(0x30);
    let state = m.lw(STATE0);
    let timer = m.lw(off(state, 0x10));
    if (timer as i32) > 0 {
        let timer = timer.wrapping_sub(1);
        m.sw(off(state, 0x10), timer);
        ov1_script::set_offset(m, 1, timer.wrapping_neg().wrapping_mul(20), 0);
    } else {
        let language = m.lw(off(state, 0xc));
        m.sw(off(frame, 0x10), 0xc8);
        m.sw(off(frame, 0x14), 0x17c);
        m.sw(off(frame, 0x18), 0x0080_8080);
        let text = m.lw(off(LOADING_TEXTS, language << 2));
        let block = menu(m);
        let ot = m.lw(off(block, 0x8c));
        let packet = m.lw(off(block, 0x8090));
        let packet = ov1_text::draw_text(m, 0x13, text, ot, packet);
        let block = menu(m);
        m.sw(off(block, 0x8090), packet);
    }
    let state = m.lw(STATE0);
    let step = m.lw(off(state, 8));
    let callback = m.lw(off(RESOURCE_STEPS, step << 2));
    let resources = dispatch::call(m, callback, args);
    if resources & 1 != 0 {
        let state = m.lw(STATE0);
        let step = m.lw(off(state, 8));
        m.sw(off(state, 8), step.wrapping_add(1));
    }
    poll_slots(m);
    let state = m.lw(STATE0);
    let step = m.lw(off(state, 4));
    let callback = m.lw(off(CARD_STEPS, step << 2));
    let card = dispatch::call(m, callback, args);
    if card & 1 != 0 {
        let state = m.lw(STATE0);
        let step = m.lw(off(state, 4));
        m.sw(off(state, 4), step.wrapping_add(1));
    } else if card & 4 != 0 {
        let state = m.lw(STATE0);
        m.sw(off(state, 4), 7);
    }
    let result = if resources & 2 == 0 {
        0
    } else if card & 2 != 0 {
        ov1_card::restore_language(m);
        1
    } else if card & 4 != 0 {
        2
    } else {
        0
    };
    m.pop_frame(0x30);
    result
}

/// 800D0B6C (state 0 phase 2): returns 2.
pub fn boot_done() -> u32 {
    2
}

/// 800D0B74 (menu state 0 initialization): phase and steps 0; a fresh
/// context gets fresh tables and the loading text in language 0, otherwise
/// in the chosen language (+0x38C); page 0, resource page 3. Returns 1.
pub fn boot_init(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let state = m.lw(STATE0);
    let context = context(m);
    m.sw(off(state, 8), 0);
    m.sw(off(state, 4), 0);
    m.sw(state, 0);
    if m.lw(off(context, 0x970)) & 1 == 0 {
        reset_tables(m);
        let state = m.lw(STATE0);
        m.sw(off(state, 0xc), 0);
    } else {
        let language = m.lw(off(context, 0x38c));
        m.sw(off(state, 0xc), language);
    }
    ov1_script::select_page(m, 0);
    set_page(m, 3);
    m.pop_frame(0x18);
    1
}

/// 800D0BF4 (menu state 0 update): run the phase (table `0x800D8748`);
/// bit 0 advances it; bit 1 installs the resource scripts (page 1) and
/// returns 0x100.
pub fn boot_update(m: &mut Machine, args: [u32; 4]) -> u32 {
    m.push_frame(0x18);
    let state = m.lw(STATE0);
    let phase = m.lw(state);
    let callback = m.lw(off(STATE0_PHASES, phase << 2));
    let result = dispatch::call(m, callback, args);
    if result & 1 != 0 {
        let state = m.lw(STATE0);
        let phase = m.lw(state);
        m.sw(state, phase.wrapping_add(1));
    }
    let value = if result & 2 != 0 {
        install_scripts(m);
        0x100
    } else {
        0
    };
    m.pop_frame(0x18);
    value
}

// ---- menu state 8 -----------------------------------------------------------------

/// 800D0C7C (state 8 phase 0): returns 1.
pub fn return_start() -> u32 {
    1
}

/// 800D0C84 (state 8 phase 1): one resource step; bit 0 advances; 1 when
/// done.
pub fn return_load(m: &mut Machine, args: [u32; 4]) -> u32 {
    m.push_frame(0x18);
    let state = m.lw(STATE8);
    let step = m.lw(off(state, 8));
    let callback = m.lw(off(RESOURCE_STEPS, step << 2));
    let result = dispatch::call(m, callback, args);
    if result & 1 != 0 {
        let state = m.lw(STATE8);
        let step = m.lw(off(state, 8));
        m.sw(off(state, 8), step.wrapping_add(1));
    }
    m.pop_frame(0x18);
    u32::from(result & 2 != 0)
}

/// 800D0CFC (state 8 phase 2): returns 2.
pub fn return_done() -> u32 {
    2
}

/// 800D0D04 (menu state 8 initialization): resource step 0, page 0,
/// resource page 2. Returns 1.
pub fn return_init(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let state = m.lw(STATE8);
    m.sw(off(state, 8), 0);
    ov1_script::select_page(m, 0);
    set_page(m, 2);
    m.pop_frame(0x18);
    1
}

/// 800D0D38 (menu state 8 update): the loading text (font 19 at
/// (232, 140)), the phase (table `0x800D8760`; bit 0 advances; bit 1
/// installs the scripts and returns 0x100), else colour 0x78 pulses
/// between `0x800CAF38` and black.
pub fn return_update(m: &mut Machine, args: [u32; 4]) -> u32 {
    let frame = m.push_frame(0x40);
    let context = context(m);
    let block = menu(m);
    let language = m.lw(off(context, 0x38c));
    m.sw(off(frame, 0x10), 0xe8);
    m.sw(off(frame, 0x14), 0x8c);
    m.sw(off(frame, 0x18), 0x0080_8080);
    let ot = m.lw(off(block, 0x8c));
    let packet = m.lw(off(block, 0x8090));
    let text = m.lw(off(RETURN_TEXTS, language << 2));
    let packet = ov1_text::draw_text(m, 0x13, text, ot, packet);
    let block = menu(m);
    m.sw(off(block, 0x8090), packet);
    let state = m.lw(STATE8);
    let phase = m.lw(state);
    let callback = m.lw(off(STATE8_PHASES, phase << 2));
    let result = dispatch::call(m, callback, args);
    if result & 1 != 0 {
        let state = m.lw(STATE8);
        let phase = m.lw(state);
        m.sw(state, phase.wrapping_add(1));
    }
    let value = if result & 2 != 0 {
        install_scripts(m);
        0x100
    } else {
        crate::ov1::copy_word(m, RETURN_PULSE, off(frame, 0x20));
        dispatch::call(m, MEMSET, [off(frame, 0x28), 0, 4, 0]);
        ov1_script::pulse_color(m, 0x78, off(frame, 0x20), off(frame, 0x28));
        0
    };
    m.pop_frame(0x40);
    value
}

// ---- resource and card steps ------------------------------------------------------------

/// 800CB648: poll the card and store both slots' states (+4, +8).
pub fn poll_slots(m: &mut Machine) {
    m.push_frame(0x18);
    ov1_card::poll(m);
    let (first, _) = ov1_card::slot_state(m, 0);
    let state = resource(m);
    m.sw(off(state, 4), first);
    let (second, _) = ov1_card::slot_state(m, 1);
    let state = resource(m);
    m.sw(off(state, 8), second);
    m.pop_frame(0x18);
}

/// 800CB690 (card step 0): with the language chosen (+0x970 bit 0) skip
/// the card (2); else enable the card events and reset the probe (1).
pub fn card_begin(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let state = resource(m);
    let context = context(m);
    m.sw(state, 0);
    let result = if m.lw(off(context, 0x970)) & 1 != 0 {
        2
    } else {
        m.sw(state, 1);
        ov1_card::enable_events(m);
        ov1_card::reset_state(m);
        1
    };
    m.pop_frame(0x18);
    result
}

/// 800CB6EC (card step 1): 1 once slot 0's status bit 0 is clear.
pub fn card_wait_probe(m: &mut Machine) -> u32 {
    let state = resource(m);
    (m.lw(off(state, 4)) & 1) ^ 1
}

/// 800CB70C (card step 2): slot 0 has a save file (bit 4): 1, else 4.
pub fn card_has_file(m: &mut Machine) -> u32 {
    let state = resource(m);
    if m.lw(off(state, 4)) & 0x10 != 0 {
        1
    } else {
        4
    }
}

/// 800CB738 (card step 3): read slot 0's save (800CE0FC mode 1): -1 → 4,
/// 1/3 → 1, 2 → 4, else 0.
pub fn card_read(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let (result, _) = ov1_card::read_save(m, 0, 1);
    let value = match result {
        0xffff_ffff => 4,
        1 | 3 => 1,
        0 => 0,
        r => u32::from(r == 2) << 2,
    };
    m.pop_frame(0x18);
    value
}

/// 800CB7A4 (card step 4): returns 1.
pub fn card_skip() -> u32 {
    1
}

/// 800CB7AC (card step 5): apply the loaded options (800CCDA4); 1.
pub fn card_apply(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    ov1_card::apply_options(m);
    m.pop_frame(0x18);
    1
}

/// 800CB7CC / 800CB828 (card steps 6 and 7): disable the card events if
/// they were enabled; return 2 / 4.
fn card_end(m: &mut Machine, result: u32) -> u32 {
    m.push_frame(0x18);
    let state = resource(m);
    if m.lw(state) & 1 != 0 {
        ov1_card::disable_events(m);
        let state = resource(m);
        let flags = m.lw(state);
        m.sw(state, flags & 0xffff_fffe);
    }
    m.pop_frame(0x18);
    result
}

/// 800CB7CC (card step 6).
pub fn card_finish(m: &mut Machine) -> u32 {
    card_end(m, 2)
}

/// 800CB828 (card step 7).
pub fn card_abandon(m: &mut Machine) -> u32 {
    card_end(m, 4)
}

/// 800CB884 (resource step 0): the built-in boot scripts (`0x800D5990`),
/// page 1. Returns 1.
pub fn resource_boot_scripts(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    ov1_script::set_scripts(m, BOOT_SCRIPTS);
    ov1_script::select_page(m, 1);
    m.pop_frame(0x18);
    1
}

/// 800CB8B0 (resource step 1): show the resource page (+0x10). Returns 1.
pub fn resource_page(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let state = resource(m);
    let page = m.lw(off(state, 0x10));
    ov1_script::select_page(m, page);
    m.pop_frame(0x18);
    1
}

/// 800CB8DC (resource step 2): request BPB file 0x45 into an arena block
/// of its size (completion flag context +0x44 = 1). Returns 1.
pub fn resource_request(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let resources = m.gw(RESOURCES);
    m.sw(off(resources, 0x60), 0x45);
    let size = dispatch::call(m, TOTAL_SIZE, [off(resources, 0x60), 1, 0, 0]);
    let destination = arena::alloc(m, size);
    let context = context(m);
    let resources = m.gw(RESOURCES);
    m.sw(off(context, 0x44), 1);
    dispatch::call(
        m,
        QUEUE_FILES,
        [off(resources, 0x60), 1, destination, READ_COMPLETE],
    );
    m.pop_frame(0x18);
    1
}

/// 800CB94C (resource step 3): 1 once the read completed (context +0x44
/// zero).
pub fn resource_wait(m: &mut Machine) -> u32 {
    let context = context(m);
    u32::from(m.lw(off(context, 0x44)) == 0)
}

/// 800CB964 (resource step 4): relocate resource 0x45 (+0xC): its string
/// banks and text descriptors (menu +0x8094/+0x8098 and the text table),
/// its sound bank (installed, then released), its texture table (ten
/// uploads per call). Returns 1.
pub fn resource_install(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let resources = m.gw(RESOURCES);
    let file = m.lw(off(resources, 0x60));
    let table = dispatch::call(m, ov1_script::RELOCATE_DIRECTORY, [file, 0, 0, 0]);
    let state = resource(m);
    let block = menu(m);
    m.sw(off(state, 0xc), table);
    let banks = m.lw(off(table, 4));
    m.sw(off(block, 0x8094), banks);
    let descriptors = m.lw(off(table, 8));
    m.sw(off(block, 0x8098), descriptors);
    let descriptors = m.lw(off(table, 8));
    let banks = m.lw(off(table, 4));
    ov1_text::set_texts(m, descriptors, banks);
    let sound = m.lw(off(table, 0x10));
    dispatch::call(m, INSTALL_SOUND_BANK, [sound, 0, 0, 0]);
    let sound = m.lw(off(table, 0x10));
    arena::reset(m, sound);
    let textures = m.lw(off(table, 0xc));
    begin_textures(m, textures, 0x1_0000, 10);
    m.pop_frame(0x18);
    1
}

/// 800CBA04 (resource step 5): upload the next textures; when all are
/// done release the texture data (arena back to table +0xC) and return 1.
pub fn resource_textures(m: &mut Machine) -> u32 {
    m.push_frame(0x18);
    let result = if upload_textures(m) == 1 {
        let state = resource(m);
        let table = m.lw(off(state, 0xc));
        let textures = m.lw(off(table, 0xc));
        arena::reset(m, textures);
        1
    } else {
        0
    };
    m.pop_frame(0x18);
    result
}

/// 800CBA54 (resource step 6): returns 2 (done).
pub fn resource_done() -> u32 {
    2
}

/// 800CBA5C: the page shown while loading (+0x10).
pub fn set_page(m: &mut Machine, page: u32) {
    let state = resource(m);
    m.sw(off(state, 0x10), page);
}

/// 800CBA6C: install resource 0x45's scripts (entry 0), page 1.
pub fn install_scripts(m: &mut Machine) {
    m.push_frame(0x18);
    let state = resource(m);
    let table = m.lw(off(state, 0xc));
    let scripts = m.lw(table);
    ov1_script::set_scripts(m, scripts);
    ov1_script::select_page(m, 1);
    m.pop_frame(0x18);
}

// ---- textures ----------------------------------------------------------------------

/// 800CE5E4: prepare uploading the TIM table `table` (count, offsets;
/// relocated), `per_call` per call, with a 4-byte arena mark for the
/// decompression scratch. (`a1` is unused.)
pub fn begin_textures(m: &mut Machine, table: u32, _unused: u32, per_call: u32) {
    m.push_frame(0x20);
    let state = m.lw(TEXTURES);
    let count = m.lw(table);
    m.sw(off(state, 4), count);
    let relocated = dispatch::call(m, ov1_script::RELOCATE_DIRECTORY, [table, 0, 0, 0]);
    let state = m.lw(TEXTURES);
    m.sw(state, relocated);
    let mark = arena::alloc(m, 4);
    let state = m.lw(TEXTURES);
    m.sw(off(state, 8), mark);
    m.sw(off(state, 0x10), per_call);
    m.sw(off(state, 0x14), 0);
    m.pop_frame(0x20);
}

/// 800CE644: upload the next batch: plain TIMs (magic 0x10) with DrawSync,
/// "Ulz" ones decompressed to the arena mark first. Returns 1 (arena reset
/// to the mark) when the table is done, else 0.
pub fn upload_textures(m: &mut Machine) -> u32 {
    m.push_frame(0x20);
    let state = m.lw(TEXTURES);
    let mut index = m.lw(off(state, 0x14));
    let per_call = m.lw(off(state, 0x10));
    let mut scratch = m.lw(off(state, 8));
    let mut done = None;
    if (index as i32) < (index.wrapping_add(per_call) as i32) {
        loop {
            let state = m.lw(TEXTURES);
            let count = m.lw(off(state, 4));
            if (index as i32) >= (count as i32) {
                done = Some(m.lw(off(state, 8)));
                break;
            }
            let table = m.lw(state);
            let tim = m.lw(off(table, index << 2));
            let magic = m.lw(tim);
            if magic == ULZ {
                let end = dispatch::call(m, DECOMPRESS, [tim, scratch, 0, 0]);
                let image = scratch;
                scratch = end;
                dispatch::call(m, UPLOAD_TIM, [image, 0, 0, 0]);
            } else if magic == 0x10 {
                dispatch::call(m, UPLOAD_TIM, [tim, 0, 0, 0]);
                dispatch::call(m, DRAW_SYNC, [0, 0, 0, 0]);
            }
            let state = m.lw(TEXTURES);
            let next = m.lw(off(state, 0x14));
            let per_call = m.lw(off(state, 0x10));
            index = index.wrapping_add(1);
            if (index as i32) >= (next.wrapping_add(per_call) as i32) {
                break;
            }
        }
    }
    let result = match done {
        Some(mark) => {
            arena::reset(m, mark);
            1
        }
        None => {
            let state = m.lw(TEXTURES);
            let count = m.lw(off(state, 4));
            if (index as i32) < (count as i32) {
                let next = m.lw(off(state, 0x14));
                let per_call = m.lw(off(state, 0x10));
                m.sw(off(state, 0x14), next.wrapping_add(per_call));
                0
            } else {
                let mark = m.lw(off(state, 8));
                arena::reset(m, mark);
                1
            }
        }
    };
    m.pop_frame(0x20);
    result
}

// ---- CD streams -----------------------------------------------------------------------

/// 800D0254: clear the stream block (flags, stream and its four fields).
pub fn reset_streams(m: &mut Machine) {
    let stream = m.lw(STREAM);
    m.sw(stream, 0);
    m.sh(off(stream, 0x1004), 0);
    for offset in [0x1006, 0x1008, 0x100a] {
        m.sh(off(stream, offset), 0);
    }
    m.sw(off(stream, 0x100c), 0);
}

/// 800D0280: the stream to play.
pub fn select_stream(m: &mut Machine, stream_id: u32) {
    let stream = m.lw(STREAM);
    m.sh(off(stream, 0x1004), stream_id as u16);
}

/// 800D0290: start streaming (flag bit 0).
pub fn start_stream(m: &mut Machine) {
    let stream = m.lw(STREAM);
    let flags = m.lw(stream);
    m.sw(stream, flags | 1);
}

/// 800D02B0: stop streaming and reset the CD queue (8002C280(0, 0)).
pub fn stop_stream(m: &mut Machine) {
    m.push_frame(0x18);
    let stream = m.lw(STREAM);
    let flags = m.lw(stream);
    m.sw(stream, flags & 0xffff_fffe);
    dispatch::call(m, RESET_QUEUE, [0, 0, 0, 0]);
    m.pop_frame(0x18);
}

/// 800D02EC: while streaming, restart the stream (8002C4B0 with the stream
/// record +0x1004, once, no callback) when it has ended (8002C730 == 1) and
/// no resource is pending (8002C158(1) == 0).
pub fn poll_stream(m: &mut Machine) {
    m.push_frame(0x18);
    let stream = m.lw(STREAM);
    if m.lw(stream) & 1 != 0 {
        let record = off(stream, 0x1004);
        let id = m.lh(off(stream, 0x1004)) as u32;
        if dispatch::call(m, STREAM_STATE, [id, 0, 0, 0]) == 1
            && dispatch::call(m, WAIT_RESOURCES, [1, 0, 0, 0]) == 0
        {
            dispatch::call(m, QUEUE_STREAM, [record, 1, 0, 0]);
        }
    }
    m.pop_frame(0x18);
}

/// 800D0358 (unreferenced): read the decoded CD audio of the stream block
/// (SpuReadDecodedData(+4, 5), then SpuIsTransferCompleted(1)) and return
/// the half (`channel` nonzero: right) of the buffer just completed (+0x4 /
/// +0x404 when the read returned nonzero, else +0xCC / +0x4CC).
pub fn decoded_audio(m: &mut Machine, channel: u32) -> u32 {
    m.push_frame(0x20);
    let stream = m.lw(STREAM);
    let half = dispatch::call(m, SPU_READ_DECODED_DATA, [off(stream, 4), 5, 0, 0]);
    dispatch::call(m, SPU_IS_TRANSFER_COMPLETED, [1, 0, 0, 0]);
    let stream = m.lw(STREAM);
    let (left, right) = if half == 0 {
        (off(stream, 0xcc), off(stream, 0x4cc))
    } else {
        (off(stream, 4), off(stream, 0x404))
    };
    m.pop_frame(0x20);
    if channel != 0 { right } else { left }
}

/// 8009F358 SpuReadDecodedData / 8009F7D8 SpuIsTransferCompleted (libspu,
/// `sdk.rs`).
const SPU_READ_DECODED_DATA: u32 = 0x8009_f358;
const SPU_IS_TRANSFER_COMPLETED: u32 = 0x8009_f7d8;
