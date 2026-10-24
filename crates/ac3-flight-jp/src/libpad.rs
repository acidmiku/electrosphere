//! libpad's interrupt side: the VBlank handler 8009BABC and everything it
//! runs, translated instruction for instruction over a RAM image.
//!
//! The original exchanges bytes with the controllers through the SIO0
//! registers, waits on root counter 2 and polls the interrupt status. Here
//! those registers are [`Sio`], a model of the port hardware and of the
//! controllers behind it ([`Controller`]: a digital pad or a DualShock, as
//! PCSX-Redux emulates them; the version-independent `ac3_machine::sio`).
//! Every RAM access of the original goes through [`Bus`], so the handler leaves libpad's driver words (0x800B4680..),
//! port records (0x800C7CB8, 0xF0 bytes per port), transfer tables
//! (0x800C7C28/0x800C7C70) and the game's receive buffers exactly as the
//! original does. The only differences are root counter 2 values (the
//! start of each wait, 0x800C7E98, and the kernel's interrupt stack), which
//! depend on cycle timing.
//!
//! Routines reached through libpad's function pointers (0x800B4684..,
//! the record's request handlers `+0x14`/`+0x18`, the step table
//! 0x800B470C) dispatch by original address ([`call`]).
//!
//! The host runs [`vblank`] at each delivered VBlank once PadStartCom has
//! run; [`pad_enable_com`] and [`pad_start_com`] are the RAM effects of
//! those calls.

// ---- RAM and I/O ---------------------------------------------------------------------

// The SIO0 port hardware, the controllers and libpad's RAM view are
// version-independent device models (`ac3_machine::sio`).

pub use ac3_machine::sio::{
    Bus, Controller, I_MASK, I_STAT, PadKind, PadMemory, RCNT2_COUNT, RCNT2_MODE, RCNT2_TARGET,
    SIO_BAUD, SIO_CTRL, SIO_DATA, SIO_MODE, SIO_STAT, Sio,
};

/// libpad driver words.
const PORT_END: u32 = 0x800b_4684;
const RESET_RECORD: u32 = 0x800b_4688;
const SEND_BYTE: u32 = 0x800b_468c;
const PREPARE: u32 = 0x800b_4690;
const SEQ_SEND: u32 = 0x800b_469c;
const SEQ_RECV: u32 = 0x800b_46a4;
const CLEAR_REQUEST: u32 = 0x800b_46a8;
const TAP_HOOK: u32 = 0x800b_46b0;
const TAP_HOOK_2: u32 = 0x800b_46b4;
pub const RECORDS_POINTER: u32 = 0x800b_46b8;
pub const STATE: u32 = 0x800b_46bc;
const TAP_FLAG: u32 = 0x800b_46c0;
pub const CURRENT_PORT: u32 = 0x800b_46c4;
pub const STEP: u32 = 0x800b_46c8;
const POWER: u32 = 0x800b_46cc;
const TAP_MODE: u32 = 0x800b_46d0;
pub const FIRST_PORT: u32 = 0x800b_46d4;
pub const LAST_PORT: u32 = 0x800b_46d8;
const TAP_COUNTS: u32 = 0x800b_46dc;
const I_STAT_POINTER: u32 = 0x800b_46e4;
const SIO_POINTER: u32 = 0x800b_46e8;
pub const VSYNC_FLAG: u32 = 0x800b_46ec;
const REMAINING: u32 = 0x800b_46fc;
const SIO_POINTER_2: u32 = 0x800b_4700;
const SEQ_RESULT: u32 = 0x800b_4704;
const TAP_SEND: u32 = 0x800b_4708;
const STEPS: u32 = 0x800b_470c;
const SIO_POINTER_3: u32 = 0x800b_472c;
/// Per-port VBlank counters while a port is disabled (saturating at 0x96).
const IDLE_COUNTS: u32 = 0x800c_7c08;
/// Root counter 2 wait: start count and length.
pub const WAIT_START: u32 = 0x800c_7e98;
pub const WAIT_LENGTH: u32 = 0x800c_7e9c;
/// 8009D954's actuator table cursor.
const ACT_CURSOR: u32 = 0x800c_7c18;
/// The two port records.
pub const RECORDS: u32 = 0x800c_7cb8;
const RECORD_BYTES: u32 = 0xf0;

fn at(base: u32, offset: u32) -> u32 {
    base.wrapping_add(offset)
}

/// Record of port `index` (`[0x800B46B8] + index * 0xF0`).
fn record_of(bus: &mut Bus, index: u32) -> u32 {
    let records = bus.lw(RECORDS_POINTER);
    at(records, index.wrapping_mul(RECORD_BYTES))
}

// ---- dispatch by original address ------------------------------------------------------

/// A libpad routine reached through a function pointer.
pub fn call(bus: &mut Bus, target: u32, args: [u32; 2]) -> u32 {
    let [a0, a1] = args;
    match target {
        0x8009_c6e0 => step_select(bus, a0),
        0x8009_c728 => step_command(bus, a0),
        0x8009_c800 => step_tap(bus, a0),
        0x8009_c8ac => step_first(bus, a0),
        0x8009_c970 => step_data(bus, a0),
        0x8009_df30 => {
            reset_record(bus, a0);
            0
        }
        0x8009_df98 => port_end(bus, a0),
        0x8009_e08c => clear_request(bus, a0),
        0x8009_e09c => send_byte(bus, a0),
        0x8009_e158 => {
            prepare_actuators(bus, a0);
            0
        }
        0x8009_e40c => crate::mainloop::pending_sdk::pad_port_record(a0),
        0x8009_e464 => seq_send(bus, a0),
        0x8009_e574 => seq_recv(bus, a0),
        0x8009_e8b8 => busy(bus, a0),
        0x8009_d5ec => send_load_info(bus, a0),
        0x8009_d694 => recv_load_info(bus, a0),
        0x8009_daf4 => send_act_align(bus, a0),
        0x8009_db10 => recv_act_align(bus, a0),
        0x8009_dc70 => send_main_mode(bus, a0),
        0x8009_dcc4 => recv_main_mode(bus, a0),
        _ => {
            bus.sio
                .error
                .get_or_insert(format!("libpad call of {target:08x} ({a0:08x}, {a1:08x})"));
            0
        }
    }
}

fn call_at(bus: &mut Bus, pointer: u32, args: [u32; 2]) -> u32 {
    let target = bus.lw(pointer);
    call(bus, target, args)
}

// ---- root counter 2 waits -----------------------------------------------------------------

/// 8009E8F0 setRC2wait(length): start a wait of `length` (in system clock / 8).
fn set_wait(bus: &mut Bus, length: u32) {
    let count = bus.lhu(RCNT2_COUNT);
    bus.sw(WAIT_LENGTH, length);
    bus.sw(WAIT_START, count);
}

/// Elapsed count since the wait started, wrapped at the counter target.
fn elapsed(bus: &mut Bus, now: u32, start: u32) -> u32 {
    let mut now = now & 0xffff;
    if now < start {
        let target = bus.lhu(RCNT2_TARGET);
        now = if target != 0 {
            now.wrapping_add(bus.lhu(RCNT2_TARGET))
        } else {
            now.wrapping_add(0x1_0000)
        };
    }
    now
}

/// 8009E910 chkRC2wait(): 1 once the wait has expired, else 0.
fn wait_expired(bus: &mut Bus) -> bool {
    let now = bus.lhu(RCNT2_COUNT);
    let start = bus.lw(WAIT_START);
    let now = elapsed(bus, now, start);
    let length = bus.lw(WAIT_LENGTH);
    let passed = if bus.lhu(RCNT2_MODE) & 0x200 != 0 {
        now.wrapping_sub(start)
    } else {
        now.wrapping_sub(start) >> 3
    };
    passed >= length
}

fn delay(bus: &mut Bus) {
    while !wait_expired(bus) {}
}

// ---- 8009BABC: the VBlank handler --------------------------------------------------------

/// Runs 8009BABC on `ram` with the port hardware `sio`.
pub fn vblank(ram: &mut [u8], sio: &mut Sio) {
    let mut bus = Bus { ram, sio };
    vblank_handler(&mut bus);
}

/// 8009BABC: libpad's VBlank handler. With no exchange in progress it sets
/// the new-data flag, counts VBlanks of disabled ports, and (driver active)
/// exchanges with each enabled port: SIO set-up (8009BD70), then the
/// port's steps (8009C0A4) until the port-end routine moves on.
pub fn vblank_handler(bus: &mut Bus) -> u32 {
    let sio = bus.lw(SIO_POINTER);
    if bus.lhu(at(sio, 0xa)) & 2 != 0 {
        bus.sh(at(sio, 0xa), 0);
        return 0;
    }
    let first = bus.lw(FIRST_PORT);
    bus.sw(VSYNC_FLAG, 1);
    if first != 0 {
        let count = bus.lw(IDLE_COUNTS);
        if (count as i32) < 0x96 {
            bus.sw(IDLE_COUNTS, count.wrapping_add(1));
        }
    }
    if bus.lw(LAST_PORT) == 0 {
        let count = bus.lw(IDLE_COUNTS + 4);
        if (count as i32) < 0x96 {
            bus.sw(IDLE_COUNTS + 4, count.wrapping_add(1));
        }
    }
    if bus.lw(STATE) == 0 {
        return 0;
    }
    let first = bus.lw(FIRST_PORT);
    if (bus.lw(LAST_PORT) as i32) < first as i32 {
        return 0;
    }
    bus.sw(STEP, 0);
    bus.sw(CURRENT_PORT, first);
    let record = record_of(bus, first);
    if init_sio_mode(bus, record) == 0 {
        call_at(bus, PORT_END, [0xffff, 0]);
    }
    bus.sw(POWER, 0);
    loop {
        let port = bus.lw(CURRENT_PORT);
        if (bus.lw(LAST_PORT) as i32) < port as i32 {
            break;
        }
        let record = record_of(bus, port);
        port_step(bus, record);
    }
    let sio = bus.lw(SIO_POINTER);
    bus.sh(at(sio, 0xe), 0x88);
    0
}

/// 8009BD70 _padInitSioMode(record): reset SIO0, select the current port,
/// settle the multitap counters, clear a pending acknowledge (recovering a
/// stuck exchange). Returns 1 when the port can be polled (no request, or
/// `+0x50` clear), else 0.
fn init_sio_mode(bus: &mut Bus, record: u32) -> u32 {
    let sio = bus.lw(SIO_POINTER);
    bus.sh(at(sio, 0xa), 0x40);
    bus.sh(at(sio, 0xa), 0);
    bus.sh(at(sio, 8), 0xd);
    bus.sh(at(sio, 0xe), 0x88);
    let settle = if bus.lbu(at(record, 0xe8)) == 8 {
        0x50
    } else {
        0x91
    };
    set_wait(bus, settle);
    let port = bus.lw(CURRENT_PORT);
    let sio = bus.lw(SIO_POINTER);
    bus.sh(at(sio, 0xa), if port == 0 { 0x1003 } else { 0x3003 });
    let counter = at(TAP_COUNTS, port << 2);
    if (bus.lw(counter) as i32) >= 0 {
        loop {
            let port = bus.lw(CURRENT_PORT);
            let counter = at(TAP_COUNTS, port << 2);
            let count = bus.lw(counter);
            if count as i32 <= 0 {
                break;
            }
            let count = count.wrapping_sub(1);
            bus.sw(counter, count);
            let base = bus.lw(at(record, 0xc));
            call_at(
                bus,
                SEQ_RECV,
                [at(base, count.wrapping_mul(RECORD_BYTES)), 0],
            );
        }
        let port = bus.lw(CURRENT_PORT);
        let counter = at(TAP_COUNTS, port << 2);
        if bus.lw(counter) == 0 {
            bus.sw(counter, 0xffff_ffff);
            call_at(bus, SEQ_RECV, [record, 0]);
            call_at(bus, CLEAR_REQUEST, [record, 0]);
        }
    }
    let sio = bus.lw(SIO_POINTER);
    if bus.lhu(at(sio, 4)) & 0x200 != 0 {
        let ctrl = bus.lhu(at(sio, 0xa));
        bus.sh(at(sio, 0xa), ctrl | 0x10);
        if bus.lhu(at(sio, 4)) & 0x200 != 0 {
            // A controller still acknowledges: finish its exchange.
            delay(bus);
            let sio = bus.lw(SIO_POINTER);
            bus.sb(sio, 1);
            set_wait(bus, 0x7d0);
            if clear_ack(bus) == 0 {
                return 0;
            }
            wait_rx(bus);
            let sio = bus.lw(SIO_POINTER);
            bus.lbu(sio);
            set_wait(bus, 0x1ae);
            loop {
                let status = bus.lw(I_STAT_POINTER);
                if bus.lw(status) & 0x80 != 0 {
                    break;
                }
                if wait_expired(bus) {
                    return 0;
                }
            }
            let sio = bus.lw(SIO_POINTER);
            bus.sb(sio, 0x42);
            set_wait(bus, 0x3c);
            if clear_ack(bus) == 0 {
                return 0;
            }
            wait_rx(bus);
            let sio = bus.lw(SIO_POINTER);
            bus.lbu(sio);
            set_wait(bus, 0x1ae);
            loop {
                let status = bus.lw(I_STAT_POINTER);
                if bus.lw(status) & 0x80 != 0 {
                    break;
                }
                if wait_expired(bus) {
                    return 0;
                }
            }
            let sio = bus.lw(SIO_POINTER);
            bus.sb(sio, 1);
            set_wait(bus, 0x3c);
            if clear_ack(bus) == 0 {
                return 0;
            }
            wait_rx(bus);
            let sio = bus.lw(SIO_POINTER);
            bus.lbu(sio);
            return 0;
        }
        let status = bus.lw(I_STAT_POINTER);
        bus.sw(status, !0x80);
    }
    if bus.lbu(at(record, 0x50)) == 0 {
        return 1;
    }
    u32::from(bus.lbu(at(record, 0x37)) == 0)
}

/// 8009C0A4: one step of the port's exchange (`[0x800B470C + step * 4]`),
/// then the acknowledge clear between bytes; an error ends the port.
fn port_step(bus: &mut Bus, record: u32) {
    let step = bus.lw(STEP);
    let routine = bus.lw(at(STEPS, step << 2));
    bus.sw(STEP, step.wrapping_add(1));
    let result = call(bus, routine, [record, 0]);
    if (result as i32) < 0 {
        call_at(bus, PORT_END, [result, 0]);
        return;
    }
    let mut step = bus.lw(STEP);
    if step != 0 {
        let tap = step == 3 && {
            let table = bus.lw(at(record, 0x3c));
            bus.lbu(table) == 0x80
        };
        if !tap {
            set_wait(bus, 0x3c);
            if clear_ack(bus) == 0 {
                call_at(bus, PORT_END, [(-3i32) as u32, 0]);
            }
        }
        step = bus.lw(STEP);
    }
    if (step as i32) >= 5 {
        bus.sw(STEP, step.wrapping_sub(1));
    }
}

/// 8009C194 _padSioRW(record, byte): with `byte` < 0 starts the exchange
/// (address byte `!byte` after the settle wait, index `+0x44` = 0xFF);
/// otherwise receives the reply to the previous byte, waits for its
/// acknowledge and sends `byte`. Returns the received byte, or -20 when the
/// controller stops acknowledging.
fn sio_rw(bus: &mut Bus, record: u32, byte: u32) -> u32 {
    if (byte as i32) < 0 {
        let sio = bus.lw(SIO_POINTER);
        let table = bus.lw(at(record, 0x40));
        let received = bus.lbu(sio);
        bus.sb(at(record, 0x44), 0xff);
        bus.sb(at(record, 0x45), 1);
        bus.sb(table, !byte);
        let sio = bus.lw(SIO_POINTER);
        while bus.lhu(at(sio, 4)) & 1 == 0 {}
        delay(bus);
        let sio = bus.lw(SIO_POINTER);
        bus.sb(sio, !byte);
        return received & 0xff;
    }
    let table = bus.lw(at(record, 0x3c));
    let mut baud = 0x88;
    if bus.lbu(table) >> 4 == 8 && bus.lbu(at(record, 0x44)) >= 9 {
        baud = 0x22;
    }
    let count = bus.lhu(RCNT2_COUNT);
    bus.sw(WAIT_LENGTH, 0x1ae);
    bus.sw(WAIT_START, count);
    let sio = bus.lw(SIO_POINTER);
    while bus.lhu(at(sio, 4)) & 2 == 0 {}
    let sio = bus.lw(SIO_POINTER);
    let status = bus.lw(I_STAT_POINTER);
    let received = bus.lbu(sio) & 0xff;
    bus.sh(at(sio, 0xe), baud);
    if bus.lw(status) & 0x80 == 0 {
        loop {
            if wait_expired(bus) {
                return (-20i32) as u32;
            }
            let status = bus.lw(I_STAT_POINTER);
            if bus.lw(status) & 0x80 != 0 {
                break;
            }
        }
    }
    let sio = bus.lw(SIO_POINTER);
    bus.sb(sio, byte);
    if baud == 0x22 {
        let status = bus.lw(I_STAT_POINTER);
        let sio = bus.lw(SIO_POINTER);
        bus.sw(status, !0x80);
        let ctrl = bus.lhu(at(sio, 0xa));
        bus.sh(at(sio, 0xa), ctrl | 0x10);
    }
    let count = bus.lbu(at(record, 0x45));
    let index = bus.lbu(at(record, 0x44));
    let table = bus.lw(at(record, 0x3c));
    bus.sb(at(record, 0x45), count.wrapping_add(1));
    bus.sb(at(table, index), received);
    let index = bus.lbu(at(record, 0x44));
    bus.sb(at(record, 0x44), index.wrapping_add(1));
    received
}

/// 8009C3A4 _padSioRW2(record, byte): as _padSioRW for the header bytes,
/// with the acknowledge wait timed on root counter 2 directly. Returns the
/// received byte, or -2 on timeout.
fn sio_rw2(bus: &mut Bus, record: u32, byte: u32) -> u32 {
    let table = bus.lw(at(record, 0x3c));
    let mut baud = 0x88;
    if bus.lbu(table) >> 4 == 8 && bus.lbu(at(record, 0x44)) >= 9 {
        baud = 0x22;
    }
    let sio = bus.lw(SIO_POINTER);
    while bus.lhu(at(sio, 4)) & 2 == 0 {}
    set_wait(bus, 0x190);
    let sio = bus.lw(SIO_POINTER);
    let received = bus.lbu(sio) & 0xff;
    let index = bus.lbu(at(record, 0x44));
    if index == 0 && received >> 4 == 8 {
        bus.sh(at(sio, 0xe), 0x22);
    } else {
        bus.sh(at(sio, 0xe), baud);
    }
    let status = bus.lw(I_STAT_POINTER);
    if bus.lw(status) & 0x80 == 0 {
        let start = bus.lw(WAIT_START);
        let length = bus.lw(WAIT_LENGTH);
        loop {
            let now = bus.lhu(RCNT2_COUNT);
            let now = elapsed(bus, now, start);
            let passed = if bus.lhu(RCNT2_MODE) & 0x200 != 0 {
                now.wrapping_sub(start)
            } else {
                now.wrapping_sub(start) >> 3
            };
            if passed >= length {
                return (-2i32) as u32;
            }
            if bus.lw(status) & 0x80 != 0 {
                break;
            }
        }
    }
    if bus.lbu(at(record, 0xe8)) != 8 && bus.lw(STEP) == 2 {
        set_wait(bus, 0x3c);
        delay(bus);
    }
    let sio = bus.lw(SIO_POINTER);
    bus.sb(sio, byte);
    if bus.lw(STEP) == 3 && received == 0x80 {
        let status = bus.lw(I_STAT_POINTER);
        let sio = bus.lw(SIO_POINTER);
        bus.sw(status, !0x80);
        let ctrl = bus.lhu(at(sio, 0xa));
        bus.sh(at(sio, 0xa), ctrl | 0x10);
    }
    let count = bus.lbu(at(record, 0x45));
    let index = bus.lbu(at(record, 0x44));
    bus.sb(at(record, 0x45), count.wrapping_add(1));
    if index != 0xff {
        let index = bus.lbu(at(record, 0x44));
        let table = bus.lw(at(record, 0x3c));
        bus.sb(at(table, index), received);
    }
    let index = bus.lbu(at(record, 0x44));
    bus.sb(at(record, 0x44), index.wrapping_add(1));
    received
}

/// 8009C61C _padClrIntSio0(): acknowledge the SIO interrupt, wait for the
/// acknowledge line to drop (timed), reset the SIO acknowledge. 1, or 0 on
/// timeout.
fn clear_ack(bus: &mut Bus) -> u32 {
    let status = bus.lw(I_STAT_POINTER);
    let sio = bus.lw(SIO_POINTER);
    bus.sw(status, !0x80);
    if bus.lhu(at(sio, 4)) & 0x80 != 0 {
        loop {
            if wait_expired(bus) {
                return 0;
            }
            let sio = bus.lw(SIO_POINTER);
            if bus.lhu(at(sio, 4)) & 0x80 == 0 {
                break;
            }
        }
    }
    let sio = bus.lw(SIO_POINTER);
    let ctrl = bus.lhu(at(sio, 0xa));
    bus.sh(at(sio, 0xa), ctrl | 0x10);
    1
}

/// 8009C6AC _padWaitRXready().
fn wait_rx(bus: &mut Bus) {
    let sio = bus.lw(SIO_POINTER);
    while bus.lhu(at(sio, 4)) & 2 == 0 {}
}

// ---- the exchange steps (0x800B470C) ----------------------------------------------------------

/// 8009C6E0: request set-up (`[0x800B469C]`, result at 0x800B4704), then the
/// address byte (`_padSioRW(record, -2)`).
fn step_select(bus: &mut Bus, record: u32) -> u32 {
    let result = call_at(bus, SEQ_SEND, [record, 0]);
    let table = bus.lw(at(record, 0x3c));
    bus.sw(SEQ_RESULT, result);
    bus.sb(table, 0);
    sio_rw(bus, record, (-2i32) as u32)
}

/// 8009C728: the command byte (`+0x37`, or 0x42 to poll).
fn step_command(bus: &mut Bus, record: u32) -> u32 {
    if bus.lw(CURRENT_PORT) == bus.lw(FIRST_PORT) && bus.lw(TAP_FLAG) != 0 {
        call_at(bus, TAP_HOOK_2, [record, 0]);
        call_at(bus, TAP_HOOK, [record, 0]);
    }
    if bus.lw(SEQ_RESULT) != 0 {
        let base = bus.lw(at(record, 0xc));
        call_at(bus, SEQ_SEND, [base, 0]);
        let base = bus.lw(at(record, 0xc));
        call_at(bus, SEQ_SEND, [at(base, 0xf0), 0]);
    }
    let command = bus.lbu(at(record, 0x37));
    sio_rw2(bus, record, if command != 0 { command } else { 0x42 })
}

/// 8009C800: the tap byte; the reply to the command byte (the ID) gives
/// the halfwords to come (0x800B46FC, 0x20 for ID nibble 0). Returns 0.
fn step_tap(bus: &mut Bus, record: u32) -> u32 {
    if bus.lw(SEQ_RESULT) != 0 {
        let base = bus.lw(at(record, 0xc));
        call_at(bus, SEQ_SEND, [at(base, 0x1e0), 0]);
        let base = bus.lw(at(record, 0xc));
        call_at(bus, SEQ_SEND, [at(base, 0x2d0), 0]);
    }
    let byte = if bus.lbu(at(record, 0x37)) != 0 {
        0
    } else {
        bus.lw(TAP_MODE)
    };
    let id = sio_rw2(bus, record, byte);
    if (id as i32) < 0 {
        return id;
    }
    let halfwords = (id & 0xf) << 1;
    bus.sw(REMAINING, halfwords);
    if halfwords == 0 {
        bus.sw(REMAINING, 0x20);
    }
    0
}

/// 8009C8AC: the first data byte (`[0x800B468C]`); the reply to the tap
/// byte must be 0x5A. Returns 0x5A (or 0), -4 for another reply.
fn step_first(bus: &mut Bus, record: u32) -> u32 {
    let mut tap = 0;
    if bus.lw(TAP_MODE) != 0 {
        let table = bus.lw(at(record, 0x3c));
        if bus.lbu(table) >> 4 == 8 {
            tap = u32::from(bus.lbu(at(record, 0x37)) == 0);
        }
    }
    bus.sw(TAP_SEND, tap);
    if tap == 0 {
        call_at(bus, PREPARE, [record, 0]);
    }
    let tap = bus.lw(TAP_SEND);
    let byte = call_at(bus, SEND_BYTE, [record, tap]);
    let reply = sio_rw2(bus, record, byte & 0xff);
    if reply == 0x5a || reply == 0 || (reply as i32) < 0 {
        reply
    } else {
        (-4i32) as u32
    }
}

/// 8009C970: the remaining data bytes (multitap sub-ports first), the last
/// reply without acknowledge, then the port end (`[0x800B4684](0)`).
fn step_data(bus: &mut Bus, record: u32) -> u32 {
    if bus.lw(TAP_SEND) != 0 {
        call_at(bus, PREPARE, [record, 0]);
    }
    let tap = bus.lw(TAP_SEND);
    if tap != 0 {
        let mut sub = -1i32;
        let mut offset = (-0xf0i32) as u32;
        loop {
            let remaining = bus.lw(REMAINING).wrapping_sub(1);
            bus.sw(REMAINING, remaining);
            if remaining as i32 <= 0 {
                break;
            }
            if sub >= 0 {
                let base = bus.lw(at(record, 0xc));
                call_at(bus, PREPARE, [at(base, offset), 0]);
            }
            let byte = call_at(bus, SEND_BYTE, [record, 1]);
            let reply = sio_rw2(bus, record, byte & 0xff);
            if (reply as i32) < 0 {
                return reply;
            }
            set_wait(bus, 0x3c);
            let acknowledged = clear_ack(bus);
            sub += 1;
            if acknowledged == 0 {
                return (-3i32) as u32;
            }
            if sub >= 4 {
                break;
            }
            offset = offset.wrapping_add(0xf0);
        }
    }
    let port = bus.lw(CURRENT_PORT);
    let other = u32::from(port == 0);
    if (bus.lw(REMAINING) as i32) >= 2 {
        let counter = at(TAP_COUNTS, other << 2);
        let other_offset = other.wrapping_mul(RECORD_BYTES);
        let mut sub_record = 0u32;
        loop {
            let count = bus.lw(counter);
            if (count as i32) < 0 {
                break;
            }
            if count as i32 > 0 {
                let records = bus.lw(RECORDS_POINTER);
                let base = bus.lw(at(at(other_offset, records), 0xc));
                sub_record = at(base, count.wrapping_mul(RECORD_BYTES)).wrapping_sub(0xf0);
                call_at(bus, SEQ_RECV, [sub_record, 0]);
            }
            let count = bus.lw(counter);
            if count == 3 {
                call_at(bus, SEQ_RECV, [sub_record.wrapping_sub(0xf0), 0]);
                bus.sw(counter, 1);
            } else if (count as i32) >= 4 {
                if count == 4 {
                    bus.sw(counter, 3);
                }
            } else if count as i32 >= 2 || (count as i32) < 0 {
            } else {
                let records = bus.lw(RECORDS_POINTER);
                sub_record = at(records, other_offset);
                call_at(bus, SEQ_RECV, [sub_record, 0]);
                call_at(bus, CLEAR_REQUEST, [sub_record, 0]);
                bus.sw(counter, 0xffff_ffff);
            }
            let byte = call_at(bus, SEND_BYTE, [record, tap]);
            let reply = sio_rw(bus, record, byte & 0xff);
            if (reply as i32) < 0 {
                return reply;
            }
            set_wait(bus, 0x3c);
            if clear_ack(bus) == 0 {
                return (-3i32) as u32;
            }
            let remaining = bus.lw(REMAINING).wrapping_sub(1);
            bus.sw(REMAINING, remaining);
            if (remaining as i32) < 2 {
                break;
            }
        }
    }
    let remaining = bus.lw(REMAINING).wrapping_sub(1);
    bus.sw(REMAINING, remaining);
    if remaining as i32 > 0 {
        loop {
            let byte = call_at(bus, SEND_BYTE, [record, tap]);
            let reply = sio_rw(bus, record, byte & 0xff);
            if (reply as i32) < 0 {
                return reply;
            }
            let sio = bus.lw(SIO_POINTER_2);
            if bus.lhu(at(sio, 0xe)) != 0x22 {
                set_wait(bus, 0x3c);
                if clear_ack(bus) == 0 {
                    return (-3i32) as u32;
                }
            }
            let remaining = bus.lw(REMAINING).wrapping_sub(1);
            bus.sw(REMAINING, remaining);
            if remaining as i32 <= 0 {
                break;
            }
        }
    }
    wait_rx(bus);
    let index = bus.lbu(at(record, 0x44));
    bus.sb(at(record, 0x44), index.wrapping_add(1));
    let sio = bus.lw(SIO_POINTER_2);
    let table = bus.lw(at(record, 0x3c));
    let last = bus.lbu(sio);
    bus.sb(at(table, index), last);
    call_at(bus, PORT_END, [0, 0]);
    0
}

// ---- PADPORTD: port records ---------------------------------------------------------------------

/// 8009DF30: forget the controller of `record` (state, request, tables).
fn reset_record<M: PadMemory>(bus: &mut M, record: u32) {
    if bus.lbu(at(record, 0x49)) == 0 {
        return;
    }
    bus.sb(at(record, 0x49), 0);
    bus.sb(at(record, 0x46), 0);
    bus.sh(at(record, 0xe6), 0);
    bus.sw(at(record, 0x14), 0);
    bus.sw(at(record, 0x18), 0);
    bus.sb(at(record, 0xe3), 0);
    bus.sb(at(record, 0xe4), 0);
    bus.sh(at(record, 0xe6), 0);
    bus.sb(at(record, 0xe9), 0);
    bus.sb(at(record, 0xea), 0);
    bus.sw(record, 0);
    bus.sw(at(record, 4), 0);
    bus.sw(at(record, 8), 0);
    for i in 0..6 {
        bus.sb(at(record, 0x5d + i), 0xff);
    }
}

/// 8009DF98: end of the current port's exchange with result `code` (0 ok,
/// -9 skip, else failure: _dirFailAuto and the request clear); SIO
/// deselected, next port set up until one can be polled or none is left.
fn port_end(bus: &mut Bus, code: u32) -> u32 {
    let mut code = code;
    loop {
        let port = bus.lw(CURRENT_PORT);
        let record = at(RECORDS, port.wrapping_mul(RECORD_BYTES));
        if code != (-9i32) as u32 {
            if code == 0 {
                bus.sw(at(TAP_COUNTS, port << 2), 0);
            } else {
                fail_auto(bus, record);
                clear_request(bus, record);
            }
        }
        let sio = bus.lw(SIO_POINTER_3);
        let port = bus.lw(CURRENT_PORT);
        bus.sw(STEP, 0);
        bus.sh(at(sio, 0xa), 0);
        let last = bus.lw(LAST_PORT);
        let port = port.wrapping_add(1);
        bus.sw(CURRENT_PORT, port);
        let ready = if (last as i32) < port as i32 {
            1
        } else {
            init_sio_mode(bus, at(RECORDS, port.wrapping_mul(RECORD_BYTES)))
        };
        code = 0xffff;
        if ready != 0 {
            return ready;
        }
    }
}

/// 8009E08C: the request command `+0x37` moves to `+0x38`.
fn clear_request(bus: &mut Bus, record: u32) -> u32 {
    let command = bus.lbu(at(record, 0x37));
    bus.sb(at(record, 0x37), 0);
    bus.sb(at(record, 0x38), command);
    command
}

/// 8009E09C: the data byte to send at position `+0x45`: actuator bytes
/// (`[+0x28]`, `+0x34` long, enabled by `+0x57..`) when polling, else the
/// request's parameters (`[+0x2C]`, `+0x36` long; 0xFF past the end of a
/// 0x4D map).
fn send_byte(bus: &mut Bus, record: u32) -> u32 {
    let position = bus.lbu(at(record, 0x45));
    let command = bus.lbu(at(record, 0x37));
    let offset = position.wrapping_sub(3);
    if command == 0 {
        if (offset as i32) < 6 && bus.lbu(at(at(record, offset), 0x57)) == 0 {
            return 0;
        }
        if (offset as i32) < bus.lbu(at(record, 0x34)) as i32 {
            let data = bus.lw(at(record, 0x28));
            return bus.lbu(at(data, offset));
        }
        return 0;
    }
    let length = bus.lbu(at(record, 0x36));
    if (offset as i32) < length as i32 {
        let data = bus.lw(at(record, 0x2c));
        return bus.lbu(at(data, offset));
    }
    if command == 0x4d { 0xff } else { 0 }
}

/// 8009E158: which actuator bytes may be sent this VBlank (`+0x57..+0x5C`),
/// within libpad's power budget 0x800B46CC (60 units).
fn prepare_actuators(bus: &mut Bus, record: u32) {
    for i in 0..6 {
        bus.sb(at(record, 0x57 + i), 0);
    }
    if bus.lhu(at(record, 0xe6)) != 0 && bus.lw(at(record, 0x28)) != 0 {
        let length = bus.lbu(at(record, 0x34));
        let count = if length < 7 { length } else { 6 };
        let mut actuator = 0u32;
        let mut offset = 0u32;
        while actuator < bus.lbu(at(record, 0xe9)) {
            let table = bus.lw(at(record, 4));
            let mask = if bus.lbu(at(at(offset, table), 2)) != 0 {
                0xff
            } else {
                1
            };
            let mut on = false;
            let data = bus.lw(at(record, 0x28));
            for i in 0..count {
                if bus.lbu(at(record, 0x5d + i)) == actuator && bus.lbu(at(data, i)) & mask != 0 {
                    on = true;
                    break;
                }
            }
            if on {
                let table = bus.lw(at(record, 4));
                let power = bus.lbu(at(at(offset, table), 3));
                let total = bus.lw(POWER).wrapping_add(power);
                if (total as i32) < 0x3d {
                    bus.sw(POWER, total);
                } else {
                    on = false;
                }
            }
            if on {
                for i in 0..count {
                    if bus.lbu(at(record, 0x5d + i)) == actuator {
                        bus.sb(at(record, 0x57 + i), 1);
                    }
                }
            }
            actuator += 1;
            offset += 5;
        }
        return;
    }
    let id = bus.lbu(at(record, 0xe8));
    if (id.wrapping_sub(4) < 2 || id == 7)
        && bus.lhu(at(record, 0xe6)) == 0
        && bus.lbu(at(record, 0x34)) >= 2
    {
        // Pads with the original vibration protocol: motor bytes 0x40/0x41.
        let data = bus.lw(at(record, 0x28));
        if bus.lbu(data) & 0xc0 != 0x40 || bus.lbu(at(data, 1)) & 1 == 0 {
            return;
        }
        if (bus.lw(POWER).wrapping_add(0xa) as i32) >= 0x3d {
            return;
        }
        bus.sb(at(record, 0x58), 1);
        bus.sb(at(record, 0x57), 1);
        let power = bus.lw(POWER);
        bus.sw(POWER, power.wrapping_add(0xa));
        return;
    }
    if bus.lbu(at(record, 0xe8)) == 3 {
        bus.sb(at(record, 0x57), 1);
        return;
    }
    if bus.lhu(at(record, 0xe6)) == 0 {
        for i in (0..6).rev() {
            bus.sb(at(record, 0x57 + i), 1);
        }
    }
}

// ---- PADSEQD: requests and replies -------------------------------------------------------------

/// 8009E464 (`[0x800B469C]`): the request of this exchange from `+0x46`:
/// enter (1) or leave (0xFE) configuration mode, the record's request
/// handler `[+0x14]` (or _padSendAtLoadInfo). Returns 0.
fn seq_send(bus: &mut Bus, record: u32) -> u32 {
    let table = bus.lw(at(record, 0x3c));
    if bus.lbu(table) == 0xf3 {
        if bus.lbu(at(record, 0xe8)) == 0 {
            cmd_para_mode(bus, record, 0);
            return 0;
        }
        let request = bus.lbu(at(record, 0x46));
        if request == 0xff {
            cmd_para_mode(bus, record, 0);
            return 0;
        }
        if bus.lbu(at(record, 0x49)) == 2 {
            call_at(bus, RESET_RECORD, [record, 0]);
        }
    }
    let request = bus.lbu(at(record, 0x46));
    match request {
        1 => {
            cmd_para_mode(bus, record, 1);
            return 0;
        }
        0 | 0xff => return 0,
        0xfe => {
            cmd_para_mode(bus, record, 0);
            return 0;
        }
        _ => {}
    }
    let handler = bus.lw(at(record, 0x14));
    if handler != 0 {
        call(bus, handler, [record, 0]);
    } else {
        send_at_load_info(bus, record);
    }
    0
}

/// 8009E574 (`[0x800B46A4]`): the previous exchange's reply: the
/// controller ID (`+0xE8`, high nibble of the first byte) and the data into
/// the game's buffer `[+0x30]` (status 0, ID, data), then the request's
/// progress `+0x46` and state `+0x49`.
fn seq_recv(bus: &mut Bus, record: u32) -> u32 {
    let table = bus.lw(at(record, 0x3c));
    let first = bus.lbu(table);
    if first & 0xf0 == 0 {
        let buffer = bus.lw(at(record, 0x30));
        bus.sb(buffer, 0xff);
        let buffer = bus.lw(at(record, 0x30));
        bus.sb(at(buffer, 1), 0);
        bus.sb(at(record, 0xe8), 0);
        bus.sb(at(record, 0x35), 0);
        return call_at(bus, RESET_RECORD, [record, 0]);
    }
    let previous = bus.lbu(at(record, 0xe8));
    let id = first >> 4;
    bus.sb(at(record, 0xe8), id);
    if id == 0xf {
        bus.sb(at(record, 0xe8), previous);
    } else {
        let buffer = bus.lw(at(record, 0x30));
        bus.sb(buffer, 0);
        let table = bus.lw(at(record, 0x3c));
        let buffer = bus.lw(at(record, 0x30));
        let first = bus.lbu(table);
        bus.sb(at(buffer, 1), first);
        let length = bus.lbu(at(record, 0x44));
        bus.sb(at(record, 0x35), length);
        let mut i = 2u32;
        while (i as i32) < bus.lbu(at(record, 0x44)) as i32 {
            let table = bus.lw(at(record, 0x3c));
            let buffer = bus.lw(at(record, 0x30));
            let byte = bus.lbu(at(table, i));
            bus.sb(at(buffer, i), byte);
            i += 1;
        }
    }
    // 8009E654: a missing 0x5A (with a request in progress or `+0x50`
    // clear) or a new ID forgets the controller.
    let table = bus.lw(at(record, 0x3c));
    let mut reset = false;
    let mut compare = true;
    if bus.lbu(at(table, 1)) == 0 {
        let request = bus.lbu(at(record, 0x46));
        if (request != 1 || bus.lw(at(record, 0x14)) != 0) && bus.lbu(at(record, 0x50)) == 0 {
            reset = true;
            compare = false;
        }
    }
    if compare && bus.lbu(at(record, 0xe8)) != previous {
        reset = true;
    }
    if reset {
        call_at(bus, RESET_RECORD, [record, 0]);
    }
    let request = bus.lbu(at(record, 0x46));
    bus.sb(at(record, 0x4a), 0);
    if request == 0xff {
        return 0;
    }
    if request != 0 && bus.lbu(at(record, 0x37)) == 0 {
        return 0;
    }
    if request.wrapping_sub(2) & 0xff < 0xfc {
        let table = bus.lw(at(record, 0x3c));
        if bus.lbu(table) != 0xf3 {
            call_at(bus, RESET_RECORD, [record, 0]);
            return 0;
        }
    }
    let request = bus.lbu(at(record, 0x46));
    match request {
        1 => {
            let request = bus.lbu(at(record, 0x46));
            bus.sb(at(record, 0x47), 0);
            bus.sb(at(record, 0x46), request.wrapping_add(1));
        }
        0 => {
            let request = bus.lbu(at(record, 0x46));
            bus.sb(at(record, 0x49), 1);
            bus.sb(at(record, 0x46), request.wrapping_add(1));
        }
        0xfe => bus.sb(at(record, 0x46), 0xff),
        _ => {
            let handler = bus.lw(at(record, 0x18));
            let advance = if handler != 0 {
                call(bus, handler, [record, 0])
            } else {
                recv_at_load_info(bus, record)
            };
            let request = bus.lbu(at(record, 0x46));
            bus.sb(at(record, 0x46), request.wrapping_add(advance));
        }
    }
    0
}

/// 8009E7D8 _dirFailAuto(record): a failed exchange: count it (`+0x4C`),
/// give up detection after 11 tries (state 2, request 0xFF) or reset the
/// record; the game's buffer reads "no data" unless in configuration mode.
fn fail_auto(bus: &mut Bus, record: u32) -> u32 {
    let failures = bus.lw(at(record, 0x4c)).wrapping_add(1);
    let request = bus.lbu(at(record, 0x46));
    bus.sw(at(record, 0x4c), failures);
    if request != 0 {
        let tries = bus.lbu(at(record, 0x4a));
        if request == 1 {
            if tries >= 0xb {
                bus.sb(at(record, 0x49), 2);
                bus.sb(at(record, 0x46), 0xff);
                return 0;
            }
            bus.sb(at(record, 0x4a), tries.wrapping_add(1));
            return 0;
        }
        if tries < 0xb {
            bus.sb(at(record, 0x4a), tries.wrapping_add(1));
            return 0;
        }
        if bus.lbu(at(record, 0x49)) != 0 {
            call_at(bus, RESET_RECORD, [record, 0]);
        }
    }
    let table = bus.lw(at(record, 0x3c));
    if bus.lbu(table) == 0xf3 {
        return 0;
    }
    let buffer = bus.lw(at(record, 0x30));
    bus.sb(buffer, 0xff);
    let buffer = bus.lw(at(record, 0x30));
    bus.sb(at(buffer, 1), 0);
    bus.sb(at(record, 0xe8), 0);
    bus.sb(at(record, 0x35), 0);
    0
}

/// 8009E8B8 (`[0x800B46A0]`): 0 when the port takes a request.
fn busy(bus: &mut Bus, record: u32) -> u32 {
    if bus.lhu(at(record, 0xe6)) == 0 {
        return 1;
    }
    u32::from(bus.lbu(at(record, 0x46)) != 0xff)
}

// ---- PADCMD: configuration-mode requests ------------------------------------------------------

/// 8009DD24 _padCmdParaMode(record, enter): command 0x43 with parameter
/// `enter` (`+0x24`).
fn cmd_para_mode(bus: &mut Bus, record: u32, enter: u32) {
    bus.sb(at(record, 0x37), 0x43);
    bus.sw(at(record, 0x2c), at(record, 0x24));
    bus.sb(at(record, 0x24), enter);
    bus.sb(at(record, 0x36), 1);
}

/// 8009DD44 / 8009DD58 / 8009DD78 / 8009DD98 / 8009DDB8: commands 0x45
/// (no parameter), 0x4C, 0x46, 0x47 (one parameter at `+0x24`), 0x4B.
fn cmd(bus: &mut Bus, record: u32, command: u32, parameter: Option<u32>) {
    bus.sb(at(record, 0x37), command);
    match parameter {
        Some(value) => {
            bus.sw(at(record, 0x2c), at(record, 0x24));
            bus.sb(at(record, 0x24), value);
            bus.sb(at(record, 0x36), 1);
        }
        None => {
            bus.sw(at(record, 0x2c), 0);
            bus.sb(at(record, 0x36), 0);
        }
    }
}

/// 8009D1EC _padSendAtLoadInfo(record): the controller survey: 0x45 (2),
/// 0x4C (3, mode `+0xE4`), 0x47 (4, `+0x47`).
fn send_at_load_info(bus: &mut Bus, record: u32) {
    match bus.lbu(at(record, 0x46)) {
        2 => cmd(bus, record, 0x45, None),
        3 => {
            let mode = bus.lbu(at(record, 0xe4));
            cmd(bus, record, 0x4c, Some(mode));
        }
        4 => {
            let index = bus.lbu(at(record, 0x47));
            cmd(bus, record, 0x47, Some(index));
        }
        _ => {}
    }
}

/// 8009D270 _padRecvAtLoadInfo(record): the survey's replies (status 0x45:
/// mode count, current mode, actuator and combination counts; 0x4C: the
/// mode IDs; 0x47: the combination sizes), retried until two replies agree
/// (`+0xEE`), then the tables (_padLoadActInfo). Returns the progress step.
fn recv_at_load_info(bus: &mut Bus, record: u32) -> u32 {
    match bus.lbu(at(record, 0x46)) {
        2 => {
            let table = bus.lw(at(record, 0x3c));
            if bus.lbu(at(table, 7)) != 0 {
                return 0;
            }
            let same = bus.lbu(at(record, 0xe3)) == bus.lbu(at(table, 3))
                && bus.lbu(at(record, 0xe4)) == bus.lbu(at(table, 4))
                && bus.lbu(at(record, 0xe9)) == bus.lbu(at(table, 5))
                && bus.lbu(at(record, 0xea)) == bus.lbu(at(table, 6));
            bus.sh(at(record, 0xee), if same { 0 } else { 0xffff });
            let table = bus.lw(at(record, 0x3c));
            let value = bus.lbu(at(table, 3));
            bus.sb(at(record, 0xe3), value);
            let value = bus.lbu(at(table, 4));
            bus.sh(at(record, 0xe6), 0);
            bus.sb(at(record, 0xe4), value);
            let value = bus.lbu(at(table, 5));
            bus.sb(at(record, 0xe9), value);
            let value = bus.lbu(at(table, 6));
            bus.sh(at(record, 0xec), 0);
            bus.sb(at(record, 0xea), value);
            if bus.lhu(at(record, 0xee)) != 0 {
                return 0;
            }
            bus.sb(at(record, 0xeb), 0);
            1
        }
        3 => {
            let table = bus.lw(at(record, 0x3c));
            if bus.lbu(at(table, 2)) != 0 || bus.lbu(at(table, 3)) != 0 {
                return 0;
            }
            let id = (bus.lbu(at(table, 4)) << 8).wrapping_add(bus.lbu(at(table, 5)));
            bus.sh(at(record, 0xe6), id);
            if bus.lhu(at(record, 0xee)) != id & 0xffff {
                bus.sh(at(record, 0xee), id);
                return 0;
            }
            bus.sh(at(record, 0xee), 0xffff);
            bus.sb(at(record, 0xeb), 0);
            bus.sb(at(record, 0x47), 0);
            1
        }
        4 => {
            let table = bus.lw(at(record, 0x3c));
            if bus.lbu(at(table, 2)) != 0 || bus.lbu(at(table, 3)) != 0 {
                return 0;
            }
            let size = bus.lhu(at(record, 0xec));
            let entry = bus.lbu(at(table, 4));
            let index = bus.lbu(at(record, 0x47)).wrapping_add(1);
            let size = size
                .wrapping_add(8)
                .wrapping_add(entry.wrapping_add(3) & 0x1fc);
            let count = bus.lbu(at(record, 0xea));
            bus.sb(at(record, 0x47), index);
            bus.sh(at(record, 0xec), size);
            if index & 0xff < count {
                return 0;
            }
            if (get_act_size(bus, record) as i32) >= 0x81 {
                call_at(bus, RESET_RECORD, [record, 0]);
                bus.sb(at(record, 0x46), 0xfe);
                bus.sb(at(record, 0x49), 2);
                return 0;
            }
            let size = bus.lhu(at(record, 0xec));
            if bus.lhu(at(record, 0xee)) != size {
                bus.sh(at(record, 0xee), size);
                bus.sb(at(record, 0x47), 0);
                bus.sh(at(record, 0xec), 0);
                return 0;
            }
            bus.sh(at(record, 0xee), 0);
            bus.sb(at(record, 0xeb), 0);
            bus.sb(at(record, 0x46), 0xff);
            load_act_info(bus, record, at(record, 0x63));
            bus.sb(at(record, 0x46), 2);
            0
        }
        _ => 1,
    }
}

/// 8009D4E0 _padGetActSize(record): bytes the mode, actuator and
/// combination tables need.
fn get_act_size(bus: &mut Bus, record: u32) -> u32 {
    let modes = bus.lbu(at(record, 0xe3));
    let actuators = bus.lbu(at(record, 0xe9));
    let combinations = bus.lhu(at(record, 0xec));
    let modes = ((modes.wrapping_add(1) as i32 >> 1) as u32) << 2;
    let actuators =
        (((actuators << 2).wrapping_add(actuators).wrapping_add(3)) & 0xffc).wrapping_add(4);
    modes.wrapping_add(actuators).wrapping_add(combinations)
}

/// 8009D518 _padLoadActInfo(record, memory): lay out the tables at
/// `memory` (`+0` modes, `+4` actuators, `+8` combinations) and start the
/// table survey (state 4, request 1, handlers 8009D5EC/8009D694).
fn load_act_info(bus: &mut Bus, record: u32, memory: u32) -> u32 {
    if memory == 0 {
        return 0;
    }
    if bus.lw(at(record, 4)) != 0 {
        return 0;
    }
    if call_at(bus, 0x800b_46a0, [record, 0]) != 0 {
        return 0;
    }
    let base = (memory.wrapping_add(3) as i32 >> 2) as u32;
    bus.sb(at(record, 0x49), 4);
    bus.sb(at(record, 0x46), 1);
    bus.sw(at(record, 0x14), 0x8009_d5ec);
    let modes = bus.lbu(at(record, 0xe3));
    bus.sw(at(record, 0x18), 0x8009_d694);
    let actuators = bus.lbu(at(record, 0xe9));
    let base = base << 2;
    bus.sw(record, base);
    bus.sb(at(record, 0x47), 0);
    let actuator_table = base.wrapping_add((modes.wrapping_add(1) as i32 >> 1) as u32 * 4);
    let size = ((actuators << 2).wrapping_add(actuators).wrapping_add(3)) & 0xffc;
    bus.sw(at(record, 4), actuator_table);
    bus.sw(at(record, 8), actuator_table.wrapping_add(size));
    1
}

/// 8009D5EC: the table survey's commands: 0x4C (2), 0x46 (3), 0x47 (4,
/// or 0x4B once `+0x48` is set), index `+0x47`.
fn send_load_info(bus: &mut Bus, record: u32) -> u32 {
    match bus.lbu(at(record, 0x46)) {
        2 => {
            let index = bus.lbu(at(record, 0x47));
            cmd(bus, record, 0x4c, Some(index));
        }
        3 => {
            let index = bus.lbu(at(record, 0x47));
            cmd(bus, record, 0x46, Some(index));
        }
        4 => {
            if bus.lbu(at(record, 0x48)) == 0 {
                let index = bus.lbu(at(record, 0x47));
                cmd(bus, record, 0x47, Some(index));
            } else {
                cmd(bus, record, 0x4b, None);
            }
        }
        _ => {}
    }
    0
}

/// 8009D694: the table survey's replies into the mode (`[+0]`), actuator
/// (`[+4]`, 5 bytes each) and combination (`[+8]`) tables, each entry read
/// twice until it agrees (`+0xEE`). Returns 1 when a table is complete.
fn recv_load_info(bus: &mut Bus, record: u32) -> u32 {
    let request = bus.lbu(at(record, 0x46));
    match request {
        2 => {
            let table = bus.lw(at(record, 0x3c));
            if bus.lbu(at(table, 2)) != 0 || bus.lbu(at(table, 3)) != 0 {
                return 0;
            }
            let index = bus.lbu(at(record, 0x47));
            let modes = bus.lw(record);
            let id = (bus.lbu(at(table, 4)) << 8).wrapping_add(bus.lbu(at(table, 5)));
            bus.sh(at(modes, index << 1), id);
            let index = bus.lbu(at(record, 0x47));
            let modes = bus.lw(record);
            let stored = bus.lhu(at(modes, index << 1));
            if bus.lhu(at(record, 0xee)) != stored {
                bus.sh(at(record, 0xee), stored);
                return 0;
            }
            let index = bus.lbu(at(record, 0x47)).wrapping_add(1);
            let count = bus.lbu(at(record, 0xe3));
            bus.sh(at(record, 0xee), 0);
            bus.sb(at(record, 0xeb), 0);
            bus.sb(at(record, 0x47), index);
            if index & 0xff < count {
                return 0;
            }
            bus.sb(at(record, 0x47), 0);
            1
        }
        3 => {
            let reply = bus.lw(at(record, 0x3c));
            if bus.lbu(at(reply, 2)) != 0 || bus.lbu(at(reply, 3)) != 0 {
                return 0;
            }
            let index = bus.lbu(at(record, 0x47));
            let actuators = bus.lw(at(record, 4));
            let entry = at(actuators, (index << 2).wrapping_add(index));
            let same = bus.lbu(entry) == bus.lbu(at(reply, 4))
                && bus.lbu(at(entry, 1)) == bus.lbu(at(reply, 5)) & 0x7f
                && bus.lbu(at(entry, 2)) == bus.lbu(at(reply, 6))
                && bus.lbu(at(entry, 3)) == bus.lbu(at(reply, 7))
                && bus.lbu(at(entry, 4)) == bus.lbu(at(reply, 5)) >> 7;
            bus.sh(at(record, 0xee), if same { 0 } else { 0xffff });
            let reply = bus.lw(at(record, 0x3c));
            let value = bus.lbu(at(reply, 4));
            bus.sb(entry, value);
            let reply = bus.lw(at(record, 0x3c));
            let value = bus.lbu(at(reply, 5)) & 0x7f;
            bus.sb(at(entry, 1), value);
            let reply = bus.lw(at(record, 0x3c));
            let value = bus.lbu(at(reply, 6));
            bus.sb(at(entry, 2), value);
            let reply = bus.lw(at(record, 0x3c));
            let value = bus.lbu(at(reply, 7));
            bus.sb(at(entry, 3), value);
            let reply = bus.lw(at(record, 0x3c));
            let value = bus.lbu(at(reply, 5)) >> 7;
            bus.sb(at(entry, 4), value);
            if bus.lhu(at(record, 0xee)) != 0 {
                return 0;
            }
            let index = bus.lbu(at(record, 0x47)).wrapping_add(1);
            let count = bus.lbu(at(record, 0xe9));
            bus.sb(at(record, 0xeb), 0);
            bus.sb(at(record, 0x47), index);
            if index & 0xff < count {
                return 0;
            }
            bus.sb(at(record, 0x47), 0);
            bus.sb(at(record, 0x48), 0);
            1
        }
        4 => recv_combinations(bus, record),
        _ => 1,
    }
}

/// 8009D8D8..8009DA80: request 4 of the table survey (combinations).
fn recv_combinations(bus: &mut Bus, record: u32) -> u32 {
    let reply = bus.lw(at(record, 0x3c));
    if bus.lbu(at(reply, 2)) != 0 {
        bus.sb(at(record, 0x48), 0);
        return 0;
    }
    let index = bus.lbu(at(record, 0x47));
    let combinations = bus.lw(at(record, 8));
    let left = bus.lbu(at(record, 0x48));
    let entry = at(combinations, index << 3);
    let mut source;
    let mut count: i32;
    if left == 0 {
        let size = bus.lbu(at(reply, 4));
        bus.sb(at(record, 0x48), size);
        bus.sb(entry, size);
        let reply = bus.lw(at(record, 0x3c));
        source = at(reply, 5);
        let index = bus.lbu(at(record, 0x47));
        let cursor = if index == 0 {
            let count = bus.lbu(at(record, 0xea));
            let combinations = bus.lw(at(record, 8));
            at(combinations, count << 3)
        } else {
            let size = bus.lbu(at(entry, (-8i32) as u32));
            let previous = bus.lw(at(entry, (-4i32) as u32));
            at(previous, size.wrapping_add(3) & 0x1fc)
        };
        bus.sw(at(entry, 4), cursor);
        bus.sw(ACT_CURSOR, cursor);
        count = 3 - 1;
    } else {
        source = at(reply, 3);
        count = 5 - 1;
    }
    let end = at(record, 0xe3);
    loop {
        if count == -1 {
            break;
        }
        if bus.lbu(at(record, 0x48)) == 0 {
            return recv_combinations_done(bus, record);
        }
        let cursor = bus.lw(ACT_CURSOR);
        if cursor >= end {
            bus.sb(at(record, 0x47), 0);
            bus.sb(at(record, 0x48), 0);
            return 0;
        }
        if bus.lbu(cursor) != bus.lbu(source) {
            bus.sh(at(record, 0xee), 0xffff);
        }
        let cursor = bus.lw(ACT_CURSOR);
        let byte = bus.lbu(source);
        source = at(source, 1);
        bus.sb(cursor, byte);
        bus.sw(ACT_CURSOR, at(cursor, 1));
        let left = bus.lbu(at(record, 0x48));
        count -= 1;
        bus.sb(at(record, 0x48), left.wrapping_sub(1));
    }
    if bus.lbu(at(record, 0x48)) != 0 {
        return 0;
    }
    recv_combinations_done(bus, record)
}

/// 8009DA14: a combination entry complete: agreed (`+0xEE` 0) moves on;
/// the last one ends the survey (state 6, request 0xFE).
fn recv_combinations_done(bus: &mut Bus, record: u32) -> u32 {
    if bus.lhu(at(record, 0xee)) != 0 {
        bus.sh(at(record, 0xee), 0);
        bus.sb(at(record, 0x48), 0);
        return 0;
    }
    let index = bus.lbu(at(record, 0x47)).wrapping_add(1);
    let count = bus.lbu(at(record, 0xea));
    bus.sb(at(record, 0x47), index);
    if index & 0xff < count {
        bus.sb(at(record, 0x48), 0);
        bus.sb(at(record, 0xeb), 0);
        return 0;
    }
    bus.sb(at(record, 0x49), 6);
    bus.sb(at(record, 0x46), 0xfe);
    bus.sb(at(record, 0xeb), 0);
    0
}

/// 8009DAF4 (PadSetActAlign's request): command 0x4D with the game's
/// alignment `[+0x20]`, 6 bytes.
fn send_act_align(bus: &mut Bus, record: u32) -> u32 {
    let data = bus.lw(at(record, 0x20));
    bus.sb(at(record, 0x37), 0x4d);
    bus.sb(at(record, 0x36), 6);
    bus.sw(at(record, 0x2c), data);
    0
}

/// 8009DB10 (PadSetActAlign's reply): the actuator map `+0x5D..+0x62` from
/// the alignment, limited by each actuator's count; request 0xFE (leave
/// configuration mode). Returns 0.
fn recv_act_align(bus: &mut Bus, record: u32) -> u32 {
    let mut actuator = 0u32;
    let mut offset = 0u32;
    while actuator < bus.lbu(at(record, 0xe9)) {
        let mut data = bus.lw(at(record, 0x20));
        let mut used = 0u32;
        for _ in 0..6 {
            if bus.lbu(data) == actuator {
                used = used.wrapping_add(1);
            }
            data = at(data, 1);
        }
        let table = bus.lw(at(record, 4));
        let mut limit = bus.lbu(at(at(offset, table), 2));
        let mut data = bus.lw(at(record, 0x20));
        if limit == 0 {
            limit = 1;
        }
        for i in 0..6 {
            if bus.lbu(data) == actuator {
                if (used as i32) < limit as i32 {
                    bus.sb(at(record, 0x5d + i), 0xff);
                    used = used.wrapping_sub(1);
                } else {
                    bus.sb(at(record, 0x5d + i), actuator);
                }
            }
            data = at(data, 1);
        }
        actuator += 1;
        offset += 5;
    }
    bus.sb(at(record, 0x46), 0xfe);
    0
}

/// 8009DC70 (PadSetMainMode's request): 0x44 with mode and lock (2), then
/// 0x4D with the actuator map (3).
fn send_main_mode(bus: &mut Bus, record: u32) -> u32 {
    match bus.lbu(at(record, 0x46)) {
        2 => {
            bus.sb(at(record, 0x37), 0x44);
            bus.sw(at(record, 0x2c), at(record, 0x51));
            bus.sb(at(record, 0x36), 2);
        }
        3 => {
            bus.sb(at(record, 0x37), 0x4d);
            bus.sw(at(record, 0x2c), at(record, 0x5d));
            bus.sb(at(record, 0x36), 6);
        }
        _ => {}
    }
    0
}

/// 8009DCC4 (PadSetMainMode's reply): with the mode unchanged (`+0x53`)
/// the map follows (request 3 after 2) or configuration mode is left;
/// otherwise the controller is forgotten (it reports a new ID).
fn recv_main_mode(bus: &mut Bus, record: u32) -> u32 {
    if bus.lbu(at(record, 0x53)) != 0 {
        if bus.lbu(at(record, 0x46)) == 2 {
            return 1;
        }
        bus.sb(at(record, 0x46), 0xfe);
        return 0;
    }
    call_at(bus, RESET_RECORD, [record, 0]);
    0
}

// ---- game-time RAM effects -----------------------------------------------------------------

/// 8009B910 PadEnableCom(mask)'s RAM effects: the enabled ports (bit 0
/// port 1 → 0x800B46D4 = 0, bit 1 port 2 → 0x800B46D8 = 1), resetting a
/// port's record (`[0x800B4688]`, 8009DF30) when it was disabled for 0x96
/// VBlanks or more, and the state flag around it. Returns the previous mask.
pub fn pad_enable_com<M: PadMemory>(m: &mut M, mask: u32) -> u32 {
    let last = m.lw(LAST_PORT);
    let first = m.lw(FIRST_PORT);
    let previous = (last << 1) | u32::from(first == 0);
    if previous == mask {
        return previous;
    }
    m.sw(STATE, 0);
    if mask & 1 != 0 {
        let count = m.lw(IDLE_COUNTS);
        m.sw(FIRST_PORT, 0);
        if (count as i32) >= 0x96 {
            let records = m.lw(RECORDS_POINTER);
            reset_through_pointer(m, records);
        }
        m.sw(IDLE_COUNTS, 0);
    } else {
        m.sw(FIRST_PORT, 1);
    }
    if mask & 2 != 0 {
        let count = m.lw(IDLE_COUNTS + 4);
        m.sw(LAST_PORT, 1);
        if (count as i32) >= 0x96 {
            let records = m.lw(RECORDS_POINTER);
            reset_through_pointer(m, at(records, RECORD_BYTES));
        }
        m.sw(IDLE_COUNTS + 4, 0);
    } else {
        m.sw(LAST_PORT, 0);
    }
    m.sw(STATE, 1);
    previous
}

/// 8009BC60 _padStartCom's RAM effects (PadStartCom 8009CD30): the state
/// flag, both records reset (`[0x800B4688]`), the disabled-port counters
/// cleared. Its interrupt registration and I_STAT/I_MASK/root counter set-up
/// are the host's.
pub fn pad_start_com<M: PadMemory>(m: &mut M) {
    m.sw(STATE, 0);
    let records = m.lw(RECORDS_POINTER);
    reset_through_pointer(m, records);
    let records = m.lw(RECORDS_POINTER);
    reset_through_pointer(m, at(records, RECORD_BYTES));
    m.sw(IDLE_COUNTS + 4, 0);
    m.sw(IDLE_COUNTS, 0);
    m.sw(STATE, 1);
}

/// `[0x800B4688](record)`: PadInitDirect installs 8009DF30.
fn reset_through_pointer<M: PadMemory>(m: &mut M, record: u32) {
    if m.lw(RESET_RECORD) == 0x8009_df30 {
        reset_record(m, record);
    }
}
