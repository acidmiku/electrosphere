//! The game discs the launcher plays: the Japanese release's two disc
//! images, told by their contents (`version::identify`: the volume ID and
//! the boot file), whatever their names.
//!
//! Where: a disc named on the command line or in `[paths] disc` (kept for
//! those who keep their discs elsewhere), else every disc image in the
//! settings file's folder (next to the launcher; Application Support for a
//! macOS app). Disc 1's resident executable is
//! checked as the player checks it, so another revision is told before
//! Play. The status says what was found and, when something is missing,
//! what the port expects ([`EXPECTED`]).
use crate::assets::jp::{RESIDENT, RESIDENT_BYTES};
use crate::version::{Version, identify, image_path, read_root_file};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// What the port plays, for the player.
pub const EXPECTED: &str = "Ace Combat 3: Electrosphere, the Japanese release \
    (Rev 1, two discs, SLPS-02020 and SLPS-02021) with the English translation \
    patch, as disc images: a .cue sheet and its .bin, or a .bin, for each disc";

/// Image extensions looked at, in order: a cue sheet before the image it
/// names.
const EXTENSIONS: [&str; 4] = ["cue", "bin", "iso", "img"];

/// How ready the discs are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Readiness {
    /// Both discs.
    Ready,
    /// Disc 1 only: the game plays until it asks for disc 2.
    Partial,
    /// Nothing the game can boot.
    Missing,
}

/// The discs found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Discs {
    pub disc1: Option<PathBuf>,
    pub disc2: Option<PathBuf>,
    /// Disc 1's resident executable is not the one this port runs.
    pub disc1_unsupported: bool,
    /// Discs of another release (the PAL disc).
    pub other: Vec<(PathBuf, Version)>,
    /// Where they were looked for.
    pub folder: PathBuf,
    /// Named in the settings or on the command line (not searched for).
    pub configured: bool,
}

impl Discs {
    /// The disc images in `folder`.
    pub fn scan(folder: &Path) -> Self {
        let mut found = Self {
            folder: folder.to_owned(),
            ..Self::default()
        };
        let mut paths: Vec<PathBuf> = std::fs::read_dir(folder)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.path())
                    .filter(|path| path.is_file() && rank(path).is_some())
                    .collect()
            })
            .unwrap_or_default();
        paths.sort_by_key(|path| (rank(path), path.file_name().map(|n| n.to_owned())));
        let mut covered = Vec::new();
        for path in paths {
            if covered.contains(&path) {
                continue;
            }
            if rank(&path) == Some(0)
                && let Ok(image) = image_path(&path)
            {
                covered.push(image);
            }
            found.add(path);
        }
        found
    }

    /// The disc `disc` (and `disc2`, else disc 2 searched next to it), as
    /// named in the settings or on the command line.
    pub fn named(disc: &Path, disc2: Option<&Path>) -> Self {
        let folder = disc.parent().unwrap_or(Path::new("")).to_owned();
        let mut found = Self {
            folder: folder.clone(),
            configured: true,
            ..Self::default()
        };
        found.add(disc.to_owned());
        match disc2 {
            Some(disc2) => found.add(disc2.to_owned()),
            None => {
                if found.disc2.is_none() {
                    found.disc2 = Self::scan(&folder).disc2;
                }
            }
        }
        found
    }

    fn add(&mut self, path: PathBuf) {
        let Ok(identity) = identify(&path) else {
            return;
        };
        match identity.version {
            Version::Jp { disc: 1 } if self.disc1.is_none() => self.disc1 = Some(path),
            Version::Jp { disc: 2 } if self.disc2.is_none() => self.disc2 = Some(path),
            Version::Jp { .. } => {}
            other => self.other.push((path, other)),
        }
    }

    /// Check disc 1's resident executable (its size, as the player checks
    /// it): another revision is unsupported.
    pub fn check_resident(&mut self) {
        self.disc1_unsupported = self.disc1.as_ref().is_some_and(|disc| {
            read_root_file(disc, RESIDENT).map_or(true, |exe| exe.len() != RESIDENT_BYTES)
        });
    }

    pub fn readiness(&self) -> Readiness {
        match (&self.disc1, &self.disc2) {
            (Some(_), _) if self.disc1_unsupported => Readiness::Missing,
            (Some(_), Some(_)) => Readiness::Ready,
            (Some(_), None) => Readiness::Partial,
            (None, _) => Readiness::Missing,
        }
    }

    /// The status: a headline and what to do.
    pub fn status(&self) -> (Readiness, String, String) {
        let name = |path: &PathBuf| {
            path.file_name().map_or_else(
                || path.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            )
        };
        let readiness = self.readiness();
        let (headline, detail) = match (&self.disc1, &self.disc2) {
            (Some(disc1), _) if self.disc1_unsupported => (
                "Disc 1 is not the version this port plays".to_owned(),
                format!(
                    "{} has another executable. Expected: {EXPECTED}.",
                    name(disc1)
                ),
            ),
            (Some(disc1), Some(disc2)) => (
                "Japanese discs found: ready to fly".to_owned(),
                format!("Disc 1: {}. Disc 2: {}.", name(disc1), name(disc2)),
            ),
            (Some(disc1), None) => (
                "Disc 1 found, disc 2 missing".to_owned(),
                format!(
                    "{} plays; the game asks for disc 2 later in the story. Put its image next to disc 1.",
                    name(disc1)
                ),
            ),
            (None, Some(disc2)) => (
                "Only disc 2 found".to_owned(),
                format!(
                    "{}: the game starts from disc 1. Expected: {EXPECTED}.",
                    name(disc2)
                ),
            ),
            (None, None) => match self.other.first() {
                Some((path, version)) => (
                    "This disc is another release".to_owned(),
                    format!(
                        "{} is the {version} release; this port plays the Japanese one. Expected: {EXPECTED}.",
                        name(path)
                    ),
                ),
                None if self.configured => (
                    "The disc in the settings was not found".to_owned(),
                    format!("Expected: {EXPECTED}."),
                ),
                None => (
                    "No game discs found".to_owned(),
                    format!(
                        "Put the disc images in the game's folder, {}. Expected: {EXPECTED}.",
                        self.folder.display()
                    ),
                ),
            },
        };
        (readiness, headline, detail)
    }

    /// The options that make `ac3 machine` boot these discs (none when the
    /// settings already name them).
    pub fn game_args(&self) -> Vec<OsString> {
        let mut args = Vec::new();
        if self.configured {
            return args;
        }
        if let Some(disc1) = &self.disc1 {
            args.extend(["--disc".into(), disc1.clone().into_os_string()]);
            if let Some(disc2) = &self.disc2 {
                args.extend(["--disc2".into(), disc2.clone().into_os_string()]);
            }
        }
        args
    }
}

/// The order of `path` by its extension; `None`: not a disc image.
fn rank(path: &Path) -> Option<usize> {
    let extension = path.extension()?.to_string_lossy().to_lowercase();
    EXTENSIONS.iter().position(|e| *e == extension)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::version::test_image;

    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ac3-discs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_disc(dir: &Path, name: &str, volume: &str, boot: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, test_image(volume, Some(boot))).unwrap();
        path
    }

    #[test]
    fn both_japanese_discs_by_their_contents() {
        let dir = folder("both");
        let one = write_disc(&dir, "first.bin", "ACE3ES1", "SLPS_020.20");
        // Disc 2 behind a cue sheet; its .bin is not looked at twice.
        let two = write_disc(&dir, "second.bin", "ACE3ES2", "SLPS_020.21");
        let cue = dir.join("second.cue");
        std::fs::write(&cue, "FILE \"second.bin\" BINARY\n  TRACK 01 MODE2/2352\n").unwrap();
        std::fs::write(dir.join("notes.txt"), "not a disc").unwrap();
        std::fs::write(dir.join("broken.bin"), "nope").unwrap();
        let discs = Discs::scan(&dir);
        assert_eq!(discs.disc1, Some(one.clone()));
        assert_eq!(discs.disc2, Some(cue.clone()));
        assert!(discs.other.is_empty());
        assert_eq!(discs.readiness(), Readiness::Ready);
        let (_, headline, detail) = discs.status();
        assert!(headline.contains("ready"), "{headline}");
        assert!(
            detail.contains("first.bin") && detail.contains("second.cue"),
            "{detail}"
        );
        assert_eq!(
            discs.game_args(),
            [
                "--disc".into(),
                one.into_os_string(),
                "--disc2".into(),
                cue.into_os_string()
            ]
        );
        // The fake disc 1 has no resident executable: unsupported.
        let mut checked = discs.clone();
        checked.check_resident();
        assert_eq!(checked.readiness(), Readiness::Missing);
        assert!(checked.status().1.contains("not the version"));
        let _ = two;
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn what_is_missing_is_said() {
        let dir = folder("missing");
        let none = Discs::scan(&dir);
        assert_eq!(none.readiness(), Readiness::Missing);
        let (_, headline, detail) = none.status();
        assert!(headline.contains("No game discs"), "{headline}");
        assert!(
            detail.contains("Japanese release") && detail.contains("Rev 1"),
            "{detail}"
        );
        assert!(none.game_args().is_empty());
        let pal = write_disc(&dir, "europe.bin", "ACE3ES", "SCES_020.66");
        let other = Discs::scan(&dir);
        assert_eq!(other.other, [(pal.clone(), Version::Pal)]);
        assert!(other.status().1.contains("another release"));
        write_disc(&dir, "d2.bin", "ACE3ES2", "SLPS_020.21");
        assert!(Discs::scan(&dir).status().1.contains("Only disc 2"));
        let one = write_disc(&dir, "d1.bin", "ACE3ES1", "SLPS_020.20");
        std::fs::remove_file(dir.join("d2.bin")).unwrap();
        let partial = Discs::scan(&dir);
        assert_eq!(partial.readiness(), Readiness::Partial);
        assert!(partial.status().1.contains("disc 2 missing"));
        // Named in the settings: disc 2 is searched next to it; the game
        // reads the settings itself.
        write_disc(&dir, "other-name.bin", "ACE3ES2", "SLPS_020.21");
        let named = Discs::named(&one, None);
        assert!(named.configured);
        assert_eq!(named.readiness(), Readiness::Ready);
        assert!(named.game_args().is_empty());
        std::fs::create_dir_all(dir.join("empty")).unwrap();
        let missing = Discs::named(&dir.join("empty/gone.cue"), None);
        assert!(missing.status().1.contains("settings was not found"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
