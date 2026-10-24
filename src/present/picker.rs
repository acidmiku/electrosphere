//! Debug: stepping through a pack of shader presets with Shift+F3, to pick
//! one by eye. It has no setting of its own and is not meant to ship:
//! `[display] shader_preset` naming a folder starts at the first `.slangp`
//! below it; naming a file, Shift+F3 steps through the presets below that
//! file's folder. Each step shows the preset's number and path on screen.
use std::path::{Path, PathBuf};

/// Folder levels searched below the pack's folder.
const DEPTH: usize = 8;

/// A pack's presets, in path order, and the one shown.
pub struct Picker {
    root: PathBuf,
    presets: Vec<PathBuf>,
    index: usize,
}

impl Picker {
    /// The pack for `preset` (a folder, or a `.slangp` file in one); `None`
    /// when it holds no presets or `preset` is another kind of file.
    pub fn new(preset: &Path) -> Option<Self> {
        let (root, start) = if preset.is_dir() {
            (preset.to_owned(), None)
        } else {
            (preset.parent()?.to_owned(), Some(preset))
        };
        let mut presets = Vec::new();
        collect(&root, &mut presets, DEPTH);
        presets.sort();
        let index = match start {
            Some(start) => presets.iter().position(|p| p == start)?,
            None => 0,
        };
        (!presets.is_empty()).then_some(Self {
            root,
            presets,
            index,
        })
    }

    /// The preset shown.
    pub fn current(&self) -> &Path {
        &self.presets[self.index]
    }

    /// Step to the next preset (after the last, the first).
    pub fn step(&mut self) -> &Path {
        self.index = (self.index + 1) % self.presets.len();
        self.current()
    }

    /// `5/92 curved/old-tv-rf.slangp`: the preset's number and its path in
    /// the pack.
    pub fn label(&self) -> String {
        let current = self.current();
        let name: Vec<_> = current
            .strip_prefix(&self.root)
            .unwrap_or(current)
            .components()
            .map(|part| part.as_os_str().to_string_lossy())
            .collect();
        format!(
            "{}/{} {}",
            self.index + 1,
            self.presets.len(),
            name.join("/")
        )
    }
}

/// Every `.slangp` below `folder` (hidden folders skipped).
fn collect(folder: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for path in entries.flatten().map(|entry| entry.path()) {
        if path.is_dir() {
            let hidden = path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with('.'));
            if depth > 0 && !hidden {
                collect(&path, out, depth - 1);
            }
        } else if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("slangp"))
        {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_through_a_packs_presets() {
        let root = std::env::temp_dir().join(format!("ac3-picker-{}", std::process::id()));
        for name in [
            "b/two.slangp",
            "a/one.slangp",
            "a/notes.txt",
            ".git/x.slangp",
            "c.slangp",
        ] {
            let path = root.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "").unwrap();
        }
        let mut folder = Picker::new(&root).unwrap();
        assert_eq!(folder.label(), "1/3 a/one.slangp");
        folder.step();
        assert_eq!(folder.label(), "2/3 b/two.slangp");
        folder.step();
        assert_eq!(folder.label(), "3/3 c.slangp");
        assert_eq!(folder.step(), root.join("a/one.slangp"));
        // A file starts at itself, among its folder's presets.
        let file = Picker::new(&root.join("b/two.slangp")).unwrap();
        assert_eq!(file.label(), "1/1 two.slangp");
        assert!(Picker::new(&root.join("a/notes.txt")).is_none());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
