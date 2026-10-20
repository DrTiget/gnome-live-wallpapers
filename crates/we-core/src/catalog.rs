//! Discovering wallpapers in Steam workshop folders and local directories.

use crate::error::Error;
use crate::project::{Project, ProjectType};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const WORKSHOP_APPID: &str = "431960";

/// Typical Steam installation roots for native, Flatpak and Snap Steam.
pub fn default_steam_roots(home: &Path) -> Vec<PathBuf> {
    [
        ".steam/steam",
        ".local/share/Steam",
        ".var/app/com.valvesoftware.Steam/.local/share/Steam",
        "snap/steam/common/.local/share/Steam",
    ]
    .iter()
    .map(|p| home.join(p))
    .collect()
}

/// Library paths listed in `<steam>/steamapps/libraryfolders.vdf`
/// (plus the steam root itself).
pub fn steam_library_folders(steam_root: &Path) -> Vec<PathBuf> {
    let mut out = vec![steam_root.to_path_buf()];
    let vdf = steam_root.join("steamapps/libraryfolders.vdf");
    if let Ok(text) = std::fs::read_to_string(vdf) {
        for line in text.lines() {
            // Line format:   "path"		"/mnt/games/SteamLibrary"
            let toks: Vec<&str> = line.split('"').collect();
            if toks.len() >= 4 && toks[1].eq_ignore_ascii_case("path") {
                let p = PathBuf::from(toks[3].replace("\\\\", "\\"));
                if !out.contains(&p) {
                    out.push(p);
                }
            }
        }
    }
    out
}

/// All existing `steamapps/workshop/content/431960` directories, deduplicated
/// by canonical path (`~/.steam/steam` is usually a symlink).
pub fn workshop_dirs(home: &Path) -> Vec<PathBuf> {
    steam_app_dirs(home, |lib| lib.join("steamapps/workshop/content").join(WORKSHOP_APPID))
}

/// Existing `steamapps/common/wallpaper_engine` installs (the Wallpaper Engine
/// app itself: `assets/` for scenes, `projects/defaultprojects/`).
pub fn wallpaper_engine_dirs(home: &Path) -> Vec<PathBuf> {
    steam_app_dirs(home, |lib| lib.join("steamapps/common/wallpaper_engine"))
}

fn steam_app_dirs(home: &Path, sub: impl Fn(&Path) -> PathBuf) -> Vec<PathBuf> {
    let mut seen = Vec::new();
    let mut out = Vec::new();
    for root in default_steam_roots(home) {
        for lib in steam_library_folders(&root) {
            let d = sub(&lib);
            if let Ok(c) = d.canonicalize() {
                if c.is_dir() && !seen.contains(&c) {
                    seen.push(c);
                    out.push(d);
                }
            }
        }
    }
    out
}

#[derive(Debug)]
pub struct ScanError {
    pub dir: PathBuf,
    pub error: Error,
}

#[derive(Debug, Default)]
pub struct Catalog {
    pub projects: Vec<Project>,
    pub errors: Vec<ScanError>,
}

impl Catalog {
    /// Scan each `root`: every immediate subdirectory containing project.json
    /// is a wallpaper. A root that itself contains project.json is added too.
    pub fn scan<P: AsRef<Path>>(roots: &[P]) -> Catalog {
        let mut cat = Catalog::default();
        for root in roots {
            let root = root.as_ref();
            if root.join("project.json").is_file() {
                cat.add_dir(root);
                continue;
            }
            let Ok(entries) = std::fs::read_dir(root) else { continue };
            let mut dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
            dirs.sort();
            for d in dirs {
                if d.join("project.json").is_file() {
                    cat.add_dir(&d);
                }
            }
        }
        cat
    }

    /// Scan all Steam workshop folders of `home`.
    pub fn scan_steam(home: &Path) -> Catalog {
        Catalog::scan(&workshop_dirs(home))
    }

    fn add_dir(&mut self, dir: &Path) {
        match Project::load(dir) {
            Ok(p) => {
                if !self.projects.iter().any(|q| q.id() == p.id()) {
                    self.projects.push(p);
                }
            }
            Err(error) => self.errors.push(ScanError { dir: dir.to_path_buf(), error }),
        }
    }

    pub fn get(&self, id: &str) -> Option<&Project> {
        self.projects.iter().find(|p| p.id() == id)
    }

    pub fn by_type(&self, kind: &ProjectType) -> impl Iterator<Item = &Project> {
        let kind = kind.clone();
        self.projects.iter().filter(move |p| p.kind == kind)
    }

    /// Supported wallpapers only (no applications / unknown types).
    pub fn supported(&self) -> impl Iterator<Item = &Project> {
        self.projects.iter().filter(|p| p.kind.is_supported())
    }

    /// Case-insensitive search over title, description and tags.
    pub fn search<'a>(&'a self, query: &str) -> impl Iterator<Item = &'a Project> + 'a {
        let q = query.to_lowercase();
        self.projects.iter().filter(move |p| {
            q.is_empty()
                || p.title.to_lowercase().contains(&q)
                || p.description.as_deref().unwrap_or("").to_lowercase().contains(&q)
                || p.tags.iter().any(|t| t.to_lowercase().contains(&q))
        })
    }

    /// For a preset: the base project with preset values applied on top.
    /// Returns `None` if the dependency is not installed.
    pub fn resolve_preset(&self, preset: &Project) -> Option<Project> {
        let base = self.get(preset.dependency.as_deref()?)?;
        let mut p = base.clone();
        p.properties.apply_overrides(&preset.preset_values);
        p.title = preset.title.clone();
        Some(p)
    }

    /// Count of projects by type, for UI filters.
    pub fn type_counts(&self) -> HashMap<ProjectType, usize> {
        let mut m = HashMap::new();
        for p in &self.projects {
            *m.entry(p.kind.clone()).or_insert(0) += 1;
        }
        m
    }
}
