//! Canonical host-call ids ([`HostCall::function`](crate::HostCall::function)).
//!
//! A device-facing routine is identified by its entry address **in the PAL
//! executable**, in every game version. The ids are names, not RAM
//! addresses: the JP translation logs `DMA_LIST` = `0x800A8360` for its
//! own copy of that libgpu routine, wherever the JP executable has it, and
//! the address re-mapping that generates a version crate leaves these
//! values alone. The host devices recognise calls by these constants.
//!
//! Translated code that takes the routine from a function pointer in RAM
//! (libgpu's table, the debug print pointer `[800B97E0]`) holds that
//! version's address of it: it logs the id at the same index of
//! [`RUNTIME`] (`ac3_flight::sdk`'s `runtime_host_id`), so the host never
//! sees a version's address. In PAL the two are one value.
//!
//! The version crates name these ids with their own consts (`boot::CD_READ`,
//! `bios::OPEN`, ...), whose values must stay these
//! (`ac3-flight/tests/host_ids.rs`; the generator keeps them).
//!
//! A routine that only another version has (JP's BIOS `lseek`, B0h:33h, for
//! one) needs an id that is not a PAL routine: its address in that version,
//! checked against this list (`ids_are_distinct`).

// ---- libgpu ----------------------------------------------------------------------------

/// 800A8360 (libgpu table +0x18): start GPU DMA channel 2 in linked-list
/// mode `[list, param, size]` (DrawOTag, PutDrawEnv, ClearImage packets,
/// MoveImage's one-packet list).
pub const DMA_LIST: u32 = 0x800a_8360;
/// 800A82E8 (libgpu table +0x10): a GP1 control word `[command]`.
pub const GP1: u32 = 0x800a_82e8;
/// 800A83A8 (libgpu table +0x30): GP1(10h) information `[index]` -> GPUREAD.
pub const GPU_INFO: u32 = 0x800a_83a8;
/// 800A7BFC (libgpu table +0xC, the ClearImage transfer): reads GPUSTAT
/// once `[rect, colour]` -> GPUSTAT.
pub const GPU_CLEAR_STATUS: u32 = 0x800a_7bfc;
/// 800A8A5C (libgpu table +0x3C): DrawSync's wait -> poll count (0: idle).
pub const GPU_SYNC: u32 = 0x800a_8a5c;
/// 800A7B04 (libgpu table +0x38): GPUSTAT read (GetODE) -> GPUSTAT.
pub const GPU_STATUS: u32 = 0x800a_7b04;
/// 800A7B1C: ClearOTagR's DMA channel 6 start -> poll count.
pub const GPU_CLEAR_OTAG_R: u32 = 0x800a_7b1c;
/// 800A6910 LoadImage(rect, pixels): CPU -> VRAM.
pub const LOAD_IMAGE: u32 = 0x800a_6910;
/// 800A6970 StoreImage(rect, pixels): VRAM -> CPU.
pub const STORE_IMAGE: u32 = 0x800a_6970;
/// 800A62E8 ResetGraph(mode).
pub const RESET_GRAPH: u32 = 0x800a_62e8;
/// 800A65CC SetDispMask(mask).
pub const SET_DISP_MASK: u32 = 0x800a_65cc;
/// 800A6208 the GTE patch of InitGeom.
pub const PATCH_GTE: u32 = 0x800a_6208;

// ---- libetc and the kernel's interrupts and root counters --------------------------------

/// 800A0568 VSync(mode): waiting modes deliver the VBlanks the wait spans.
pub const VSYNC: u32 = 0x800a_0568;
/// 800A0778 ResetCallback().
pub const RESET_CALLBACK: u32 = 0x800a_0778;
/// 800A07A8 InterruptCallback(irq, func).
pub const INTERRUPT_CALLBACK: u32 = 0x800a_07a8;
/// 800A086C StopCallback().
pub const STOP_CALLBACK: u32 = 0x800a_086c;
/// 800A089C RestartCallback().
pub const RESTART_CALLBACK: u32 = 0x800a_089c;
/// 800A08F4 SetIntrMask(mask) -> previous mask.
pub const SET_INTR_MASK: u32 = 0x800a_08f4;
/// 800A1124 DMACallback(channel, func).
pub const DMA_CALLBACK: u32 = 0x800a_1124;
/// 800AD898 SetRCnt(spec, target, mode).
pub const SET_RCNT: u32 = 0x800a_d898;
/// 800AD934 GetRCnt(spec) -> count.
pub const GET_RCNT: u32 = 0x800a_d934;
/// 800AD96C StartRCnt(spec).
pub const START_RCNT: u32 = 0x800a_d96c;
/// 800AD9D0 ResetRCnt(spec).
pub const RESET_RCNT: u32 = 0x800a_d9d0;

// ---- libpad ----------------------------------------------------------------------------

/// 8009C2A0 PadInitDirect(port 1 buffer, port 2 buffer).
pub const PAD_INIT_DIRECT: u32 = 0x8009_c2a0;
/// 8009B200 PadStartCom(): libpad's VBlank handler runs from here.
pub const PAD_START_COM: u32 = 0x8009_b200;
/// 8009B220 PadStopCom(): ... until here.
pub const PAD_STOP_COM: u32 = 0x8009_b220;
/// 80099DE0 PadEnableCom(mask).
pub const PAD_ENABLE_COM: u32 = 0x8009_9de0;

// ---- libcd -----------------------------------------------------------------------------

/// The CD host protocol (the drive model, [`crate::cd::CdDrive`]): the
/// original routines whose device accesses these calls stand for.
/// 800A2434: CD_cw's command write `[command, parameter count, parameters]`.
pub const CD_COMMAND: u32 = 0x800a_2434;
/// 800A1988: the interrupt flag read `[site, result frame]`.
pub const CD_INTERRUPT: u32 = 0x800a_1988;
/// 800A2E20: CD_getsector's DMA `[destination, words]`.
pub const CD_GET_SECTOR: u32 = 0x800a_2e20;
/// 800A2F20: CD_getsector2's DMA `[destination, words]`.
pub const CD_GET_SECTOR2: u32 = 0x800a_2f20;
/// 800A18E0 CdMix(attenuation): the CD audio mix (the drive and the SPU).
pub const CD_MIX: u32 = 0x800a_18e0;
/// 800A299C: libcd's CD volume set-up.
pub const CD_INIT_VOLUME: u32 = 0x800a_299c;
/// 800A14BC CdSync(mode, result).
pub const CD_SYNC: u32 = 0x800a_14bc;
/// 800A1440 CdSetDebug(level).
pub const CD_SET_DEBUG: u32 = 0x800a_1440;
/// 800A14FC CdSyncCallback(func).
pub const CD_SYNC_CALLBACK: u32 = 0x800a_14fc;
/// 800A1510 CdReadyCallback(func).
pub const CD_READY_CALLBACK: u32 = 0x800a_1510;
/// 800A1940 CdDataCallback(func).
pub const CD_DATA_CALLBACK: u32 = 0x800a_1940;
/// The blocking file calls (the boot's disc, [`crate::disc::DiscDevice`]).
/// 800A30F8 CdSearchFile(fp, name) -> libcd's cached entry.
pub const CD_SEARCH_FILE: u32 = 0x800a_30f8;
/// 800A1794 CdControlB(command, parameters, result).
pub const CD_CONTROL_B: u32 = 0x800a_1794;
/// 800A4168 CdRead(sectors, buffer, mode).
pub const CD_READ: u32 = 0x800a_4168;
/// 800A4304 CdReadSync(mode, result).
pub const CD_READ_SYNC: u32 = 0x800a_4304;

// ---- libspu ----------------------------------------------------------------------------

/// 8009E218 SpuInit() (`_SpuInit(0)`; SsUtReverbOff's call in the boot).
pub const SPU_INIT: u32 = 0x8009_e218;
/// 8009D928: programmed-I/O upload `[source, size, SPU address]`.
pub const SPU_WRITE_PIO: u32 = 0x8009_d928;
/// 8009DC4C `_spu_t(op, value, size)` `[op, value, size, SPU address]`.
pub const SPU_TRANSFER: u32 = 0x8009_dc4c;
/// 8009DFF8 `_spu_FsetRXXa` register store `[register index, value]`.
pub const SPU_SET_REGISTER: u32 = 0x8009_dff8;
/// 8009E2B8 SpuSetReverb `[on, enabled]`.
pub const SPU_SET_REVERB: u32 = 0x8009_e2b8;
/// 8009E498 SpuSetReverbModeParam `[attr]` (returns 0 or -1).
pub const SPU_SET_REVERB_MODE_PARAM: u32 = 0x8009_e498;
/// 8009EE48 SpuSetReverbDepth `[attr, mask]`.
pub const SPU_SET_REVERB_DEPTH: u32 = 0x8009_ee48;
/// 8009EEF8 `_SpuSetAnyVoice` `[on, voices, low, high]`.
pub const SPU_SET_ANY_VOICE: u32 = 0x8009_eef8;
/// 8009F3C8 SpuSetKey `[on, voices]`.
pub const SPU_SET_KEY: u32 = 0x8009_f3c8;
/// 8009F888 SpuSetCommonAttr `[attr]`.
pub const SPU_SET_COMMON_ATTR: u32 = 0x8009_f888;
/// 8009FCDC SpuGetAllKeysStatus `[status]`.
pub const SPU_GET_ALL_KEYS_STATUS: u32 = 0x8009_fcdc;
/// 8009FD4C: one voice of SpuGetAllKeysStatus `[status, voice, keyed]`.
pub const SPU_KEY_STATUS: u32 = 0x8009_fd4c;
/// 8009FD68 SpuSetVoiceAttr `[attr]`.
pub const SPU_SET_VOICE_ATTR: u32 = 0x8009_fd68;
/// 8009DBA4 `_spu_Fr_` `[buf, SPU address (8-byte units), blocks]`.
pub const SPU_READ_DMA: u32 = 0x8009_dba4;
/// 8009F358 SpuReadDecodedData's SPUSTAT read `[buf, part]` -> SPUSTAT.
pub const SPU_CAPTURE_STATUS: u32 = 0x8009_f358;

// ---- libpress (the MDEC) -----------------------------------------------------------------

/// 800AAF98: the MDEC reset of DecDCTReset `[mode]`.
pub const MDEC_RESET: u32 = 0x800a_af98;
/// 800AB088: the command and input DMA of DecDCTin `[buffer, words]`.
pub const MDEC_IN: u32 = 0x800a_b088;
/// 800AB118: the output DMA of DecDCTout `[buffer, words]`.
pub const MDEC_OUT: u32 = 0x800a_b118;
/// 800AAF08 DecDCTinSync(mode).
pub const DEC_DCT_IN_SYNC: u32 = 0x800a_af08;

// ---- the kernel (BIOS vectors) and the memory cards --------------------------------------

/// 800AD6F8 InitHeap(base, size).
pub const INIT_HEAP: u32 = 0x800a_d6f8;
/// 800AD618 puts(text).
pub const PUTS: u32 = 0x800a_d618;
/// 800AC3E8 printf(format, ...).
pub const PRINTF: u32 = 0x800a_c3e8;
/// 800AD748 DeliverEvent(class, spec).
pub const DELIVER_EVENT: u32 = 0x800a_d748;
/// 800AD758 OpenEvent(class, spec, mode, func).
pub const OPEN_EVENT: u32 = 0x800a_d758;
/// 800AD768 CloseEvent(event).
pub const CLOSE_EVENT: u32 = 0x800a_d768;
/// 800AD778 WaitEvent(event).
pub const WAIT_EVENT: u32 = 0x800a_d778;
/// 800AD788 TestEvent(event).
pub const TEST_EVENT: u32 = 0x800a_d788;
/// 800AD798 EnableEvent(event).
pub const ENABLE_EVENT: u32 = 0x800a_d798;
/// 800AD7A8 DisableEvent(event).
pub const DISABLE_EVENT: u32 = 0x800a_d7a8;
/// 800AD7E8 EnterCriticalSection().
pub const ENTER_CRITICAL_SECTION: u32 = 0x800a_d7e8;
/// 800AD7F8 ExitCriticalSection().
pub const EXIT_CRITICAL_SECTION: u32 = 0x800a_d7f8;
/// 800AD808 open(name, mode).
pub const OPEN: u32 = 0x800a_d808;
/// 800AD818 read(fd, buffer, size).
pub const READ: u32 = 0x800a_d818;
/// 800AD828 write(fd, buffer, size).
pub const WRITE: u32 = 0x800a_d828;
/// 800AD838 close(fd).
pub const CLOSE: u32 = 0x800a_d838;
/// 800AD848 nextfile(entry).
pub const NEXT_FILE: u32 = 0x800a_d848;
/// 800ADA08 firstfile(name, entry).
pub const FIRST_FILE: u32 = 0x800a_da08;
/// 800AB7D8 _bu_init().
pub const BU_INIT: u32 = 0x800a_b7d8;
/// 800AB7E8 _card_info(channel).
pub const CARD_INFO: u32 = 0x800a_b7e8;
/// 800AB7F8 _card_load(channel).
pub const CARD_LOAD: u32 = 0x800a_b7f8;
/// 800AB808 _card_clear(channel).
pub const CARD_CLEAR: u32 = 0x800a_b808;
/// 800AB868 InitCARD(pad enable).
pub const INIT_CARD: u32 = 0x800a_b868;
/// 800AB8F8 StartCARD().
pub const START_CARD: u32 = 0x800a_b8f8;
/// 800AB940 StopCARD().
pub const STOP_CARD: u32 = 0x800a_b940;
/// 800ABC38 format(name).
pub const CARD_FORMAT: u32 = 0x800a_bc38;
/// lseek(fd, offset, origin) (BIOS B0h:33h): JP only, so its id is the
/// Japanese resident's stub address 800AF3E8 (PAL has no lseek, and no PAL
/// routine starts there).
pub const LSEEK: u32 = 0x800a_f3e8;

// ---- game code ---------------------------------------------------------------------------

/// 80055648: one iteration of a busy-wait loop `[site, cycles]` (the movie
/// decoder's frame poll; the loop address of 800555C0) -> completion bits.
pub const BUSY_WAIT: u32 = 0x8005_5648;
/// 8002C384: the resource queue's read request `[file, destination,
/// callback, size]` (logged by 8002C1B8's queue).
pub const QUEUE_READ: u32 = 0x8002_c384;

/// The device routines translated code may reach through a routine
/// pointer it loads from RAM (libgpu's low-level table entry +0x18, its
/// debug print pointer): a version crate lists its own addresses of these,
/// in this order, and logs the id at the same index (`sdk.rs`
/// `runtime_host_id`). A version's pointer holds its own address, never
/// the canonical id.
pub const RUNTIME: [u32; 2] = [DMA_LIST, PRINTF];

/// Every id above (the distinctness test; tooling).
pub const ALL: [u32; 90] = [
    DMA_LIST,
    GP1,
    GPU_INFO,
    GPU_CLEAR_STATUS,
    GPU_SYNC,
    GPU_STATUS,
    GPU_CLEAR_OTAG_R,
    LOAD_IMAGE,
    STORE_IMAGE,
    RESET_GRAPH,
    SET_DISP_MASK,
    PATCH_GTE,
    VSYNC,
    RESET_CALLBACK,
    INTERRUPT_CALLBACK,
    STOP_CALLBACK,
    RESTART_CALLBACK,
    SET_INTR_MASK,
    DMA_CALLBACK,
    SET_RCNT,
    GET_RCNT,
    START_RCNT,
    RESET_RCNT,
    PAD_INIT_DIRECT,
    PAD_START_COM,
    PAD_STOP_COM,
    PAD_ENABLE_COM,
    CD_COMMAND,
    CD_INTERRUPT,
    CD_GET_SECTOR,
    CD_GET_SECTOR2,
    CD_MIX,
    CD_INIT_VOLUME,
    CD_SYNC,
    CD_SET_DEBUG,
    CD_SYNC_CALLBACK,
    CD_READY_CALLBACK,
    CD_DATA_CALLBACK,
    CD_SEARCH_FILE,
    CD_CONTROL_B,
    CD_READ,
    CD_READ_SYNC,
    SPU_INIT,
    SPU_WRITE_PIO,
    SPU_TRANSFER,
    SPU_SET_REGISTER,
    SPU_SET_REVERB,
    SPU_SET_REVERB_MODE_PARAM,
    SPU_SET_REVERB_DEPTH,
    SPU_SET_ANY_VOICE,
    SPU_SET_KEY,
    SPU_SET_COMMON_ATTR,
    SPU_GET_ALL_KEYS_STATUS,
    SPU_KEY_STATUS,
    SPU_SET_VOICE_ATTR,
    SPU_READ_DMA,
    SPU_CAPTURE_STATUS,
    MDEC_RESET,
    MDEC_IN,
    MDEC_OUT,
    DEC_DCT_IN_SYNC,
    INIT_HEAP,
    PUTS,
    PRINTF,
    DELIVER_EVENT,
    OPEN_EVENT,
    CLOSE_EVENT,
    WAIT_EVENT,
    TEST_EVENT,
    ENABLE_EVENT,
    DISABLE_EVENT,
    ENTER_CRITICAL_SECTION,
    EXIT_CRITICAL_SECTION,
    OPEN,
    READ,
    WRITE,
    CLOSE,
    NEXT_FILE,
    FIRST_FILE,
    BU_INIT,
    CARD_INFO,
    CARD_LOAD,
    CARD_CLEAR,
    INIT_CARD,
    START_CARD,
    STOP_CARD,
    CARD_FORMAT,
    LSEEK,
    BUSY_WAIT,
    QUEUE_READ,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_distinct() {
        let mut all = ALL.to_vec();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), ALL.len(), "two host-call ids share a value");
    }
}
