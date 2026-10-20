//! `general.properties` of project.json.

use crate::error::{Error, Result};
use crate::lenient::{as_bool, as_f64, as_string, format_color, parse_color, str_field};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComboOption {
    pub label: String,
    pub value: String,
}

/// Typed property definition with its current value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum PropertyKind {
    /// RGB in 0..1.
    Color { value: [f32; 3] },
    Slider { value: f64, min: f64, max: f64, step: f64, fraction: bool },
    Bool { value: bool },
    Combo { value: String, options: Vec<ComboOption> },
    #[serde(rename = "textinput")]
    TextInput { value: String },
    /// Static label, no value (WE uses it for headings / descriptions).
    Text,
    /// Group header; children reference it through conditions.
    Group,
    /// User-chosen file path (web wallpapers).
    File { value: Option<String> },
    /// User-chosen directory; `mode` is "fetchall" or "ondemand".
    Directory { value: Option<String>, mode: String },
    /// Scene texture override (`scenetexture`).
    #[serde(rename = "scenetexture")]
    SceneTexture { value: Option<String> },
    /// Anything we don't know yet; kept verbatim.
    Unknown { type_name: String, value: Value },
}

/// Value as stored in user settings / sent over D-Bus.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PropertyValue {
    Bool(bool),
    Number(f64),
    Text(String),
    Null,
}

impl From<&Value> for PropertyValue {
    fn from(v: &Value) -> Self {
        match v {
            Value::Bool(b) => PropertyValue::Bool(*b),
            Value::Number(n) => PropertyValue::Number(n.as_f64().unwrap_or(0.0)),
            Value::String(s) => PropertyValue::Text(s.clone()),
            Value::Null => PropertyValue::Null,
            other => PropertyValue::Text(other.to_string()),
        }
    }
}

impl From<PropertyValue> for Value {
    fn from(v: PropertyValue) -> Self {
        match v {
            PropertyValue::Bool(b) => Value::Bool(b),
            PropertyValue::Number(n) => json!(n),
            PropertyValue::Text(s) => Value::String(s),
            PropertyValue::Null => Value::Null,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Property {
    pub key: String,
    /// Display text (may be a localisation key like `ui_browse_properties_scheme_color`).
    pub text: String,
    pub order: i64,
    /// JS-like visibility expression, e.g. `showclock.value == true`.
    pub condition: Option<String>,
    pub kind: PropertyKind,
}

impl Property {
    pub fn parse(key: &str, def: &Value) -> Property {
        let text = str_field(def, "text").unwrap_or_else(|| key.to_string());
        let order = def.get("order").and_then(as_f64).map(|f| f as i64).unwrap_or(i64::MAX);
        let condition = str_field(def, "condition");
        let type_name = str_field(def, "type").unwrap_or_default().to_ascii_lowercase();
        let value = def.get("value").cloned().unwrap_or(Value::Null);
        let kind = match type_name.as_str() {
            "color" => PropertyKind::Color { value: parse_color(&value).unwrap_or([0.0; 3]) },
            "slider" => {
                let min = def.get("min").and_then(as_f64).unwrap_or(0.0);
                let max = def.get("max").and_then(as_f64).unwrap_or(100.0).max(min);
                let fraction = def.get("fraction").and_then(as_bool).unwrap_or(false);
                let step = def
                    .get("step")
                    .and_then(as_f64)
                    .filter(|s| *s > 0.0)
                    .unwrap_or(if fraction { 0.1 } else { 1.0 });
                let value = as_f64(&value).unwrap_or(min).clamp(min, max);
                PropertyKind::Slider { value, min, max, step, fraction }
            }
            "bool" => PropertyKind::Bool { value: as_bool(&value).unwrap_or(false) },
            "combo" => {
                let options: Vec<ComboOption> = def
                    .get("options")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|o| {
                                let value = o.get("value").and_then(as_string)?;
                                let label = o.get("label").and_then(as_string).unwrap_or_else(|| value.clone());
                                Some(ComboOption { label, value })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let value = as_string(&value)
                    .or_else(|| options.first().map(|o| o.value.clone()))
                    .unwrap_or_default();
                PropertyKind::Combo { value, options }
            }
            "textinput" => PropertyKind::TextInput { value: as_string(&value).unwrap_or_default() },
            "text" => PropertyKind::Text,
            "group" => PropertyKind::Group,
            "file" => PropertyKind::File { value: as_string(&value).filter(|s| !s.is_empty()) },
            "directory" => PropertyKind::Directory {
                value: as_string(&value).filter(|s| !s.is_empty()),
                mode: str_field(def, "mode").unwrap_or_else(|| "fetchall".into()),
            },
            "scenetexture" => PropertyKind::SceneTexture { value: as_string(&value).filter(|s| !s.is_empty()) },
            _ => PropertyKind::Unknown { type_name, value },
        };
        Property { key: key.to_string(), text, order, condition, kind }
    }

    /// WE type name as used in project.json.
    pub fn type_name(&self) -> &str {
        match &self.kind {
            PropertyKind::Color { .. } => "color",
            PropertyKind::Slider { .. } => "slider",
            PropertyKind::Bool { .. } => "bool",
            PropertyKind::Combo { .. } => "combo",
            PropertyKind::TextInput { .. } => "textinput",
            PropertyKind::Text => "text",
            PropertyKind::Group => "group",
            PropertyKind::File { .. } => "file",
            PropertyKind::Directory { .. } => "directory",
            PropertyKind::SceneTexture { .. } => "scenetexture",
            PropertyKind::Unknown { type_name, .. } => type_name,
        }
    }

    /// Current value in the representation WE passes to web wallpapers.
    pub fn value_json(&self) -> Value {
        match &self.kind {
            PropertyKind::Color { value } => Value::String(format_color(*value)),
            PropertyKind::Slider { value, .. } => json!(value),
            PropertyKind::Bool { value } => Value::Bool(*value),
            PropertyKind::Combo { value, .. } | PropertyKind::TextInput { value } => Value::String(value.clone()),
            PropertyKind::File { value } | PropertyKind::Directory { value, .. } | PropertyKind::SceneTexture { value } => {
                value.clone().map(Value::String).unwrap_or(Value::Null)
            }
            PropertyKind::Text | PropertyKind::Group => Value::Null,
            PropertyKind::Unknown { value, .. } => value.clone(),
        }
    }

    pub fn has_value(&self) -> bool {
        !matches!(self.kind, PropertyKind::Text | PropertyKind::Group)
    }

    /// Set a new value, validating/clamping it against the definition.
    pub fn set_value(&mut self, v: &Value) -> Result<()> {
        let bad = |reason: &str| Error::InvalidValue { key: self.key.clone(), reason: reason.into() };
        match &mut self.kind {
            PropertyKind::Color { value } => *value = parse_color(v).ok_or_else(|| bad("expected color \"r g b\""))?,
            PropertyKind::Slider { value, min, max, step, fraction } => {
                let mut x = as_f64(v).ok_or_else(|| bad("expected number"))?;
                if !x.is_finite() {
                    return Err(bad("not finite"));
                }
                if !*fraction {
                    x = (((x - *min) / *step).round() * *step) + *min;
                }
                *value = x.clamp(*min, *max);
            }
            PropertyKind::Bool { value } => *value = as_bool(v).ok_or_else(|| bad("expected bool"))?,
            PropertyKind::Combo { value, options } => {
                let s = as_string(v).ok_or_else(|| bad("expected string"))?;
                if !options.is_empty() && !options.iter().any(|o| o.value == s) {
                    return Err(bad("not one of the combo options"));
                }
                *value = s;
            }
            PropertyKind::TextInput { value } => *value = as_string(v).ok_or_else(|| bad("expected string"))?,
            PropertyKind::File { value } | PropertyKind::Directory { value, .. } | PropertyKind::SceneTexture { value } => {
                *value = if v.is_null() { None } else { Some(as_string(v).ok_or_else(|| bad("expected path"))?) }
            }
            PropertyKind::Text | PropertyKind::Group => return Err(bad("property has no value")),
            PropertyKind::Unknown { value, .. } => *value = v.clone(),
        }
        Ok(())
    }
}

/// Ordered set of properties (sorted by `order`, then key).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Properties(pub Vec<Property>);

impl Properties {
    pub fn parse(obj: Option<&Value>) -> Properties {
        let mut v: Vec<Property> = obj
            .and_then(Value::as_object)
            .map(|m| m.iter().filter(|(_, d)| d.is_object()).map(|(k, d)| Property::parse(k, d)).collect())
            .unwrap_or_default();
        v.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.key.cmp(&b.key)));
        Properties(v)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Property> {
        self.0.iter()
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn get(&self, key: &str) -> Option<&Property> {
        self.0.iter().find(|p| p.key == key)
    }
    pub fn get_mut(&mut self, key: &str) -> Option<&mut Property> {
        self.0.iter_mut().find(|p| p.key == key)
    }

    pub fn set(&mut self, key: &str, v: &Value) -> Result<()> {
        self.get_mut(key).ok_or_else(|| Error::UnknownProperty(key.into()))?.set_value(v)
    }

    /// Apply saved user overrides (`{"key": value}`). Unknown keys and invalid
    /// values are skipped and returned, so stale settings never break loading.
    pub fn apply_overrides(&mut self, overrides: &Map<String, Value>) -> Vec<Error> {
        overrides.iter().filter_map(|(k, v)| self.set(k, v).err()).collect()
    }

    /// Current values only: `{"key": value}` — the format persisted in settings.
    pub fn values(&self) -> Map<String, Value> {
        self.0.iter().filter(|p| p.has_value()).map(|p| (p.key.clone(), p.value_json())).collect()
    }

    /// Object passed to `wallpaperPropertyListener.applyUserProperties`:
    /// `{"key": {"type": "...", "value": ...}}`.
    pub fn to_user_properties(&self) -> Value {
        Value::Object(
            self.0
                .iter()
                .filter(|p| p.has_value())
                .map(|p| (p.key.clone(), json!({ "type": p.type_name(), "value": p.value_json() })))
                .collect(),
        )
    }
}
