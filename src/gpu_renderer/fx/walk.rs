//! The phase markers: where the GPU's DMA walk of a frame's list passes the
//! flight ordering table's entries 4095 (the sky), 4094 (the world begins),
//! 1 (the world ends; the flare) and 0 (the HUD).
//!
//! [`schedule`] walks a DMA list as `flight_host::walk_list_addressed`
//! does (24-bit physical links, a header's word count, the same packet
//! limit) before the device walks it, reading only the headers, and notes
//! each marker's ordering-table node with the first GP0 word of the next
//! packet that has words: the recorder fires the marker just before that
//! word ([`super::Recording`]), or at the list's end when no packet follows.
//!
//! The JP flight list (frame end `800260C0`, `jp::res_p1_frame::frame`):
//! the draw buffer's three-entry table, whose entry 1 links to the flight
//! table's entry 4095; the flight table walks down to entry 0, whose link
//! goes on to the three-entry table's entry 0 (the state overlays). Every
//! world packet hangs off entries 4094..2: the sky's off 4095, the camera
//! shake's drawing offset off 1, the screen offset off 0.
use crate::flight_host::{LIST_END, MAX_LIST_PACKETS, kseg0};
use serde::Serialize;

use super::scene::OT_ENTRIES;

/// A marker of the flight frame, in the order the walk passes them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum Marker {
    /// Entry 4095: the sky's packets follow.
    Sky,
    /// Entry 4094: the world (terrain and objects, entries 4094..2).
    WorldBegin,
    /// Entry 1: the world is drawn; the flare's packets follow.
    WorldEnd,
    /// Entry 0: the HUD and screen overlays.
    Hud,
}

impl Marker {
    pub const ALL: [Self; 4] = [Self::Sky, Self::WorldBegin, Self::WorldEnd, Self::Hud];

    /// The ordering-table entry the marker is at.
    pub fn entry(self) -> u32 {
        match self {
            Self::Sky => OT_ENTRIES - 1,
            Self::WorldBegin => OT_ENTRIES - 2,
            Self::WorldEnd => 1,
            Self::Hud => 0,
        }
    }

    fn at_entry(entry: u32) -> Option<Self> {
        Self::ALL.into_iter().find(|marker| marker.entry() == entry)
    }
}

/// One marker of a list: its node (the entry's address) and the first GP0
/// word of the next packet with words (`None`: none follows; the list's end).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Fire {
    pub marker: Marker,
    pub node: u32,
    pub word: Option<u32>,
}

/// The markers of a flight list: all four, in order, from one table.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Schedule {
    /// The table walked (entry 0's address).
    pub ot: u32,
    pub fires: Vec<Fire>,
    /// Where the walk's ordering-table entry changes: the first GP0 word
    /// of the first packet after another entry, and that entry (the slot
    /// the packets from there on hang off), in walk order.
    pub slots: Vec<(u32, u16)>,
}

fn header(ram: &[u8], address: u32) -> Option<u32> {
    let physical = (address & LIST_END) as usize;
    let bytes = ram.get(physical..physical + 4)?;
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

/// The markers of the DMA list at `start` when it walks one of `ots`
/// (flight ordering tables, entry 0's address) through entries 4095, 4094,
/// 1 and 0 in that order; `None` for any other list (or one the device
/// fails on).
pub fn schedule(ram: &[u8], start: u32, ots: &[u32]) -> Option<Schedule> {
    let mut fires: Vec<Fire> = Vec::new();
    let mut slots: Vec<(u32, u16)> = Vec::new();
    let mut unplaced = 0;
    let mut table = None;
    // The entry last passed, and the one the last packet hung off.
    let (mut slot, mut placed) = (None, None);
    let mut address = start & LIST_END;
    let mut ended = false;
    for _ in 0..MAX_LIST_PACKETS {
        let word = header(ram, address)?;
        let at = kseg0(address);
        if word >> 24 == 0 {
            let node = ots.iter().find_map(|&ot| {
                let offset = at.wrapping_sub(kseg0(ot));
                (offset < OT_ENTRIES * 4 && offset.is_multiple_of(4))
                    .then_some((kseg0(ot), offset / 4))
            });
            if let Some((ot, entry)) = node {
                if table.is_some_and(|table| table != ot) {
                    return None;
                }
                slot = Some(entry as u16);
                if let Some(marker) = Marker::at_entry(entry) {
                    table = Some(ot);
                    fires.push(Fire {
                        marker,
                        node: at,
                        word: None,
                    });
                }
            }
        } else {
            for fire in &mut fires[unplaced..] {
                fire.word = Some(at.wrapping_add(4));
            }
            unplaced = fires.len();
            if slot != placed
                && let Some(entry) = slot
            {
                slots.push((at.wrapping_add(4), entry));
                placed = slot;
            }
        }
        let next = word & LIST_END;
        if next == LIST_END {
            ended = true;
            break;
        }
        address = next;
    }
    let order: Vec<Marker> = fires.iter().map(|fire| fire.marker).collect();
    (ended && order == Marker::ALL).then_some(Schedule {
        ot: table?,
        fires,
        slots,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// RAM with a flight list: a three-entry table at 0x80108000 (entry 2
    /// first) whose entry 1 links to the flight table at 0x80100000 (4096
    /// entries from 4095 down), entry 0 of which links to the three-entry
    /// table's entry 0. Packets: `packets` as (entry, words), each linked
    /// off its entry in order. Returns the RAM and the start (entry 2).
    pub(crate) fn flight_list(packets: &[(u32, Vec<u32>)]) -> (Vec<u8>, u32) {
        let mut ram = vec![0u8; 0x20_0000];
        let start = flight_list_in(&mut ram, packets);
        (ram, start)
    }

    /// [`flight_list`] into `ram` (2 MiB); returns the start.
    pub(crate) fn flight_list_in(ram: &mut [u8], packets: &[(u32, Vec<u32>)]) -> u32 {
        let put = |ram: &mut [u8], address: u32, value: u32| {
            let at = (address & 0x1f_ffff) as usize;
            ram[at..at + 4].copy_from_slice(&value.to_le_bytes());
        };
        let ot = 0x8010_0000u32;
        let small = 0x8010_8000u32;
        // ClearOTagR: entry i links to i - 1; the small table likewise.
        for i in 1..OT_ENTRIES {
            put(ram, ot + i * 4, (ot + (i - 1) * 4) & LIST_END);
        }
        put(ram, small + 8, (small + 4) & LIST_END);
        put(ram, small + 4, (ot + 4095 * 4) & LIST_END);
        put(ram, ot, small & LIST_END);
        put(ram, small, LIST_END);
        // Packets, inserted at the front of their entry's chain (the last
        // given runs first, as addPrim does).
        let mut next = 0x8011_0000u32;
        for (entry, words) in packets {
            let node = if *entry >= 4096 {
                small + (entry - 4096) * 4
            } else {
                ot + entry * 4
            };
            let link =
                u32::from_le_bytes(ram[(node & 0x1f_ffff) as usize..][..4].try_into().unwrap());
            put(ram, next, ((words.len() as u32) << 24) | (link & LIST_END));
            for (i, &word) in words.iter().enumerate() {
                put(ram, next + 4 + 4 * i as u32, word);
            }
            put(ram, node, next & LIST_END);
            next += 0x100;
        }
        small + 8
    }

    #[test]
    fn markers_go_before_the_next_packet() {
        // A sky packet (4095), terrain at 4094 and 2000, a flare packet (1),
        // the HUD's (0) and an overlay off the small table's entry 0.
        let (ram, start) = flight_list(&[
            (4095, vec![0xe500_0000]),
            (4094, vec![0x6000_0000, 0, 0x0010_0010]),
            (2000, vec![0x6000_00ff, 0, 0x0010_0010]),
            (1, vec![0xe500_0001]),
            (0, vec![0xe500_0002]),
            (4096, vec![0xe500_0003]),
        ]);
        let schedule = schedule(&ram, start, &[0x8010_0000, 0x8010_4000]).unwrap();
        assert_eq!(schedule.ot, 0x8010_0000);
        let packet = |index: u32| Some(0x8011_0000 + index * 0x100 + 4);
        assert_eq!(
            schedule.fires,
            [
                Fire {
                    marker: Marker::Sky,
                    node: 0x8010_3ffc,
                    word: packet(0)
                },
                Fire {
                    marker: Marker::WorldBegin,
                    node: 0x8010_3ff8,
                    word: packet(1)
                },
                Fire {
                    marker: Marker::WorldEnd,
                    node: 0x8010_0004,
                    word: packet(3)
                },
                Fire {
                    marker: Marker::Hud,
                    node: 0x8010_0000,
                    word: packet(4)
                },
            ]
        );
        // The slots each packet hangs off (the overlay off the small table
        // keeps entry 0's).
        assert_eq!(
            schedule.slots,
            [
                (0x8011_0004, 4095),
                (0x8011_0104, 4094),
                (0x8011_0204, 2000),
                (0x8011_0304, 1),
                (0x8011_0404, 0),
            ]
        );
        // Every walked word agrees with the device's walk.
        let walked = crate::flight_host::walk_list_addressed(&ram, start).unwrap();
        let firsts: Vec<u32> = walked.iter().map(|(address, _)| address + 4).collect();
        for fire in &schedule.fires {
            assert!(firsts.contains(&fire.word.unwrap()), "{fire:?}");
        }
    }

    #[test]
    fn empty_entries_fire_together_and_the_end_fires_last() {
        // Nothing but a world packet: the sky and world markers share it;
        // the world end and HUD come at the list's end.
        let (ram, start) = flight_list(&[(100, vec![0x6000_0000, 0, 0x0010_0010])]);
        let schedule = schedule(&ram, start, &[0x8010_0000]).unwrap();
        let words: Vec<Option<u32>> = schedule.fires.iter().map(|fire| fire.word).collect();
        assert_eq!(words, [Some(0x8011_0004), Some(0x8011_0004), None, None]);
    }

    #[test]
    fn other_lists_have_no_markers() {
        let (ram, start) = flight_list(&[]);
        // Not a flight table.
        assert_eq!(schedule(&ram, start, &[0x8018_0000]), None);
        // A one-packet list (PutDrawEnv).
        let mut ram = ram;
        ram[0x12_0000..0x12_0004].copy_from_slice(&0x01ff_ffffu32.to_le_bytes());
        ram[0x12_0004..0x12_0008].copy_from_slice(&0xe600_0000u32.to_le_bytes());
        assert_eq!(schedule(&ram, 0x0012_0000, &[0x8010_0000]), None);
        // A link outside RAM fails as the device does.
        ram[0x12_0000..0x12_0004].copy_from_slice(&0x00e0_0000u32.to_le_bytes());
        assert_eq!(schedule(&ram, 0x0012_0000, &[0x8010_0000]), None);
    }
}
