//! Sub-pixel vertex tracking (`ac3_flight::precision`) on synthetic machine
//! sequences: GTE projections stored straight into packets and through a
//! vertex buffer, invalidation, stale words, screen-space words, and the
//! machine state (RAM, GTE, clone, snapshot) staying the same.
use ac3_flight::precision::{Options, PrecisionTable, VertexPrecision};
use ac3_flight::{Device, HostCall, Machine};
use ac3_gte::{ctrl, reg};

const BUFFER: u32 = 0x1f80_0100;
const PACKET: u32 = 0x8010_0000;

fn pair(x: i16, y: i16) -> u32 {
    u32::from(x as u16) | u32::from(y as u16) << 16
}

/// A camera 2560 units from the origin, H 256, screen centre (160, 120):
/// one model unit is 0.1 pixel.
fn camera(m: &mut Machine) {
    for (register, value) in [
        (ctrl::R11R12, 0x1000),
        (ctrl::R22R23, 0x1000),
        (ctrl::R33, 0x1000),
    ] {
        m.gte.ctc2(register, value);
    }
    m.gte.ctc2(ctrl::TRZ, 2560);
    m.gte.ctc2(ctrl::OFX, 160 << 16);
    m.gte.ctc2(ctrl::OFY, 120 << 16);
    m.gte.ctc2(ctrl::H, 256);
}

/// RTPT of three model points; each SXY to `BUFFER + 4 * i` (`swc2`).
fn project_into_buffer(m: &mut Machine, points: [(i16, i16); 3]) {
    for (i, (x, y)) in points.into_iter().enumerate() {
        m.gte.mtc2(reg::VXY0 + 2 * i, pair(x, y));
        m.gte.mtc2(reg::VZ0 + 2 * i, 0);
    }
    m.gte.rtpt(true, false);
    for i in 0..3 {
        let word = m.gte.mfc2(reg::SXY0 + i);
        m.sw(BUFFER + 4 * i as u32, word);
    }
}

/// A POLY_F3-like packet from the buffer: copies through CPU registers
/// (`lw` + `sw`) and through the GTE (`lwc2` + NCLIP + `swc2`).
fn emit_packet(m: &mut Machine) {
    m.sw(PACKET, 0x0400_0000);
    m.sw(PACKET + 4, 0x2000_00ff);
    let first = m.lw(BUFFER);
    m.sw(PACKET + 8, first);
    let second = m.lw(BUFFER + 4);
    m.gte.mtc2(reg::SXY1, second);
    let third = m.lw(BUFFER + 8);
    m.gte.mtc2(reg::SXY2, third);
    m.gte.mtc2(reg::SXY0, first);
    m.gte.nclip();
    let word = m.gte.mfc2(reg::SXY1);
    m.sw(PACKET + 12, word);
    let word = m.gte.mfc2(reg::SXY2);
    m.sw(PACKET + 16, word);
}

fn tracked() -> Machine {
    let mut m = Machine::default();
    m.set_precision(Some(PrecisionTable::shared()));
    camera(&mut m);
    m
}

fn assert_floors(position: [f32; 3], word: u32) {
    assert_eq!(
        position[0].floor() as i16,
        word as u16 as i16,
        "{position:?}"
    );
    assert_eq!(
        position[1].floor() as i16,
        (word >> 16) as u16 as i16,
        "{position:?}"
    );
}

#[test]
fn projected_words_keep_their_position_through_buffers_and_registers() {
    let mut m = tracked();
    project_into_buffer(&mut m, [(13, 25), (-47, 3), (88, -61)]);
    emit_packet(&mut m);
    let stats = m.precision().unwrap().borrow().stats();
    assert_eq!((stats.recorded, stats.reloaded), (6, 3), "{stats:?}");
    let expected = [(161.3, 122.5), (155.3, 120.3), (168.8, 113.9)];
    for (i, (x, y)) in expected.into_iter().enumerate() {
        let address = PACKET + 8 + 4 * i as u32;
        let position = m.precise_vertex(address).expect("tracked");
        assert_floors(position, word(&m, address));
        assert!((position[0] - x).abs() < 0.01 && (position[1] - y).abs() < 0.01);
        assert_eq!(position[2], 2560.0);
    }
    // The renderer's view of the same table over the RAM slice, and DMA
    // (24-bit physical) addresses.
    let table = m.precision().unwrap().clone();
    let table = table.borrow();
    let view = table.view(m.ram());
    assert_eq!(
        view.precise_vertex(PACKET + 8),
        m.precise_vertex(PACKET + 8)
    );
    assert_eq!(
        view.precise_vertex(0x0010_0008),
        m.precise_vertex(PACKET + 8)
    );
    // Not vertex words: the tag, the colour, a misaligned or scratchpad address.
    for address in [PACKET, PACKET + 4, PACKET + 9, BUFFER] {
        assert_eq!(view.precise_vertex(address), None, "{address:#x}");
    }
}

/// The RAM word at `address` (no machine access, so nothing is tracked).
fn word(m: &Machine, address: u32) -> u32 {
    let at = (address & 0x1f_ffff) as usize;
    u32::from_le_bytes(m.ram()[at..at + 4].try_into().unwrap())
}

#[test]
fn screen_space_and_saturated_words_have_no_position() {
    let mut m = tracked();
    // Typed in screen space: never projected.
    m.sw(PACKET + 8, pair(160, 120));
    assert_eq!(m.precise_vertex(PACKET + 8), None);
    // A vertex far off screen saturates SX: no position, even though an
    // older entry may hold the clamped word.
    m.gte.mtc2(reg::VXY0, pair(30_000, 0));
    m.gte.mtc2(reg::VZ0, 0);
    m.gte.rtps(true, false);
    let word = m.gte.mfc2(reg::SXY2);
    assert_eq!(word as u16 as i16, 0x3ff);
    m.sw(PACKET + 12, word);
    assert_eq!(m.precise_vertex(PACKET + 12), None);
}

#[test]
fn any_other_store_clears_and_stale_words_never_leak() {
    let mut m = tracked();
    project_into_buffer(&mut m, [(13, 25), (-47, 3), (88, -61)]);
    emit_packet(&mut m);
    let words: Vec<u32> = (0..3).map(|i| word(&m, PACKET + 8 + 4 * i)).collect();
    // Another value, a halfword, a byte.
    m.sw(PACKET + 8, 0x1234_5678);
    m.sh(PACKET + 14, (words[1] >> 16) as u16);
    m.sb(PACKET + 16, words[2] as u8);
    for i in 0..3 {
        assert_eq!(m.precise_vertex(PACKET + 8 + 4 * i), None, "vertex {i}");
    }
    // The same word stored again once the window has moved on: not tracked.
    let mut m = tracked();
    project_into_buffer(&mut m, [(13, 25), (-47, 3), (88, -61)]);
    m.sw(PACKET + 8, m.gte.data[reg::SXY0]);
    assert!(m.precise_vertex(PACKET + 8).is_some());
    for x in 0..100 {
        m.gte.mtc2(reg::VXY0, pair(-300 + x, 400));
        m.gte.rtps(true, false);
    }
    m.sw(PACKET + 8, words[0]);
    assert_eq!(m.precise_vertex(PACKET + 8), None, "outside the window");
    // RAM changed behind the machine's back (a device write): the view
    // checks the word.
    struct Scribble;
    impl Device for Scribble {
        fn call(&mut self, ram: &mut [u8], _: &mut [u8], _: HostCall) -> Option<u32> {
            ram[0x10_000c] ^= 1;
            None
        }
    }
    let mut m = tracked();
    project_into_buffer(&mut m, [(13, 25), (-47, 3), (88, -61)]);
    emit_packet(&mut m);
    assert!(m.precise_vertex(PACKET + 12).is_some());
    m.device = Some(Box::new(Scribble));
    m.host_call(0, [0; 4]);
    assert_eq!(m.precise_vertex(PACKET + 12), None);
    assert!(m.precise_vertex(PACKET + 16).is_some());
}

#[test]
fn tracking_changes_no_machine_state() {
    let run = |precise: bool| {
        let mut m = Machine::default();
        if precise {
            m.set_precision(Some(PrecisionTable::shared()));
        }
        camera(&mut m);
        for step in 0..50_i16 {
            project_into_buffer(&mut m, [(step, 25), (-47, step * 3), (88 - step, -61)]);
            emit_packet(&mut m);
        }
        m
    };
    let (plain, precise) = (run(false), run(true));
    assert_eq!(plain.ram(), precise.ram());
    assert_eq!(plain.scratch(), precise.scratch());
    assert_eq!(plain.gte.data, precise.gte.data);
    assert_eq!(plain.gte.control, precise.gte.control);
    assert_eq!(plain.snapshot(), precise.snapshot());
    assert!(precise.precise_vertex(PACKET + 8).is_some());
    assert!(plain.precise_vertex(PACKET + 8).is_none());
}

#[test]
fn clones_and_snapshots_leave_the_table_out() {
    let mut m = tracked();
    project_into_buffer(&mut m, [(13, 25), (-47, 3), (88, -61)]);
    emit_packet(&mut m);
    let clone = m.clone();
    assert!(clone.precision().is_none() && clone.gte.subpixel().is_none());
    assert_eq!(clone.precise_vertex(PACKET + 8), None);
    let state = m.snapshot();
    assert!(state.gte.subpixel().is_none());
    // A restore keeps tracking on but forgets the old positions.
    m.restore(state).unwrap();
    assert!(m.gte.subpixel().is_some_and(|w| w.is_empty()));
    assert_eq!(m.precise_vertex(PACKET + 8), None);
    project_into_buffer(&mut m, [(13, 25), (-47, 3), (88, -61)]);
    emit_packet(&mut m);
    assert!(m.precise_vertex(PACKET + 8).is_some());
    m.set_precision(None);
    assert!(m.gte.subpixel().is_none());
    assert_eq!(m.precise_vertex(PACKET + 8), None);
}

#[test]
fn copies_need_followed_loads() {
    let mut m = Machine::default();
    let table = std::rc::Rc::new(std::cell::RefCell::new(PrecisionTable::new(Options {
        window: 3,
        follow_loads: false,
    })));
    m.set_precision(Some(table));
    camera(&mut m);
    project_into_buffer(&mut m, [(13, 25), (-47, 3), (88, -61)]);
    // Straight from the FIFO (into the buffer) works; a copy made after
    // other projections pushed the words out does not.
    let recorded = || m.precision().unwrap().borrow().stats().recorded;
    assert_eq!(recorded(), 3);
    for x in 0..3 {
        m.gte.mtc2(reg::VXY0, pair(x, 0));
        m.gte.rtps(true, false);
    }
    emit_packet(&mut m);
    assert_eq!(m.precise_vertex(PACKET + 8), None);
    assert_eq!(m.precision().unwrap().borrow().stats().recorded, 3);
}
