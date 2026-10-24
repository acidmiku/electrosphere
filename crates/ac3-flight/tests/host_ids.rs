//! The host-call ids this translation logs are the canonical ids the host
//! devices match on (`ac3_machine::host_ids`): every named id const of the
//! translated SDK, kernel and game code that a device answers equals its
//! `host_ids` constant. (A version crate generated from this one keeps
//! these consts at their PAL values: they are ids, not addresses.)
use ac3_flight::{bios, boot, host_ids, mainloop::pending_sdk, sdk, states::busy};

#[test]
fn translated_ids_are_the_canonical_ones() {
    let pairs: &[(&str, u32, u32)] = &[
        // libcd: the drive's host protocol and the boot's file calls.
        ("CD_COMMAND", pending_sdk::CD_COMMAND, host_ids::CD_COMMAND),
        (
            "CD_INTERRUPT",
            pending_sdk::CD_INTERRUPT,
            host_ids::CD_INTERRUPT,
        ),
        (
            "CD_GET_SECTOR",
            pending_sdk::CD_GET_SECTOR,
            host_ids::CD_GET_SECTOR,
        ),
        (
            "CD_GET_SECTOR2",
            pending_sdk::CD_GET_SECTOR2,
            host_ids::CD_GET_SECTOR2,
        ),
        ("CD_MIX", pending_sdk::CD_MIX, host_ids::CD_MIX),
        ("CD_MIX (boot)", boot::CD_MIX, host_ids::CD_MIX),
        (
            "CD_INIT_VOLUME",
            pending_sdk::CD_INIT_VOLUME,
            host_ids::CD_INIT_VOLUME,
        ),
        (
            "CD_SEARCH_FILE",
            boot::CD_SEARCH_FILE,
            host_ids::CD_SEARCH_FILE,
        ),
        ("CD_CONTROL_B", boot::CD_CONTROL_B, host_ids::CD_CONTROL_B),
        ("CD_READ", boot::CD_READ, host_ids::CD_READ),
        ("CD_READ_SYNC", boot::CD_READ_SYNC, host_ids::CD_READ_SYNC),
        ("CD_SET_DEBUG", boot::CD_SET_DEBUG, host_ids::CD_SET_DEBUG),
        (
            "CD_SYNC_CALLBACK",
            boot::CD_SYNC_CALLBACK,
            host_ids::CD_SYNC_CALLBACK,
        ),
        (
            "CD_READY_CALLBACK",
            boot::CD_READY_CALLBACK,
            host_ids::CD_READY_CALLBACK,
        ),
        (
            "CD_DATA_CALLBACK",
            boot::CD_DATA_CALLBACK,
            host_ids::CD_DATA_CALLBACK,
        ),
        // libpad, libgpu, libetc, the root counters.
        (
            "PAD_INIT_DIRECT",
            boot::PAD_INIT_DIRECT,
            host_ids::PAD_INIT_DIRECT,
        ),
        (
            "PAD_START_COM",
            boot::PAD_START_COM,
            host_ids::PAD_START_COM,
        ),
        (
            "PAD_ENABLE_COM",
            boot::PAD_ENABLE_COM,
            host_ids::PAD_ENABLE_COM,
        ),
        ("LOAD_IMAGE", boot::LOAD_IMAGE, host_ids::LOAD_IMAGE),
        ("RESET_GRAPH", sdk::RESET_GRAPH, host_ids::RESET_GRAPH),
        ("PATCH_GTE", sdk::PATCH_GTE, host_ids::PATCH_GTE),
        (
            "SET_DISP_MASK",
            boot::SET_DISP_MASK,
            host_ids::SET_DISP_MASK,
        ),
        ("VSYNC", boot::VSYNC, host_ids::VSYNC),
        (
            "RESET_CALLBACK",
            boot::RESET_CALLBACK,
            host_ids::RESET_CALLBACK,
        ),
        ("SET_RCNT", boot::SET_RCNT, host_ids::SET_RCNT),
        ("START_RCNT", boot::START_RCNT, host_ids::START_RCNT),
        // libspu, libpress.
        (
            "SS_UT_REVERB_OFF",
            boot::SS_UT_REVERB_OFF,
            host_ids::SPU_INIT,
        ),
        (
            "SPU_SET_COMMON_ATTR",
            boot::SPU_SET_COMMON_ATTR,
            host_ids::SPU_SET_COMMON_ATTR,
        ),
        (
            "SPU_SET_REVERB_MODE_PARAM",
            boot::SPU_SET_REVERB_MODE_PARAM,
            host_ids::SPU_SET_REVERB_MODE_PARAM,
        ),
        (
            "DEC_DCT_IN_SYNC",
            pending_sdk::DEC_DCT_IN_SYNC,
            host_ids::DEC_DCT_IN_SYNC,
        ),
        // The kernel and the memory cards.
        ("INIT_HEAP", boot::INIT_HEAP, host_ids::INIT_HEAP),
        ("PRINTF", boot::PRINTF, host_ids::PRINTF),
        (
            "DELIVER_EVENT",
            bios::DELIVER_EVENT,
            host_ids::DELIVER_EVENT,
        ),
        ("OPEN_EVENT", bios::OPEN_EVENT, host_ids::OPEN_EVENT),
        ("CLOSE_EVENT", bios::CLOSE_EVENT, host_ids::CLOSE_EVENT),
        ("WAIT_EVENT", bios::WAIT_EVENT, host_ids::WAIT_EVENT),
        ("TEST_EVENT", bios::TEST_EVENT, host_ids::TEST_EVENT),
        ("ENABLE_EVENT", bios::ENABLE_EVENT, host_ids::ENABLE_EVENT),
        (
            "DISABLE_EVENT",
            bios::DISABLE_EVENT,
            host_ids::DISABLE_EVENT,
        ),
        (
            "ENTER_CRITICAL_SECTION",
            bios::ENTER_CRITICAL_SECTION,
            host_ids::ENTER_CRITICAL_SECTION,
        ),
        (
            "EXIT_CRITICAL_SECTION",
            bios::EXIT_CRITICAL_SECTION,
            host_ids::EXIT_CRITICAL_SECTION,
        ),
        ("OPEN", bios::OPEN, host_ids::OPEN),
        ("LSEEK", bios::LSEEK, host_ids::LSEEK),
        ("READ", bios::READ, host_ids::READ),
        ("WRITE", bios::WRITE, host_ids::WRITE),
        ("CLOSE", bios::CLOSE, host_ids::CLOSE),
        ("NEXT_FILE", bios::NEXT_FILE, host_ids::NEXT_FILE),
        ("FIRST_FILE", bios::FIRST_FILE, host_ids::FIRST_FILE),
        ("BU_INIT", bios::BU_INIT, host_ids::BU_INIT),
        ("CARD_INFO", bios::CARD_INFO, host_ids::CARD_INFO),
        ("CARD_LOAD", bios::CARD_LOAD, host_ids::CARD_LOAD),
        ("CARD_CLEAR", bios::CARD_CLEAR, host_ids::CARD_CLEAR),
        ("INIT_CARD", bios::INIT_CARD, host_ids::INIT_CARD),
        ("START_CARD", bios::START_CARD, host_ids::START_CARD),
        ("STOP_CARD", bios::STOP_CARD, host_ids::STOP_CARD),
        ("CARD_FORMAT", bios::CARD_FORMAT, host_ids::CARD_FORMAT),
        // Game code.
        ("BUSY_WAIT", busy::BUSY_WAIT, host_ids::BUSY_WAIT),
    ];
    let wrong: Vec<String> = pairs
        .iter()
        .filter(|(_, logged, canonical)| logged != canonical)
        .map(|(name, logged, canonical)| format!("{name}: {logged:08x} != {canonical:08x}"))
        .collect();
    assert!(wrong.is_empty(), "ids differ: {wrong:?}");
}
