//! we-core: parsing of Wallpaper Engine (Steam appid 431960) wallpapers.
//!
//! * [`Project`] — a single wallpaper directory with `project.json`.
//! * [`Properties`] — `general.properties` (user-tunable settings) with
//!   lenient parsing, validation and conversion to the JSON shape expected by
//!   `window.wallpaperPropertyListener.applyUserProperties`.
//! * [`Catalog`] — scanning Steam library folders / arbitrary directories.

mod catalog;
mod error;
mod lenient;
mod project;
mod property;

pub use catalog::{default_steam_roots, steam_library_folders, workshop_dirs, Catalog, ScanError, WORKSHOP_APPID};
pub use error::{Error, Result};
pub use project::{Project, ProjectType, ValidationIssue};
pub use property::{ComboOption, Properties, Property, PropertyKind, PropertyValue};
