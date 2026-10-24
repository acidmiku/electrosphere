//! Local integration oracle: never includes game files in the source tree.
use ac3_formats::{bph, psy, ulz};
use std::{fs, path::PathBuf};

fn root() -> PathBuf {
    PathBuf::from(
        std::env::var_os("AC3_DATA_DIR")
            .expect("set AC3_DATA_DIR to the recovery workspace before running ignored tests"),
    )
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the user's recovered PAL game files"]
fn psy_and_all_bpb_extents_match_validated_recovery() {
    let root = root();
    let exe = psy::unpack(&fs::read(root.join("extracted/disc/ACE.BIN")).unwrap()).unwrap();
    assert_eq!(
        exe.payload,
        fs::read(root.join("extracted/programs/ACE.unpacked.bin")).unwrap()
    );
    assert_eq!(
        (
            exe.load_address,
            exe.entry_pc,
            exe.initial_gp,
            exe.stack_address
        ),
        (0x8001_0000, 0x8002_521c, 0, 0x801f_fff0)
    );
    assert_eq!(
        (
            exe.payload.len(),
            exe.stream_bytes_consumed,
            exe.trailing_bytes
        ),
        (700416, 413680, 0)
    );
    assert_eq!(exe.output_checksum, 0x4288_ffb0);
    let body = fs::read(root.join("extracted/disc/ACE.BPB")).unwrap();
    let header = fs::read(root.join("extracted/disc/ACE.BPH")).unwrap();
    let directory = bph::parse(&header, body.len()).unwrap();
    assert_eq!(directory.entries.len(), 127);
    for entry in &directory.entries {
        if let Some(extent) = entry.extent {
            let expected =
                fs::read(root.join(format!("extracted/archives/bpb_{:04}.bin", entry.index)))
                    .unwrap();
            assert_eq!(
                extent.bytes(&body).unwrap(),
                expected,
                "BPB entry {}",
                entry.index
            );
        }
    }
    for (index, size) in [(1, 70224), (2, 196696), (3, 85800), (4, 63740), (5, 7288)] {
        assert_eq!(directory.entries[index].extent.unwrap().size, size);
        assert_eq!(directory.entries[index].mode(), 0);
    }
}

#[test]
#[ignore = "requires AC3_DATA_DIR and the user's recovered PAL resource corpus"]
fn ulz_matches_every_successfully_recovered_container() {
    let archives = root().join("extracted/archives");
    let mut compared = 0;
    for entry in fs::read_dir(&archives).unwrap() {
        let path = entry.unwrap().path();
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        let Some(source_stem) = stem.strip_suffix("_dec") else {
            continue;
        };
        let source = archives.join(format!("{source_stem}.bin"));
        let data = fs::read(&source).unwrap();
        if !data.starts_with(b"Ulz\x1a") {
            continue;
        }
        let decoded =
            ulz::decompress(&data).unwrap_or_else(|error| panic!("{}: {error}", source.display()));
        assert_eq!(
            decoded.payload,
            fs::read(&path).unwrap(),
            "{}",
            source.display()
        );
        compared += 1;
    }
    assert!(
        compared >= 1362,
        "incomplete recovered corpus: compared {compared} containers"
    );
    eprintln!("Compared {compared} Ulz containers byte-for-byte with the Python recovery output");
}
