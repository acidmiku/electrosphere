//! Host SPU device (`src/machine_audio.rs`).
//!
//! Unit tests drive the device through the `ac3_flight::sdk` libspu
//! translations exactly as the game does (host calls on a `Machine`) and
//! check every register store. The ignored tests need `AC3_DATA_DIR`:
//! - the mixer against the `ac3-audio` renderers (validated against Redux),
//! - original Redux captures in `analysis/flight-runtime`, recorded with
//!   `.spu/capture-spu.py`: every libspu register store (instruction, cycle,
//!   value), the libspu calls with their arguments, the SpuGetAllKeysStatus
//!   reads, save states and the mixed PCM. `spu-002` (cold boot from
//!   SpuInit), `spu-005` (natural first hub) and `spu-006` (mission start
//!   and flight) check the libspu register stores; `spu-008` and `spu-009`
//!   (the same two scenes with a quarter-speed recorder) the mixed PCM and
//!   the key-status reads; `spu-009` also snapshots the sound update 8002DDCC.
use ac3_flight::{Device, HostCall, Machine, sdk};
use ac3_port::machine_audio;
use ac3_port::machine_audio::{
    OutputStage, Phase, RegisterWrite, SPU_BASE, SPU_RAM_BYTES, SpuDevice, calls, reg,
};
use serde_json::Value;
use std::{cell::RefCell, path::PathBuf, rc::Rc};

struct Shared(Rc<RefCell<SpuDevice>>);
impl Device for Shared {
    fn call(&mut self, ram: &mut [u8], scratch: &mut [u8], call: HostCall) -> Option<u32> {
        self.0.borrow_mut().call(ram, scratch, call)
    }
}

fn attach(m: &mut Machine, spu: SpuDevice) -> Rc<RefCell<SpuDevice>> {
    let spu = Rc::new(RefCell::new(spu));
    m.device = Some(Box::new(Shared(spu.clone())));
    spu
}

/// A machine with libspu's resident initialized data that the routines read
/// (address alignment 2/3/8/7) and a logging device.
fn rig() -> (Machine, Rc<RefCell<SpuDevice>>) {
    let mut m = Machine::default();
    for (address, value) in [
        (0x800b_2668, 2),
        (0x800b_266c, 3),
        (0x800b_2670, 8),
        (0x800b_2674, 7),
    ] {
        m.sw(address, value);
    }
    let mut spu = SpuDevice::new();
    spu.set_logging(true);
    let spu = attach(&mut m, spu);
    (m, spu)
}

fn writes(spu: &Rc<RefCell<SpuDevice>>) -> Vec<(u32, u32, u32)> {
    spu.borrow_mut()
        .take_writes()
        .into_iter()
        .map(|w| (w.pc, w.address - SPU_BASE, w.value))
        .collect()
}

fn voice_attr(m: &mut Machine, attr: u32, voices: u32, mask: u32, fields: &[(u32, u32, u8)]) {
    for offset in (0..0x40).step_by(4) {
        m.sw(attr + offset, 0);
    }
    m.sw(attr, voices);
    m.sw(attr + 4, mask);
    for &(offset, value, width) in fields {
        match width {
            2 => m.sh(attr + offset, value as u16),
            _ => m.sw(attr + offset, value),
        }
    }
}

// ---- libspu register semantics ---------------------------------------------------------

#[test]
fn set_key_stores_both_halves_and_starts_after_the_key_on_latency() {
    let (mut m, spu) = rig();
    sdk::spu_set_key(&mut m, 1, 0x0180_0003);
    assert_eq!(
        writes(&spu),
        [(0x8009_f4a0, 0x188, 0x0003), (0x8009_f4a8, 0x18a, 0x80)]
    );
    assert_eq!(m.lw(sdk::SPU_KEYED), 0x0080_0003);
    let state = spu.borrow().voice_state(0);
    assert!(state.pending && !state.running);
    // A fresh key-on reads ENVX 0 (the original then reports status 3).
    assert_eq!(spu.borrow().read_register(reg::VOICE_ENVX), 0);
    spu.borrow_mut().mix(machine_audio::KEY_ON_LATENCY as usize);
    assert!(spu.borrow().voice_state(0).pending);
    spu.borrow_mut().mix(1);
    let state = spu.borrow().voice_state(0);
    assert!(state.running && !state.pending);
    sdk::spu_set_key(&mut m, 0, 0x0000_0002);
    assert_eq!(
        writes(&spu),
        [(0x8009_f55c, 0x18c, 2), (0x8009_f560, 0x18e, 0)]
    );
    assert_eq!(m.lw(sdk::SPU_KEYED), 0x0080_0001);
    // Buffered mode (800B2630 bit 0) keeps keys in RAM: no device access.
    m.sw(0x800b_2630, 1);
    sdk::spu_set_key(&mut m, 1, 4);
    assert!(writes(&spu).is_empty());
}

#[test]
fn set_common_attr_writes_volumes_modes_and_control_bits() {
    let (mut m, spu) = rig();
    let attr = 0x8010_0000;
    // Mask 0: every field. Main volume left in sweep mode 3 (linear
    // decrease), right direct; CD/external volumes; CD reverb off, CD mix on,
    // external reverb on, external mix off.
    for (offset, value) in [
        (0x4, 0x0123u32),
        (0x6, 0x8456),
        (0x8, 3),
        (0xa, 0),
        (0x10, 0x7fff),
        (0x12, 0x1234),
        (0x1c, 0x0011),
        (0x1e, 0x0022),
    ] {
        m.sh(attr + offset, value as u16);
    }
    m.sw(attr, 0);
    m.sw(attr + 0x14, 0);
    m.sw(attr + 0x18, 1);
    m.sw(attr + 0x20, 1);
    m.sw(attr + 0x24, 0);
    spu.borrow_mut().write_register(reg::CONTROL, 0xc004);
    spu.borrow_mut().take_writes();
    sdk::spu_set_common_attr(&mut m, attr);
    assert_eq!(
        writes(&spu),
        [
            // Mode 3 → 0xA000 with the rate clamped to 0x7F.
            (0x8009_f958, 0x180, 0xa07f),
            (0x8009_fa18, 0x182, 0x0456),
            (0x8009_fa3c, 0x1b0, 0x7fff),
            (0x8009_fa60, 0x1b2, 0x1234),
            (0x8009_fa84, 0x1b4, 0x0011),
            (0x8009_faa8, 0x1b6, 0x0022),
            (0x8009_fafc, 0x1aa, 0xc000),
            (0x8009_fb50, 0x1aa, 0xc001),
            (0x8009_fba4, 0x1aa, 0xc009),
            (0x8009_fbf8, 0x1aa, 0xc009),
        ]
    );
    // AC3's master volume call (8002D3E4): mask 3, modes ignored.
    m.sw(attr, 3);
    m.sh(attr + 4, 0x3fff);
    m.sh(attr + 6, 0x3fff);
    sdk::spu_set_common_attr(&mut m, attr);
    assert_eq!(
        writes(&spu),
        [(0x8009_f958, 0x180, 0x3fff), (0x8009_fa18, 0x182, 0x3fff)]
    );
    // A negative sweep rate clamps to 0; mode 7 is exponential decrease.
    m.sw(attr, 0x5);
    m.sh(attr + 4, 0xfff0);
    m.sh(attr + 8, 7);
    sdk::spu_set_common_attr(&mut m, attr);
    assert_eq!(writes(&spu), [(0x8009_f958, 0x180, 0xe000)]);
}

#[test]
fn set_voice_attr_writes_every_field_in_the_original_order() {
    let (mut m, spu) = rig();
    // _spu_note2pitch tables (any contents: the device must agree with the
    // translated routine on them).
    for i in 0..12 {
        m.sh(0x800b_2ab4 + i * 2, (0x8000 + i * 0x0d00) as u16);
    }
    for i in 0..128 {
        m.sh(0x800b_2acc + i * 2, (0x8000 + i * 0x7f) as u16);
    }
    let (note, sample_note) = (0x4123u32, 0x3c40u32);
    let expected_pitch = sdk::spu_note2pitch(
        &mut m.clone(),
        sample_note >> 8,
        sample_note & 0xff,
        note >> 8,
        note & 0xff,
    );
    assert_ne!(expected_pitch, 0);
    spu.borrow_mut()
        .write_register(5 * 16 + reg::VOICE_ADSR1, 0xffff);
    spu.borrow_mut()
        .write_register(5 * 16 + reg::VOICE_ADSR2, 0xffff);
    spu.borrow_mut().take_writes();
    let attr = 0x8010_0000;
    voice_attr(
        &mut m,
        attr,
        1 << 5,
        0,
        &[
            (0x08, 0x9234, 2), // volume left (bit 15 is dropped)
            (0x0a, 0x0040, 2), // volume right, mode 6 → 0xD000
            (0x0c, 0, 2),
            (0x0e, 6, 2),
            (0x14, 0x1000, 2),
            (0x16, note, 2),
            (0x18, sample_note, 2),
            (0x1c, 0x1234, 4), // misaligned: rounded up to 0x1238
            (0x20, 0x2000, 4),
            (0x24, 5, 4), // attack exponential
            (0x28, 7, 4), // sustain exponential decrease
            (0x2c, 7, 4), // release exponential
            (0x30, 0x90, 2),
            (0x32, 0x20, 2),
            (0x34, 0x33, 2),
            (0x36, 0x40, 2),
            (0x38, 0x09, 2),
            (0x3a, 0xa5a5, 2),
            (0x3c, 0x5a5a, 2),
        ],
    );
    sdk::spu_set_voice_attr(&mut m, attr);
    let base = 5 * 16;
    let adsr1_ar = (0xa5a5 & 0xff) | ((0x7f | 0x80) << 8);
    let adsr1_dr = (adsr1_ar & 0xff0f) | (0xf << 4);
    let adsr2_sr = (0x5a5a & 0x3f) | ((0x33 | 0x300) << 6);
    let adsr2_rr = (adsr2_sr & 0xffc0) | 0x1f | 0x20;
    let adsr1_sl = (adsr1_dr & 0xfff0) | 9;
    assert_eq!(
        writes(&spu),
        [
            (0x8009_fde0, base + 4, 0x1000),
            (0x8009_fe3c, base + 4, expected_pitch),
            (0x8009_ff1c, base, 0x1234),
            (0x8009_fffc, base + 2, 0xd040),
            (0x8009_e090, base + 6, 0x1238 >> 3),
            (0x8009_e090, base + 0xe, 0x2000 >> 3),
            (0x800a_0064, base + 8, 0xa5a5),
            (0x800a_0090, base + 0xa, 0x5a5a),
            (0x800a_010c, base + 8, adsr1_ar),
            (0x800a_0158, base + 8, adsr1_dr),
            (0x800a_0210, base + 0xa, adsr2_sr),
            (0x800a_028c, base + 0xa, adsr2_rr & 0xffff),
            (0x800a_02d8, base + 8, adsr1_sl),
        ]
    );
    assert_eq!(m.lhu(sdk::SPU_VOICE_NOTE + 10), sample_note as u16);
    // Without SAMPLE_NOTE in the mask, NOTE uses the stored sample note.
    m.sh(sdk::SPU_VOICE_NOTE + 10, 0x3c00);
    voice_attr(&mut m, attr, 1 << 5, 0x20, &[(0x16, note, 2)]);
    sdk::spu_set_voice_attr(&mut m, attr);
    let pitch = sdk::spu_note2pitch(&mut m.clone(), 0x3c, 0, note >> 8, note & 0xff);
    assert_eq!(writes(&spu), [(0x8009_fe3c, base + 4, pitch)]);
    // The key-on attribute AC3 builds (8002DCA8, mask 0x600E3).
    voice_attr(
        &mut m,
        attr,
        1,
        0x0006_00e3,
        &[
            (0x08, 0x1d38, 2),
            (0x0a, 0xe2c8, 2),
            (0x16, 0x3c00, 2),
            (0x18, 0x4800, 2),
            (0x1c, 0x4c850, 4),
            (0x3a, 0x80ff, 2),
            (0x3c, 0x5fc9, 2),
        ],
    );
    sdk::spu_set_voice_attr(&mut m, attr);
    let pitch = sdk::spu_note2pitch(&mut m.clone(), 0x48, 0, 0x3c, 0);
    assert_eq!(
        writes(&spu),
        [
            (0x8009_fe3c, 4, pitch),
            (0x8009_ff1c, 0, 0x1d38),
            (0x8009_fffc, 2, 0x62c8),
            (0x8009_e090, 6, 0x4c850 >> 3),
            (0x800a_0064, 8, 0x80ff),
            (0x800a_0090, 0xa, 0x5fc9),
        ]
    );
}

/// Reverb tables of mode `mode` in RAM: work-area start and 32 registers.
fn reverb_tables(m: &mut Machine, mode: u32, start: u32) {
    m.sw(0x800b_2bd4 + mode * 4, start);
    let params = 0x800b_2c04 + mode * 0x44;
    m.sw(params, 0);
    for k in 0..32 {
        m.sh(params + 4 + k * 2, (0x100 * k + mode) as u16);
    }
}

#[test]
fn reverb_mode_param_writes_the_mode_registers_and_zeroes_the_depth() {
    let (mut m, spu) = rig();
    reverb_tables(&mut m, 3, 0xf6f8);
    m.sh(0x800b_25ec, 0x1111);
    spu.borrow_mut().write_register(reg::CONTROL, 0xc080);
    spu.borrow_mut().take_writes();
    let attr = 0x800c_43b8;
    // AC3's 8002D5F8: mask 1, mode 3, depth 0x1999 (ignored with a mode).
    m.sw(attr, 1);
    m.sw(attr + 4, 3);
    m.sh(attr + 8, 0x1999);
    m.sh(attr + 0xa, 0x1999);
    let v0 = m.host_call_result(calls::SPU_SET_REVERB_MODE_PARAM, [attr, 0, 0, 0], 99);
    assert_eq!(v0, 0);
    let mut expected = vec![
        (0x8009_e830, 0x1aa, 0xc000),
        (0x8009_e8a4, 0x184, 0),
        (0x8009_e8a8, 0x186, 0),
    ];
    for k in 0..32 {
        let pc = match k {
            0..16 => 0x8009_e9a4 + 0x24 * k,
            16..31 => 0x8009_ebe8 + 0x28 * (k - 16),
            _ => 0x8009_ee3c,
        };
        expected.push((pc, 0x1c0 + 2 * k, 0x100 * k + 3));
    }
    expected.push((0x8009_dfd0, 0x1a2, 0xf6f8));
    expected.push((0x8009_e934, 0x1aa, 0xc080));
    assert_eq!(writes(&spu), expected);
    assert_eq!(m.lw(0x800b_25e8), 3);
    assert_eq!(m.lw(0x800b_25e0), 0xf6f8);
    assert_eq!((m.lw(0x800b_25f0), m.lw(0x800b_25f4)), (0, 0));
    assert_eq!(m.lhu(0x800b_25ec), 0);
    // Echo mode 7: delay and feedback rewrite their registers only.
    reverb_tables(&mut m, 7, 0xcff8);
    m.sw(0x800b_25e8, 7);
    m.sw(attr, 0x18);
    m.sw(attr + 0xc, 0x40);
    m.sw(attr + 0x10, 0x50);
    assert_eq!(
        m.host_call_result(calls::SPU_SET_REVERB_MODE_PARAM, [attr, 0, 0, 0], 99),
        0
    );
    let p = |k: u32| 0x100 * k + 7;
    let (long, short) = ((0x40u32 << 13) / 127, (0x40u32 << 12) / 127);
    let reg_of = |k: u32| 0x1c0 + 2 * k;
    let got: Vec<(u32, u32)> = writes(&spu).into_iter().map(|(_, o, v)| (o, v)).collect();
    assert_eq!(
        got,
        [
            (reg_of(7), (0x50 * 0x8100) / 127),
            (reg_of(10), (long - p(0)) & 0xffff),
            (reg_of(11), (short - p(1)) & 0xffff),
            (reg_of(12), p(13) + short),
            (reg_of(16), p(17) + short),
            (reg_of(26), p(28) + short),
            (reg_of(27), p(29) + short),
        ]
    );
    assert_eq!((m.lw(0x800b_25f0), m.lw(0x800b_25f4)), (0x40, 0x50));
    // Bad mode and a reserved work area fail without stores.
    m.sw(attr, 1);
    m.sw(attr + 4, 10);
    assert_eq!(
        m.host_call_result(calls::SPU_SET_REVERB_MODE_PARAM, [attr, 0, 0, 0], 0),
        u32::MAX
    );
    m.sw(0x800b_26ac, 0x8010_1000);
    m.sw(0x8010_1000, 0x1000);
    m.sw(0x8010_1004, 0x8_0000);
    m.sw(attr + 4, 3);
    assert_eq!(
        m.host_call_result(calls::SPU_SET_REVERB_MODE_PARAM, [attr, 0, 0, 0], 0),
        u32::MAX
    );
    assert!(writes(&spu).is_empty());
}

#[test]
fn reverb_depth_switch_and_voice_mask() {
    let (mut m, spu) = rig();
    let attr = 0x8010_0000;
    m.sw(attr, 6);
    m.sh(attr + 8, 0x1234);
    m.sh(attr + 0xa, 0x5678);
    sdk::spu_set_reverb_depth(&mut m, attr);
    assert_eq!(
        writes(&spu),
        [(0x8009_ee74, 0x184, 0x1234), (0x8009_eea8, 0x186, 0x5678)]
    );
    assert_eq!(m.lhu(sdk::SPU_REVERB_DEPTH + 2), 0x5678);
    spu.borrow_mut().write_register(reg::CONTROL, 0xc000);
    spu.borrow_mut().take_writes();
    assert_eq!(sdk::spu_set_reverb(&mut m, 1), 1);
    assert_eq!(writes(&spu), [(0x8009_e368, 0x1aa, 0xc080)]);
    assert_eq!(sdk::spu_set_reverb(&mut m, 0), 0);
    assert_eq!(writes(&spu), [(0x8009_e368, 0x1aa, 0xc000)]);
    assert_eq!(
        sdk::spu_set_reverb_voice(&mut m, 1, 0x0081_0001),
        0x0081_0001
    );
    assert_eq!(
        writes(&spu),
        [(0x8009_f00c, 0x198, 0x0001), (0x8009_f020, 0x19a, 0x81)]
    );
    assert_eq!(
        sdk::spu_set_reverb_voice(&mut m, 0, 0x0080_0000),
        0x0001_0001
    );
    assert_eq!(
        writes(&spu),
        [(0x8009_f0d8, 0x198, 0x0001), (0x8009_f0f0, 0x19a, 0x01)]
    );
    assert_eq!(
        sdk::spu_set_reverb_voice(&mut m, 8, 0x00ff_00f0),
        0x00ff_00f0
    );
    assert_eq!(
        writes(&spu),
        [(0x8009_f188, 0x198, 0x00f0), (0x8009_f194, 0x19a, 0xff)]
    );
    // AC3's 8002D6CC: every voice off.
    assert_eq!(sdk::spu_set_reverb_voice(&mut m, 0, 0x00ff_ffff), 0);
}

#[test]
fn uploads_by_programmed_io_and_dma_reach_sound_ram() {
    let (mut m, spu) = rig();
    let source = 0x8010_0000;
    for i in 0..0x100 {
        m.sb(source + i, i as u8);
    }
    // AC3's 8002D4E8 upload: I/O mode, start address, write, completion.
    assert_eq!(
        ac3_flight::loading::upload_samples(&mut m, 0x1010, source, 0x45),
        1
    );
    {
        let spu = spu.borrow();
        let ram = spu.sound_ram();
        // 0x45 bytes as 0x23 whole halfwords: one byte more than asked.
        assert_eq!(&ram[0x1010..0x1056], &(0..0x46).collect::<Vec<u8>>()[..]);
        assert_eq!(ram[0x1056], 0);
    }
    assert_eq!(
        writes(&spu),
        [
            (0x8009_d95c, 0x1a6, 0x1010 >> 3),
            (0x8009_d9c8, 0x1aa, 0x10),
            (0x8009_d9c8, 0x1aa, 0x10),
            (0x8009_da68, 0x1aa, 0),
        ]
    );
    // DMA (transfer mode 0): 100 bytes go as two 64-byte blocks.
    sdk::spu_set_transfer_mode(&mut m, 0);
    sdk::spu_set_transfer_start_addr(&mut m, 0x2000);
    sdk::spu_write(&mut m, source, 100);
    let log = spu.borrow_mut().take_writes();
    let summary: Vec<(u32, u32, u32)> = log.iter().map(|w| (w.pc, w.address, w.value)).collect();
    assert_eq!(
        summary,
        [
            (0x8009_dccc, SPU_BASE + 0x1a6, 0x400),
            (0x8009_dd38, SPU_BASE + 0x1aa, 0x20),
            (0x8009_de6c, 0x1f80_10c0, source),
            (0x8009_de88, 0x1f80_10c4, 0x0002_0010),
            (0x8009_deb4, 0x1f80_10c8, 0x0100_0201),
            (0x8009_db1c, SPU_BASE + 0x1aa, 0),
        ]
    );
    assert!(log[2..5].iter().all(|w| w.width == 4));
    let spu = spu.borrow();
    assert_eq!(
        &spu.sound_ram()[0x2000..0x2080],
        &(0..0x80).collect::<Vec<u8>>()[..]
    );
}

#[test]
fn spu_init_resets_registers_voices_and_libspu_state() {
    let (mut m, spu) = rig();
    for i in 0..16 {
        m.sb(0x800b_2684 + i, 7);
    }
    m.sw(0x800b_2bd4, 0xfffe);
    m.sw(0x800b_2630, 1);
    spu.borrow_mut().write_register(reg::CONTROL, 0x0080);
    spu.borrow_mut().take_writes();
    m.host_call(calls::SPU_INIT, [0; 4]);
    let log = writes(&spu);
    assert_eq!(log.len(), 5 + 7 + 8 + 3 + 24 * 6 + 4 + 1 + 1);
    assert_eq!(log[0], (0x8009_d6f4, 0x180, 0));
    assert_eq!(log[23], (0x8009_d858, 0, 0));
    assert_eq!(log.last(), Some(&(0x8009_dfd0, 0x1a2, 0xfffe)));
    let spu = spu.borrow();
    assert_eq!(spu.register(reg::CONTROL), 0xc000);
    assert_eq!(spu.register(23 * 16 + reg::VOICE_PITCH), 0x3fff);
    assert_eq!(spu.register(23 * 16 + reg::VOICE_START), 0x200);
    assert_eq!(&spu.sound_ram()[0x1000..0x1010], &[7; 16]);
    // The waits let the 24 voices start and release (at volume 0).
    assert!((0..24).all(|v| {
        let state = spu.voice_state(v);
        state.running && state.phase == Phase::Release && state.level == 0
    }));
    assert_eq!(
        spu.samples(),
        2 * (machine_audio::KEY_ON_LATENCY as u64 + 1)
    );
    assert!((0..24).all(|v| m.lhu(sdk::SPU_VOICE_NOTE + v * 2) == 0xc000));
    assert_eq!(m.lw(0x800b_25e0), 0xfffe);
    assert_eq!(m.lw(0x800b_2630), 0);
    assert_eq!(m.lw(0x800b_2678), 1);
    assert_eq!(m.lhu(0x800b_265c), 0x200);
}

#[test]
fn cd_mix_programs_the_cdrom_attenuation() {
    let (mut m, spu) = rig();
    let atv = 0x8010_0000;
    for (i, byte) in [150u8, 1, 140, 2].into_iter().enumerate() {
        m.sb(atv + i as u32, byte);
    }
    assert_eq!(m.host_call_result(calls::CD_MIX, [atv, 0, 0, 0], 0), 1);
    assert_eq!(spu.borrow().cd_attenuation(), [150, 1, 140, 2]);
    let log = spu.borrow_mut().take_writes();
    let summary: Vec<(u32, u32, u32, u8)> = log
        .iter()
        .map(|w| (w.pc, w.address, w.value, w.width))
        .collect();
    assert_eq!(
        summary,
        [
            (0x800a_284c, 0x1f80_1800, 2, 1),
            (0x800a_2860, 0x1f80_1802, 150, 1),
            (0x800a_2874, 0x1f80_1803, 1, 1),
            (0x800a_2884, 0x1f80_1800, 3, 1),
            (0x800a_2898, 0x1f80_1801, 140, 1),
            (0x800a_28ac, 0x1f80_1802, 2, 1),
            (0x800a_28bc, 0x1f80_1803, 0x20, 1),
        ]
    );
}

// ---- mixer ------------------------------------------------------------------------------

/// An ADPCM block: filter/shift header, flags, 28 nibbles of `nibble`.
fn block(header: u8, flags: u8, nibble: u8) -> [u8; 16] {
    let mut b = [nibble | (nibble << 4); 16];
    b[0] = header;
    b[1] = flags;
    b
}

fn load(spu: &mut SpuDevice, address: usize, data: &[u8]) {
    let mut ram = spu.sound_ram().to_vec();
    ram[address..address + data.len()].copy_from_slice(data);
    let ports: Vec<u8> = (0..0x200)
        .flat_map(|i| spu.register(i * 2).to_le_bytes())
        .collect();
    spu.load_state(&ram, &ports);
}

/// Voice 0 at full volume over the sample at `address`.
fn voice0(spu: &mut SpuDevice, address: u32, pitch: u16, adsr1: u16, adsr2: u16) {
    for (offset, value) in [
        (reg::VOICE_VOLUME_LEFT, 0x3fff),
        (reg::VOICE_VOLUME_RIGHT, 0x4000), // -0x4000: inverted
        (reg::VOICE_PITCH, pitch),
        (reg::VOICE_START, (address >> 3) as u16),
        (reg::VOICE_ADSR1, adsr1),
        (reg::VOICE_ADSR2, adsr2),
    ] {
        spu.write_register(offset, value);
    }
    spu.write_register(reg::KEY_ON, 1);
}

#[test]
fn adpcm_loop_start_end_and_repeat_flags() {
    let mut spu = SpuDevice::new();
    spu.output = OutputStage::Redux;
    let mut data = Vec::new();
    data.extend(block(0x00, 0, 1));
    data.extend(block(0x00, 4, 2)); // loop start
    data.extend(block(0x00, 3, 3)); // end + repeat
    load(&mut spu, 0x2000, &data);
    voice0(&mut spu, 0x2000, 0x1000, 0x0000, 0x0000);
    // 6 latency samples, then 3 primed source samples: block 0 is consumed
    // after 25 more ticks, the end block after 81.
    let pcm = spu.mix(6 + 80);
    assert!(pcm[..6].iter().all(|f| *f == [0, 0]));
    assert!(pcm[6..].iter().any(|f| f[0] > 0 && f[1] < 0));
    let state = spu.voice_state(0);
    assert_eq!(state.loop_address, 0x2010);
    assert_eq!(state.address, 0x2030);
    assert_eq!(spu.read_register(reg::END) & 1, 1, "end flag seen");
    spu.mix(28);
    assert_eq!(
        spu.voice_state(0).address,
        0x2020,
        "repeat jumps to the loop"
    );
    assert_eq!(spu.voice_state(0).phase, Phase::Sustain);
    // A key-on clears the end flag.
    spu.write_register(reg::KEY_ON, 1);
    assert_eq!(spu.read_register(reg::END) & 1, 0);

    // End without repeat: muted (released at level 0) once the block is out.
    let mut spu = SpuDevice::new();
    let mut data = Vec::new();
    data.extend(block(0x00, 0, 1));
    data.extend(block(0x00, 1, 1));
    load(&mut spu, 0x3000, &data);
    voice0(&mut spu, 0x3000, 0x1000, 0x0000, 0x0000);
    // Source sample k is pulled on tick k - 2: the block after the end
    // block (sample 56) on tick 54.
    spu.mix(6 + 54);
    assert!(spu.voice_state(0).level > 0);
    spu.mix(1);
    let state = spu.voice_state(0);
    assert_eq!((state.phase, state.level), (Phase::Release, 0));
    assert_eq!(spu.read_register(reg::VOICE_ENVX), 0);
    assert!(spu.mix(100).iter().all(|f| *f == [0, 0]));
}

#[test]
fn envelope_rates_follow_the_hardware_counters() {
    // The ac3-audio attack vectors (knee at 6000h, period/step after it).
    for (adsr1, ticks_to_knee, knee_level, period, delta) in [
        (0xafffu16, 6144usize, 24576, 4, 4),
        (0xb1ff, 8192, 24576, 8, 6),
        (0xb5ff, 16384, 24576, 16, 6),
        (0xacff, 3511, 24577, 4, 7),
    ] {
        let mut spu = SpuDevice::new();
        load(&mut spu, 0x2000, &block(0, 3, 1));
        voice0(&mut spu, 0x2000, 0x1000, adsr1, 0x5fc9);
        spu.mix(6 + ticks_to_knee);
        assert_eq!(spu.voice_state(0).level, knee_level, "{adsr1:04x}");
        for _ in 1..period {
            spu.mix(1);
            assert_eq!(spu.voice_state(0).level, knee_level);
        }
        spu.mix(1);
        assert_eq!(spu.voice_state(0).level, knee_level + delta);
    }
    // Linear release after key-off: rate 0x1F*4 → -8 << 0 every 2^20 ticks
    // is too slow; rate 0x0A*4 → -8 << 1 per tick.
    let mut spu = SpuDevice::new();
    load(&mut spu, 0x2000, &block(0, 3, 1));
    voice0(&mut spu, 0x2000, 0x1000, 0x000f, 0x000a);
    spu.mix(6 + 10);
    let state = spu.voice_state(0);
    assert_eq!((state.phase, state.level), (Phase::Sustain, 0x7fff));
    spu.write_register(reg::KEY_OFF, 1);
    spu.mix(1);
    assert_eq!(spu.voice_state(0).phase, Phase::Release);
    assert_eq!(spu.voice_state(0).level, 0x7fff - 16);
    spu.mix(0x7fff / 16 + 1);
    assert_eq!(spu.voice_state(0).level, 0);
}

#[test]
fn pitch_counter_interpolation_and_volume_arithmetic() {
    // A constant +4096 source at unit pitch: the Gaussian weights sum to
    // ~1.0, the envelope holds 7FFFh and volumes divide toward zero.
    let mut spu = SpuDevice::new();
    spu.output = OutputStage::Redux;
    load(&mut spu, 0x2000, &block(0, 7, 1));
    voice0(&mut spu, 0x2000, 0x1000, 0x000f, 0x0000);
    let pcm = spu.mix(400);
    let last = pcm[399];
    assert!((4060..=4100).contains(&last[0]), "{last:?}");
    // Left 0x3FFF is 7FFEh: one step below unity; right 0x4000 is -8000h.
    assert!(last[1] < 0 && (i32::from(last[0]) + i32::from(last[1])).abs() <= 1);
    // Pitch above 0x3FFF runs at 0x4000: four source samples per tick.
    let mut spu = SpuDevice::new();
    let mut data = Vec::new();
    for _ in 0..64 {
        data.extend(block(0, 0, 0));
    }
    data.extend(block(0, 3, 0));
    load(&mut spu, 0x2000, &data);
    voice0(&mut spu, 0x2000, 0xffff, 0, 0);
    spu.mix(6 + 1);
    // 3 primed + 4 per tick after the first: 7 samples in block 0.
    spu.mix(7);
    assert_eq!(spu.voice_state(0).address, 0x2020);
}

#[test]
fn key_status_follows_the_original_formula() {
    let (mut m, spu) = rig();
    {
        let mut s = spu.borrow_mut();
        load(&mut s, 0x2000, &block(0, 3, 1));
        voice0(&mut s, 0x2000, 0x1000, 0x000f, 0x0000);
        s.mix(20);
    }
    let status = |m: &mut Machine, voice: u32, keyed: u32| {
        m.host_call_result(calls::SPU_KEY_STATUS, [0, voice, keyed, 0], 99)
    };
    assert_eq!(status(&mut m, 0, 1), 1, "keyed, sounding");
    assert_eq!(status(&mut m, 0, 0), 2, "released, sounding");
    assert_eq!(status(&mut m, 1, 1), 3, "keyed, silent");
    assert_eq!(status(&mut m, 1, 0), 0, "off");
}

#[test]
fn output_stages_and_cd_input() {
    for (stage, expected) in [
        // CD volume 0x4000 halves the input; main volume 0x2000 (x2 = 0x4000)
        // halves again.
        (OutputStage::Hardware, [2500, -2500]),
        (OutputStage::Redux, [10000, -10000]),
    ] {
        let mut spu = SpuDevice::new();
        spu.output = stage;
        spu.write_register(reg::CONTROL, 0xc001);
        spu.write_register(reg::CD_VOLUME_LEFT, 0x4000);
        spu.write_register(reg::CD_VOLUME_RIGHT, 0x4000);
        spu.write_register(reg::MAIN_VOLUME_LEFT, 0x2000);
        spu.write_register(reg::MAIN_VOLUME_RIGHT, 0x2000);
        spu.push_cd_audio(&[[10000, -10000]; 4]);
        assert_eq!(spu.mix(5), [expected, expected, expected, expected, [0, 0]]);
        assert_eq!(spu.cd_audio_queued(), 0);
    }
    // CD audio disabled (SPUCNT bit 0 clear): consumed, not mixed.
    let mut spu = SpuDevice::new();
    spu.output = OutputStage::Redux;
    spu.push_cd_audio(&[[1000, 1000]]);
    assert_eq!(spu.mix(1), [[0, 0]]);
}

#[test]
fn pal_fields_follow_the_redux_cycle_clock() {
    let mut spu = SpuDevice::new();
    let total: usize = (0..128).map(|_| spu.mix_field().len()).sum();
    // 128 * 677298 / 768 = 112883.0625 (MISSION_AUDIO.md).
    assert_eq!(total, 112_883);
    assert_eq!(spu.samples(), 112_883);
}

#[test]
fn wav_header_describes_44100_hz_stereo_pcm16() {
    let header = machine_audio::wav_header(400);
    assert_eq!(&header[0..4], b"RIFF");
    assert_eq!(u32::from_le_bytes(header[4..8].try_into().unwrap()), 436);
    assert_eq!(
        u32::from_le_bytes(header[24..28].try_into().unwrap()),
        44_100
    );
    assert_eq!(u16::from_le_bytes(header[22..24].try_into().unwrap()), 2);
    assert_eq!(u32::from_le_bytes(header[40..44].try_into().unwrap()), 400);
}

// ---- original data ----------------------------------------------------------------------

fn data_dir() -> PathBuf {
    PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("set AC3_DATA_DIR"))
}

/// The resident payload at 0x80010000 (libspu tables and data), SpuInit run.
fn resident_machine(spu: SpuDevice) -> (Machine, Rc<RefCell<SpuDevice>>) {
    let payload = std::fs::read(data_dir().join("extracted/programs/ACE.unpacked.bin")).unwrap();
    assert_eq!(payload.len(), ac3_flight::boot::PAYLOAD_BYTES);
    let mut m = Machine::default();
    m.write_bytes(0x8001_0000, &payload);
    let spu = attach(&mut m, spu);
    m.host_call(calls::SPU_INIT, [0; 4]);
    (m, spu)
}

/// Upload a bank's samples and key `id` on through the libspu translations
/// as AC3's 8002DCA8 does (SFX gain 208); returns the device.
fn play(bank_bytes: &[u8], id: u16) -> Rc<RefCell<SpuDevice>> {
    let bank = ac3_audio::Bank::parse(bank_bytes).unwrap();
    let mut spu = SpuDevice::new();
    spu.output = OutputStage::Redux;
    let (mut m, spu) = resident_machine(spu);
    let source = 0x8010_0000;
    m.write_bytes(source, bank.sample_bytes());
    let size = bank.sample_bytes().len() as u32;
    ac3_flight::loading::upload_samples(&mut m, bank.spu_address(), source, size);
    assert_eq!(
        &spu.borrow().sound_ram()[bank.spu_address() as usize..][..size as usize],
        bank.sample_bytes()
    );
    let sound = bank.sound(id).unwrap();
    let voice = if sound.voice_hint < 24 {
        u32::from(sound.voice_hint)
    } else {
        12
    };
    let scale = |v: i16| ((i32::from(v) * 208) >> 8) as u32;
    let attr = 0x8018_0000;
    voice_attr(
        &mut m,
        attr,
        1 << voice,
        0x0006_00e3,
        &[
            (0x08, scale(sound.volume_left), 2),
            (0x0a, scale(sound.volume_right), 2),
            (0x16, u32::from(sound.note), 2),
            (0x18, u32::from(sound.sample_note), 2),
            (0x1c, sound.spu_address, 4),
            (0x3a, u32::from(sound.adsr1), 2),
            (0x3c, u32::from(sound.adsr2), 2),
        ],
    );
    // SpuInit's voices end their dummy block before the first key-on.
    spu.borrow_mut().mix(64);
    sdk::spu_set_voice_attr(&mut m, attr);
    sdk::spu_set_key(&mut m, 1, 1 << voice);
    assert!(m.take_fault().is_ok());
    spu
}

#[test]
#[ignore = "requires AC3_DATA_DIR (sound banks and the resident program)"]
fn voices_match_the_validated_ac3_audio_renderers() {
    let banks = [
        "bpb_0000_0004.bin",
        "bpb_0069_0004.bin",
        "bpb_0072_0009.bin",
    ]
    .map(|name| std::fs::read(data_dir().join("extracted/archives").join(name)).unwrap());
    let mut compared = 0usize;
    let mut sounds = Vec::new();
    for id in [
        0x3bu16, 0x3c, 0x3d, 0x56, 0x58, 0x5a, 0x5b, 0x5c, 0x62, 0x63, 0x64, 0x65, 0x66,
    ] {
        for bytes in &banks {
            let bank = ac3_audio::Bank::parse(bytes).unwrap();
            let Ok(expected) = ac3_audio::render_menu_click(bank, id, 208) else {
                continue;
            };
            let spu = play(bytes, id);
            let pcm = spu.borrow_mut().mix(expected.frames.len() + 256);
            assert_eq!(
                pcm[..expected.frames.len()],
                expected.frames[..],
                "sound {id:02x}"
            );
            assert!(
                pcm[expected.frames.len()..].iter().all(|f| *f == [0, 0]),
                "sound {id:02x} ends with the source"
            );
            compared += expected.frames.len();
            sounds.push(id);
        }
    }
    let bank = ac3_audio::Bank::parse(&banks[2]).unwrap();
    for id in 0x5d..=0x61u16 {
        let mut voice = ac3_audio::BackgroundVoice::from_bank(bank, id, 208).unwrap();
        let expected = voice.render_frames(400_000).unwrap();
        let spu = play(&banks[2], id);
        let pcm = spu.borrow_mut().mix(400_000);
        assert_eq!(pcm, expected.frames, "background {id:02x}");
        compared += pcm.len();
        sounds.push(id);
    }
    println!(
        "{} sounds, {compared} stereo frames identical: {sounds:02x?}",
        sounds.len()
    );
    // 0x56 is in the menu and the mission bank.
    assert_eq!(sounds.len(), 14 + 5);
}

// ---- original captures --------------------------------------------------------------------

fn capture(name: &str) -> Option<PathBuf> {
    let directory = data_dir().join("analysis/flight-runtime").join(name);
    directory.join("events.jsonl").exists().then_some(directory)
}

fn events(directory: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(directory.join("events.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn n(value: &Value) -> u32 {
    value.as_u64().unwrap() as u32
}

/// SPU registers, DMA channel 4 and the CD-ROM registers: the device side.
fn device_address(address: u32) -> bool {
    (SPU_BASE..SPU_BASE + 0x400).contains(&address)
        || (0x1f80_10c0..=0x1f80_10c8).contains(&address)
        || (0x1f80_1800..0x1f80_1804).contains(&address)
}

fn original_store(e: &Value) -> RegisterWrite {
    let width = n(&e["w"]) as u8;
    let mask = match width {
        1 => 0xff,
        2 => 0xffff,
        _ => u32::MAX,
    };
    RegisterWrite {
        pc: n(&e["pc"]),
        address: n(&e["a"]),
        value: (e["v"].as_u64().unwrap() as u32) & mask,
        width,
    }
}

fn game_call(e: &Value) -> bool {
    e["k"] == "e" && !(0x8009_d548..0x800a_0568).contains(&n(&e["ra"]))
}

fn hex_bytes(text: &str) -> Vec<u8> {
    (0..text.len() / 2)
        .map(|i| u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).unwrap())
        .collect()
}

/// Run one original libspu call through the sdk translations.
fn replay_call(m: &mut Machine, e: &Value, blobs: &[u8], statuses: &[u32]) {
    let f = n(&e["f"]);
    let a: Vec<u32> = e["a"].as_array().unwrap().iter().map(n).collect();
    if let Some(bytes) = e["bytes"].as_str() {
        m.write_bytes(a[0], &hex_bytes(bytes));
    }
    if let Some(offset) = e["blob"].as_u64() {
        let size = n(&e["size"]) as usize;
        m.write_bytes(a[0], &blobs[offset as usize..offset as usize + size]);
    }
    match f {
        0x8009_e218 => m.host_call(f, [0; 4]),
        0x8009_f888 => drop(sdk::spu_set_common_attr(m, a[0])),
        0x8009_e498 => drop(m.host_call_result(f, [a[0], 0, 0, 0], 0)),
        0x800a_18e0 => drop(m.host_call_result(f, [a[0], 0, 0, 0], 1)),
        0x8009_f3c8 => drop(sdk::spu_set_key(m, a[0], a[1])),
        0x8009_fd68 => drop(sdk::spu_set_voice_attr(m, a[0])),
        0x8009_fcdc => {
            m.host_results.extend(statuses.iter().copied());
            sdk::spu_get_all_keys_status(m, a[0]);
            m.host_results.clear();
        }
        0x8009_ee48 => drop(sdk::spu_set_reverb_depth(m, a[0])),
        0x8009_e2b8 => drop(sdk::spu_set_reverb(m, a[0])),
        0x8009_eec8 => drop(sdk::spu_set_reverb_voice(m, a[0], a[1])),
        0x8009_f1b8 => drop(sdk::spu_clear_reverb_work_area(m, a[0])),
        0x8009_f7a8 => drop(sdk::spu_set_transfer_mode(m, a[0])),
        0x8009_f748 => drop(sdk::spu_set_transfer_start_addr(m, a[0])),
        0x8009_f588 => drop(sdk::spu_write(m, a[0], a[1])),
        0x8009_f7d8 => drop(sdk::spu_is_transfer_completed(m, a[0])),
        _ => panic!("unexpected libspu entry {f:08x}"),
    }
}

/// Registers whose saved value is not the last CPU store (or not modelled
/// by the oracle): ENVX, the end flags, status, the data port and the
/// current main volume.
fn compared_port(offset: u32) -> bool {
    !(offset < 0x180 && offset & 0xf == reg::VOICE_ENVX
        || matches!(offset, 0x19c | 0x19e | 0x1a8 | 0x1ae | 0x1b8 | 0x1ba))
}

const CAPTURE_AREA: usize = 0x1000;

struct Replay {
    calls: usize,
    stores: usize,
    mismatches: Vec<String>,
    ports_compared: usize,
    sound_ram_compared: usize,
}

/// Replay every libspu call the game made, from the window's RAM and SPU
/// state, through `sdk.rs` and the device; compare each call's register
/// stores with the original's, then the final register file and sound RAM
/// with the end save state.
fn replay_capture(directory: &std::path::Path) -> Replay {
    let events = events(directory);
    let read = |name: &str| std::fs::read(directory.join(name)).unwrap();
    let mut m = Machine::from_images(&read("start.ram.bin"), &read("start.scratch.bin")).unwrap();
    let mut spu = SpuDevice::new();
    spu.load_state(&read("start.spu-ram.bin"), &read("start.spu-ports.bin"));
    spu.set_logging(true);
    let spu = attach(&mut m, spu);
    let blobs = read("blobs.bin");
    let mut report = Replay {
        calls: 0,
        stores: 0,
        mismatches: Vec::new(),
        ports_compared: 0,
        sound_ram_compared: 0,
    };
    let starts: Vec<usize> = (0..events.len())
        .filter(|&i| game_call(&events[i]))
        .collect();
    for (k, &i) in starts.iter().enumerate() {
        let end = starts.get(k + 1).copied().unwrap_or(events.len());
        let window = &events[i + 1..end];
        let expected: Vec<RegisterWrite> = window
            .iter()
            .filter(|e| e["k"] == "w")
            .map(original_store)
            .filter(|w| device_address(w.address))
            .collect();
        let statuses: Vec<u32> = window
            .iter()
            .filter(|e| e["k"] == "s")
            .map(|e| n(&e["status"]))
            .collect();
        replay_call(&mut m, &events[i], &blobs, &statuses);
        if let Err(fault) = m.take_fault() {
            report.mismatches.push(format!("call {k}: fault {fault}"));
        }
        let got = spu.borrow_mut().take_writes();
        report.calls += 1;
        report.stores += expected.len();
        if got != expected {
            let first = got
                .iter()
                .zip(&expected)
                .position(|(a, b)| a != b)
                .unwrap_or(got.len().min(expected.len()));
            report.mismatches.push(format!(
                "call {k} {:08x} at cycle {}: {} stores, expected {}; first difference #{first}: {:x?} vs {:x?}",
                n(&events[i]["f"]),
                events[i]["c"],
                got.len(),
                expected.len(),
                got.get(first),
                expected.get(first)
            ));
        }
    }
    let ports = read("end.spu-ports.bin");
    let spu = spu.borrow();
    for offset in (0..0x200).step_by(2) {
        if !compared_port(offset) {
            continue;
        }
        let original = u16::from_le_bytes([ports[offset as usize], ports[offset as usize + 1]]);
        report.ports_compared += 1;
        if spu.register(offset) != original {
            report.mismatches.push(format!(
                "register {offset:03x}: {:04x}, original {original:04x}",
                spu.register(offset)
            ));
        }
    }
    let sound_ram = read("end.spu-ram.bin");
    assert_eq!(sound_ram.len(), SPU_RAM_BYTES);
    // 0x0000..0x0FFF is the hardware capture area (CD and voice 1/3 output),
    // which the device writes while it runs and the oracle does not model.
    let differing_at: Vec<usize> = (CAPTURE_AREA..SPU_RAM_BYTES)
        .filter(|&i| spu.sound_ram()[i] != sound_ram[i])
        .collect();
    let differing = differing_at.len();
    for &i in differing_at.iter().take(8) {
        println!(
            "  sound RAM {i:05x}: {:02x}, original {:02x}",
            spu.sound_ram()[i],
            sound_ram[i]
        );
    }
    report.sound_ram_compared = SPU_RAM_BYTES - CAPTURE_AREA;
    if differing != 0 {
        report
            .mismatches
            .push(format!("sound RAM: {differing} bytes differ"));
    }
    report
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the spu-002/005/006 original captures"]
fn libspu_calls_reproduce_the_original_register_stores() {
    let mut total = (0, 0);
    for name in ["spu-002", "spu-005", "spu-006"] {
        let Some(directory) = capture(name) else {
            panic!("capture {name} missing");
        };
        let report = replay_capture(&directory);
        println!(
            "{name}: {} libspu calls, {} original register stores reproduced in order, {} registers and {} sound RAM bytes compared at the end",
            report.calls, report.stores, report.ports_compared, report.sound_ram_compared
        );
        for line in report.mismatches.iter().take(20) {
            println!("  {line}");
        }
        assert!(
            report.mismatches.is_empty(),
            "{name}: {} mismatches",
            report.mismatches.len()
        );
        assert!(report.stores > 1000);
        total.0 += report.calls;
        total.1 += report.stores;
    }
    println!("total: {} calls, {} stores", total.0, total.1);
}

/// The sample an SPU store first affects in the oracle, from the cycle
/// count at its breakpoint (before the instruction executes; the store
/// itself happens `STORE_DELAY` cycles later).
fn sample_of(cycle: u64) -> u64 {
    (cycle + store_delay()) / 768 + 1
}

fn store_delay() -> u64 {
    std::env::var("AC3_SPU_STORE_DELAY").map_or(2, |v| v.parse().unwrap())
}

struct TraceMix {
    pcm: Vec<[i16; 2]>,
    /// (cycle, voice, original ENVX, device ENVX) of every key-status read.
    envx: Vec<(u64, u32, u16, u16)>,
}

/// Drive the device from the original register stores, each applied before
/// sample `cycle / 768 + 1` (counted from the window start). The oracle
/// brings the SPU up to date at register stores only: a read sees the
/// voices as of the last store, so reads do not advance the mix. Uploads go
/// through the device's data port: the recorded SpuWrite payload (SpuInit:
/// its 16-byte block) is pushed in 64-byte chunks right before each original
/// SPUCNT manual-write store (8009D9C8) that flushed them; DMA payloads land
/// at the channel-control store.
fn trace_mix(directory: &std::path::Path, last_cycle: u64) -> (TraceMix, SpuDevice) {
    let events = events(directory);
    let read = |name: &str| std::fs::read(directory.join(name)).unwrap();
    let start_ram = read("start.ram.bin");
    let blobs = read("blobs.bin");
    let mut spu = SpuDevice::new();
    spu.output = OutputStage::Redux;
    spu.load_state(&read("start.spu-ram.bin"), &read("start.spu-ports.bin"));
    let start = events.iter().find(|e| e["k"] == "start").unwrap();
    let origin = sample_of(start["c"].as_u64().unwrap());
    let mut pcm = Vec::new();
    let mut envx = Vec::new();
    let mut upload: Vec<u8> = Vec::new();
    let mut uploaded = 0usize;
    let (mut madr, mut bcr) = (0u32, 0u32);
    for e in &events {
        let Some(cycle) = e["c"].as_u64() else {
            continue;
        };
        if cycle > last_cycle {
            break;
        }
        match e["k"].as_str() {
            Some("w") => {
                let w = original_store(e);
                let due = (sample_of(cycle) - origin) as usize;
                if due > pcm.len() {
                    let frames = due - pcm.len();
                    pcm.extend(spu.mix(frames));
                }
                match w.address {
                    0x1f80_10c0 => madr = w.value,
                    0x1f80_10c4 => bcr = w.value,
                    0x1f80_10c8 if w.value == 0x0100_0201 => {
                        // DMA to sound RAM (the reverb work-area clear), sent
                        // through the data port in 32-halfword flushes.
                        let at = (madr & 0x1f_ffff) as usize;
                        let control = spu.register(reg::CONTROL);
                        let bytes = &start_ram[at..at + (bcr >> 16) as usize * 64];
                        for chunk in bytes.chunks(64) {
                            for pair in chunk.chunks(2) {
                                spu.write_register(
                                    reg::TRANSFER_FIFO,
                                    u16::from_le_bytes([pair[0], pair[1]]),
                                );
                            }
                            spu.write_register(reg::CONTROL, (control & 0xffcf) | 0x10);
                        }
                        spu.write_register(reg::CONTROL, control);
                    }
                    a if (SPU_BASE..SPU_BASE + 0x400).contains(&a) => {
                        let offset = a - SPU_BASE;
                        if w.pc == 0x8009_d9c8 {
                            let chunk = (upload.len() - uploaded).min(0x40);
                            for pair in upload[uploaded..uploaded + chunk].chunks(2) {
                                let half =
                                    u16::from_le_bytes([pair[0], *pair.get(1).unwrap_or(&0)]);
                                spu.write_register(reg::TRANSFER_FIFO, half);
                            }
                            uploaded += chunk;
                        }
                        spu.write_register(offset, w.value as u16);
                    }
                    _ => {}
                }
            }
            Some("e") if n(&e["f"]) == 0x8009_f588 => {
                let offset = e["blob"].as_u64().unwrap() as usize;
                upload = blobs[offset..offset + n(&e["size"]) as usize].to_vec();
                uploaded = 0;
            }
            Some("e") if n(&e["f"]) == 0x8009_e218 => {
                upload = start_ram[0xb_2684..0xb_2694].to_vec();
                uploaded = 0;
            }
            Some("s") => {
                let voice = n(&e["voice"]);
                let original = n(&e["envx"]) as u16;
                let device = spu.read_register(voice * 16 + reg::VOICE_ENVX);
                envx.push((cycle, voice, original, device));
            }
            _ => {}
        }
    }
    (TraceMix { pcm, envx }, spu)
}

/// The captured float PCM (`i16 / 32767`) as signed samples, if exact.
fn captured_pcm(directory: &std::path::Path) -> Vec<[f32; 2]> {
    std::fs::read(directory.join("original-mix.f32le-stereo.raw"))
        .unwrap()
        .chunks_exact(8)
        .map(|b| {
            [
                f32::from_le_bytes(b[0..4].try_into().unwrap()),
                f32::from_le_bytes(b[4..8].try_into().unwrap()),
            ]
        })
        .collect()
}

struct Comparison {
    /// Original frame of native frame 0.
    offset: usize,
    /// Native frames compared with a recorded frame; all must be equal.
    compared: usize,
    differing: usize,
    first: Option<usize>,
    /// Recorder underflows: (native frame, zero frames the recorder
    /// inserted while the emulator ran behind its audio clock).
    gaps: Vec<(usize, usize)>,
    /// Recorder overflows: (native frame, native frames the recording lacks
    /// because the emulator dropped them while the sink was behind).
    drops: Vec<(usize, usize)>,
}

const RESUME: usize = 64;

/// Align `native` in `original` once from an exact 32-frame probe, then
/// compare frame by frame (`i16 / 32767`, as the recorder stores it). The
/// SDL disk recorder is not sample-locked to the emulator: when it runs
/// ahead it inserts silence, when it falls behind the emulator drops
/// samples. Both are resynchronised only on exact evidence: after a
/// mismatch, a run of recorded zeros is an insertion when the next
/// recorded frames equal the native continuation for `RESUME` frames; the
/// recording skipping ahead is a drop when it equals a later native stretch
/// for `RESUME` frames. Every compared frame is equal or counted as
/// differing; inserted and dropped frames are reported, never compared.
fn compare_pcm(native: &[[i16; 2]], original: &[[f32; 2]]) -> Comparison {
    let as_float = |f: &[i16; 2]| [f32::from(f[0]) / 32767.0, f32::from(f[1]) / 32767.0];
    let floats: Vec<[f32; 2]> = native.iter().map(as_float).collect();
    let probe = (0..native.len().saturating_sub(32))
        .find(|&i| native[i..i + 32].iter().filter(|f| **f != [0, 0]).count() >= 24)
        .expect("no audible native frames");
    let found: Vec<usize> = (0..original.len().saturating_sub(32))
        .filter(|&j| original[j..j + 32] == floats[probe..probe + 32])
        .collect();
    assert_eq!(
        found.len(),
        1,
        "probe at native frame {probe}: {} matches",
        found.len()
    );
    let offset = found[0] - probe;
    let mut result = Comparison {
        offset,
        compared: 0,
        differing: 0,
        first: None,
        gaps: Vec::new(),
        drops: Vec::new(),
    };
    let silent = |f: &[f32; 2]| *f == [0.0, 0.0];
    let same = |i: usize, j: usize| {
        i + RESUME <= floats.len()
            && j + RESUME <= original.len()
            && floats[i..i + RESUME] == original[j..j + RESUME]
    };
    let (mut i, mut j) = (0, offset);
    // After a failed resynchronisation, the next attempt waits (bounded cost).
    let mut retry = 0;
    while i < native.len() && j < original.len() {
        if original[j] == floats[i] {
            result.compared += 1;
            i += 1;
            j += 1;
            continue;
        }
        if i < retry {
            result.compared += 1;
            result.differing += 1;
            i += 1;
            j += 1;
            continue;
        }
        retry = i + 4096;
        // Skip a recorded zero run, then find the native frame it resumes at.
        let mut resumed = j;
        while resumed < original.len() && silent(&original[resumed]) {
            resumed += 1;
        }
        let ahead = (0..100_000).find(|&k| same(i + k, resumed));
        if let Some(k) = ahead {
            retry = 0;
            if resumed > j {
                result.gaps.push((i, resumed - j));
            }
            if k > 0 {
                result.drops.push((i, k));
            }
            i += k;
            j = resumed;
            continue;
        }
        result.compared += 1;
        result.differing += 1;
        result.first.get_or_insert(i);
        i += 1;
        j += 1;
    }
    result
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the spu-008/spu-009 original captures"]
fn trace_driven_mix_matches_the_original_pcm_and_envelopes() {
    let names = std::env::var("AC3_SPU_PCM_CAPTURES").unwrap_or_else(|_| "spu-008,spu-009".into());
    for name in names.split(',') {
        let directory = capture(name).expect("capture missing");
        let (mix, _) = trace_mix(&directory, u64::MAX);
        if let Some(out) = std::env::var_os("AC3_SPU_DUMP") {
            let bytes: Vec<u8> = mix
                .pcm
                .iter()
                .flatten()
                .flat_map(|v| v.to_le_bytes())
                .collect();
            std::fs::write(PathBuf::from(out).join(format!("{name}.native.s16")), bytes).unwrap();
        }
        let original = captured_pcm(&directory);
        let c = compare_pcm(&mix.pcm, &original);
        let envx_differing: Vec<_> = mix.envx.iter().filter(|(_, _, o, d)| o != d).collect();
        let class_differing = mix
            .envx
            .iter()
            .filter(|(_, _, o, d)| (*o == 0) != (*d == 0))
            .count();
        let inserted: usize = c.gaps.iter().map(|g| g.1).sum();
        let dropped: usize = c.drops.iter().map(|d| d.1).sum();
        let audible = mix.pcm.iter().filter(|f| **f != [0, 0]).count();
        println!(
            "{name}: {} native frames ({audible} audible), recording offset {}: {} compared, {} differ (first {:?}); recorder: {} insertions ({inserted} frames), {} drops ({dropped} native frames not recorded); ENVX reads {}: {} differ in value, {class_differing} in zero/nonzero",
            mix.pcm.len(),
            c.offset,
            c.compared,
            c.differing,
            c.first,
            c.gaps.len(),
            c.drops.len(),
            mix.envx.len(),
            envx_differing.len()
        );
        for (cycle, voice, o, d) in envx_differing.iter().take(12) {
            println!("  ENVX cycle {cycle} voice {voice}: original {o:04x} device {d:04x}");
        }
        for (cycle, voice, o, d) in mix
            .envx
            .iter()
            .filter(|(_, _, o, d)| (*o == 0) != (*d == 0))
            .take(12)
        {
            println!(
                "  ENVX zero/nonzero cycle {cycle} voice {voice}: original {o:04x} device {d:04x}"
            );
        }
        assert_eq!(c.differing, 0, "{name}");
        assert_eq!(class_differing, 0, "{name}");
        assert!(c.compared > mix.pcm.len() * 9 / 10, "{name}: coverage");
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the spu-009 original capture (8002DDCC snapshots)"]
fn sound_driver_frames_reproduce_the_original_register_stores() {
    // The translated sound update (8002DDCC, mainloop::update_sound) on the
    // original RAM at its entry, with the device brought to the same moment
    // by the original stores before it, must make the original's stores.
    let directory = capture("spu-009").expect("capture missing");
    let events = events(&directory);
    let mut frames = 0;
    let mut total = 0;
    let mut key_status_checked = 0;
    for (index, snap) in events.iter().enumerate().filter(|(_, e)| e["k"] == "snap") {
        let cycle = snap["c"].as_u64().unwrap();
        let name = snap["name"].as_str().unwrap();
        let read = |suffix: &str| std::fs::read(directory.join(format!("{name}{suffix}"))).unwrap();
        let (_, mut spu) = trace_mix(&directory, cycle);
        spu.set_logging(true);
        let after = &events[index + 1..];
        let end = after
            .iter()
            .position(|e| e["k"] == "w" && n(&e["pc"]) == 0x8009_f560)
            .expect("no key-off store after the snapshot");
        let expected: Vec<RegisterWrite> = after[..=end]
            .iter()
            .filter(|e| e["k"] == "w")
            .map(original_store)
            .filter(|w| device_address(w.address))
            .collect();
        let statuses: Vec<u32> = after[..=end]
            .iter()
            .filter(|e| e["k"] == "s")
            .map(|e| n(&e["status"]))
            .collect();
        assert_eq!(statuses.len(), 24);
        let mut m = Machine::from_images(&read(".ram.bin"), &read(".scratch.bin")).unwrap();
        m.sp = n(&snap["sp"]);
        let spu = attach(&mut m, spu);
        m.host_results.extend(statuses.iter().copied());
        ac3_flight::mainloop::update_sound(&mut m);
        assert!(m.take_fault().is_ok(), "{name}: {:?}", m.fault());
        // With the per-voice key-status call (calls::SPU_KEY_STATUS) the
        // device answers and must give the original's bytes; otherwise
        // the bytes come from the queue.
        let status_call = m
            .host_calls
            .iter()
            .find(|c| c.function == calls::SPU_GET_ALL_KEYS_STATUS)
            .expect("no SpuGetAllKeysStatus")
            .args[0];
        if m.host_calls
            .iter()
            .any(|c| c.function == calls::SPU_KEY_STATUS)
        {
            assert_eq!(m.host_results.len(), 24, "{name}");
            let bytes: Vec<u32> = (0..24).map(|v| u32::from(m.lbu(status_call + v))).collect();
            assert_eq!(bytes, statuses, "{name}: device key status");
            key_status_checked += 24;
        } else {
            assert!(m.host_results.is_empty(), "{name}: status bytes unused");
        }
        let got = spu.borrow_mut().take_writes();
        if got != expected {
            let first = got.iter().zip(&expected).position(|(a, b)| a != b);
            panic!(
                "{name}: {} stores, original {}; first difference {first:?}: {:x?} vs {:x?}",
                got.len(),
                expected.len(),
                first.and_then(|i| got.get(i)),
                first.and_then(|i| expected.get(i))
            );
        }
        frames += 1;
        total += expected.len();
    }
    println!(
        "{frames} sound updates, {total} original register stores reproduced, {key_status_checked} device key-status bytes"
    );
    assert_eq!(frames, 3);
}
