//! Host MDEC (`src/machine_mdec.rs`) against the original's decoded movie
//! slices.
//!
//! In the PCSX-Redux captures of the attract-mode movie (`ovmisc-006`,
//! `ovmisc-009`: state 5 from frame 1358; `ovmisc-010`, `ovmisc-011`: its
//! start and slice callbacks), every entry of the DMA-1 callback 80055878
//! finds the slice the MDEC just wrote in image buffer IMAGE_ID. The test
//! feeds the device through the host protocol from the same RAM: the
//! default tables (MDEC_RESET 0 reads them at 800B9934 / 800B99B8), the
//! frame's run-level data (MDEC_IN; the frame is the VLC buffer whose
//! header DecDCTin marked with bit 25: VLC_ID's, or the other one once the
//! next frame was VLC-decoded), then one MDEC_OUT per slice up to this one,
//! which must equal the captured buffer byte for byte.
use ac3_flight::{
    Device, HostCall,
    snapshot::{self, Snapshot},
};
use ac3_port::machine_mdec::{MdecDevice, calls};
use std::path::PathBuf;

const GP: u32 = 0x800b_a9f4;
/// `[gp+0x4DC]`: the movie decoder (`ac3_flight::states::decoder`).
const DECODER: u32 = GP + 0x4dc;

fn word(ram: &[u8], address: u32) -> u32 {
    let at = (address & 0x1f_ffff) as usize;
    u32::from_le_bytes(ram[at..at + 4].try_into().unwrap())
}
fn half(ram: &[u8], address: u32) -> u32 {
    let at = (address & 0x1f_ffff) as usize;
    u32::from(u16::from_le_bytes([ram[at], ram[at + 1]]))
}
fn byte(ram: &[u8], address: u32) -> u32 {
    u32::from(ram[(address & 0x1f_ffff) as usize])
}

fn call(mdec: &mut MdecDevice, ram: &mut [u8], function: u32, args: [u32; 4]) -> Option<u32> {
    let mut scratch = [0u8; 0x400];
    mdec.call(ram, &mut scratch, HostCall { function, args })
}

#[test]
#[ignore = "needs analysis/flight-runtime/ovmisc-006, -009, -010, -011"]
fn captured_slices_decode_exactly() {
    let root = PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR"))
        .join("analysis/flight-runtime");
    let mut slices = 0;
    let mut bytes = 0;
    for name in ["ovmisc-006", "ovmisc-009", "ovmisc-010", "ovmisc-011"] {
        let index = root.join(name).join("snapshots.jsonl");
        for info in snapshot::read_index(&index)
            .unwrap()
            .iter()
            .filter(|s| s.callee == 0x8005_5878 && s.stage == "entry")
        {
            let snap = Snapshot::load(index.parent().unwrap(), info).unwrap();
            let ram = &snap.ram;
            let d = word(ram, DECODER);
            let vlc_id = byte(ram, d);
            let image_id = byte(ram, d + 1);
            let rect_id = byte(ram, d + 2);
            let mut vlc = word(ram, d + 8 + 4 * vlc_id);
            if word(ram, vlc) & 0x0200_0000 == 0 {
                vlc = word(ram, d + 8 + 4 * (vlc_id ^ 1));
            }
            let image = word(ram, d + 0x10 + 4 * image_id);
            let rect_x = half(ram, d + 0x18 + 8 * rect_id);
            let slice_x = half(ram, d + 0x28);
            let slice_w = half(ram, d + 0x2c);
            let slice = (slice_x - rect_x) / slice_w;
            let words = half(ram, d + 0x1_1036);
            let mut work = ram.clone();
            let mut mdec = MdecDevice::new();
            call(&mut mdec, &mut work, calls::MDEC_RESET, [0, 0, 0, 0]);
            let header = word(ram, vlc);
            call(
                &mut mdec,
                &mut work,
                calls::MDEC_IN,
                [vlc, header & 0xffff, 0, 0],
            );
            let mut output = vec![0u8; 0x20_0000];
            for _ in 0..=slice {
                assert_eq!(
                    call(
                        &mut mdec,
                        &mut output,
                        calls::MDEC_OUT,
                        [image, words, 0, 0]
                    ),
                    Some(1)
                );
            }
            let start = (image & 0x1f_ffff) as usize;
            let length = words as usize * 4;
            let differ = (start..start + length)
                .filter(|&i| output[i] != ram[i])
                .count();
            assert_eq!(
                differ, 0,
                "{name} frame {} slice {slice}: {differ} of {length} bytes differ",
                info.frame
            );
            slices += 1;
            bytes += length;
        }
    }
    println!("{slices} captured slices, {bytes} bytes equal");
    assert!(slices > 0);
}
