#[path = "support/scene.rs"]
mod scene;

#[test]
#[ignore = "requires AC3_DATA_DIR and natural first-hub original PCM/register trace"]
fn natural_hub_mix_matches_with_original_register_schedule() {
    let root =
        std::path::PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("set AC3_DATA_DIR"));
    let bytes = [
        "bpb_0000_0004.bin",
        "bpb_0069_0004.bin",
        "bpb_0072_0009.bin",
    ]
    .map(|name| std::fs::read(root.join("extracted/archives").join(name)).unwrap());
    let directory = root.join("analysis/audio-runtime/natural-hub-001");
    let events = std::fs::read_to_string(directory.join("mix-events.csv")).unwrap();
    let pcm = scene::replay([&bytes[0], &bytes[1], &bytes[2]], &events, 450000).unwrap();
    let captured = std::fs::read(directory.join("original-mix.f32le-stereo.raw")).unwrap();
    assert_eq!(captured.len(), 1427076 * 8);
    assert_eq!(pcm.frames.len(), 450000);
    // One fixed alignment covers SDL queue latency. No shifting/rescaling of
    // individual voices, omitted samples or tolerance inside the whole window.
    for (index, native) in pcm.frames.iter().enumerate() {
        for (channel, value) in native.iter().copied().enumerate() {
            let offset = (968160 + index) * 8 + channel * 4;
            let original = f32::from_le_bytes(captured[offset..offset + 4].try_into().unwrap());
            assert_eq!(
                original,
                f32::from(value) / 32767.0,
                "frame {index} channel {channel}"
            );
        }
    }
}
