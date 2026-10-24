//! Bounded headless XA export from original SPH/raw SPB files.
use ac3_audio::{
    StereoPcm,
    xa::{RAW_SECTOR_BYTES, StreamDirectory, XaDecoder},
};
use std::{
    env,
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::PathBuf,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let resource = u16::from_str_radix(
        args.next()
            .ok_or("resource hex ID required")?
            .trim_start_matches("0x"),
        16,
    )?;
    let sectors: usize = args
        .next()
        .ok_or("selected audio sector count required")?
        .parse()?;
    if sectors > 2048 {
        return Err("at most 2048 audio sectors per export".into());
    }
    let output = args.next().ok_or("output WAV required")?;
    if args.next().is_some() {
        return Err("too many arguments".into());
    }
    let root = PathBuf::from(env::var_os("AC3_DATA_DIR").ok_or("set AC3_DATA_DIR")?);
    let header = fs::read(root.join("extracted/disc/ACE.SPH"))?;
    let mut body = File::open(root.join("extracted/disc/ACE.SPB.raw2352"))?;
    let size = body.metadata()?.len();
    if size % RAW_SECTOR_BYTES as u64 != 0 {
        return Err("raw SPB is not sector aligned".into());
    }
    let entry = StreamDirectory::parse(&header, size / RAW_SECTOR_BYTES as u64)?.entry(resource)?;
    if entry.mode != 2 {
        return Err("only audio stream entries are supported".into());
    }
    body.seek(SeekFrom::Start(
        u64::from(entry.first_sector) * RAW_SECTOR_BYTES as u64,
    ))?;
    let mut decoder = XaDecoder::new(1, entry.channel, [150, 0, 150, 0]);
    let mut raw = [0; RAW_SECTOR_BYTES];
    let mut result = StereoPcm { frames: Vec::new() };
    let mut count = 0;
    for _ in 0..entry.sector_count {
        if count == sectors {
            break;
        }
        body.read_exact(&mut raw)?;
        if let Some(pcm) = decoder.decode_sector(&raw)? {
            result.frames.extend(pcm.frames);
            count += 1;
        }
    }
    fs::write(output, result.to_wav()?)?;
    println!(
        "resource {resource:#x}: {count} audio sectors, {} stereo frames",
        result.frames.len()
    );
    Ok(())
}
