//! libetc's interrupt manager in `sdk.rs`: ResetCallback 800A0778,
//! InterruptCallback 800A07A8, VSyncCallback(s) 800A0808/800A083C,
//! DMACallback 800A07D8, StopCallback 800A086C, RestartCallback 800A089C.
//! The boot comparison (`tests/boot.rs`, objects-boot-002) checks the
//! manager, the interrupt table and the callback arrays as the original
//! leaves them at the main loop; this checks each routine on its own.
use ac3_flight::{Machine, dispatch, sdk};

/// The executable's interrupt table (0x800B3F94, static data) and its
/// pointer, before ResetCallback.
fn machine() -> Machine {
    let mut m = Machine::default();
    let table = [
        0x8002_45e4u32,
        0,
        0x800a_0bb4,
        0x800a_090c,
        0x800a_0cfc,
        0,
        0x800a_0d9c,
        0x800b_2f2c,
    ];
    for (i, word) in table.into_iter().enumerate() {
        m.sw(0x800b_3f94 + i as u32 * 4, word);
    }
    m.sw(0x800b_3fb4, 0x800b_3f94);
    // Leftovers the reset clears.
    m.sw(0x800b_3ff4, 0x1234);
    m.sw(0x800b_2f40, 0xdead_beef);
    m.sw(sdk::DMA_CALLBACKS + 8, 0x8000_1000);
    m
}

#[test]
fn reset_callback_installs_the_vblank_and_dma_handlers() {
    let mut m = machine();
    assert_eq!(dispatch::call(&mut m, 0x800a_0778, [0; 4]), 0x800b_2f2c);
    assert_eq!(m.lhu(0x800b_2f2c), 1, "initialized");
    assert_eq!(m.lw(0x800b_2f30), 0x800a_0e90, "VBlank handler");
    assert_eq!(m.lw(0x800b_2f3c), 0x800a_0fa4, "DMA handler");
    assert_eq!(m.lw(0x800b_2f40), 0, "manager cleared");
    assert_eq!(m.lhu(0x800b_2f5c), 0x9, "interrupts 0 and 3 enabled");
    assert_eq!(m.lw(0x800b_2f64), 0x800a_0974, "setjmp ra");
    assert_eq!(m.lw(0x800b_2f68), 0x800b_3f44, "handler stack");
    assert_eq!(m.lw(0x800b_2f70), 0x800b_2f2c, "setjmp s0");
    assert_eq!(m.lw(0x800b_2f90), ac3_flight::GP, "setjmp gp");
    assert_eq!(m.lw(0x800b_3f98), 0x800a_1124, "DMA setter");
    assert_eq!(m.lw(0x800b_3fa8), 0x800a_0efc, "VSync setter");
    assert_eq!(m.lw(0x800b_3ff4), 0, "VBlank counter");
    assert_eq!(m.lw(sdk::DMA_CALLBACKS + 8), 0, "DMA callbacks cleared");
    let calls: Vec<u32> = m.host_calls.iter().map(|c| c.function).collect();
    assert_eq!(calls, [0x800a_0778]);
    // Once initialized it does nothing more.
    m.sw(0x800b_2f40, 7);
    assert_eq!(sdk::reset_callback(&mut m), 0);
    assert_eq!(m.lw(0x800b_2f40), 7);
    assert!(m.take_fault().is_ok());
}

#[test]
fn callbacks_through_the_table() {
    let mut m = machine();
    sdk::reset_callback(&mut m);
    // InterruptCallback(2, CD handler), then removal.
    assert_eq!(sdk::interrupt_callback(&mut m, 2, 0x800a_3018), 0);
    assert_eq!(m.lw(0x800b_2f38), 0x800a_3018);
    assert_eq!(m.lhu(0x800b_2f5c), 0xd);
    assert_eq!(sdk::interrupt_callback(&mut m, 2, 0), 0x800a_3018);
    assert_eq!(m.lw(0x800b_2f38), 0);
    assert_eq!(m.lhu(0x800b_2f5c), 0x9);
    // VSyncCallback(func) is slot 4 of VSyncCallbacks.
    assert_eq!(sdk::vsync_callback(&mut m, 0x8001_2340), 0);
    assert_eq!(m.lw(sdk::VSYNC_CALLBACKS + 16), 0x8001_2340);
    assert_eq!(sdk::vsync_callbacks(&mut m, 4, 0), 0x8001_2340);
    // DMACallback through the setter ResetCallback installed.
    assert_eq!(sdk::dma_callback(&mut m, 1, 0x8005_5878), 0);
    assert_eq!(m.lw(sdk::DMA_CALLBACKS + 4), 0x8005_5878);
    // StopCallback saves the masks and marks the manager stopped;
    // RestartCallback marks it running again.
    m.host_results.extend([0x0d, 0x3333_3333]);
    assert_eq!(sdk::stop_callback(&mut m), 0x800b_2f2c);
    assert_eq!(m.lhu(0x800b_2f2c), 0);
    assert_eq!(m.lhu(0x800b_2f5e), 0x0d);
    assert_eq!(m.lw(0x800b_2f60), 0x3333_3333);
    // A stopped manager ignores InterruptCallback.
    assert_eq!(sdk::interrupt_callback(&mut m, 5, 0x8000_2000), 0);
    assert_eq!(m.lw(0x800b_2f44), 0);
    assert_eq!(sdk::restart_callback(&mut m), 0x800b_2f2c);
    assert_eq!(m.lhu(0x800b_2f2c), 1);
    let last = *m.host_calls.last().unwrap();
    assert_eq!(
        (last.function, last.args),
        (0x800a_089c, [0x0d, 0x3333_3333, 0, 0])
    );
    assert!(m.take_fault().is_ok());
}
