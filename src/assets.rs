//! Loads local game files at runtime; no game code or data is compiled in.
use crate::{Result, sha256, version::Version};
use ac3_core::{StateEntry, parse_state_table};
use ac3_formats::{bph, psy};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub const RESIDENT_BASE: u32 = 0x80010000;
pub const OVERLAY_BASE: u32 = 0x800c9dc0;
pub const STATE_TABLE: u32 = 0x800ae544;
pub const OVERLAY_SIZES: [usize; 5] = [70224, 196696, 85800, 63740, 7288];
pub const OVERLAY_STATES: [u32; 5] = [4, 3, 7, 8, 9];
pub const FIRST_CALLBACKS: [u32; 5] = [0x800cb2bc, 0x800cb8b4, 0x800d67d4, 0x800cd1b4, 0x800ca0a0];
const OVERLAY_HASHES: [&str; 5] = [
    "23d97413c34c6e478eec3834e853bc773c98e01278c3f953db81926811d1b1c1",
    "4380029bd3cfa8d54ff7b03898a1f600181723ea15da010545cfe4bb8d03e452",
    "eca3266050649b3eefa6c49cafad874c5a8ea4069ef66b8060489e376c3f55c9",
    "03858e03097633ee1ce4fe247c0de82530a4bb8dfe53589e0689590f4f8a729e",
    "983d19041964fcfc2284e2ad8e283bbb9c66dc22b34c3573d80370d12c11e7dc",
];

/// The Japanese release's resident and overlays (JP_PORT_PLAN §1, §2).
pub mod jp {
    pub const RESIDENT: &str = "ACE3.BIN";
    pub const RESIDENT_BYTES: usize = 710_656;
    pub const ENTRY: u32 = 0x8002_572c;
    pub const STATE_TABLE: u32 = 0x800b_0134;
    pub const STATES: usize = 9;
    pub const OVERLAY_BASE: u32 = 0x800c_bfe8;
    pub const OVERLAY_SIZES: [usize; 4] = [72340, 290380, 338656, 21860];
}

pub struct GameData {
    pub executable: psy::PsyExecutable,
    pub directory: bph::Directory,
    pub states: Vec<StateEntry>,
    /// The release the files are from ([`GameData::load`] PAL,
    /// [`GameData::load_jp`] Japanese).
    pub version: Version,
    body: Vec<u8>,
    disc_directory: PathBuf,
}

fn read_verified(dir: &Path, name: &str, size: u64, hash: &str) -> Result<Vec<u8>> {
    let path = dir.join(name);
    let metadata = fs::metadata(&path)
        .map_err(|e| format!("{}: {e}; expected extracted PAL disc files", path.display()))?;
    if metadata.len() != size {
        return Err(format!(
            "{}: expected {size} bytes, got {}",
            path.display(),
            metadata.len()
        )
        .into());
    }
    let bytes = fs::read(&path)?;
    if sha256(&bytes) != hash {
        return Err(format!(
            "{}: wrong disc revision or modified data (SHA-256 mismatch)",
            path.display()
        )
        .into());
    }
    Ok(bytes)
}

impl GameData {
    /// Intentionally rejects unvalidated disc revisions rather than reusing PAL addresses.
    pub fn load(directory: &Path) -> Result<Self> {
        let packed = read_verified(
            directory,
            "ACE.BIN",
            413772,
            "7cf6f57cb39d7828bea074f77de3d485e162eb3b18b7359e0eafcdc2671e0afe",
        )?;
        let header = read_verified(
            directory,
            "ACE.BPH",
            1036,
            "827cd43c8809b816e8848928119a46fd0528879bcaa2c8a1adea5d05d134a4aa",
        )?;
        let body = read_verified(
            directory,
            "ACE.BPB",
            34650112,
            "1dc0ac0e71c2793fb1fad8617acfc00d4756f9d6d4c5f8c9316a91163f9cb239",
        )?;
        let executable = psy::unpack(&packed)?;
        if executable.load_address != RESIDENT_BASE
            || executable.entry_pc != 0x8002521c
            || sha256(&executable.payload)
                != "1a4880bdc84bdf60fa330db051df3efa5346e589ccf841f9819082494987ce67"
        {
            return Err(
                "resident image does not match the runtime-validated PAL executable".into(),
            );
        }
        let disc_directory = directory.to_path_buf();
        let directory = bph::parse(&header, body.len())?;
        let offset = (STATE_TABLE - RESIDENT_BASE) as usize;
        let states = parse_state_table(&executable.payload[offset..offset + 10 * 16])?;
        let data = Self {
            executable,
            directory,
            states,
            version: Version::Pal,
            body,
            disc_directory,
        };
        for module in 1..=5 {
            let bytes = data.overlay(module)?;
            if bytes.len() != OVERLAY_SIZES[module - 1]
                || sha256(bytes) != OVERLAY_HASHES[module - 1]
            {
                return Err(format!("module {module}: recovered extent/hash mismatch").into());
            }
        }
        Ok(data)
    }

    /// The extracted Japanese disc 1 or 2 files (`ACE3.BIN`, a plain PS-X
    /// EXE; `ACE.BPH` / `ACE.BPB`; `ACE.SPH` for the disc number), for the
    /// asset tools. Sizes and the layout are checked, not hashes: the
    /// translation patch has more than one release.
    pub fn load_jp(directory: &Path) -> Result<Self> {
        let path = directory.join(jp::RESIDENT);
        let exe = fs::read(&path).map_err(|e| {
            format!(
                "{}: {e}; expected extracted Japanese disc files",
                path.display()
            )
        })?;
        let header = fs::read(directory.join("ACE.BPH"))?;
        let body = fs::read(directory.join("ACE.BPB"))?;
        let sph = fs::read(directory.join("ACE.SPH")).ok();
        Self::from_jp_files(&path, exe, &header, body, sph.as_deref(), directory)
    }

    /// The Japanese files read straight from a disc image (or its cue
    /// sheet): `ACE3.BIN`, `ACE.BPH`, `ACE.BPB` and `ACE.SPH` of its root
    /// directory, as [`Self::load_jp`] checks them. The native player boots
    /// the resident of the disc in the drive, as the console does.
    pub fn load_jp_disc(image: &Path) -> Result<Self> {
        use crate::version::read_root_file;
        let exe = read_root_file(image, jp::RESIDENT)?;
        let header = read_root_file(image, "ACE.BPH")?;
        let body = read_root_file(image, "ACE.BPB")?;
        let sph = read_root_file(image, "ACE.SPH").ok();
        let folder = image.parent().unwrap_or(Path::new(""));
        let name = image.join(jp::RESIDENT);
        Self::from_jp_files(&name, exe, &header, body, sph.as_deref(), folder)
    }

    /// The Japanese release from its files (`path` names `ACE3.BIN` in
    /// errors; `directory` is [`Self::disc_directory`]).
    fn from_jp_files(
        path: &Path,
        exe: Vec<u8>,
        header: &[u8],
        body: Vec<u8>,
        sph: Option<&[u8]>,
        directory: &Path,
    ) -> Result<Self> {
        if exe.len() != jp::RESIDENT_BYTES || !exe.starts_with(b"PS-X EXE") {
            return Err(format!(
                "{}: not the Japanese resident ({} bytes)",
                path.display(),
                exe.len()
            )
            .into());
        }
        let word = |at: usize| u32::from_le_bytes(exe[at..at + 4].try_into().unwrap());
        let payload = exe[0x800..].to_vec();
        let executable = psy::PsyExecutable {
            output_checksum: psy::checksum(&payload),
            payload,
            entry_pc: word(0x10),
            initial_gp: word(0x14),
            load_address: word(0x18),
            stack_address: word(0x30),
            stack_offset: word(0x34),
            stream_offset: 0,
            stream_bytes_consumed: 0,
            trailing_bytes: 0,
        };
        if executable.load_address != RESIDENT_BASE || executable.entry_pc != jp::ENTRY {
            return Err("resident image does not match the Japanese executable layout".into());
        }
        // The SPH header's type word is the disc number (JP_PORT_PLAN §1.4).
        let disc = sph
            .and_then(|sph| {
                sph.get(4..8)
                    .map(|w| u32::from_le_bytes(w.try_into().unwrap()))
            })
            .filter(|disc| matches!(disc, 1 | 2))
            .unwrap_or(1) as u8;
        let offset = (jp::STATE_TABLE - RESIDENT_BASE) as usize;
        let states = parse_state_table(&executable.payload[offset..offset + jp::STATES * 16])?;
        let data = Self {
            executable,
            directory: bph::parse(header, body.len())?,
            states,
            version: Version::Jp { disc },
            body,
            disc_directory: directory.to_path_buf(),
        };
        for module in 1..=4 {
            if data.overlay(module)?.len() != jp::OVERLAY_SIZES[module - 1] {
                return Err(format!("module {module}: unexpected Japanese overlay size").into());
            }
        }
        Ok(data)
    }

    /// The Japanese files when `directory` holds `ACE3.BIN`, else PAL.
    pub fn load_any(directory: &Path) -> Result<Self> {
        if directory.join(jp::RESIDENT).is_file() {
            Self::load_jp(directory)
        } else {
            Self::load(directory)
        }
    }

    pub fn overlay(&self, module: usize) -> Result<&[u8]> {
        let modules = match self.version {
            Version::Pal => 5,
            Version::Jp { .. } => 4,
        };
        if !(1..=modules).contains(&module) {
            return Err(format!("unsupported overlay ID {module}; expected 1..{modules}").into());
        }
        self.resource(module)
    }

    /// Location of the verified disc data; large streaming files are opened
    /// lazily and independently verified without loading them into memory.
    pub fn disc_directory(&self) -> &Path {
        &self.disc_directory
    }

    /// Original BPB resource ID; CD streaming resources use a separate namespace.
    pub fn resource(&self, id: usize) -> Result<&[u8]> {
        let entry = self
            .directory
            .entries
            .get(id)
            .ok_or("missing BPB resource directory entry")?;
        if entry.mode() != 0 {
            return Err(format!("resource {id}: expected raw resource mode").into());
        }
        let extent = entry
            .extent
            .as_ref()
            .ok_or("resource is a special directory entry")?;
        Ok(extent.bytes(&self.body)?)
    }
}
