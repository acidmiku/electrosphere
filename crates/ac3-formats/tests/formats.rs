use ac3_formats::{ErrorKind, bph, psy, ulz};

fn put_word(data: &mut [u8], offset: usize, value: u32) {
    data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn fix_packed_checksum(data: &mut [u8]) {
    put_word(data, 4, 0);
    let correction = psy::checksum(data).wrapping_neg();
    put_word(data, 4, correction);
}

fn psy_file() -> Vec<u8> {
    let mut data = vec![0; 0x5c];
    data[..4].copy_from_slice(b"PS-Y");
    data.extend_from_slice(&[4, 15, b'A', b'B', b'C', 0]);
    let size = data.len() as u32;
    put_word(&mut data, 0x0c, size);
    put_word(&mut data, 0x10, 3);
    put_word(&mut data, 0x14, 0x5d); // Loader masks the low bits.
    put_word(&mut data, 0x1c, 0x8002_521c);
    put_word(&mut data, 0x20, 0x8001_4000);
    put_word(&mut data, 0x24, 0x8001_0000);
    put_word(&mut data, 0x28, 3);
    put_word(&mut data, 0x3c, 0x801f_fff0);
    put_word(&mut data, 0x40, 16);
    put_word(&mut data, 0x58, psy::checksum(b"ABC"));
    fix_packed_checksum(&mut data);
    data
}

#[test]
fn psy_control_sentinel_and_consumption() {
    assert_eq!(
        psy::decompress(&[4, 15, b'A', b'B', b'C', 0, 99], 3).unwrap(),
        (b"ABC".to_vec(), 6)
    );
}

#[test]
fn psy_overlapping_references_and_zero_token_fields() {
    assert_eq!(
        psy::decompress(&[4, 5, b'A', 0, 0x61, 0], 7).unwrap().0,
        b"AAAAAAA"
    );
    assert_eq!(
        psy::decompress(&[1, 11, b'A', b'B', 0, 6, 0], 5).unwrap().0,
        b"ABABA"
    );
    assert_eq!(
        psy::decompress(&[15, 5, b'A', 0, 1, 0], 3).unwrap().0,
        b"AAA"
    );
}

#[test]
fn psy_rejects_bad_references_sizes_and_truncation() {
    assert_eq!(
        psy::decompress(&[4, 2, 0, 0x11, 0], 1).unwrap_err().kind,
        ErrorKind::InvalidBackReference
    );
    assert_eq!(
        psy::decompress(&[4, 3, 65, 0], 2).unwrap_err().kind,
        ErrorKind::SizeMismatch
    );
    assert_eq!(
        psy::decompress(&[4, 3, 65], 1).unwrap_err().kind,
        ErrorKind::Truncated
    );
    assert_eq!(
        psy::decompress(&[4, 2, 0], 1).unwrap_err().kind,
        ErrorKind::Truncated
    );
    assert_eq!(
        psy::decompress(&[4, 7, 65, 66, 0], 1).unwrap_err().kind,
        ErrorKind::SizeMismatch
    );
    assert_eq!(
        psy::decompress(&[4, 5, 65, 0, 0x61, 0], 6)
            .unwrap_err()
            .kind,
        ErrorKind::SizeMismatch
    );
    for size in [0, psy::MAX_PAYLOAD_SIZE + 1, usize::MAX] {
        assert_eq!(
            psy::decompress(&[4, 0], size).unwrap_err().kind,
            ErrorKind::InvalidSize
        );
    }
    for bits in [0, 16, 255] {
        assert_eq!(
            psy::decompress(&[bits, 0], 1).unwrap_err().kind,
            ErrorKind::Unsupported
        );
    }
}

#[test]
fn psy_checksum_wraps_and_pads_without_copying_the_input() {
    assert_eq!(psy::checksum(&[255, 255, 255, 255, 1, 0, 0, 0]), 0);
    assert_eq!(psy::checksum(&[1, 2, 3]), 0x0003_0201);
    assert_eq!(psy::checksum(&[]), 0);
}

#[test]
fn psy_header_registers_and_checksums() {
    let exe = psy::unpack(&psy_file()).unwrap();
    assert_eq!(exe.payload, b"ABC");
    assert_eq!(exe.entry_pc, 0x8002_521c);
    assert_eq!(exe.initial_gp, 0x8001_4000);
    assert_eq!(exe.load_address, 0x8001_0000);
    assert_eq!(exe.stack_address, 0x801f_fff0);
    assert_eq!(exe.stack_offset, 16);
    assert_eq!(exe.stream_offset, 0x5c);
    assert_eq!(exe.stream_bytes_consumed, 6);
    assert_eq!(exe.trailing_bytes, 0);
}

#[test]
fn psy_header_mutations_are_rejected() {
    let mut data = psy_file();
    data[0] = 0;
    assert_eq!(
        psy::unpack(&data).unwrap_err().kind,
        ErrorKind::InvalidMagic
    );
    let mut data = psy_file();
    data[4] ^= 1;
    assert_eq!(
        psy::unpack(&data).unwrap_err().kind,
        ErrorKind::ChecksumMismatch
    );
    for (offset, value, expected) in [
        (0x0c, 0, ErrorKind::SizeMismatch),
        (0x10, u32::MAX, ErrorKind::InvalidSize),
        (0x14, 0x10, ErrorKind::InvalidOffset),
        (0x14, u32::MAX, ErrorKind::InvalidOffset),
        (0x28, 4, ErrorKind::SizeMismatch),
        (0x58, 0, ErrorKind::ChecksumMismatch),
    ] {
        let mut data = psy_file();
        put_word(&mut data, offset, value);
        fix_packed_checksum(&mut data);
        assert_eq!(
            psy::unpack(&data).unwrap_err().kind,
            expected,
            "offset {offset:x}"
        );
    }
}

fn bph_header(entries: &[(u32, u32)]) -> Vec<u8> {
    let mut data = vec![0; 20];
    data[..4].copy_from_slice(b"AC3E");
    put_word(&mut data, 4, 3);
    put_word(&mut data, 16, entries.len() as u32);
    for (flags, sector) in entries {
        data.extend_from_slice(&flags.to_le_bytes());
        data.extend_from_slice(&sector.to_le_bytes());
    }
    data
}

#[test]
fn bph_retains_all_metadata_and_checks_body_on_access() {
    let dir = bph::parse(&bph_header(&[(0xe000_0002, 1), (0, u32::MAX)]), 2056).unwrap();
    assert_eq!(dir.entries.len(), 2);
    let entry = &dir.entries[0];
    assert_eq!(
        (
            entry.index,
            entry.flags,
            entry.sector,
            entry.directory_offset,
            entry.mode()
        ),
        (0, 0xe000_0002, 1, 20, 0)
    );
    let extent = entry.extent.unwrap();
    assert_eq!(
        extent,
        bph::Extent {
            offset: 2048,
            size: 8
        }
    );
    assert_eq!(extent.bytes(&vec![42; 2056]).unwrap(), [42; 8]);
    assert_eq!(
        extent.bytes(&vec![0; 2055]).unwrap_err().kind,
        ErrorKind::InvalidOffset
    );
    assert_eq!(dir.entries[1].extent, None);
    assert_eq!(dir.entries[1].sector, u32::MAX);
    assert!(
        bph::Extent {
            offset: usize::MAX,
            size: 1
        }
        .bytes(&[])
        .is_err()
    );
}

#[test]
fn bph_rejects_overlaps_truncation_unsupported_flags_and_out_of_bounds() {
    assert_eq!(
        bph::parse(&bph_header(&[(0xe000_0002, 0)]), 7)
            .unwrap_err()
            .kind,
        ErrorKind::InvalidOffset
    );
    assert_eq!(
        bph::parse(&bph_header(&[(0xe000_0002, 0), (0xe000_0001, 0)]), 8)
            .unwrap_err()
            .kind,
        ErrorKind::OverlappingExtent
    );
    assert_eq!(
        bph::parse(&bph_header(&[(0xe800_0001, 0)]), 8)
            .unwrap_err()
            .kind,
        ErrorKind::Unsupported
    );
    assert_eq!(
        bph::parse(&bph_header(&[(0xe000_0001, u32::MAX)]), 8)
            .unwrap_err()
            .kind,
        ErrorKind::InvalidOffset
    );
    let mut data = bph_header(&[(0, 0)]);
    data.pop();
    assert_eq!(bph::parse(&data, 0).unwrap_err().kind, ErrorKind::Truncated);
    let mut data = bph_header(&[]);
    put_word(&mut data, 16, u32::MAX);
    assert_eq!(bph::parse(&data, 0).unwrap_err().kind, ErrorKind::Truncated);
}

fn ulz_file(version: u8) -> Vec<u8> {
    let literal: u32 = if version == 2 { 20 } else { 24 };
    let mut data = b"Ulz\x1a".to_vec();
    data.extend_from_slice(&[9, 0, 0, version]);
    data.extend_from_slice(&literal.to_le_bytes()[..3]);
    data.push(12);
    data.extend_from_slice(&(literal + 4).to_le_bytes());
    data.extend_from_slice(
        &(if version == 2 {
            0xe000_0000u32
        } else {
            0xe800_0000u32
        })
        .to_le_bytes(),
    );
    if version == 0 {
        data.extend_from_slice(&[0; 4]);
    }
    data.extend_from_slice(b"ABC\0");
    data.extend_from_slice(&0x3002u16.to_le_bytes());
    data
}

#[test]
fn ulz_versions_overlap_and_consumption() {
    for version in [0, 2] {
        let data = ulz_file(version);
        let decoded = ulz::decompress(&data).unwrap();
        assert_eq!(decoded.payload, b"ABCABCABC");
        assert_eq!(decoded.version, version);
        assert_eq!(decoded.displacement_bits, 12);
        assert_eq!(decoded.match_end, data.len());
        assert_eq!(decoded.literal_end, data.len() - 3);
        assert_eq!(decoded.flags_end, if version == 2 { 20 } else { 24 });
    }
}

#[test]
fn ulz_rejects_bad_references_and_sizes() {
    let mut data = ulz_file(2);
    let size = data.len();
    data[size - 2..].copy_from_slice(&0x300au16.to_le_bytes());
    assert_eq!(
        ulz::decompress(&data).unwrap_err().kind,
        ErrorKind::InvalidBackReference
    );
    let mut data = ulz_file(2);
    data[4] = 8;
    assert_eq!(
        ulz::decompress(&data).unwrap_err().kind,
        ErrorKind::SizeMismatch
    );
    let mut data = ulz_file(0);
    data[4] = 10;
    assert_eq!(
        ulz::decompress(&data).unwrap_err().kind,
        ErrorKind::SizeMismatch
    );
    let mut data = ulz_file(2);
    data[7] = 1;
    assert_eq!(
        ulz::decompress(&data).unwrap_err().kind,
        ErrorKind::Unsupported
    );
    let mut data = ulz_file(2);
    put_word(&mut data, 12, u32::MAX);
    assert_eq!(
        ulz::decompress(&data).unwrap_err().kind,
        ErrorKind::InvalidOffset
    );
}

#[test]
fn ulz_flags_and_literal_bounds_are_enforced() {
    let mut data = ulz_file(2);
    data[8] = 16;
    assert_eq!(
        ulz::decompress(&data).unwrap_err().kind,
        ErrorKind::Truncated
    );
    let mut data = ulz_file(2);
    data[8] = 0;
    assert_eq!(
        ulz::decompress(&data).unwrap_err().kind,
        ErrorKind::Truncated
    );
    let mut data = ulz_file(2);
    data[8] = 25;
    assert_eq!(
        ulz::decompress(&data).unwrap_err().kind,
        ErrorKind::InvalidOffset
    );
}

#[test]
fn all_truncated_fixture_prefixes_return_errors_without_panicking() {
    let psy_data = psy_file();
    for end in 0..psy_data.len() {
        assert!(psy::unpack(&psy_data[..end]).is_err());
    }
    let bph_data = bph_header(&[(0xe000_0002, 0)]);
    for end in 0..bph_data.len() {
        assert!(bph::parse(&bph_data[..end], 8).is_err());
    }
    for version in [0, 2] {
        let ulz_data = ulz_file(version);
        for end in 0..ulz_data.len() {
            assert!(ulz::decompress(&ulz_data[..end]).is_err());
        }
    }
}
