//! Resident arena allocator over the game context (`[gp+0x2DC]`).
//! Context +0x18 is the arena base and +0x1C the bump pointer; the fixed limit
//! is 0x801FB700. Allocations are rounded up to words and never cleared.
use crate::machine::{Fault, Machine, SmallData};

pub const CONTEXT_POINTER: u32 = 0x2dc;
pub const ARENA_LIMIT: u32 = 0x801f_b700;

pub fn context(m: &mut Machine) -> u32 {
    m.gw(CONTEXT_POINTER)
}

/// 80026F70: advance the pointer even on overflow; overflow prints and
/// returns null. The native port records a fault instead of printing.
pub fn alloc(m: &mut Machine, bytes: u32) -> u32 {
    let size = (bytes.wrapping_add(3) >> 2) << 2;
    let ctx = context(m);
    let start = m.lw(ctx + 0x1c);
    let end = start.wrapping_add(size);
    m.sw(ctx + 0x1c, end);
    if ARENA_LIMIT < end {
        m.raise(Fault::ArenaExhausted {
            requested: bytes,
            end,
        });
        return 0;
    }
    start
}

/// 80026FD8: the address the next allocation of `bytes` would return,
/// without advancing; null (and a fault) if it would not fit.
pub fn peek(m: &mut Machine, bytes: u32) -> u32 {
    let size = (bytes.wrapping_add(3) >> 2) << 2;
    let ctx = context(m);
    let start = m.lw(ctx + 0x1c);
    let end = start.wrapping_add(size);
    if ARENA_LIMIT < end {
        m.raise(Fault::ArenaExhausted {
            requested: bytes,
            end,
        });
        return 0;
    }
    start
}

/// 80027098: set the bump pointer; below the arena base it prints and
/// returns null after storing it.
pub fn reset(m: &mut Machine, pointer: u32) -> u32 {
    let ctx = context(m);
    let base = m.lw(ctx + 0x18);
    m.sw(ctx + 0x1c, pointer);
    if pointer < base {
        m.raise(Fault::ArenaExhausted {
            requested: 0,
            end: pointer,
        });
        return 0;
    }
    pointer
}

/// 800270E4: bytes remaining below the limit.
pub fn remaining(m: &mut Machine) -> u32 {
    let ctx = context(m);
    ARENA_LIMIT.wrapping_sub(m.lw(ctx + 0x1c))
}

/// The allocator by original address (a0 in, v0 out), for callers that
/// reach it through `dispatch::call` (overlays, the test oracle).
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    Some(match target {
        0x8002_6f70 => alloc(m, args[0]),
        0x8002_6fd8 => peek(m, args[0]),
        0x8002_7098 => reset(m, args[0]),
        0x8002_70e4 => remaining(m),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::GP;
    #[test]
    fn rounds_advances_and_reports_overflow() {
        let mut m = Machine::default();
        m.sw(GP + CONTEXT_POINTER, 0x800b_d158);
        m.sw(0x800b_d158 + 0x18, 0x8010_0000);
        m.sw(0x800b_d158 + 0x1c, 0x8010_0000);
        assert_eq!(alloc(&mut m, 5), 0x8010_0000);
        assert_eq!(peek(&mut m, 0), 0x8010_0008);
        assert_eq!(alloc(&mut m, 0), 0x8010_0008);
        assert_eq!(remaining(&mut m), ARENA_LIMIT - 0x8010_0008);
        assert!(m.take_fault().is_ok());
        assert_eq!(alloc(&mut m, 0x20_0000), 0);
        assert_eq!(
            m.lw(0x800b_d158 + 0x1c),
            0x8030_0008,
            "pointer still advanced"
        );
        assert!(m.take_fault().is_err());
        assert_eq!(reset(&mut m, 0x8010_0004), 0x8010_0004);
        assert_eq!(reset(&mut m, 0x800f_0000), 0);
        assert!(m.take_fault().is_err());
    }
}
