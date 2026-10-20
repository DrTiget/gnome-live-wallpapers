//! A single wallpaper: a directory containing `project.json`.

use crate::error::{Error, Result};
use crate::lenient::{as_bool, as_string, str_field};
use crate::property::Properties;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProjectType {
    Video,
    Web,
    Scene,
    /// Windows executables — never supported on Linux, listed for completeness.
    Application,
    /// Settings preset referring to another wallpaper via `dependency`.
    Preset,
    Unknown(String),
}

impl ProjectType {
    pub fn parse(s: &str) -> ProjectType {
        match s.trim().to_ascii_lowercase().as_str() {
            "video" => ProjectType::Video,
            "web" => ProjectType::Web,
            "scene" => ProjectType::Scene,
            "application" => ProjectType::Application,
            "preset" => ProjectType::Preset,
            other => ProjectType::Unknown(other.to_string()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            ProjectType::Video => "video",
            ProjectType::Web => "web",
            ProjectType::Scene => "scene",
            ProjectType::Application => "application",
            ProjectType::Preset => "preset",
            ProjectType::Unknown(s) => s,
        }
    }

    /// Whether this project type can be rendered by our daemon.
    pub fn is_supported(&self) -> bool {
        matches!(self, ProjectType::Video | ProjectType::Web | ProjectType::Scene | ProjectType::Preset)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ValidationIssue {
    MissingEntryFile,
    EntryFileNotFound(PathBuf),
    PreviewNotFound(PathBuf),
    UnsupportedType(String),
    PresetWithoutDependency,
    /// Path in project.json escapes the project directory.
    PathEscapesRoot(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    /// Directory containing project.json.
    pub root: PathBuf,
    pub title: String,
    pub description: Option<String>,
    pub kind: ProjectType,
    /// Entry file relative to root (video file, index.html, scene.json/scene.pkg).
    pub file: Option<String>,
    pub preview: Option<String>,
    pub workshop_id: Option<String>,
    pub tags: Vec<String>,
    pub content_rating: Option<String>,
    pub version: Option<i64>,
    /// `general.supportsaudioprocessing` — wallpaper wants the audio spectrum.
    pub supports_audio: bool,
    /// For presets: workshop id of the base wallpaper.
    pub dependency: Option<String>,
    /// For presets: property values to apply on top of the base wallpaper.
    pub preset_values: Map<String, Value>,
    pub properties: Properties,
    /// `general.localization`: lang -> (key -> text).
    pub localization: BTreeMap<String, BTreeMap<String, String>>,
}

fn is_safe_relative(p: &str) -> bool {
    let path = Path::new(p);
    !path.is_absolute() && path.components().all(|c| matches!(c, std::path::Component::Normal(_) | std::path::Component::CurDir))
}

impl Project {
    /// Load `<dir>/project.json`.
    pub fn load(dir: impl AsRef<Path>) -> Result<Project> {
        let dir = dir.as_ref();
        let path = dir.join("project.json");
        let bytes = std::fs::read(&path).map_err(|source| Error::Io { path: path.clone(), source })?;
        // Some files carry a UTF-8 BOM.
        let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
        let value: Value = serde_json::from_slice(bytes).map_err(|source| Error::Json { path, source })?;
        Project::from_value(&value, dir)
    }

    pub fn from_json_str(s: &str, root: impl AsRef<Path>) -> Result<Project> {
        let value: Value = serde_json::from_str(s.trim_start_matches('\u{feff}'))
            .map_err(|source| Error::Json { path: root.as_ref().join("project.json"), source })?;
        Project::from_value(&value, root)
    }

    pub fn from_value(v: &Value, root: impl AsRef<Path>) -> Result<Project> {
        if !v.is_object() {
            return Err(Error::NotAnObject);
        }
        let root = root.as_ref().to_path_buf();
        let general = v.get("general");
        let dir_name = root.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();

        let mut kind = str_field(v, "type").map(|t| ProjectType::parse(&t));
        if kind.is_none() && v.get("preset").is_some() {
            kind = Some(ProjectType::Preset);
        }
        // Old projects sometimes omit "type": infer it from the entry file.
        let file = str_field(v, "file");
        let kind = kind.unwrap_or_else(|| match file.as_deref().map(|f| f.rsplit('.').next().unwrap_or("").to_ascii_lowercase()) {
            Some(ext) if ["mp4", "webm", "mkv", "avi", "mov", "m4v", "gif"].contains(&ext.as_str()) => ProjectType::Video,
            Some(ext) if ext == "html" || ext == "htm" => ProjectType::Web,
            Some(ext) if ext == "json" || ext == "pkg" => ProjectType::Scene,
            _ => ProjectType::Unknown(String::new()),
        });

        let tags = v
            .get("tags")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(as_string).collect())
            .unwrap_or_default();

        let localization = general
            .and_then(|g| g.get("localization"))
            .and_then(Value::as_object)
            .map(|langs| {
                langs
                    .iter()
                    .filter_map(|(lang, m)| {
                        let m = m.as_object()?;
                        Some((
                            lang.to_ascii_lowercase(),
                            m.iter().filter_map(|(k, t)| Some((k.clone(), as_string(t)?))).collect(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(Project {
            title: str_field(v, "title").unwrap_or_else(|| dir_name.clone()),
            description: str_field(v, "description"),
            kind,
            file,
            preview: str_field(v, "preview"),
            workshop_id: str_field(v, "workshopid").filter(|s| s != "0"),
            tags,
            content_rating: str_field(v, "contentrating"),
            version: v.get("version").and_then(crate::lenient::as_f64).map(|f| f as i64),
            supports_audio: general
                .and_then(|g| g.get("supportsaudioprocessing"))
                .and_then(as_bool)
                .unwrap_or(false),
            dependency: str_field(v, "dependency"),
            preset_values: v.get("preset").and_then(Value::as_object).cloned().unwrap_or_default(),
            properties: Properties::parse(general.and_then(|g| g.get("properties"))),
            localization,
            root,
        })
    }

    /// Stable identifier: workshop id, or the directory name for local projects.
    pub fn id(&self) -> String {
        self.workshop_id.clone().unwrap_or_else(|| {
            self.root.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
        })
    }

    fn resolve(&self, rel: &Option<String>) -> Option<PathBuf> {
        rel.as_deref().filter(|p| is_safe_relative(p)).map(|p| self.root.join(p))
    }

    pub fn entry_path(&self) -> Option<PathBuf> {
        self.resolve(&self.file)
    }

    pub fn preview_path(&self) -> Option<PathBuf> {
        self.resolve(&self.preview)
    }

    /// Scenes may be packed into `scene.pkg` (handled by linux-wallpaperengine).
    pub fn is_packaged_scene(&self) -> bool {
        self.kind == ProjectType::Scene && self.root.join("scene.pkg").is_file()
    }

    /// Translate a UI string like `ui_browse_properties_scheme_color` using
    /// `general.localization` (`lang` e.g. "ru-ru"; falls back to "en-us").
    pub fn localize<'a>(&'a self, text: &'a str, lang: &str) -> &'a str {
        let lang = lang.to_ascii_lowercase();
        let short = lang.split(['-', '_']).next().unwrap_or("");
        let lookup = |l: &str| self.localization.get(l).and_then(|m| m.get(text));
        if let Some(t) = lookup(&lang).or_else(|| lookup(short)) {
            return t;
        }
        // Same language, other region ("ru" vs "ru-ru").
        if !short.is_empty() {
            let prefix = format!("{short}-");
            for (l, m) in &self.localization {
                if l.starts_with(&prefix) {
                    if let Some(t) = m.get(text) {
                        return t;
                    }
                }
            }
        }
        if let Some(t) = lookup("en-us").or_else(|| lookup("en")) {
            return t;
        }
        text
    }

    /// Check that the project is usable; returns all problems found.
    pub fn validate(&self) -> Vec<ValidationIssue> {
        let mut issues = Vec::new();
        for p in [&self.file, &self.preview].into_iter().flatten() {
            if !is_safe_relative(p) {
                issues.push(ValidationIssue::PathEscapesRoot(p.clone()));
            }
        }
        if !self.kind.is_supported() {
            issues.push(ValidationIssue::UnsupportedType(self.kind.as_str().to_string()));
        }
        match self.kind {
            ProjectType::Preset => {
                if self.dependency.is_none() {
                    issues.push(ValidationIssue::PresetWithoutDependency);
                }
            }
            ProjectType::Scene if self.is_packaged_scene() => {}
            _ => match (&self.file, self.entry_path()) {
                (None, _) => issues.push(ValidationIssue::MissingEntryFile),
                (Some(_), Some(p)) if !p.is_file() => issues.push(ValidationIssue::EntryFileNotFound(p)),
                _ => {}
            },
        }
        if let Some(p) = self.preview_path() {
            if !p.is_file() {
                issues.push(ValidationIssue::PreviewNotFound(p));
            }
        }
        issues
    }
}
