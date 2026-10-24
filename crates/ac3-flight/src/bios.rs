//! The BIOS kernel and libcard / libc routines the game reaches for events,
//! critical sections, files and memory cards, on the address-preserving
//! machine.
//!
//! The kernel's work (event control blocks, critical sections, the `bu`
//! file system, the memory-card driver) is device work: each entry is a host
//! call under its original address, with the argument registers the routine
//! reads, and returns the host's result (the default in parentheses when no
//! host answers). The kernel's RAM (`0x80000000..0x80010000`: event and file
//! control blocks, the card driver's buffers, the device table) is host
//! state; no game code reads it. The library routines below that only
//! sequence kernel calls (libcard's `_card_clear`, `_card_format`, InitCARD,
//! StartCARD, StopCARD, libc's firstfile wrapper) are host calls as well:
//! their own RAM (libcard's format buffers, libc's firstfile statics) is
//! written by the host, which reproduces it (`src/machine_card.rs`).
//!
//! | Address | Routine | Arguments | Returns |
//! | --- | --- | --- | --- |
//! | 800AD748 | DeliverEvent B(07) | class, spec | (0) |
//! | 800AD758 | OpenEvent B(08) | class, spec, mode, handler | descriptor `0xF10000nn` (0xF1000000) |
//! | 800AD768 | CloseEvent B(09) | event | 1 (1) |
//! | 800AD778 | WaitEvent B(0A) | `sdk::wait_event` | |
//! | 800AD788 | TestEvent B(0B) | `sdk::test_event` | |
//! | 800AD798 | EnableEvent B(0C) | event | 1 (1) |
//! | 800AD7A8 | DisableEvent B(0D) | event | 1 (1) |
//! | 800AD7E8 | EnterCriticalSection (syscall 1) | — | 1 when interrupts were on (1) |
//! | 800AD7F8 | ExitCriticalSection (syscall 2) | — | `v0` unchanged: the caller's (see [`exit_critical_section`]) |
//! | 800AD808 | open B(32) | name ("buP0:FILE"), mode (1 read, 2 write, 0x200 create with the blocks in bits 16..; 0x8000 asynchronous) | fd or -1 (-1) |
//! | 800AF3E8 (JP) | lseek B(33) | fd, offset, origin (0 start, 1 current) | the new position, or -1 (-1) |
//! | 800AD818 | read B(34) | fd, buffer, bytes (whole sectors, from the position) | 0 started (asynchronous) / bytes, or -1 (-1) |
//! | 800AD828 | write B(35) | fd, buffer, bytes (whole sectors) | as read (-1) |
//! | 800AD838 | close B(36) | fd | fd or -1 (-1) |
//! | 800AD848 | nextfile B(43) | DIRENTRY* | the pointer or 0 (0) |
//! | 800ADA08 | firstfile (libc wrapper of B(42)) | pattern, DIRENTRY* | the pointer or 0 (0) |
//! | 800AB7D8 | _bu_init A(70) | — | (0) |
//! | 800AB7E8 | _card_info A(AB) | channel (0x00 / 0x10) | 1 accepted (1) |
//! | 800AB7F8 | _card_load A(AC) | channel | 1 accepted (1) |
//! | 800AB808 | _card_clear (libcard: _new_card B(50), _card_write(channel, 0x3F, 0) B(4E)) | channel | 1 accepted (1) |
//! | 800AB868 | InitCARD (libcard) | pad_enable | (0) |
//! | 800AB8F8 | StartCARD (libcard) | — | 0 |
//! | 800AB940 | StopCARD (libcard) | — | 0 |
//! | 800ABC38 | _card_format (libcard, synchronous) | channel | 1 / 0 (0) |
//!
//! `lseek` is the Japanese release's: its card file (`BISLPSP02020AC3ES`,
//! two blocks) is read and written in records at fixed offsets (a 0x280-byte
//! head written from the start; records of 0x80 bytes at 0x280, 0x480 at
//! 0x300 + 0x480 n, 0x180 at 0x1080 + 0x900 n + 0x180 k), each `open`,
//! `lseek(fd, offset, 0)`, then one `read` / `write`. PAL never seeks (one
//! 0x11FC-byte transfer from the start); its resident has data at 800AF3E8,
//! so the arm claims no PAL routine.
//!
//! Completion of the asynchronous card work is reported through events:
//! class [`SW_CARD`] (`_card_info`, `_card_load`, asynchronous
//! `read`/`write`) and [`HW_CARD`] (`_card_clear`), spec [`EV_IOE`] done,
//! [`EV_ERROR`], [`EV_TIMEOUT`] (no card) or [`EV_NEW`] (new card).
use crate::machine::Machine;

pub const DELIVER_EVENT: u32 = 0x800a_d748;
pub const OPEN_EVENT: u32 = 0x800a_d758;
pub const CLOSE_EVENT: u32 = 0x800a_d768;
/// WaitEvent and TestEvent are `sdk::wait_event` / `sdk::test_event`.
pub const WAIT_EVENT: u32 = 0x800a_d778;
pub const TEST_EVENT: u32 = 0x800a_d788;
pub const ENABLE_EVENT: u32 = 0x800a_d798;
pub const DISABLE_EVENT: u32 = 0x800a_d7a8;
pub const ENTER_CRITICAL_SECTION: u32 = 0x800a_d7e8;
pub const EXIT_CRITICAL_SECTION: u32 = 0x800a_d7f8;
pub const OPEN: u32 = 0x800a_d808;
/// The Japanese resident's lseek stub (`ACE3.BIN`; there is no PAL one).
/// Already a JP address: re-addressing must leave it as it is.
pub const LSEEK: u32 = 0x800a_f3e8;
pub const READ: u32 = 0x800a_d818;
pub const WRITE: u32 = 0x800a_d828;
pub const CLOSE: u32 = 0x800a_d838;
pub const NEXT_FILE: u32 = 0x800a_d848;
pub const FIRST_FILE: u32 = 0x800a_da08;
pub const BU_INIT: u32 = 0x800a_b7d8;
pub const CARD_INFO: u32 = 0x800a_b7e8;
pub const CARD_LOAD: u32 = 0x800a_b7f8;
pub const CARD_CLEAR: u32 = 0x800a_b808;
pub const INIT_CARD: u32 = 0x800a_b868;
pub const START_CARD: u32 = 0x800a_b8f8;
pub const STOP_CARD: u32 = 0x800a_b940;
pub const CARD_FORMAT: u32 = 0x800a_bc38;

/// Event classes, specs and modes (libapi `kernel.h`).
pub const HW_CARD: u32 = 0xf000_0011;
pub const SW_CARD: u32 = 0xf400_0001;
pub const EV_IOE: u32 = 0x0004;
pub const EV_ERROR: u32 = 0x8000;
pub const EV_TIMEOUT: u32 = 0x0100;
pub const EV_NEW: u32 = 0x2000;
pub const EV_MODE_INTERRUPT: u32 = 0x1000;
pub const EV_MODE_NO_INTERRUPT: u32 = 0x2000;
/// `open` modes: read, write, create (block count in bits 16..), asynchronous.
pub const O_READ: u32 = 0x0001;
pub const O_WRITE: u32 = 0x0002;
pub const O_CREATE: u32 = 0x0200;
pub const O_ASYNC: u32 = 0x8000;
/// `lseek` origins: the file's start, the current position.
pub const SEEK_SET: u32 = 0;
pub const SEEK_CUR: u32 = 1;

/// libcard's `_card_format` data (BSS): fifteen 32-byte directory frame
/// templates, twenty broken-sector words and the 128-byte sector buffer.
pub const FORMAT_FRAMES: u32 = 0x800c_9a70;
pub const FORMAT_BROKEN: u32 = 0x800c_9c50;
pub const FORMAT_BUFFER: u32 = 0x800c_9cb0;
/// libc firstfile's statics: the kernel device entry it patched (+0) and
/// the device name taken from the pattern (+8, "bu").
pub const FIRST_FILE_STATICS: u32 = 0x800c_9d60;

// ---- kernel: events and critical sections -------------------------------------------

/// 800AD748 DeliverEvent(class, spec). Returns 0.
pub fn deliver_event(m: &mut Machine, class: u32, spec: u32) -> u32 {
    m.host_call(DELIVER_EVENT, [class, spec, 0, 0]);
    0
}

/// 800AD758 OpenEvent(class, spec, mode, handler): the descriptor.
pub fn open_event(m: &mut Machine, class: u32, spec: u32, mode: u32, handler: u32) -> u32 {
    m.host_call_result(OPEN_EVENT, [class, spec, mode, handler], 0xf100_0000)
}

/// 800AD768 CloseEvent(event): 1.
pub fn close_event(m: &mut Machine, event: u32) -> u32 {
    m.host_call_result(CLOSE_EVENT, [event, 0, 0, 0], 1)
}

/// 800AD798 EnableEvent(event): 1.
pub fn enable_event(m: &mut Machine, event: u32) -> u32 {
    m.host_call_result(ENABLE_EVENT, [event, 0, 0, 0], 1)
}

/// 800AD7A8 DisableEvent(event): 1.
pub fn disable_event(m: &mut Machine, event: u32) -> u32 {
    m.host_call_result(DISABLE_EVENT, [event, 0, 0, 0], 1)
}

/// 800AD7E8 EnterCriticalSection(): 1 when interrupts were enabled.
pub fn enter_critical_section(m: &mut Machine) -> u32 {
    m.host_call_result(ENTER_CRITICAL_SECTION, [0; 4], 1)
}

/// 800AD7F8 ExitCriticalSection(). The kernel leaves `v0` alone, so the
/// result is the caller's `v0` before the call (usually the previous
/// call's result), unless the host supplies one. Reached through a stored
/// address ([`dispatch`]) it is 0.
pub fn exit_critical_section(m: &mut Machine, v0: u32) -> u32 {
    m.host_call_result(EXIT_CRITICAL_SECTION, [0; 4], v0)
}

// ---- kernel: files ----------------------------------------------------------------

/// 800AD808 open(name, mode): a file descriptor, or -1.
pub fn open(m: &mut Machine, name: u32, mode: u32) -> u32 {
    m.host_call_result(OPEN, [name, mode, 0, 0], u32::MAX)
}

/// 800AF3E8 (JP) lseek(fd, offset, origin): the position `offset` bytes
/// from the start ([`SEEK_SET`]) or from the current position
/// ([`SEEK_CUR`]), which the next read or write starts from; -1 for a
/// descriptor that is not open or another origin.
pub fn lseek(m: &mut Machine, fd: u32, offset: u32, origin: u32) -> u32 {
    m.host_call_result(LSEEK, [fd, offset, origin, 0], u32::MAX)
}

/// 800AD818 read(fd, buffer, bytes): 0 when an asynchronous read started,
/// the bytes read when synchronous, -1 on error.
pub fn read(m: &mut Machine, fd: u32, buffer: u32, bytes: u32) -> u32 {
    m.host_call_result(READ, [fd, buffer, bytes, 0], u32::MAX)
}

/// 800AD828 write(fd, buffer, bytes): as [`read`].
pub fn write(m: &mut Machine, fd: u32, buffer: u32, bytes: u32) -> u32 {
    m.host_call_result(WRITE, [fd, buffer, bytes, 0], u32::MAX)
}

/// 800AD838 close(fd): `fd`, or -1 for a descriptor that is not open.
pub fn close(m: &mut Machine, fd: u32) -> u32 {
    m.host_call_result(CLOSE, [fd, 0, 0, 0], u32::MAX)
}

/// 800ADA08 firstfile(pattern, entry): fills the 40-byte DIRENTRY (name[20],
/// attributes, size, next, head, system[4]) of the first file matching
/// `pattern` (`?` and `*` wildcards) and returns `entry`, or 0.
pub fn first_file(m: &mut Machine, pattern: u32, entry: u32) -> u32 {
    m.host_call_result(FIRST_FILE, [pattern, entry, 0, 0], 0)
}

/// 800AD848 nextfile(entry): the next match of the last [`first_file`],
/// `entry` or 0.
pub fn next_file(m: &mut Machine, entry: u32) -> u32 {
    m.host_call_result(NEXT_FILE, [entry, 0, 0, 0], 0)
}

// ---- memory cards -------------------------------------------------------------------

/// 800AB7D8 _bu_init(). Returns 0.
pub fn bu_init(m: &mut Machine) -> u32 {
    m.host_call(BU_INIT, [0; 4]);
    0
}

/// 800AB7E8 _card_info(channel): 1 when the request was accepted, 0 when
/// the port is busy.
pub fn card_info(m: &mut Machine, channel: u32) -> u32 {
    m.host_call_result(CARD_INFO, [channel, 0, 0, 0], 1)
}

/// 800AB7F8 _card_load(channel): as [`card_info`].
pub fn card_load(m: &mut Machine, channel: u32) -> u32 {
    m.host_call_result(CARD_LOAD, [channel, 0, 0, 0], 1)
}

/// 800AB808 _card_clear(channel): writes the write-test sector 0x3F (from
/// address 0), clearing the card's new flag. As [`card_info`].
pub fn card_clear(m: &mut Machine, channel: u32) -> u32 {
    m.host_call_result(CARD_CLEAR, [channel, 0, 0, 0], 1)
}

/// 800ABC38 _card_format(channel): the empty directory, synchronously. 1
/// on success, else 0.
pub fn card_format(m: &mut Machine, channel: u32) -> u32 {
    m.host_call_result(CARD_FORMAT, [channel, 0, 0, 0], 0)
}

/// 800AB868 InitCARD(pad_enable). Returns 0 (the original leaves a kernel
/// table word; no caller reads it).
pub fn init_card(m: &mut Machine, pad_enable: u32) -> u32 {
    m.host_call(INIT_CARD, [pad_enable, 0, 0, 0]);
    0
}

/// 800AB8F8 StartCARD(). Returns 0.
pub fn start_card(m: &mut Machine) -> u32 {
    m.host_call(START_CARD, [0; 4]);
    0
}

/// 800AB940 StopCARD(). Returns 0.
pub fn stop_card(m: &mut Machine) -> u32 {
    m.host_call(STOP_CARD, [0; 4]);
    0
}

// ---- dispatch -----------------------------------------------------------------------

/// The entry points by original address.
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    let [a0, a1, a2, a3] = args;
    Some(match target {
        DELIVER_EVENT => deliver_event(m, a0, a1),
        OPEN_EVENT => open_event(m, a0, a1, a2, a3),
        CLOSE_EVENT => close_event(m, a0),
        ENABLE_EVENT => enable_event(m, a0),
        DISABLE_EVENT => disable_event(m, a0),
        ENTER_CRITICAL_SECTION => enter_critical_section(m),
        EXIT_CRITICAL_SECTION => exit_critical_section(m, 0),
        OPEN => open(m, a0, a1),
        LSEEK => lseek(m, a0, a1, a2),
        READ => read(m, a0, a1, a2),
        WRITE => write(m, a0, a1, a2),
        CLOSE => close(m, a0),
        NEXT_FILE => next_file(m, a0),
        FIRST_FILE => first_file(m, a0, a1),
        BU_INIT => bu_init(m),
        CARD_INFO => card_info(m, a0),
        CARD_LOAD => card_load(m, a0),
        CARD_CLEAR => card_clear(m, a0),
        CARD_FORMAT => card_format(m, a0),
        INIT_CARD => init_card(m, a0),
        START_CARD => start_card(m),
        STOP_CARD => stop_card(m),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_calls_carry_the_registers_the_routines_read() {
        let mut m = Machine::default();
        assert_eq!(
            dispatch(&mut m, OPEN_EVENT, [SW_CARD, EV_IOE, 0x2000, 0]),
            Some(0xf100_0000)
        );
        assert_eq!(dispatch(&mut m, CARD_CLEAR, [0x10, 7, 7, 7]), Some(1));
        assert_eq!(dispatch(&mut m, CARD_FORMAT, [0, 7, 7, 7]), Some(0));
        assert_eq!(
            dispatch(&mut m, READ, [2, 0x800f_69d8, 0x11fc, 7]),
            Some(u32::MAX)
        );
        assert_eq!(
            dispatch(&mut m, 0x800a_d788, [0; 4]),
            None,
            "TestEvent is sdk's"
        );
        let calls: Vec<(u32, [u32; 4])> =
            m.host_calls.iter().map(|c| (c.function, c.args)).collect();
        assert_eq!(
            calls,
            [
                (OPEN_EVENT, [SW_CARD, EV_IOE, 0x2000, 0]),
                (CARD_CLEAR, [0x10, 0, 0, 0]),
                (CARD_FORMAT, [0, 0, 0, 0]),
                (READ, [2, 0x800f_69d8, 0x11fc, 0]),
            ]
        );
        assert!(m.take_fault().is_ok());
    }

    #[test]
    fn lseek_carries_descriptor_offset_and_origin() {
        let mut m = Machine::default();
        assert_eq!(
            dispatch(&mut m, LSEEK, [3, 0x280, SEEK_SET, 7]),
            Some(u32::MAX)
        );
        assert_eq!(
            lseek(&mut m, 3, 0x80, SEEK_CUR),
            u32::MAX,
            "no host answers"
        );
        let calls: Vec<(u32, [u32; 4])> =
            m.host_calls.iter().map(|c| (c.function, c.args)).collect();
        assert_eq!(calls, [(LSEEK, [3, 0x280, 0, 0]), (LSEEK, [3, 0x80, 1, 0])]);
        assert!(m.take_fault().is_ok());
    }

    #[test]
    fn exit_critical_section_keeps_the_callers_v0() {
        let mut m = Machine::default();
        assert_eq!(exit_critical_section(&mut m, 3), 3);
        assert_eq!(dispatch(&mut m, EXIT_CRITICAL_SECTION, [9; 4]), Some(0));
        assert_eq!(enter_critical_section(&mut m), 1);
        assert!(m.host_calls.iter().all(|c| c.args == [0; 4]));
    }
}
