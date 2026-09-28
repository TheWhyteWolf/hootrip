//! Archive-level catalogue: hoot.xml + all child gamelists + set folders on disk.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::model::{Bind, Game, GameList};
use crate::parse::parse_gamelist_file;

/// Platform data folders scanned by hoot, from the `data_dir=` line of
/// hoot.ini (fallback when the ini is absent or unreadable).
pub const DEFAULT_DATA_DIRS: &[&str] = &[
    "fm7", "fmtowns", "mv", "msx", "pc", "pc88", "pc88va", "pc98", "pc9821", "pico", "roms",
    "sc3000", "smd", "x1", "x68k",
];

#[derive(Debug)]
pub struct Catalogue {
    pub root: PathBuf,
    /// Extension → driver auto-detection rules from hoot.xml.
    pub binds: Vec<Bind>,
    /// All `<game>` entries with their source list file (archive-relative).
    pub games: Vec<(PathBuf, Game)>,
    /// Gamelist files that were referenced but failed to load/parse.
    pub errors: Vec<(PathBuf, String)>,
    /// Set folders found on disk: platform dir → set folder names.
    pub set_dirs: BTreeMap<String, Vec<String>>,
}

impl Catalogue {
    /// Load the full catalogue from an unpacked hoot archive root.
    pub fn load(root: &Path) -> Result<Self> {
        let master_path = root.join("hoot.xml");
        let master = parse_gamelist_file(&master_path)
            .with_context(|| format!("loading master list {}", master_path.display()))?;

        let mut cat = Catalogue {
            root: root.to_path_buf(),
            binds: master.binds.clone(),
            games: Vec::new(),
            errors: Vec::new(),
            set_dirs: BTreeMap::new(),
        };
        cat.absorb(PathBuf::from("hoot.xml"), &master);

        for rel in &master.childlists {
            let rel_path = PathBuf::from(rel);
            match parse_gamelist_file(&root.join(&rel_path)) {
                Ok(list) => cat.absorb(rel_path, &list),
                Err(e) => cat.errors.push((rel_path, format!("{e:#}"))),
            }
        }

        cat.scan_set_dirs(&data_dirs(root));
        Ok(cat)
    }

    fn absorb(&mut self, source: PathBuf, list: &GameList) {
        self.binds.extend(list.binds.iter().cloned());
        for g in &list.games {
            self.games.push((source.clone(), g.clone()));
        }
        // Nested childlists beyond hoot.xml's are not used by the archive;
        // record any so we notice if that assumption breaks.
        if source != Path::new("hoot.xml") && !list.childlists.is_empty() {
            self.errors.push((
                source,
                format!("unexpected nested childlists ({})", list.childlists.len()),
            ));
        }
    }

    fn scan_set_dirs(&mut self, dirs: &[String]) {
        for dir in dirs {
            let full = self.root.join(dir);
            let Ok(rd) = std::fs::read_dir(&full) else {
                continue;
            };
            let mut sets: Vec<String> = rd
                .flatten()
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            sets.sort();
            self.set_dirs.insert(dir.clone(), sets);
        }
    }

    /// Find the on-disk folder for a game's `romlist archive=` name.
    /// hoot scans all data dirs, so the archive name is matched in each.
    ///
    /// For a multi-archive set this is only the *first* folder; a rom may live
    /// in any of them, so anything that reads set files wants
    /// [`find_set_dirs`](Self::find_set_dirs).
    pub fn find_set_dir(&self, archive: &str) -> Option<PathBuf> {
        self.find_set_dirs(archive).into_iter().next()
    }

    /// Resolve a `romlist archive=` attribute to every folder it names.
    ///
    /// The attribute is a comma-separated *list*, and a set's roms are spread
    /// across all of them: `Emerald Dragon (OPNA)` declares `emdr88,emdr98`
    /// and keeps 47 of its 48 roms in `emdr88` with `EMVI64.S` only in
    /// `emdr98`. The MSX sets use the same mechanism to pull in the FM-PAC
    /// BIOS alongside the game (`<game>_msx,fmpac_msx`). Comparing the whole
    /// attribute against a folder name matches nothing for these, which
    /// silently dropped the set as if its archive were missing.
    ///
    /// Names that resolve to no folder are skipped rather than failing the
    /// lot: a set whose optional second archive is absent should still load
    /// what it has.
    pub fn find_set_dirs(&self, archive: &str) -> Vec<PathBuf> {
        archive
            .split(',')
            .map(str::trim)
            .filter(|a| !a.is_empty())
            .filter_map(|a| self.find_one_set_dir(a))
            .collect()
    }

    fn find_one_set_dir(&self, archive: &str) -> Option<PathBuf> {
        let lower = archive.to_lowercase();
        for (dir, sets) in &self.set_dirs {
            if let Some(name) = sets.iter().find(|s| s.to_lowercase() == lower) {
                return Some(self.root.join(dir).join(name));
            }
        }
        None
    }

    /// Map a file extension to its bind rule, if any.
    pub fn bind_for_ext(&self, ext: &str) -> Option<&Bind> {
        let ext = ext.to_lowercase();
        self.binds
            .iter()
            .find(|b| b.exts.iter().any(|e| *e == ext))
    }
}

/// Read `data_dir=a;b;c` from hoot.ini, falling back to the known default.
fn data_dirs(root: &Path) -> Vec<String> {
    let ini = root.join("hoot.ini");
    if let Ok(bytes) = std::fs::read(&ini) {
        let text = crate::parse::decode_bytes(&bytes);
        for line in text.lines() {
            let line = line.trim();
            if let Some(v) = line.strip_prefix("data_dir=") {
                return v
                    .split(';')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
        }
    }
    DEFAULT_DATA_DIRS.iter().map(|s| s.to_string()).collect()
}
