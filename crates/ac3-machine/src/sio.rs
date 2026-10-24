//! The controller port hardware behind libpad, as device models: SIO0 with
//! its two ports ([`Sio`]), the controllers on them ([`Controller`]: a
//! digital pad or a DualShock, as PCSX-Redux emulates them), and the RAM
//! view libpad's interrupt-time routines run on ([`Bus`]: main RAM plus the
//! SIO0, interrupt and root counter 2 registers of a [`Sio`]).
//!
//! Version-independent: only hardware register addresses appear here. The
//! translated libpad routines (the VBlank handler and what it runs, at a
//! version's addresses) live in the version crate (`ac3_flight::mainloop::libpad`).

// ---- I/O registers -------------------------------------------------------------------

/// SIO0 data/status/mode/control/baud, the interrupt status and mask, and
/// root counter 2 count/mode/target.
pub const SIO_DATA: u32 = 0x1f80_1040;
pub const SIO_STAT: u32 = 0x1f80_1044;
pub const SIO_MODE: u32 = 0x1f80_1048;
pub const SIO_CTRL: u32 = 0x1f80_104a;
pub const SIO_BAUD: u32 = 0x1f80_104e;
pub const I_STAT: u32 = 0x1f80_1070;
pub const I_MASK: u32 = 0x1f80_1074;
pub const RCNT2_COUNT: u32 = 0x1f80_1120;
pub const RCNT2_MODE: u32 = 0x1f80_1124;
pub const RCNT2_TARGET: u32 = 0x1f80_1128;

/// libpad's RAM view: main RAM plus the SIO0, interrupt and root counter 2
/// registers of [`Sio`].
pub struct Bus<'a> {
    pub ram: &'a mut [u8],
    pub sio: &'a mut Sio,
}

#[inline]
fn io(address: u32) -> bool {
    address & 0x1fff_f000 == 0x1f80_1000
}

impl Bus<'_> {
    #[inline]
    fn index(address: u32) -> usize {
        (address & 0x1f_ffff) as usize
    }
    #[inline]
    pub fn lbu(&mut self, address: u32) -> u32 {
        if io(address) {
            return self.sio.read(address) & 0xff;
        }
        u32::from(self.ram[Self::index(address)])
    }
    #[inline]
    pub fn lhu(&mut self, address: u32) -> u32 {
        if io(address) {
            return self.sio.read(address) & 0xffff;
        }
        let at = Self::index(address);
        u32::from(u16::from_le_bytes([self.ram[at], self.ram[at + 1]]))
    }
    #[inline]
    pub fn lw(&mut self, address: u32) -> u32 {
        if io(address) {
            return self.sio.read(address);
        }
        let at = Self::index(address);
        u32::from_le_bytes([
            self.ram[at],
            self.ram[at + 1],
            self.ram[at + 2],
            self.ram[at + 3],
        ])
    }
    #[inline]
    pub fn sb(&mut self, address: u32, value: u32) {
        if io(address) {
            return self.sio.write(address, value & 0xff);
        }
        self.ram[Self::index(address)] = value as u8;
    }
    #[inline]
    pub fn sh(&mut self, address: u32, value: u32) {
        if io(address) {
            return self.sio.write(address, value & 0xffff);
        }
        let at = Self::index(address);
        self.ram[at..at + 2].copy_from_slice(&(value as u16).to_le_bytes());
    }
    #[inline]
    pub fn sw(&mut self, address: u32, value: u32) {
        if io(address) {
            return self.sio.write(address, value);
        }
        let at = Self::index(address);
        self.ram[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }
}

/// Memory of libpad's game-time routines: a [`Bus`] or the machine.
pub trait PadMemory {
    fn lbu(&mut self, address: u32) -> u32;
    fn lhu(&mut self, address: u32) -> u32;
    fn lw(&mut self, address: u32) -> u32;
    fn sb(&mut self, address: u32, value: u32);
    fn sh(&mut self, address: u32, value: u32);
    fn sw(&mut self, address: u32, value: u32);
}

impl PadMemory for Bus<'_> {
    #[inline]
    fn lbu(&mut self, address: u32) -> u32 {
        Bus::lbu(self, address)
    }
    #[inline]
    fn lhu(&mut self, address: u32) -> u32 {
        Bus::lhu(self, address)
    }
    #[inline]
    fn lw(&mut self, address: u32) -> u32 {
        Bus::lw(self, address)
    }
    #[inline]
    fn sb(&mut self, address: u32, value: u32) {
        Bus::sb(self, address, value)
    }
    #[inline]
    fn sh(&mut self, address: u32, value: u32) {
        Bus::sh(self, address, value)
    }
    #[inline]
    fn sw(&mut self, address: u32, value: u32) {
        Bus::sw(self, address, value)
    }
}

impl PadMemory for crate::machine::Machine {
    #[inline]
    fn lbu(&mut self, address: u32) -> u32 {
        u32::from(crate::machine::Machine::lbu(self, address))
    }
    #[inline]
    fn lhu(&mut self, address: u32) -> u32 {
        u32::from(crate::machine::Machine::lhu(self, address))
    }
    #[inline]
    fn lw(&mut self, address: u32) -> u32 {
        crate::machine::Machine::lw(self, address)
    }
    #[inline]
    fn sb(&mut self, address: u32, value: u32) {
        crate::machine::Machine::sb(self, address, value as u8)
    }
    #[inline]
    fn sh(&mut self, address: u32, value: u32) {
        crate::machine::Machine::sh(self, address, value as u16)
    }
    #[inline]
    fn sw(&mut self, address: u32, value: u32) {
        crate::machine::Machine::sw(self, address, value)
    }
}

// ---- the port hardware and the controllers -------------------------------------------

/// A controller on a port, answering the PlayStation controller protocol
/// as PCSX-Redux's digital pad and DualShock do.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Controller {
    pub kind: PadKind,
    /// Held buttons, libpad's active-high order (bit 0 SELECT .. bit 15
    /// SQUARE); the controller sends them inverted.
    pub buttons: u16,
    /// Right X/Y and left X/Y stick positions (DualShock).
    pub sticks: [u8; 4],
    /// DualShock analog mode (ID 0x73), configuration mode (ID 0xF3).
    pub analog: bool,
    pub config: bool,
    /// Actuator mapping of command 0x4D: the actuator each data byte of a
    /// poll drives (0 the small motor, 1 the large one, 0xFF none).
    pub motors: [u8; 6],
    /// The data bytes the console sent with the last completed poll
    /// (command 0x42, bytes 3..8; 0 past the packet): the actuator levels,
    /// through `motors`. Host output only; the exchange never reads it.
    pub actuators: [u8; 6],
    // Exchange in progress: bytes received so far and the command.
    received: Vec<u8>,
    command: u8,
    /// Whether this exchange entered configuration mode (its packet).
    packet_config: bool,
    packet_analog: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PadKind {
    Digital,
    DualShock,
}

impl Controller {
    pub fn digital() -> Self {
        Self::new(PadKind::Digital, false)
    }
    pub fn dualshock(analog: bool) -> Self {
        Self::new(PadKind::DualShock, analog)
    }
    fn new(kind: PadKind, analog: bool) -> Self {
        Self {
            kind,
            buttons: 0,
            sticks: [0x80; 4],
            analog,
            config: false,
            motors: [0xff; 6],
            actuators: [0; 6],
            received: Vec::new(),
            command: 0,
            packet_config: false,
            packet_analog: false,
        }
    }

    /// End of a selection: the next byte starts a new exchange.
    fn deselect(&mut self) {
        self.received.clear();
    }

    /// Data bytes (after ID and 0x5A) of the packet answering `command`.
    fn data(&self) -> Vec<u8> {
        let [lo, hi] = (!self.buttons).to_le_bytes();
        let param = self.received.get(3).copied().unwrap_or(0);
        if !self.packet_config {
            let mut data = vec![lo, hi];
            if self.packet_analog {
                data.extend_from_slice(&self.sticks);
            }
            return data;
        }
        match self.command {
            0x42 => vec![
                lo,
                hi,
                self.sticks[0],
                self.sticks[1],
                self.sticks[2],
                self.sticks[3],
            ],
            0x45 => vec![0x01, 0x02, u8::from(self.analog), 0x02, 0x01, 0x00],
            0x46 => match param {
                0 => vec![0x00, 0x00, 0x01, 0x02, 0x00, 0x0a],
                1 => vec![0x00, 0x00, 0x01, 0x01, 0x01, 0x14],
                _ => vec![0; 6],
            },
            0x47 => vec![0x00, 0x00, 0x02, 0x00, 0x01, 0x00],
            0x4c => match param {
                0 => vec![0x00, 0x00, 0x00, 0x04, 0x00, 0x00],
                1 => vec![0x00, 0x00, 0x00, 0x07, 0x00, 0x00],
                _ => vec![0; 6],
            },
            0x4d => self.motors.to_vec(),
            _ => vec![0; 6],
        }
    }

    fn id(&self) -> u8 {
        if self.packet_config {
            0xf3
        } else if self.packet_analog {
            0x73
        } else {
            0x41
        }
    }

    /// One byte each way; returns the controller's byte and whether it
    /// acknowledges (asks for the next one).
    fn exchange(&mut self, byte: u8) -> (u8, bool) {
        let index = self.received.len();
        if index == 0 {
            if byte != 0x01 {
                return (0xff, false);
            }
            self.received.push(byte);
            return (0xff, true);
        }
        if index == 1 {
            self.command = byte;
            let dualshock = self.kind == PadKind::DualShock;
            self.packet_config = dualshock && self.config;
            self.packet_analog = dualshock && self.analog;
            // A digital pad answers only the poll; a DualShock also 0x43, and
            // in configuration mode every command 0x40..0x4F.
            let accepted = match (dualshock, self.packet_config) {
                (false, _) => byte == 0x42,
                (true, false) => byte == 0x42 || byte == 0x43,
                (true, true) => byte & 0xf0 == 0x40,
            };
            self.received.push(byte);
            if !accepted {
                self.command = 0;
            }
            return (self.id(), accepted);
        }
        if self.command == 0 {
            // A refused command: nothing more is driven.
            self.received.push(byte);
            return (0xff, false);
        }
        let response = match index {
            2 => 0x5a,
            _ => {
                let data = self.data();
                match data.get(index - 3) {
                    Some(&value) => value,
                    None => {
                        // Past the packet: nothing is driven.
                        self.received.push(byte);
                        return (0xff, false);
                    }
                }
            }
        };
        self.received.push(byte);
        let length = 3 + self.data().len();
        let last = index + 1 == length;
        if last {
            self.finish();
        }
        (response, !last)
    }

    /// Effects of a completed packet (DualShock configuration commands).
    fn finish(&mut self) {
        if self.kind != PadKind::DualShock {
            return;
        }
        let param = |i: usize| self.received.get(i).copied().unwrap_or(0);
        match (self.packet_config, self.command) {
            (_, 0x42) => {
                for (i, level) in self.actuators.iter_mut().enumerate() {
                    *level = param(3 + i);
                }
            }
            (false, 0x43) => self.config = param(3) == 1,
            (true, 0x43) => self.config = param(3) == 1,
            (true, 0x44) => self.analog = param(3) == 1,
            (true, 0x4d) => {
                for (i, motor) in self.motors.iter_mut().enumerate() {
                    *motor = param(3 + i);
                }
            }
            _ => {}
        }
    }
}

/// SIO0 with its two ports, the SIO interrupt bit of I_STAT and root
/// counter 2 (advancing at every read of its count). Serialized whole by
/// save states (the controllers and the exchange in progress).
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Sio {
    pub ports: [Option<Controller>; 2],
    ctrl: u32,
    mode: u32,
    baud: u32,
    rx: Option<u8>,
    /// SIO_STAT bit 9 and I_STAT bit 7 (controller acknowledge).
    stat_irq: bool,
    i_stat_irq: bool,
    /// The acknowledge of the byte last sent: it arrives once time has
    /// passed (a root counter read) and the status is next read, after the
    /// acknowledge clear that follows each byte.
    ack_pending: bool,
    since_write: u32,
    i_mask: u32,
    count: u32,
    /// First unexpected register access, if any.
    pub error: Option<String>,
}

/// Root counter 2 target (the game's SetRCnt(RCntCNT2, 0x1E00, ..)).
const RCNT2_PERIOD: u32 = 0x1e00;

impl Default for Sio {
    fn default() -> Self {
        Self::new([Some(Controller::digital()), None])
    }
}

impl Sio {
    pub fn new(ports: [Option<Controller>; 2]) -> Self {
        Self {
            ports,
            ctrl: 0,
            mode: 0,
            baud: 0,
            rx: None,
            stat_irq: false,
            i_stat_irq: false,
            ack_pending: false,
            since_write: 0,
            i_mask: 0,
            count: 0,
            error: None,
        }
    }

    fn selected(&self) -> Option<usize> {
        (self.ctrl & 2 != 0).then_some(usize::from(self.ctrl & 0x2000 != 0))
    }

    fn deselect_all(&mut self) {
        for controller in self.ports.iter_mut().flatten() {
            controller.deselect();
        }
    }

    /// A pending acknowledge becomes visible.
    fn arrive(&mut self) {
        if self.ack_pending && self.since_write > 0 {
            self.ack_pending = false;
            self.i_stat_irq = true;
            if self.ctrl & 0x1000 != 0 {
                self.stat_irq = true;
            }
        }
    }

    fn read(&mut self, address: u32) -> u32 {
        let address = address & 0x1fff_ffff;
        if address == SIO_STAT || address == I_STAT {
            self.arrive();
        }
        match address {
            SIO_DATA => self.rx.take().map_or(0xff, u32::from),
            SIO_STAT => 5 | (u32::from(self.rx.is_some()) << 1) | (u32::from(self.stat_irq) << 9),
            SIO_MODE => self.mode,
            SIO_CTRL => self.ctrl,
            SIO_BAUD => self.baud,
            I_STAT => u32::from(self.i_stat_irq) << 7,
            I_MASK => self.i_mask,
            RCNT2_COUNT => {
                let count = self.count;
                self.count = (self.count + 1) % RCNT2_PERIOD;
                self.since_write = self.since_write.saturating_add(1);
                count
            }
            // Clock source system clock / 8 (bit 9).
            RCNT2_MODE => 0x0200,
            RCNT2_TARGET => RCNT2_PERIOD,
            other => {
                self.error
                    .get_or_insert(format!("libpad read of {other:08x}"));
                0
            }
        }
    }

    fn write(&mut self, address: u32, value: u32) {
        match address & 0x1fff_ffff {
            SIO_DATA => {
                let (response, ack) = match self.selected() {
                    Some(port) if self.ctrl & 1 != 0 => match &mut self.ports[port] {
                        Some(controller) => controller.exchange(value as u8),
                        None => (0xff, false),
                    },
                    _ => (0xff, false),
                };
                self.rx = Some(response);
                self.ack_pending = ack;
                self.since_write = 0;
            }
            SIO_MODE => self.mode = value,
            SIO_CTRL => {
                let before = self.selected();
                if value & 0x40 != 0 {
                    self.rx = None;
                    self.stat_irq = false;
                    self.ack_pending = false;
                }
                if value & 0x10 != 0 {
                    self.stat_irq = false;
                }
                self.ctrl = value & !0x50;
                if self.selected() != before || value & 0x40 != 0 {
                    self.ack_pending = false;
                    self.deselect_all();
                }
            }
            SIO_BAUD => self.baud = value,
            I_STAT => self.i_stat_irq &= value & 0x80 != 0,
            I_MASK => self.i_mask = value,
            other => {
                self.error
                    .get_or_insert(format!("libpad write of {other:08x}"));
            }
        }
    }
}
