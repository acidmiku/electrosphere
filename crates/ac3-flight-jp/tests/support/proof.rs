//! The shared capture-replay helpers of the JP tests (found by `jp-ov1a`;
//! the replay device and the OWNED table gathered from `jp-ov2a`,
//! `jp-ov2e`, `jp-ov2f` and `jp-res3`), and **device work**: the proof
//! that a patch-layer translation reproduces the original instructions on
//! a captured call that neither of them can reproduce.
//!
//! A replay ([`replay_machine`]) runs a captured call natively from its
//! entry snapshot with the [`Replay`] device (the BIOS events from the
//! kernel's event table, the CD drive and the VRAM reads as the capture
//! had them, the MDEC as the routine expects it) at [`run_sp`], and leaves
//! out of its comparison the RAM the BIOS, the interrupt handlers and the
//! devices own ([`OWNED`]) and the destinations of device transfers
//! ([`device_ranges`]).
//!
//! A replay can still miss its capture for reasons outside the routine:
//! work of the devices its callees reach (the CD, SPU and pad drivers and
//! their interrupt handlers), which the port answers with host calls and
//! does not model in RAM, or an interrupt that ran during the call in
//! Redux. Such a call passes as **device work** only with this proof
//! ([`device_work`]):
//! * from the same entry machine and device, the translation
//!   (`dispatch::call`) and the original instructions (the test oracle,
//!   [`oracle::run`], whose calls out run the same callees) leave the same
//!   RAM outside the guest stack below the entry `sp` and its argument
//!   slots, scratchpad, GTE, `sp`, first fault and, unless the routine
//!   models none, `v0` ([`compare`]);
//! * the call reached a device (made host calls);
//! * and [`DEVICE_WORK`] explains it (which device work the capture holds),
//!   or the original instructions' difference is confined to [`OWNED`] RAM
//!   (and what the caller leaves out of its comparison) with `v0`
//!   reproduced.
//!
//! The capture's difference is then the original instructions' too, with
//! the same answers to the same device calls. The proof covers the
//! routine's own instructions; its callees are validated by their own
//! captures. A call [`BLOCKED`] by a callee that is not yet the JP routine
//! is proven the same way, without the device ([`blocked`]).
#![allow(dead_code)]
use super::oracle;
use ac3_flight_jp::mainloop::pending_sdk as cd;
use ac3_flight_jp::snapshot::{self, Snapshot};
use ac3_flight_jp::{Device, Fault, HostCall, Machine, dispatch, host_ids, sdk};
use std::ops::Range;
use std::panic::{self, AssertUnwindSafe};

/// More host answers of a test, tried after the BIOS events: `(RAM, call)
/// -> Some(result)`.
pub type Answers = fn(&mut [u8], HostCall) -> Option<u32>;

/// The BIOS event calls, answered from the kernel's event table in RAM as
/// the BIOS does (table `[0x80000120]`, size `[0x80000124]`, 0x1C bytes per
/// event: class +0, status +4, spec +8, mode +0xC, handler +0x10; status 0
/// free, 0x1000 disabled, 0x2000 enabled, 0x4000 ready). By canonical ID:
/// * OpenEvent (0x800AD758) takes the first free block (status 0x1000)
///   and returns 0xF1000000 + its index, or -1;
/// * EnableEvent (0x800AD798) and DisableEvent (0x800AD7A8) set an open
///   event's status to 0x2000 / 0x1000, CloseEvent (0x800AD768) frees it;
///   each returns 1;
/// * TestEvent (0x800AD788) of a ready event makes it enabled and returns
///   1, else 0.
///
/// Other host calls go to [`Bios::extra`], else keep their defaults.
#[derive(Default)]
pub struct Bios {
    pub extra: Option<Answers>,
}

impl Bios {
    pub fn new() -> Self {
        Self { extra: None }
    }
    pub fn with(extra: Answers) -> Self {
        Self { extra: Some(extra) }
    }
}

impl Device for Bios {
    fn call(&mut self, ram: &mut [u8], _: &mut [u8], call: HostCall) -> Option<u32> {
        let at = |a: u32| (a & 0x1f_ffff) as usize;
        let word =
            |ram: &[u8], a: u32| u32::from_le_bytes(ram[at(a)..at(a) + 4].try_into().unwrap());
        let put = |ram: &mut [u8], a: u32, v: u32| {
            ram[at(a)..at(a) + 4].copy_from_slice(&v.to_le_bytes())
        };
        let table = word(ram, 0x120);
        let block = |event: u32| table.wrapping_add((event & 0xffff) * 0x1c);
        let [a0, a1, a2, a3] = call.args;
        match call.function {
            0x800a_d758 => {
                let count = word(ram, 0x124) / 0x1c;
                let Some(i) = (0..count).find(|&i| word(ram, block(i) + 4) == 0) else {
                    return Some(u32::MAX);
                };
                let b = block(i);
                put(ram, b, a0);
                put(ram, b + 4, 0x1000);
                put(ram, b + 8, a1);
                put(ram, b + 0xc, a2);
                put(ram, b + 0x10, a3);
                Some(0xf100_0000 | i)
            }
            0x800a_d798 | 0x800a_d7a8 | 0x800a_d768 => {
                let status = block(a0) + 4;
                if word(ram, status) != 0 {
                    let value = match call.function {
                        0x800a_d798 => 0x2000,
                        0x800a_d7a8 => 0x1000,
                        _ => 0,
                    };
                    put(ram, status, value);
                }
                Some(1)
            }
            0x800a_d788 => {
                let status = block(a0) + 4;
                if word(ram, status) != 0x4000 {
                    return Some(0);
                }
                put(ram, status, 0x2000);
                Some(1)
            }
            _ => self.extra.and_then(|extra| extra(ram, call)),
        }
    }
}

/// A snapshot's machine with the [`Bios`] device.
pub fn machine_of(snapshot: &Snapshot) -> Machine {
    let mut m = snapshot.machine();
    m.device = Some(Box::new(Bios::new()));
    m
}

/// RAM a captured call may change without the routine itself: the BIOS
/// kernel, the interrupt handlers (VBlank: libetc, libpad, the VSync
/// counter; the GPU DMA), the libgpu queue and DMA polling, and ClearImage's
/// GPU-status words (as the PAL tests allow, `ac3-flight` `tests/frame.rs`
/// and `tests/ov3.rs` OWNED; JP addresses through the re-addressed `sdk`
/// constants). Found by `jp-ov2a` on the JP captures; the pad words and
/// ClearImage's by `jp-ov2e` (`tests/ov2_p5.rs`) and `jp-res3`
/// (`tests/res_p3.rs` VBLANK_CONTEXT).
pub const OWNED: &[(u32, u32, &str)] = &[
    (0x8000_0000, 0x8001_0000, "BIOS kernel RAM"),
    (sdk::PAD_VSYNC_FLAG, sdk::PAD_VSYNC_FLAG + 4, "VBlank flag"),
    (
        sdk::VSYNC_LAST_HCOUNT,
        sdk::VSYNC_LAST_COUNT + 4,
        "VSync root-counter reading",
    ),
    (
        sdk::INTERRUPT_MANAGER,
        0x800b_60c4,
        "libetc interrupt context and stack",
    ),
    (sdk::VSYNC_COUNTER, sdk::VSYNC_COUNTER + 4, "VSync counter"),
    (
        sdk::DMA_CALLBACKS + 8,
        sdk::DMA_CALLBACKS + 0xc,
        "GPU DMA callback slot",
    ),
    (
        sdk::GPU_QUEUE_WRITE,
        sdk::GPU_SAVED_MASK + 4,
        "libgpu queue indexes, saved interrupt mask",
    ),
    (
        sdk::GPU_TIMEOUT_DEADLINE,
        sdk::GPU_TIMEOUT_POLLS + 4,
        "libgpu timeout and DMA poll counter",
    ),
    (
        0x800c_7df4,
        0x800c_7df8,
        "libpad port 2 record +0x4C (0x800C7DA8 + 0x4C): the failed-exchange count of \
         _dirFailAuto, counted by the VBlank pad handler",
    ),
    (
        0x800c_7e98,
        0x800c_7e9c,
        "libpad's root counter 2 wait start (WAIT_START), which depends on cycle timing",
    ),
    (
        0x800c_a310,
        0x800c_a314,
        "ClearImage draw mode (GPU status; GPU_CLEAR_PACKET + 8)",
    ),
    (
        0x800c_a31c,
        0x800c_a320,
        "ClearImage draw mode (GPU status; GPU_CLEAR_PACKET + 0x14)",
    ),
    (
        0x800c_a334,
        0x800c_a340,
        "ClearImage drawing area (GPU status; GPU_CLEAR_RESTORE + 4)",
    ),
    (0x800c_a458, 0x800c_bc58, "libgpu queue entries"),
    // The CD stream's interrupt work (found by `jp-ends` in the credits' captures,
    // ends7-002 #77 8011CB5C and #79 8011E474: drawing routines without a host call
    // that a stream sector interrupted in Redux; the original instructions from the
    // same entries leave these words as the translation does). The CdReadyCallback
    // of streams (80029D70, then 80029BD8 per sector) and libcd's callbacks run
    // under the CD interrupt; the port runs that work at the end of a main-loop
    // pass (`Game::cd_service`), never inside a routine.
    (
        0x800c_13b0,
        0x800c_13b4,
        "the CD monitor [gp+0x34] (0x800C1388) +0x28: the last CD callback result, kept by \
         the stream, data and sync callbacks under the CD interrupt",
    ),
    (
        0x800c_13cc,
        0x800c_13d0,
        "the CD driver state [gp+0x38] (0x800C13B8) +0x14: a stream's sectors left, set per \
         data sector by 80029BD8 under the CD interrupt",
    ),
    (
        0x800c_13dc,
        0x800c_13e0,
        "the CD driver state +0x24: the last sector read, set per sector under the CD interrupt",
    ),
    (
        0x800c_13fc,
        0x800c_1400,
        "the CD driver state +0x44: the VBlank time of the last data sector, set per sector \
         under the CD interrupt",
    ),
    (
        0x800c_bf68,
        0x800c_bf88,
        "libc printf's TTY line buffer (PAL 800C9D40; printf is a host call)",
    ),
];

/// Whether [`OWNED`] holds `address` (any segment).
pub fn owned(address: u32) -> bool {
    let address = address | 0x8000_0000;
    OWNED
        .iter()
        .any(|(start, end, _)| (*start..*end).contains(&address))
}

/// RAM whose content a device delivered during a run (physical
/// addresses): StoreImage destinations (VRAM -> RAM, `[rect, pixels]`;
/// found by `jp-ov2f`: 80101EE4 reading a palette back through 800FF390)
/// and the MDEC output DMA (`[buffer, words]`, decoded pixels; `jp-ov2e`).
/// The captures carry no VRAM or MDEC state, so those bytes are left out
/// of a replay's comparison.
pub fn device_ranges(m: &Machine) -> Vec<Range<u32>> {
    m.host_calls
        .iter()
        .filter_map(|call| {
            let [a0, a1, ..] = call.args;
            let (start, bytes) = match call.function {
                host_ids::STORE_IMAGE => {
                    let (w, h) = (
                        m.peek_half(a0.wrapping_add(4)),
                        m.peek_half(a0.wrapping_add(6)),
                    );
                    (a1, u32::from(w) * u32::from(h) * 2)
                }
                host_ids::MDEC_OUT => (a0, a1.wrapping_mul(4)),
                _ => return None,
            };
            let start = start & 0x1f_ffff;
            Some(start..start + bytes)
        })
        .collect()
}

/// The `sp` the native host runs interrupt work at: a call captured on
/// libetc's interrupt stack (an interrupt callback, below the machine's
/// stack region, where native frames fault) runs from here (PAL
/// `tests/cdstream.rs` HOST_IRQ_SP; `jp-ov2e`, `jp-res3`).
pub const HOST_IRQ_SP: u32 = 0x801f_f000;

/// The `sp` a captured call runs at: its own, or [`HOST_IRQ_SP`] for one
/// captured on the interrupt stack.
pub fn run_sp(sp: u32) -> u32 {
    if sp < ac3_flight_jp::machine::STACK_LIMIT {
        HOST_IRQ_SP
    } else {
        sp
    }
}

/// Routines whose captures ran with the MDEC output DMA still in progress
/// when DecDCTout returned: the MDEC callback 800FCB44 starts the next
/// slice's DMA, whose completion interrupt calls it again only after it
/// returned; the host model completes it at once unless the device says
/// otherwise (`jp-ov2e`, `tests/ov2_p5.rs` Recorder). (module, address.)
pub const MDEC_PENDING: &[(u32, u32)] = &[(2, 0x800f_cb44)];

/// The device of a capture replay and of its proofs: the BIOS events
/// ([`Bios`]); the original's CD drive and VRAM reads, replayed from the
/// capture pair (test fixture data, as `jp-res3`'s CaptureDrive in
/// `tests/res_p3.rs` and the PAL `Replay` of `tests/cdstream.rs`): a
/// blocking command polls as often as the original did (`0x800C7EDC` of
/// the return snapshot), the interrupt handler then reads its acknowledge
/// with the status byte the original kept (`0x800C7EC0`), one per command
/// sent, as the drive answered each of a call's commands (CdInit sends
/// Getstat, Init and Demute in one call); a command with a second response
/// (libcd's table 0x800B6320) completes at the handler's next run after
/// the one that took the acknowledge (when the routine waits for it, as
/// CdInit's next command does), not in the same run; a sector transfer
/// delivers the bytes the original's DMA left at its destination
/// (a destination in the host's interrupt frame maps to the original's
/// frame), and StoreImage the VRAM pixels the original read; and the MDEC
/// output pending for [`MDEC_PENDING`] routines.
pub struct Replay {
    bios: Bios,
    exit: Vec<u8>,
    polls: u32,
    status: u8,
    acknowledge: Option<u8>,
    complete: Option<u8>,
    /// The handler run that took the acknowledge has not ended yet.
    acknowledged: bool,
    frame_shift: u32,
    mdec_pending: bool,
}

impl Replay {
    /// The device of the captured call `entry` .. `exit` of a routine in
    /// `module` (0 resident).
    pub fn new(entry: &Snapshot, exit: &Snapshot, module: u32) -> Self {
        let at = |a: u32| (a & 0x1f_ffff) as usize;
        let polls = u32::from_le_bytes(
            exit.ram[at(0x800c_7edc)..at(0x800c_7edc) + 4]
                .try_into()
                .unwrap(),
        );
        let sp = entry.gpr(29);
        Self {
            bios: Bios::new(),
            exit: exit.ram.clone(),
            polls: polls.max(1),
            status: exit.ram[at(0x800c_7ec0)],
            acknowledge: Some(exit.ram[at(0x800c_7ec0)]),
            complete: None,
            acknowledged: false,
            frame_shift: sp.wrapping_sub(run_sp(sp)),
            mdec_pending: MDEC_PENDING.contains(&(module, entry.info.callee)),
        }
    }
}

impl Device for Replay {
    fn call(&mut self, ram: &mut [u8], scratch: &mut [u8], call: HostCall) -> Option<u32> {
        if let Some(result) = self.bios.call(ram, scratch, call) {
            return Some(result);
        }
        let [a0, a1, ..] = call.args;
        match call.function {
            cd::CD_COMMAND => {
                // libcd's per-command "a second response follows" words.
                let second = 0x800b_6320u32.wrapping_add((a0 & 0xff) << 2);
                let at = (second & 0x1f_ffff) as usize;
                let follows = ram[at..at + 4].iter().any(|&b| b != 0);
                self.acknowledge = Some(self.status);
                self.complete = follows.then_some(self.status);
                self.acknowledged = false;
                Some(self.polls)
            }
            cd::CD_INTERRUPT => {
                if a0 == cd::SITE_HANDLER {
                    let response = match self.acknowledge.take() {
                        Some(status) => {
                            self.acknowledged = true;
                            Some((3, status))
                        }
                        // The handler run of the acknowledge ends here.
                        None if self.acknowledged => {
                            self.acknowledged = false;
                            None
                        }
                        None => self.complete.take().map(|status| (2, status)),
                    };
                    if let Some((kind, status)) = response {
                        let mut put = |offset: u32, byte: u8| {
                            ram[(a1.wrapping_add(offset) & 0x1f_ffff) as usize] = byte;
                        };
                        put(0x10, kind);
                        put(0x11, 1);
                        put(0x18, status);
                    }
                }
                Some(0)
            }
            cd::CD_GET_SECTOR | cd::CD_GET_SECTOR2 => {
                let source = if (0x801f_c000..0x8020_0000).contains(&a0) {
                    a0.wrapping_add(self.frame_shift)
                } else {
                    a0
                };
                for i in 0..a1.wrapping_mul(4) {
                    let to = (a0.wrapping_add(i) & 0x1f_ffff) as usize;
                    let from = (source.wrapping_add(i) & 0x1f_ffff) as usize;
                    ram[to] = self.exit[from];
                }
                Some(0)
            }
            host_ids::STORE_IMAGE => {
                let rect = (a0 & 0x1f_ffff) as usize;
                let half = |at: usize| u32::from(u16::from_le_bytes([ram[at], ram[at + 1]]));
                let bytes = half(rect + 4) * half(rect + 6) * 2;
                for i in 0..bytes {
                    let at = (a1.wrapping_add(i) & 0x1f_ffff) as usize;
                    ram[at] = self.exit[at];
                }
                Some(0)
            }
            host_ids::MDEC_OUT if self.mdec_pending => Some(0),
            _ => None,
        }
    }
}

/// The machine a captured call (`entry` .. `exit`, of a routine in
/// `module`, 0 resident) replays from: the entry snapshot at [`run_sp`]
/// with the [`Replay`] device.
pub fn replay_machine(entry: &Snapshot, exit: &Snapshot, module: u32) -> Machine {
    let mut m = entry.machine();
    m.sp = run_sp(entry.gpr(29));
    m.device = Some(Box::new(Replay::new(entry, exit, module)));
    m
}

/// The calls known to pass as device work: (module, 0 resident; JP
/// address; what the capture holds that no replay reproduces), for the
/// differences outside [`OWNED`].
pub const DEVICE_WORK: &[(u32, u32, &str)] = &[
    (
        0,
        0x8002_5b00,
        "the SDK drivers' RAM of the device start-up: SsUtReverbOff's libspu reverb state \
         (0x800B474F..0x800B47DD; a host call in the port), the CD DMA callback word \
         (0x800B6198), libgpu's display-mask state (0x800BBA9C), libcd's command deadline \
         (0x800C7ED8, VSync(-1) + 0x3C0: the host's VBlank count) and the driver words \
         0x800C7BF8, 0x800CBC78; `tests/res_p2.rs` replays the call with the JP disc drive \
         and reproduces the rest",
    ),
    (
        0,
        0x8002_ae0c,
        "the disc's ACE3 files: CdSearchFile of the four files (its ISO9660 path table and \
         directory sectors) and the CdRead of both tables (BPB, SPB) find and read them on the \
         disc in Redux, where the replay has no disc, so the search fails (v0 -1, the archive \
         status [gp+0x34] + 0xC = 5) and nothing is read; `tests/res_p2.rs` replays the call \
         with the JP disc and reproduces it",
    ),
    (
        1,
        0x800c_f06c,
        "a CD sector of the menu's music stream (0x80112DA4.., 0x800 bytes) and libcd's state \
         (0x800B6028.., 0x80110DE8) arrive under the CD interrupt during the call",
    ),
    (
        1,
        0x800d_a340,
        "a CD sector of the menu's music stream (0x80112DA4.., 0x800 bytes) arrives under the CD \
         interrupt during the call; the routine's v0 (the audio level) is computed from that \
         sector's samples",
    ),
    (
        1,
        0x800d_a2a8,
        "a CD sector of the menu's music stream (0x80112DA4.., 0x800 bytes) arrives under the CD \
         interrupt during the call",
    ),
    (
        2,
        0x800e_5100,
        "the memory card's save file: BIOS open (0x800AD808) succeeds in Redux (the descriptor at \
         [0x80112DF0] + 0x3C, the transfer flag + 0x336), where the replay has no card file (open \
         -1, a retry counted at + 0x358); `tests/ov2_p3.rs` replays it with the original's answers",
    ),
    (
        2,
        0x800e_8c50,
        "800E5100's memory card save file (see there): BIOS open succeeds in Redux, the replay has \
         no card file",
    ),
];

/// The [`DEVICE_WORK`] explanation of a call.
pub fn explanation(module: u32, address: u32) -> Option<&'static str> {
    DEVICE_WORK
        .iter()
        .find(|(m, a, _)| *m == module && *a == address)
        .map(|(_, _, why)| *why)
}

/// Captured calls that cannot match yet because a callee outside the
/// routine is not the JP routine: (module, 0 resident; JP address; the
/// callee and its state). Each passes as **blocked** only when its
/// translation equals the original instructions from the entry
/// ([`blocked`]), which run that callee the same way. From `jp-ov2e`
/// (`tests/ov2_p5.rs` BLOCKED) and `jp-ov1a` (the callee found by forcing
/// the fallback to interpret the game code, `Fallback::force`, and
/// bisecting the routines left native). An entry whose captures pass is
/// reported by the global test, to be removed.
pub const BLOCKED: &[(u32, u32, &str)] = &[(
    2,
    0x800f_c488,
    "v0 only: scene_camera returns SetGeomScreen's v0, a leftover register in the original \
         (0x8013686C) and a host-call 0 natively; RAM equals the original (80077880 is patched \
         by `jp-res2`)",
)];

/// The [`BLOCKED`] reason of a call.
pub fn blocked_by(module: u32, address: u32) -> Option<&'static str> {
    BLOCKED
        .iter()
        .find(|(m, a, _)| *m == module && *a == address)
        .map(|(_, _, why)| *why)
}

fn panic_text(e: &(dyn std::any::Any + Send)) -> String {
    e.downcast_ref::<String>()
        .cloned()
        .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default()
}

/// A fresh device for each run.
pub type Devices<'a> = &'a dyn Fn() -> Box<dyn Device>;

/// One run of the routine at `range` from a clone of `base` with a fresh
/// device.
pub struct Run {
    pub machine: Machine,
    pub v0: u32,
    pub fault: Option<Fault>,
    /// Host calls the run made.
    pub host_calls: usize,
}

/// Runs the original instructions (`original`, the test oracle) or the
/// translation (`dispatch::call`) of the routine `range` with `args`.
pub fn run(
    base: &Machine,
    range: (u32, u32),
    args: [u32; 4],
    original: bool,
    device: Devices,
) -> Result<Run, String> {
    let mut m = base.clone();
    m.device = Some(device());
    let v0 = panic::catch_unwind(AssertUnwindSafe(|| {
        if original {
            oracle::run(&mut m, range.0, range.1, args)
        } else {
            dispatch::call(&mut m, range.0, args)
        }
    }))
    .map_err(|e| {
        let who = if original { "original" } else { "translation" };
        format!("{who} panicked: {}", panic_text(&*e))
    })?;
    let fault = m.take_fault().err();
    let host_calls = m.host_calls.len() - base.host_calls.len();
    Ok(Run {
        machine: m,
        v0,
        fault,
        host_calls,
    })
}

/// Where two runs from `base` differ: RAM outside the guest stack below
/// `base.sp` and the four argument slots at `base.sp` (the caller's frame
/// space for the callee's `a0..a3`, which an original may store there and
/// a translation need not), scratchpad, GTE, `sp`, first fault, and `v0`
/// unless `void`. None when they agree.
pub fn differ(base: &Machine, a: &Run, b: &Run, void: bool) -> Option<String> {
    if a.fault != b.fault {
        return Some(format!("faults {:?} / {:?}", a.fault, b.fault));
    }
    let dead = snapshot::STACK_START..((base.sp & 0x1f_ffff) + 0x10) as usize;
    let (x, y) = (&a.machine, &b.machine);
    let ram: Vec<u32> = (0..0x20_0000usize)
        .filter(|&i| !dead.contains(&i) && x.ram()[i] != y.ram()[i])
        .map(|i| 0x8000_0000 + i as u32)
        .collect();
    if !ram.is_empty() || x.scratch() != y.scratch() || x.gte != y.gte || x.sp != y.sp {
        return Some(format!(
            "ram {} {:08x?} scratch equal {} gte equal {} sp {:x}/{:x}",
            ram.len(),
            &ram[..ram.len().min(12)],
            x.scratch() == y.scratch(),
            x.gte == y.gte,
            x.sp,
            y.sp
        ));
    }
    if !void && a.v0 != b.v0 {
        return Some(format!("v0 {:#x} / {:#x}", a.v0, b.v0));
    }
    None
}

/// The original instructions and the translation of `range` from `base`
/// must agree ([`differ`], the original first). Ok(the original's run).
pub fn compare(
    base: &Machine,
    range: (u32, u32),
    args: [u32; 4],
    void: bool,
    device: Devices,
) -> Result<Run, String> {
    let original = run(base, range, args, true, device)?;
    let native = run(base, range, args, false, device)?;
    match differ(base, &original, &native, void) {
        Some(d) => Err(format!("original / translation: {d}")),
        None => Ok(original),
    }
}

/// A captured call that its replay does not reproduce, for [`device_work`]
/// and [`blocked`].
pub struct Call<'a> {
    pub entry: &'a Snapshot,
    pub exit: &'a Snapshot,
    /// The machine the replay started from ([`replay_machine`], or the
    /// test's own).
    pub base: &'a Machine,
    /// The module of the routine (0 resident) and its extent.
    pub module: u32,
    pub range: (u32, u32),
    /// Whether the translation models no `v0`.
    pub void: bool,
}

impl Call<'_> {
    fn args(&self) -> [u32; 4] {
        let e = self.entry;
        [e.gpr(4), e.gpr(5), e.gpr(6), e.gpr(7)]
    }
}

/// The device-work proof for `call`: Ok(the explanation, and how the
/// original instructions miss the capture as well) when the translation
/// equals the original instructions from the same machine and device, the
/// call reached a device and [`DEVICE_WORK`] explains it, or the original
/// instructions' difference lies in [`OWNED`] RAM and the run's
/// [`device_ranges`], with `v0` reproduced; Err(why not).
pub fn device_work(call: &Call, device: Devices) -> Result<String, String> {
    let original = compare(call.base, call.range, call.args(), call.void, device)?;
    if original.host_calls == 0 {
        return Err("no device reached (no host calls)".into());
    }
    let d = snapshot::compare(&original.machine, call.exit);
    let ranges = device_ranges(&original.machine);
    let ignored = |a: u32| ranges.iter().any(|r| r.contains(&(a & 0x1f_ffff)));
    let v0_equal = call.void || original.v0 == call.exit.gpr(2);
    let v0 = if v0_equal {
        String::new()
    } else {
        format!("; v0 {:#x}, captured {:#x}", original.v0, call.exit.gpr(2))
    };
    let miss = format!(
        "the original instructions miss the capture as well: {}{v0}",
        d.summary()
    );
    let in_owned = v0_equal
        && d.scratch.is_empty()
        && d.gte_data.is_empty()
        && d.gte_control.is_empty()
        && d.ram.iter().all(|&a| owned(a) || ignored(a));
    match explanation(call.module, call.entry.info.callee) {
        Some(why) => Ok(format!("{why}; {miss}")),
        None if in_owned => {
            let mut what: Vec<&str> = Vec::new();
            for &a in d.ram.iter().filter(|&&a| owned(a)) {
                let a = a | 0x8000_0000;
                if let Some((_, _, w)) = OWNED.iter().find(|(s, e, _)| (*s..*e).contains(&a))
                    && !what.contains(w)
                {
                    what.push(w);
                }
            }
            Ok(format!(
                "only RAM the BIOS, the handlers and the devices own: {}; {miss}",
                what.join("; ")
            ))
        }
        None => Err(format!(
            "the translation equals the original instructions ({miss}), but DEVICE_WORK does not \
             explain the call"
        )),
    }
}

/// The proof for a [`BLOCKED`] call: Ok(the reason) when the translation
/// equals the original instructions from the same machine and device
/// (which run the blocking callee the same way); Err(why not).
pub fn blocked(call: &Call, device: Devices) -> Result<String, String> {
    let why = blocked_by(call.module, call.entry.info.callee).ok_or("not BLOCKED")?;
    compare(call.base, call.range, call.args(), call.void, device)?;
    Ok(format!(
        "{why}; the translation equals the original instructions from the entry"
    ))
}
