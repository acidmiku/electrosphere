//! Which release a disc image holds, from its ISO 9660 volume ID and the
//! boot executable named in `SYSTEM.CNF`, and the set of discs a game
//! needs (PAL one, the Japanese release two).
//!
//! | Release | Volume ID | `SYSTEM.CNF` BOOT |
//! | --- | --- | --- |
//! | PAL (Europe) | `ACE3ES` | `SCES_020.66` |
//! | Japan disc 1 | `ACE3ES1` | `SLPS_020.20` |
//! | Japan disc 2 | `ACE3ES2` | `SLPS_020.21` |
//!
//! Both must agree. This tells the release and the disc only, not the
//! revision or whether a translation patch is applied: the player checks
//! the resident executable it loads separately.
//!
//! Images are raw 2352-byte sectors (`.bin`, Mode 2 Form 1 user data from
//! byte 24 of each sector; Mode 1 from byte 16), or a `.cue` sheet naming
//! one. Only the primary volume descriptor (sector 16), the root directory
//! and `SYSTEM.CNF` are read.
use std::{
    fmt, fs,
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

pub const RAW_SECTOR_BYTES: u64 = 2352;
const USER_BYTES: usize = 2048;
/// The most `SYSTEM.CNF` bytes read, and root directory sectors walked.
const MAX_CONFIG_BYTES: u32 = 4 * USER_BYTES as u32;
const MAX_ROOT_SECTORS: u32 = 16;
/// Directory entries a disc-set search looks at.
const MAX_SEARCH_ENTRIES: usize = 256;

/// A release of the game.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Version {
    /// The European release (one disc).
    Pal,
    /// The Japanese release, disc 1 or 2.
    Jp { disc: u8 },
}

impl Version {
    /// The (volume ID, boot executable) of each known disc.
    pub const KNOWN: [(Version, &'static str, &'static str); 3] = [
        (Version::Pal, "ACE3ES", "SCES_020.66"),
        (Version::Jp { disc: 1 }, "ACE3ES1", "SLPS_020.20"),
        (Version::Jp { disc: 2 }, "ACE3ES2", "SLPS_020.21"),
    ];

    /// The release of a volume ID and boot name (case-insensitive), when
    /// both name the same known disc.
    pub fn of(volume: &str, boot: &str) -> Option<Self> {
        Self::KNOWN
            .iter()
            .find(|(_, v, b)| volume.eq_ignore_ascii_case(v) && boot.eq_ignore_ascii_case(b))
            .map(|(version, _, _)| *version)
    }
    /// The discs this release needs.
    pub fn discs(self) -> u8 {
        match self {
            Self::Pal => 1,
            Self::Jp { .. } => 2,
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pal => f.write_str("PAL (Europe)"),
            Self::Jp { disc } => write!(f, "Japan, disc {disc}"),
        }
    }
}

/// What a disc image says about itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscIdentity {
    pub version: Version,
    /// The ISO volume ID, trailing spaces removed.
    pub volume: String,
    /// The boot executable of `SYSTEM.CNF`, without device, path and
    /// version (`SLPS_020.20`).
    pub boot: String,
}

/// Why an image was not identified.
#[derive(Debug)]
pub enum VersionError {
    Io {
        path: PathBuf,
        error: io::Error,
    },
    /// Not a raw 2352-byte image of an ISO 9660 disc, or a `.cue` sheet
    /// without a usable FILE.
    NotADisc {
        path: PathBuf,
        reason: String,
    },
    /// An ISO disc, but not one of [`Version::KNOWN`].
    Unknown {
        path: PathBuf,
        volume: String,
        boot: String,
    },
    /// A disc set that is incomplete or mixed.
    DiscSet(String),
}

impl fmt::Display for VersionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, error } => write!(f, "{}: {error}", path.display()),
            Self::NotADisc { path, reason } => {
                write!(f, "{}: not a usable disc image: {reason}", path.display())
            }
            Self::Unknown { path, volume, boot } => write!(
                f,
                "{}: not an Ace Combat 3 disc this build supports (volume ID {volume:?}, \
                 boot {boot:?}; expected the European disc ACE3ES / SCES_020.66 or the \
                 Japanese discs ACE3ES1 / SLPS_020.20 and ACE3ES2 / SLPS_020.21)",
                path.display()
            ),
            Self::DiscSet(reason) => f.write_str(reason),
        }
    }
}

impl std::error::Error for VersionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { error, .. } => Some(error),
            _ => None,
        }
    }
}

fn not_a_disc(path: &Path, reason: impl Into<String>) -> VersionError {
    VersionError::NotADisc {
        path: path.to_owned(),
        reason: reason.into(),
    }
}

/// A raw image's sectors.
struct Image<R> {
    reader: R,
}

impl<R: Read + Seek> Image<R> {
    /// The 2048 user bytes of sector `lba` (Mode 2 Form 1 or Mode 1).
    fn user_data(&mut self, lba: u32) -> io::Result<[u8; USER_BYTES]> {
        let mut raw = [0; RAW_SECTOR_BYTES as usize];
        self.reader
            .seek(SeekFrom::Start(u64::from(lba) * RAW_SECTOR_BYTES))?;
        self.reader.read_exact(&mut raw)?;
        let sync = [0, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 0];
        let start = match (raw[..12] == sync, raw[15]) {
            (true, 2) => 24,
            (true, 1) => 16,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("sector {lba} is not a raw Mode 1 / Mode 2 data sector"),
                ));
            }
        };
        Ok(raw[start..start + USER_BYTES].try_into().unwrap())
    }
}

fn word(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

/// The volume ID and `SYSTEM.CNF`'s boot name of the raw image `reader`
/// (`path` names it in errors).
pub fn read_identity<R: Read + Seek>(
    reader: R,
    path: &Path,
) -> Result<(String, String), VersionError> {
    let mut image = Image { reader };
    let io = |error: io::Error| match error.kind() {
        io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof => {
            not_a_disc(path, error.to_string())
        }
        _ => VersionError::Io {
            path: path.to_owned(),
            error,
        },
    };
    let volume = image.user_data(16).map_err(io)?;
    if volume[0] != 1 || &volume[1..6] != b"CD001" {
        return Err(not_a_disc(
            path,
            "no ISO 9660 primary volume descriptor at sector 16",
        ));
    }
    let volume_id = String::from_utf8_lossy(&volume[40..72])
        .trim_end_matches([' ', '\0'])
        .to_owned();
    let Some((lba, bytes)) = find_root_file(&mut image, "SYSTEM.CNF").map_err(io)? else {
        return Err(VersionError::Unknown {
            path: path.to_owned(),
            volume: volume_id,
            boot: "(no SYSTEM.CNF)".to_owned(),
        });
    };
    let bytes = bytes.min(MAX_CONFIG_BYTES) as usize;
    let mut text = Vec::with_capacity(bytes);
    for sector in 0..bytes.div_ceil(USER_BYTES) as u32 {
        text.extend_from_slice(&image.user_data(lba + sector).map_err(io)?);
    }
    text.truncate(bytes);
    let boot = boot_name(&String::from_utf8_lossy(&text)).unwrap_or_default();
    Ok((volume_id, boot))
}

/// The (first sector, byte size) of the root-directory file `name` (an ISO
/// name without version, any case) of the raw image `image`, or `None`.
fn find_root_file<R: Read + Seek>(
    image: &mut Image<R>,
    name: &str,
) -> io::Result<Option<(u32, u32)>> {
    let volume = image.user_data(16)?;
    if volume[0] != 1 || &volume[1..6] != b"CD001" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "no ISO 9660 primary volume descriptor at sector 16",
        ));
    }
    let root = &volume[156..190];
    let (start, size) = (word(root, 2), word(root, 10));
    for sector in 0..size.div_ceil(USER_BYTES as u32).min(MAX_ROOT_SECTORS) {
        let data = image.user_data(start + sector)?;
        let mut offset = 0;
        while offset + 33 < data.len() {
            let length = usize::from(data[offset]);
            if length < 34 || offset + length > data.len() {
                break;
            }
            let record = &data[offset..offset + length];
            let name_length = usize::from(record[32]);
            let found = record.get(33..33 + name_length).unwrap_or_default();
            let found = found.split(|&b| b == b';').next().unwrap_or_default();
            if found.eq_ignore_ascii_case(name.as_bytes()) {
                return Ok(Some((word(record, 2), word(record, 10))));
            }
            offset += length;
        }
    }
    Ok(None)
}

/// The bytes of the root-directory file `name` (`ACE3.BIN`) of the disc
/// image (or cue sheet) at `path`, read from its 2048-byte user data.
pub fn read_root_file(path: &Path, name: &str) -> Result<Vec<u8>, VersionError> {
    let image_file = image_path(path)?;
    let file = fs::File::open(&image_file).map_err(|error| VersionError::Io {
        path: image_file.clone(),
        error,
    })?;
    // Unbuffered: every sector is a seek and one 2352-byte read.
    let mut image = Image { reader: file };
    let io = |error: io::Error| match error.kind() {
        io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof => {
            not_a_disc(&image_file, error.to_string())
        }
        _ => VersionError::Io {
            path: image_file.clone(),
            error,
        },
    };
    let Some((lba, size)) = find_root_file(&mut image, name).map_err(io)? else {
        return Err(not_a_disc(&image_file, format!("no {name} on the disc")));
    };
    let mut bytes = Vec::with_capacity(size as usize);
    for sector in 0..size.div_ceil(USER_BYTES as u32) {
        bytes.extend_from_slice(&image.user_data(lba + sector).map_err(io)?);
    }
    bytes.truncate(size as usize);
    Ok(bytes)
}

/// `BOOT = cdrom:\SLPS_020.20;1` → `SLPS_020.20`.
fn boot_name(config: &str) -> Option<String> {
    config.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        if !key.trim().eq_ignore_ascii_case("BOOT") {
            return None;
        }
        let value = value.trim();
        let value = value.split_once(':').map_or(value, |(_, rest)| rest);
        let value = value.rsplit(['\\', '/']).next().unwrap_or(value);
        let value = value.split(';').next().unwrap_or(value).trim();
        Some(value.to_owned())
    })
}

/// The image a path names: itself, or for a `.cue` sheet its first FILE.
pub fn image_path(path: &Path) -> Result<PathBuf, VersionError> {
    let is_cue = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("cue"));
    if !is_cue {
        return Ok(path.to_owned());
    }
    let text = fs::read_to_string(path).map_err(|error| VersionError::Io {
        path: path.to_owned(),
        error,
    })?;
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line
            .get(..5)
            .filter(|k| k.eq_ignore_ascii_case("FILE "))
            .map(|_| line[5..].trim())
        else {
            continue;
        };
        let name = match rest.strip_prefix('"') {
            Some(quoted) => quoted.split('"').next().unwrap_or_default(),
            None => rest.split_whitespace().next().unwrap_or_default(),
        };
        if name.is_empty() {
            break;
        }
        return Ok(path.parent().unwrap_or(Path::new("")).join(name));
    }
    Err(not_a_disc(path, "the cue sheet names no FILE"))
}

/// The identity of the disc image (or cue sheet) at `path`.
pub fn identify(path: &Path) -> Result<DiscIdentity, VersionError> {
    let image = image_path(path)?;
    let file = fs::File::open(&image).map_err(|error| VersionError::Io {
        path: image.clone(),
        error,
    })?;
    let (volume, boot) = read_identity(io::BufReader::new(file), &image)?;
    match Version::of(&volume, &boot) {
        Some(version) => Ok(DiscIdentity {
            version,
            volume,
            boot,
        }),
        None => Err(VersionError::Unknown {
            path: image,
            volume,
            boot,
        }),
    }
}

/// The discs of one game, in disc order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscSet {
    /// [`Version::Pal`] or [`Version::Jp`] with disc 1.
    pub version: Version,
    /// One path for PAL, disc 1 and disc 2 for the Japanese release (as
    /// given: an image or a cue sheet).
    pub discs: Vec<PathBuf>,
}

impl DiscSet {
    /// The set from explicit paths: one PAL disc, or both Japanese discs in
    /// any order.
    pub fn from_paths(paths: &[PathBuf]) -> Result<Self, VersionError> {
        let mut found = Vec::new();
        for path in paths {
            found.push((identify(path)?.version, path.clone()));
        }
        Self::assemble(found)
    }

    /// The set of the disc at `path`: PAL alone; for a Japanese disc, the
    /// other one next to it, named as it is with `(Disc 1)` / `(Disc 2)`
    /// swapped, or else any image in the same folder that identifies as it.
    pub fn find(path: &Path) -> Result<Self, VersionError> {
        let identity = identify(path)?;
        let Version::Jp { disc } = identity.version else {
            return Self::assemble(vec![(identity.version, path.to_owned())]);
        };
        let other = 3 - disc;
        let wanted = Version::Jp { disc: other };
        if let Some(candidate) = sibling_name(path, other)
            && candidate.exists()
            && identify(&candidate).is_ok_and(|i| i.version == wanted)
        {
            return Self::assemble(vec![
                (identity.version, path.to_owned()),
                (wanted, candidate),
            ]);
        }
        let folder = path.parent().filter(|p| !p.as_os_str().is_empty());
        let folder = folder.unwrap_or(Path::new("."));
        let extension = path.extension().map(|e| e.to_ascii_lowercase());
        let mut entries: Vec<PathBuf> = fs::read_dir(folder)
            .map_err(|error| VersionError::Io {
                path: folder.to_owned(),
                error,
            })?
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|p| p.extension().map(|e| e.to_ascii_lowercase()) == extension)
            .take(MAX_SEARCH_ENTRIES)
            .collect();
        entries.sort();
        for candidate in entries {
            if candidate != path && identify(&candidate).is_ok_and(|i| i.version == wanted) {
                return Self::assemble(vec![
                    (identity.version, path.to_owned()),
                    (wanted, candidate),
                ]);
            }
        }
        let expected = sibling_name(path, other)
            .map(|p| format!(" (expected {})", p.display()))
            .unwrap_or_default();
        Err(VersionError::DiscSet(format!(
            "{} is the Japanese disc {disc}; disc {other} was not found next to it{expected}",
            path.display()
        )))
    }

    fn assemble(mut found: Vec<(Version, PathBuf)>) -> Result<Self, VersionError> {
        found.sort_by_key(|(version, _)| match version {
            Version::Pal => 0,
            Version::Jp { disc } => *disc,
        });
        let versions: Vec<Version> = found.iter().map(|(v, _)| *v).collect();
        let version = match versions[..] {
            [Version::Pal] => Version::Pal,
            [Version::Jp { disc: 1 }, Version::Jp { disc: 2 }] => Version::Jp { disc: 1 },
            _ => {
                let list: Vec<String> = found
                    .iter()
                    .map(|(v, p)| format!("{} ({v})", p.display()))
                    .collect();
                return Err(VersionError::DiscSet(format!(
                    "not one game's discs: {}; give the PAL disc, or both Japanese discs",
                    list.join(", ")
                )));
            }
        };
        Ok(Self {
            version,
            discs: found.into_iter().map(|(_, p)| p).collect(),
        })
    }
}

/// `path` with its file name's `(Disc N)` (any case) naming `disc`.
fn sibling_name(path: &Path, disc: u8) -> Option<PathBuf> {
    let name = path.file_name()?.to_str()?;
    let lower = name.to_ascii_lowercase();
    let at = lower.find("(disc ")?;
    let digit = at + "(disc ".len();
    if !lower[digit..].starts_with(['1', '2']) || !lower[digit + 1..].starts_with(')') {
        return None;
    }
    let renamed = format!("{}{disc}{}", &name[..digit], &name[digit + 1..]);
    Some(path.with_file_name(renamed))
}

/// A raw test image of `volume` booting `boot` (none: no `SYSTEM.CNF`),
/// for other modules' tests.
#[cfg(test)]
pub(crate) fn test_image(volume: &str, boot: Option<&str>) -> Vec<u8> {
    tests::image(volume, boot.map(tests::config).as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// A raw Mode 2 image: sector 16 the primary volume descriptor, 18 the
    /// root directory (`.`, `..`, `README.TXT;1`, `SYSTEM.CNF;1`), 19 the
    /// configuration text, 20 a filler file.
    pub(super) fn image(volume: &str, config: Option<&str>) -> Vec<u8> {
        let sectors = 22;
        let mut raw = vec![0u8; sectors * RAW_SECTOR_BYTES as usize];
        let mut user = |lba: usize, data: &[u8]| {
            let at = lba * RAW_SECTOR_BYTES as usize;
            raw[at + 1..at + 11].fill(0xff);
            raw[at + 15] = 2;
            raw[at + 24..at + 24 + data.len()].copy_from_slice(data);
        };
        for lba in 0..sectors {
            user(lba, &[]);
        }
        let record = |lba: u32, size: u32, name: &[u8]| {
            let length = 33 + name.len() + (1 - name.len() % 2);
            let mut r = vec![0u8; length];
            r[0] = length as u8;
            r[2..6].copy_from_slice(&lba.to_le_bytes());
            r[10..14].copy_from_slice(&size.to_le_bytes());
            r[32] = name.len() as u8;
            r[33..33 + name.len()].copy_from_slice(name);
            r
        };
        let mut pvd = vec![0u8; USER_BYTES];
        pvd[0] = 1;
        pvd[1..6].copy_from_slice(b"CD001");
        let mut id = [b' '; 32];
        id[..volume.len()].copy_from_slice(volume.as_bytes());
        pvd[40..72].copy_from_slice(&id);
        pvd[156..156 + 34].copy_from_slice(&record(18, 2048, &[0]));
        user(16, &pvd);
        let mut root = Vec::new();
        root.extend(record(18, 2048, &[0]));
        root.extend(record(18, 2048, &[1]));
        root.extend(record(20, 5, b"README.TXT;1"));
        if let Some(config) = config {
            root.extend(record(19, config.len() as u32, b"SYSTEM.CNF;1"));
            user(19, config.as_bytes());
        }
        user(18, &root);
        user(20, b"hello");
        raw
    }
    pub(super) fn config(boot: &str) -> String {
        format!("BOOT = cdrom:\\{boot};1\r\nTCB = 4\r\nEVENT = 10\r\nSTACK = 801FFFF0\r\n")
    }
    fn identity(raw: Vec<u8>) -> Result<(String, String), VersionError> {
        read_identity(Cursor::new(raw), Path::new("test.bin"))
    }

    #[test]
    fn known_discs_are_identified_by_volume_and_boot() {
        for (version, volume, boot) in Version::KNOWN {
            let (v, b) = identity(image(volume, Some(&config(boot)))).unwrap();
            assert_eq!((v.as_str(), b.as_str()), (volume, boot));
            assert_eq!(Version::of(&v, &b), Some(version));
        }
        assert_eq!(
            Version::of("ace3es1", "slps_020.20"),
            Some(Version::Jp { disc: 1 })
        );
        // Both must name the same disc.
        assert_eq!(Version::of("ACE3ES1", "SLPS_020.21"), None);
        assert_eq!(Version::of("ACE3ES", "SLPS_020.20"), None);
        assert_eq!(Version::Jp { disc: 2 }.discs(), 2);
        assert_eq!(Version::Pal.discs(), 1);
        assert_eq!(Version::Jp { disc: 2 }.to_string(), "Japan, disc 2");
    }

    #[test]
    fn root_files_are_read_from_the_image() {
        let folder = std::env::temp_dir().join(format!("ac3-version-{}-f", std::process::id()));
        fs::create_dir_all(&folder).unwrap();
        let path = folder.join("disc.bin");
        fs::write(&path, image("ACE3ES1", Some(&config("SLPS_020.20")))).unwrap();
        assert_eq!(read_root_file(&path, "readme.txt").unwrap(), b"hello");
        assert_eq!(
            read_root_file(&path, "SYSTEM.CNF").unwrap(),
            config("SLPS_020.20").as_bytes()
        );
        let error = read_root_file(&path, "ACE3.BIN").unwrap_err();
        assert!(error.to_string().contains("no ACE3.BIN"), "{error}");
        // A cue sheet names its image.
        let cue = folder.join("disc.cue");
        fs::write(&cue, "FILE \"disc.bin\" BINARY\n").unwrap();
        assert_eq!(read_root_file(&cue, "README.TXT").unwrap(), b"hello");
        fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn boot_lines_in_their_usual_spellings() {
        for (text, name) in [
            ("BOOT = cdrom:\\SLPS_020.20;1", "SLPS_020.20"),
            ("boot=cdrom:SCES_020.66;1\n", "SCES_020.66"),
            ("TCB = 4\nBOOT\t=\tcdrom:\\\\EXE\\MAIN.EXE;1 \n", "MAIN.EXE"),
            ("BOOT = cdrom0:/SLPS_020.21", "SLPS_020.21"),
        ] {
            assert_eq!(boot_name(text).as_deref(), Some(name), "{text:?}");
        }
        assert_eq!(boot_name("TCB = 4\n"), None);
    }

    #[test]
    fn unknown_and_broken_images_say_why() {
        let (volume, boot) = identity(image("OTHERGAME", Some(&config("SLUS_000.01")))).unwrap();
        assert_eq!(Version::of(&volume, &boot), None);
        let folder = std::env::temp_dir().join(format!("ac3-version-{}-u", std::process::id()));
        fs::create_dir_all(&folder).unwrap();
        let path = folder.join("other.bin");
        fs::write(&path, image("OTHERGAME", Some(&config("SLUS_000.01")))).unwrap();
        let error = identify(&path).unwrap_err();
        assert!(matches!(error, VersionError::Unknown { .. }));
        let text = error.to_string();
        assert!(
            text.contains("\"OTHERGAME\"") && text.contains("SLUS_000.01"),
            "{text}"
        );
        assert!(text.contains("ACE3ES1 / SLPS_020.20"), "{text}");
        // No SYSTEM.CNF, no ISO descriptor, a short file, a cooked image.
        fs::write(&path, image("ACE3ES", None)).unwrap();
        assert!(
            identify(&path)
                .unwrap_err()
                .to_string()
                .contains("no SYSTEM.CNF")
        );
        let mut raw = image("ACE3ES", Some(&config("SCES_020.66")));
        raw[16 * RAW_SECTOR_BYTES as usize + 25] = b'X';
        fs::write(&path, &raw).unwrap();
        assert!(matches!(
            identify(&path),
            Err(VersionError::NotADisc { .. })
        ));
        fs::write(&path, &raw[..1000]).unwrap();
        assert!(matches!(
            identify(&path),
            Err(VersionError::NotADisc { .. })
        ));
        fs::write(&path, vec![0u8; 40 * 2048]).unwrap();
        assert!(matches!(
            identify(&path),
            Err(VersionError::NotADisc { .. })
        ));
        assert!(matches!(
            identify(&folder.join("missing.bin")),
            Err(VersionError::Io { .. })
        ));
        fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn disc_sets_find_the_other_japanese_disc() {
        let folder = std::env::temp_dir().join(format!("ac3-version-{}-s", std::process::id()));
        let _ = fs::remove_dir_all(&folder);
        fs::create_dir_all(&folder).unwrap();
        let name =
            |disc: u8| format!("Ace Combat 3 - Electrosphere (Japan, Asia) (Disc {disc}) (Rev 1)");
        let write = |file: &str, volume: &str, boot: &str| {
            let path = folder.join(file);
            fs::write(&path, image(volume, Some(&config(boot)))).unwrap();
            path
        };
        let one = write(&format!("{}.bin", name(1)), "ACE3ES1", "SLPS_020.20");
        // Only disc 1: a clear error naming the file it looked for.
        let error = DiscSet::find(&one).unwrap_err().to_string();
        assert!(
            error.contains("disc 2 was not found") && error.contains("(Disc 2) (Rev 1).bin"),
            "{error}"
        );
        let two = write(&format!("{}.bin", name(2)), "ACE3ES2", "SLPS_020.21");
        let expected = DiscSet {
            version: Version::Jp { disc: 1 },
            discs: vec![one.clone(), two.clone()],
        };
        assert_eq!(DiscSet::find(&one).unwrap(), expected);
        assert_eq!(DiscSet::find(&two).unwrap(), expected);
        // Cue sheets name their images; the set keeps the given paths.
        let cue = folder.join(format!("{}.cue", name(1)));
        fs::write(
            &cue,
            format!(
                "FILE \"{}.bin\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n",
                name(1)
            ),
        )
        .unwrap();
        fs::write(
            folder.join(format!("{}.cue", name(2))),
            format!("FILE \"{}.bin\" BINARY\n", name(2)),
        )
        .unwrap();
        assert_eq!(identify(&cue).unwrap().version, Version::Jp { disc: 1 });
        let set = DiscSet::find(&cue).unwrap();
        assert_eq!(set.discs[1], folder.join(format!("{}.cue", name(2))));
        // Explicit paths, in any order; mixed or incomplete sets fail.
        assert_eq!(
            DiscSet::from_paths(&[two.clone(), one.clone()]).unwrap(),
            expected
        );
        let pal = write("europe.bin", "ACE3ES", "SCES_020.66");
        assert_eq!(
            DiscSet::find(&pal).unwrap(),
            DiscSet {
                version: Version::Pal,
                discs: vec![pal.clone()]
            }
        );
        for paths in [
            vec![one.clone()],
            vec![one.clone(), pal.clone()],
            vec![one.clone(), one.clone()],
        ] {
            let error = DiscSet::from_paths(&paths).unwrap_err();
            assert!(
                error.to_string().starts_with("not one game's discs"),
                "{error}"
            );
        }
        // Names without "(Disc N)": the folder is searched.
        fs::remove_file(&two).unwrap();
        fs::remove_file(folder.join(format!("{}.cue", name(2)))).unwrap();
        let renamed = write("second.bin", "ACE3ES2", "SLPS_020.21");
        assert_eq!(
            DiscSet::find(&one).unwrap().discs,
            vec![one.clone(), renamed]
        );
        assert_eq!(
            sibling_name(Path::new("a (DISC 2).bin"), 1),
            Some(PathBuf::from("a (DISC 1).bin"))
        );
        assert_eq!(sibling_name(Path::new("a (Disc 3).bin"), 1), None);
        fs::remove_dir_all(&folder).unwrap();
    }

    /// The real images: the PAL disc in `AC3_DATA_DIR`, the Japanese ones in
    /// `AC3_JP_DIR` (default `~/ac3-jp`) `/discs`.
    #[test]
    #[ignore = "needs the PAL disc image (AC3_DATA_DIR) and the Japanese discs (AC3_JP_DIR)"]
    fn real_discs() {
        let data = PathBuf::from(std::env::var_os("AC3_DATA_DIR").expect("AC3_DATA_DIR"));
        let pal = data.join("Ace Combat 3 - Electrosphere (Europe) (En,Fr,De,Es,It).cue");
        let identity = identify(&pal).unwrap();
        assert_eq!(
            (
                identity.version,
                identity.volume.as_str(),
                identity.boot.as_str()
            ),
            (Version::Pal, "ACE3ES", "SCES_020.66")
        );
        assert_eq!(DiscSet::find(&pal).unwrap().discs, vec![pal]);
        let jp = std::env::var_os("AC3_JP_DIR")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join("ac3-jp")))
            .unwrap()
            .join("discs");
        let disc = |n: u8, extension: &str| {
            jp.join(format!(
                "Ace Combat 3 - Electrosphere (Japan, Asia) (Disc {n}) (Rev 1).{extension}"
            ))
        };
        for (n, volume, boot) in [(1, "ACE3ES1", "SLPS_020.20"), (2, "ACE3ES2", "SLPS_020.21")] {
            for extension in ["bin", "cue"] {
                let identity = identify(&disc(n, extension)).unwrap();
                assert_eq!(identity.version, Version::Jp { disc: n });
                assert_eq!(
                    (identity.volume.as_str(), identity.boot.as_str()),
                    (volume, boot)
                );
            }
        }
        let set = DiscSet::find(&disc(2, "bin")).unwrap();
        assert_eq!(set.version, Version::Jp { disc: 1 });
        assert_eq!(set.discs, vec![disc(1, "bin"), disc(2, "bin")]);
        assert_eq!(
            DiscSet::find(&disc(1, "cue")).unwrap().discs,
            vec![disc(1, "cue"), disc(2, "cue")]
        );
        // The resident straight from each disc is the extracted one, and the
        // game files load from the image (the native player's JP path).
        for n in [1u8, 2] {
            let resident = read_root_file(&disc(n, "cue"), "ACE3.BIN").unwrap();
            assert_eq!(resident.len(), crate::assets::jp::RESIDENT_BYTES);
            let extracted = jp
                .parent()
                .unwrap()
                .join(format!("extracted/disc{n}/disc/ACE3.BIN"));
            if let Ok(bytes) = fs::read(&extracted) {
                assert_eq!(resident, bytes, "disc {n}");
            }
            let data = crate::assets::GameData::load_jp_disc(&disc(n, "bin")).unwrap();
            assert_eq!(data.version, Version::Jp { disc: n });
            assert_eq!(data.executable.payload.len(), 0xad000);
        }
    }
}
