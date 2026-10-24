//! The interpreter on small hand-assembled programs: delay slots, calls and
//! tail calls, arithmetic edge cases, unaligned access, traps, the step
//! limit, the COP2 pipeline, and the extent tables.
use ac3_fallback::{Fallback, Guest, RETURN, Version, flags};
use ac3_gte::Gte;

/// A bare guest: 2 MiB RAM at 0x80000000, no scratchpad, faults as text.
struct Bare {
    ram: Vec<u8>,
    gte: Gte,
    sp: u32,
    module: u32,
    faults: Vec<String>,
    stubs: Vec<(u32, [u32; 4])>,
    busy: u32,
}
impl Bare {
    fn new() -> Self {
        Self {
            ram: vec![0; 0x20_0000],
            gte: Gte::default(),
            sp: 0x801f_fff0,
            module: u32::MAX,
            faults: Vec::new(),
            stubs: Vec::new(),
            busy: 0,
        }
    }
    fn at(&self, address: u32) -> usize {
        (address & 0x1f_ffff) as usize
    }
    fn load(&mut self, address: u32, program: &[u32]) {
        for (i, w) in program.iter().enumerate() {
            self.sw(address + 4 * i as u32, *w);
        }
    }
}
impl Guest for Bare {
    fn fetch(&self, pc: u32) -> u32 {
        let at = self.at(pc);
        u32::from_le_bytes(self.ram[at..at + 4].try_into().unwrap())
    }
    fn lb(&mut self, a: u32) -> i8 {
        self.ram[self.at(a)] as i8
    }
    fn lbu(&mut self, a: u32) -> u8 {
        self.ram[self.at(a)]
    }
    fn lh(&mut self, a: u32) -> i16 {
        self.lhu(a) as i16
    }
    fn lhu(&mut self, a: u32) -> u16 {
        let at = self.at(a);
        u16::from_le_bytes([self.ram[at], self.ram[at + 1]])
    }
    fn lw(&mut self, a: u32) -> u32 {
        self.fetch(a)
    }
    fn sb(&mut self, a: u32, v: u8) {
        let at = self.at(a);
        self.ram[at] = v;
    }
    fn sh(&mut self, a: u32, v: u16) {
        let at = self.at(a);
        self.ram[at..at + 2].copy_from_slice(&v.to_le_bytes());
    }
    fn sw(&mut self, a: u32, v: u32) {
        let at = self.at(a);
        self.ram[at..at + 4].copy_from_slice(&v.to_le_bytes());
    }
    fn gte(&mut self) -> &mut Gte {
        &mut self.gte
    }
    fn sp(&self) -> u32 {
        self.sp
    }
    fn set_sp(&mut self, sp: u32) {
        self.sp = sp;
    }
    fn loaded_module(&mut self) -> u32 {
        self.module
    }
    fn faulted(&self) -> bool {
        !self.faults.is_empty()
    }
    fn raise_trap(&mut self, address: u32, code: u32) {
        self.faults.push(format!("trap {code} at {address:08x}"));
    }
    fn raise_untranslated(&mut self, address: u32) {
        self.faults.push(format!("untranslated {address:08x}"));
    }
    fn raise_unsupported(&mut self, address: u32, word: u32) {
        self.faults
            .push(format!("unsupported {word:08x} at {address:08x}"));
    }
    fn raise_runaway(&mut self, address: u32) {
        self.faults.push(format!("runaway at {address:08x}"));
    }
    fn host_stub(&mut self, _target: u32, id: u32, args: [u32; 4]) -> Option<u32> {
        self.stubs.push((id, args));
        Some(0x5555)
    }
    /// Every tenth busy-wait iteration a VBlank arrives (JP counter).
    fn busy_wait(&mut self, _site: u32, cycles: u32) -> bool {
        assert!(cycles > 0);
        self.busy += 1;
        if self.busy.is_multiple_of(10) {
            let count = self.fetch(0x800b_6174);
            self.sw(0x800b_6174, count + 1);
        }
        true
    }
}

// A tiny assembler.
const ZERO: u32 = 0;
const V0: u32 = 2;
const V1: u32 = 3;
const A0: u32 = 4;
const A1: u32 = 5;
const A2: u32 = 6;
const A3: u32 = 7;
const T0: u32 = 8;
const T1: u32 = 9;
const T2: u32 = 10;
const S0: u32 = 16;
const SP: u32 = 29;
const RA: u32 = 31;
fn i(op: u32, rs: u32, rt: u32, imm: i32) -> u32 {
    op << 26 | rs << 21 | rt << 16 | (imm as u32 & 0xffff)
}
fn r(rs: u32, rt: u32, rd: u32, sa: u32, funct: u32) -> u32 {
    rs << 21 | rt << 16 | rd << 11 | sa << 6 | funct
}
fn addiu(rt: u32, rs: u32, imm: i32) -> u32 {
    i(9, rs, rt, imm)
}
fn addu(rd: u32, rs: u32, rt: u32) -> u32 {
    r(rs, rt, rd, 0, 0x21)
}
fn lui(rt: u32, imm: u32) -> u32 {
    i(0xf, 0, rt, imm as i32)
}
fn ori(rt: u32, rs: u32, imm: u32) -> u32 {
    i(0xd, rs, rt, imm as i32)
}
fn lw(rt: u32, off: i32, base: u32) -> u32 {
    i(0x23, base, rt, off)
}
fn sw(rt: u32, off: i32, base: u32) -> u32 {
    i(0x2b, base, rt, off)
}
fn bne(rs: u32, rt: u32, words: i32) -> u32 {
    i(5, rs, rt, words)
}
fn jal(target: u32) -> u32 {
    3 << 26 | (target >> 2 & 0x03ff_ffff)
}
fn j(target: u32) -> u32 {
    2 << 26 | (target >> 2 & 0x03ff_ffff)
}
fn jr(rs: u32) -> u32 {
    r(rs, 0, 0, 0, 8)
}
const NOP: u32 = 0;
fn mtc2(rt: u32, rd: u32) -> u32 {
    0x12 << 26 | 4 << 21 | rt << 16 | rd << 11
}
fn mfc2(rt: u32, rd: u32) -> u32 {
    0x12 << 26 | rt << 16 | rd << 11
}
fn cop2(code: u32) -> u32 {
    0x12 << 26 | 1 << 25 | code
}

const CODE: u32 = 0x8010_0000;

fn run(
    g: &mut Bare,
    program: &[u32],
    args: [u32; 4],
    calls: &mut Vec<(u32, [u32; 4])>,
) -> (u32, u32) {
    g.load(CODE, program);
    let fallback = Fallback::new(Version::Pal);
    let end = CODE + 4 * program.len() as u32;
    fallback.run_range(g, (CODE, end), args, &mut |g: &mut Bare, target, a| {
        calls.push((target, a));
        // "Native" callee: v0 = a0 + a1, and it writes a marker.
        g.sw(0x8000_1000, target);
        a[0].wrapping_add(a[1])
    })
}

#[test]
fn loop_with_branch_delay_slots() {
    // v0 = sum of 1..=a0; the add sits in the branch's delay slot.
    let program = [
        addiu(V0, ZERO, 0),
        addiu(T0, ZERO, 0),
        addiu(T0, T0, 1), // loop:
        bne(T0, A0, -2),  // bne t0, a0, loop
        addu(V0, V0, T0), // delay slot
        jr(RA),
        NOP,
    ];
    let mut g = Bare::new();
    let (v0, _) = run(&mut g, &program, [10, 0, 0, 0], &mut Vec::new());
    assert_eq!(v0, 55);
    assert!(g.faults.is_empty());
}

#[test]
fn calls_return_to_the_link_and_tail_calls_return_the_callee_value() {
    let program = [
        addiu(SP, SP, -24),
        sw(RA, 20, SP),
        addiu(A0, ZERO, 7),
        jal(0x8020_0000),
        addiu(A1, ZERO, 5), // delay slot: a1 = 5 before the call
        addu(S0, V0, ZERO), // s0 = 12
        lw(RA, 20, SP),
        addiu(SP, SP, 24),
        addu(A0, S0, ZERO),
        j(0x8030_0000), // tail call: returns 12 + a1
        addiu(A1, ZERO, 100),
    ];
    let mut g = Bare::new();
    let mut calls = Vec::new();
    let (v0, _) = run(&mut g, &program, [0, 0, 0, 0], &mut calls);
    assert_eq!(
        calls,
        vec![(0x8020_0000, [7, 5, 0, 0]), (0x8030_0000, [12, 100, 0, 0])]
    );
    assert_eq!(v0, 112);
    assert_eq!(g.sp, 0x801f_fff0, "the frame is popped");
    // The saved return address is the sentinel.
    assert_eq!(g.fetch(0x801f_fff0 - 24 + 20), RETURN);
}

#[test]
fn arithmetic_edge_cases() {
    let program = [
        // div by zero: lo = -1 (non-negative dividend), hi = dividend
        r(A0, ZERO, 0, 0, 0x1a),
        r(0, 0, T0, 0, 0x12), // mflo t0
        r(0, 0, T1, 0, 0x10), // mfhi t1
        // -2^31 / -1: lo = -2^31, hi = 0
        lui(T2, 0x8000),
        addiu(A3, ZERO, -1),
        r(T2, A3, 0, 0, 0x1a),
        r(0, 0, V0, 0, 0x12), // v0 = lo
        r(0, 0, V1, 0, 0x10), // v1 = hi
        // multu 0xffffffff * 0xffffffff
        r(A3, A3, 0, 0, 0x19),
        r(0, 0, A1, 0, 0x10), // a1 = hi = 0xfffffffe
        // sra / srl
        r(0, A3, A2, 4, 0x03), // a2 = -1 >> 4 (arith) = -1
        sw(T0, 0, S0),
        sw(T1, 4, S0),
        sw(A1, 8, S0),
        sw(A2, 12, S0),
        jr(RA),
        NOP,
    ];
    let mut g = Bare::new();
    let mut program = program.to_vec();
    program.insert(0, lui(S0, 0x8000));
    program.insert(1, ori(S0, S0, 0x2000));
    let (v0, v1) = run(&mut g, &program, [9, 0, 0, 0], &mut Vec::new());
    assert_eq!((v0, v1), (0x8000_0000, 0));
    let words: Vec<u32> = (0..4).map(|k| g.fetch(0x8000_2000 + 4 * k)).collect();
    assert_eq!(words, vec![u32::MAX, 9, 0xffff_fffe, u32::MAX]);
}

#[test]
fn unaligned_loads_and_stores() {
    let mut g = Bare::new();
    g.sw(0x8000_3000, 0x4433_2211);
    g.sw(0x8000_3004, 0x8877_6655);
    let program = [
        lui(T0, 0x8000),
        ori(T0, T0, 0x3000),
        i(0x22, T0, V0, 4), // lwl v0, 4(t0)
        i(0x26, T0, V0, 1), // lwr v0, 1(t0): v0 = bytes 1..4 = 0x55443322
        lui(T1, 0xaabb),
        ori(T1, T1, 0xccdd),
        i(0x2a, T0, T1, 0x12), // swl t1, 0x12(t0)
        i(0x2e, T0, T1, 0x0f), // swr t1, 0xf(t0)
        jr(RA),
        NOP,
    ];
    let (v0, _) = run(&mut g, &program, [0; 4], &mut Vec::new());
    assert_eq!(v0, 0x5544_3322);
    // swr at 0x300F stores the low byte; swl at 0x3012 the top three bytes.
    assert_eq!(g.lbu(0x8000_300f), 0xdd);
    assert_eq!(g.lhu(0x8000_3010), 0xbbcc);
    assert_eq!(g.lbu(0x8000_3012), 0xaa);
}

#[test]
fn break_raises_the_trap_and_ends_the_run() {
    // `break 7`, as the compiler emits it for a division by zero.
    let program = [addiu(V0, ZERO, 3), 0x0007_000d, NOP, addiu(V0, ZERO, 9)];
    let mut g = Bare::new();
    let (v0, _) = run(&mut g, &program, [0; 4], &mut Vec::new());
    assert_eq!(v0, 3);
    assert_eq!(g.faults, vec![format!("trap 7 at {:08x}", CODE + 4)]);
}

#[test]
fn unsupported_instructions_and_runaways_fault() {
    let mut g = Bare::new();
    let (_, _) = run(&mut g, &[r(0, 0, 0, 0, 0x0c), NOP], [0; 4], &mut Vec::new());
    assert_eq!(
        g.faults,
        vec![format!("unsupported 0000000c at {CODE:08x}")]
    );

    let mut g = Bare::new();
    let program = [j(CODE), NOP];
    g.load(CODE, &program);
    let mut fallback = Fallback::new(Version::Pal);
    fallback.set_max_steps(1000);
    fallback.run_range(
        &mut g,
        (CODE, CODE + 8),
        [0; 4],
        &mut |_: &mut Bare, _, _| 0,
    );
    assert_eq!(g.faults.len(), 1);
    assert!(g.faults[0].starts_with("runaway"), "{:?}", g.faults);
}

#[test]
fn cop2_writes_land_two_instructions_later() {
    // mtc2 IR1; mfc2 at once sees the old IR1, after two more instructions
    // the new one.
    let program = [
        addiu(T0, ZERO, 0x123),
        mtc2(T0, 9),
        mfc2(V0, 9), // distance 0: old
        mfc2(V1, 9), // distance 1: old
        mfc2(A0, 9), // distance 2: new
        sw(A0, 0, S0),
        jr(RA),
        NOP,
    ];
    let mut g = Bare::new();
    g.gte.data[9] = 0x55;
    let mut program = program.to_vec();
    program.insert(0, lui(S0, 0x8000));
    let (v0, v1) = run(&mut g, &program, [0; 4], &mut Vec::new());
    assert_eq!((v0, v1), (0x55, 0x55));
    assert_eq!(g.fetch(0x8000_0000), 0x123);

    // SQR right after an IR1 write computes with the old IR1 and leaves the
    // written word in IR1 (docs/GTE.md probe), unlike with two NOPs between.
    let sqr = 0x28;
    for (gap, expect_old) in [(0, true), (2, false)] {
        let mut program = vec![addiu(T0, ZERO, 3), mtc2(T0, 9)];
        program.extend(std::iter::repeat_n(NOP, gap));
        program.extend([cop2(sqr), mfc2(V0, 25), jr(RA), NOP]);
        let mut g = Bare::new();
        g.gte.data[9] = 5;
        let (v0, _) = run(&mut g, &program, [0; 4], &mut Vec::new());
        assert_eq!(v0, if expect_old { 25 } else { 9 }, "gap {gap}");
        assert_eq!(
            g.gte.data[9],
            if expect_old { 3 } else { 9 },
            "IR1, gap {gap}"
        );
    }
}

#[test]
fn extent_tables_are_sorted_and_complete() {
    for version in [Version::Pal, Version::Jp] {
        let fallback = Fallback::new(version);
        let tables = fallback.tables();
        for table in std::iter::once(tables.resident).chain(tables.overlays.iter().copied()) {
            for pair in table.windows(2) {
                assert!(pair[0].end() <= pair[1].start, "{:08x}", pair[0].start);
            }
        }
        let count = fallback.routines().count();
        let device = fallback
            .routines()
            .filter(|(_, _, e)| !e.interpretable())
            .count();
        let wide: Vec<_> = fallback
            .routines()
            .filter(|(_, _, e)| e.flags & flags::WIDE != 0)
            .map(|(_, _, e)| e.name)
            .collect();
        match version {
            Version::Pal => {
                assert_eq!(count, 2742);
                assert_eq!(tables.overlays.len(), 5);
            }
            Version::Jp => {
                assert_eq!(count, 3032);
                assert_eq!(tables.overlays.len(), 4);
            }
        }
        assert!(device > 600 && device < 700, "{device} device routines");
        assert_eq!(wide, ["__divdi3", "__udivdi3", "__udivmoddi4"]);
    }
    // JP: the JP-only device SDK routines are below the boundary, the
    // JP-only pure ones above it.
    let jp = Fallback::new(Version::Jp);
    let by_name = |name: &str| {
        jp.routines()
            .find(|(_, _, e)| e.name == name)
            .map(|(_, _, e)| *e)
            .unwrap_or_else(|| panic!("{name}"))
    };
    for name in [
        "lseek",
        "Krom2RawAdd",
        "LoadImage2",
        "VSync",
        "DrawSync",
        "CdSync",
    ] {
        assert!(!by_name(name).interpretable(), "{name}");
    }
    for name in [
        "GetClut",
        "SetDrawLoad",
        "strcmp",
        "RotTransPers",
        "__divdi3",
    ] {
        assert!(by_name(name).interpretable(), "{name}");
    }
}

#[test]
fn force_spec_selects_routines() {
    let mut pal = Fallback::new(Version::Pal);
    let ov1 = pal.force("ov1").unwrap();
    assert_eq!(ov1, 176);
    assert_eq!(pal.force("ov1,-ov1").unwrap(), 0);
    assert!(pal.force("owner:hud").unwrap() > 50);
    assert_eq!(pal.force("8002616c").unwrap(), 1);
    let some = pal.force("ov2:800c0000-800dffff").unwrap();
    assert!(some > 0 && some < 448, "{some}");
    // Device routines are never forced.
    let all = pal.force("all").unwrap();
    let forceable = pal.routines().filter(|(_, _, e)| e.forceable()).count();
    assert_eq!(all, forceable);
    assert!(all > 2000, "{all}");
    assert!(pal.force("nonsense").is_err());
    assert!(pal.force("ov9").is_err());
}

#[test]
fn jp_stubs_carry_the_canonical_pal_id() {
    let jp = Fallback::new(Version::Jp);
    let id = |address: u32| {
        jp.routines()
            .find(|(_, module, e)| *module == 0 && e.start == address)
            .and_then(|(_, _, e)| e.host_id(Version::Jp))
    };
    // JP _card_format sits where PAL has `open`: its ID is PAL _card_format.
    assert_eq!(id(0x800a_d808), Some(0x800a_bc38));
    assert_eq!(id(0x800a_f3d8), Some(0x800a_d808), "JP open");
    assert_eq!(id(0x800a_f3c8), Some(0x800a_d7f8), "JP ExitCriticalSection");
    assert_eq!(id(0x800a_f5f8), Some(0x800a_da08), "JP firstfile");
    assert_eq!(
        id(0x800a_f3e8),
        Some(0x800a_f3e8),
        "JP-only lseek keeps its address"
    );
    assert_eq!(id(0x800a_f438), Some(0x800a_f438), "JP-only Krom2RawAdd");
    let stubs = jp
        .routines()
        .filter(|(_, _, e)| e.flags & flags::STUB != 0)
        .count();
    assert_eq!(stubs, 25);
    let pal = Fallback::new(Version::Pal);
    let open = pal
        .routines()
        .find(|(_, module, e)| *module == 0 && e.start == 0x800a_d808)
        .unwrap()
        .2;
    assert_eq!(open.host_id(Version::Pal), Some(0x800a_d808));

    // Interpreted JP code calling JP _card_format and JP ExitCriticalSection:
    // the host calls get the PAL IDs, `dispatch::call` never sees the JP
    // addresses, and ExitCriticalSection leaves v0 alone.
    let program = [
        addiu(A0, ZERO, 0x10),
        jal(0x800a_d808),
        NOP,
        addiu(V0, ZERO, 0x77),
        jal(0x800a_f3c8),
        NOP,
        jr(RA),
        NOP,
    ];
    let mut g = Bare::new();
    g.load(CODE, &program);
    let mut dispatched = Vec::new();
    let (v0, _) = jp.run_range(
        &mut g,
        (CODE, CODE + 32),
        [0; 4],
        &mut |_: &mut Bare, t, _| {
            dispatched.push(t);
            0
        },
    );
    assert!(dispatched.is_empty(), "{dispatched:08x?}");
    assert_eq!(
        g.stubs,
        vec![
            (0x800a_bc38, [0x10, 0, 0, 0]),
            (0x800a_d7f8, [0x10, 0, 0, 0])
        ]
    );
    assert_eq!(v0, 0x77);
    assert_eq!(jp.report()["stubs"].as_array().unwrap().len(), 2);
}

#[test]
fn a_spin_on_the_vblank_counter_busy_waits() {
    // lw t0, counter; loop: lw t1, counter; beq t1, t0, loop; nop; jr ra
    let program = [
        lui(T2, 0x800b),
        lw(T0, 0x6174, T2),
        lw(T1, 0x6174, T2),
        i(4, T1, T0, -2),
        NOP,
        addu(V0, T1, ZERO),
        jr(RA),
        NOP,
    ];
    let mut g = Bare::new();
    g.sw(0x800b_6174, 41);
    g.load(CODE, &program);
    let jp = Fallback::new(Version::Jp);
    let (v0, _) = jp.run_range(
        &mut g,
        (CODE, CODE + 32),
        [0; 4],
        &mut |_: &mut Bare, _, _| 0,
    );
    assert_eq!(v0, 42, "the loop saw the next VBlank");
    assert!(g.faults.is_empty(), "{:?}", g.faults);
    assert_eq!(g.busy, 10);
    assert_eq!(jp.report()["busy_waits"], 10);
}
