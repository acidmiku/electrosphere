//! The bundled CRT looks: ten presets of Lola Retro (MIT,
//! `shaders/lola-retro`, see its README), five looks each curved and flat.
//! `[display] crt` turns them on, `crt_preset` names the look and
//! `crt_curved` picks the glass. While playing, F2 turns the CRT off and on,
//! Shift+F3 steps to the next look and F4 swaps curved and flat
//! ([`super::Presenter`]). A preset of one's own (`shader_preset`) takes
//! their place.
use std::path::{Path, PathBuf};

/// The bundle's folder, relative to the settings file's folder (the
/// Windows build ships it next to the executable).
pub const FOLDER: &str = "shaders/lola-retro";

/// A look: its name and its presets in the bundle.
pub struct Look {
    pub name: &'static str,
    pub curved: &'static str,
    pub flat: &'static str,
}

/// The looks, in Shift+F3 order; the first is the default.
pub const LOOKS: [Look; 5] = [
    Look {
        name: "trinitron-s-video",
        curved: "curved/trinitron-s-video.slangp",
        flat: "flat/trinitron-s-video.slangp",
    },
    Look {
        name: "consumer-tv-composite",
        curved: "curved/consumer-tv-composite.slangp",
        flat: "flat/consumer-tv-composite.slangp",
    },
    Look {
        name: "dos-pc",
        curved: "systems/curved/dos-pc.slangp",
        flat: "systems/flat/dos-pc.slangp",
    },
    Look {
        name: "gamecube",
        curved: "systems/curved/gamecube.slangp",
        flat: "systems/flat/gamecube.slangp",
    },
    Look {
        name: "playstation",
        curved: "systems/curved/playstation.slangp",
        flat: "systems/flat/playstation.slangp",
    },
];

/// The default look's name.
pub const DEFAULT: &str = LOOKS[0].name;

/// The look named `name`.
pub fn index(name: &str) -> Option<usize> {
    LOOKS.iter().position(|look| look.name == name.trim())
}

/// The preset file of look `index`, curved or flat, in the bundle at
/// `folder`.
pub fn path(folder: &Path, index: usize, curved: bool) -> PathBuf {
    let look = &LOOKS[index % LOOKS.len()];
    folder.join(if curved { look.curved } else { look.flat })
}

/// The look being shown: which, curved or flat, and the bundle's folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Crt {
    pub folder: PathBuf,
    pub index: usize,
    pub curved: bool,
}

impl Crt {
    /// The current preset file.
    pub fn preset(&self) -> PathBuf {
        path(&self.folder, self.index, self.curved)
    }

    /// `2/5 playstation (curved)`.
    pub fn label(&self) -> String {
        format!(
            "{}/{} {} ({})",
            self.index + 1,
            LOOKS.len(),
            LOOKS[self.index].name,
            if self.curved { "curved" } else { "flat" }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_look_is_in_the_bundle() {
        let folder = Path::new(env!("CARGO_MANIFEST_DIR")).join(FOLDER);
        for (i, look) in LOOKS.iter().enumerate() {
            assert_eq!(index(look.name), Some(i));
            for curved in [true, false] {
                let preset = path(&folder, i, curved);
                assert!(preset.is_file(), "{}", preset.display());
            }
        }
        assert_eq!(index(" playstation "), Some(4));
        assert_eq!(index("royale"), None);
        let crt = Crt {
            folder,
            index: 4,
            curved: false,
        };
        assert_eq!(crt.label(), "5/5 playstation (flat)");
        assert!(crt.preset().ends_with("systems/flat/playstation.slangp"));
    }
}
