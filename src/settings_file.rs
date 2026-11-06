use crate::{capture, eds, model::Settings, scanner::Schedule};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Preferences {
    pub directory: String,
    pub capacity: usize,
    pub rotate_bytes: u64,
    pub rotate_seconds: u64,
    pub schedule: Schedule,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            directory: "captures".into(),
            capacity: 32_768,
            rotate_bytes: 0,
            rotate_seconds: 0,
            schedule: Schedule::Deadline,
        }
    }
}

impl Preferences {
    pub fn capture_options(&self, directory: PathBuf) -> capture::Options {
        capture::Options {
            directory,
            capacity: self.capacity,
            rotate_bytes: self.rotate_bytes,
            rotate_seconds: self.rotate_seconds,
        }
    }
}

pub fn load_profile(path: &Path) -> Result<(Settings, Option<Preferences>), String> {
    let read = || -> Result<_, Box<dyn std::error::Error>> {
        let mut table: toml::Table = toml::from_str(&fs::read_to_string(path)?)?;
        let preferences = table.remove("app").map(toml::Value::try_into).transpose()?;
        Ok((toml::Value::Table(table).try_into()?, preferences))
    };
    read().map_err(|error| format!("{}: {error}", path.display()))
}

pub fn profile_text(settings: &Settings, preferences: &Preferences) -> Result<String, String> {
    settings.validate()?;
    if preferences.directory.trim().is_empty() {
        return Err("Enter a capture directory".into());
    }
    if !(1..=1_000_000).contains(&preferences.capacity) {
        return Err("Capture capacity must be between 1 and 1,000,000 samples".into());
    }
    let encode = || -> Result<_, toml::ser::Error> {
        let mut value = toml::Value::try_from(settings)?;
        value
            .as_table_mut()
            .unwrap()
            .insert("app".into(), toml::Value::try_from(preferences)?);
        toml::to_string_pretty(&value)
    };
    encode().map_err(|error| error.to_string())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Session {
    pub settings: Settings,
    pub preferences: Preferences,
    pub device: String,
    pub local: String,
    pub eds_path: String,
    pub profile_path: String,
    pub eds_profiles: Vec<eds::Profile>,
    pub selected_eds: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamedDevice {
    pub name: String,
    pub setup: Session,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workspace {
    pub selected_device: usize,
    pub devices: Vec<NamedDevice>,
}

impl Workspace {
    pub fn path() -> Option<PathBuf> {
        std::env::var_os("LOCALAPPDATA")
            .filter(|value| !value.is_empty())
            .map(|path| {
                PathBuf::from(path)
                    .join("EIPMaster")
                    .join("last-session.toml")
            })
    }

    pub fn load(path: &Path) -> io::Result<Option<Self>> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let table: toml::Table = toml::from_str(&text).map_err(io::Error::other)?;
        let workspace: Self = if table.contains_key("devices") {
            table.try_into().map_err(io::Error::other)?
        } else {
            Self {
                selected_device: 0,
                devices: vec![NamedDevice {
                    name: "Device 1".into(),
                    setup: table.try_into().map_err(io::Error::other)?,
                }],
            }
        };
        if workspace.selected_device >= workspace.devices.len() {
            return Err(io::Error::other("Invalid saved device selection"));
        }
        for device in &workspace.devices {
            if device
                .setup
                .selected_eds
                .is_some_and(|index| index >= device.setup.eds_profiles.len())
            {
                return Err(io::Error::other(format!(
                    "Invalid saved EDS selection for {}",
                    device.name
                )));
            }
        }
        Ok(Some(workspace))
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        let text = toml::to_string_pretty(self).map_err(io::Error::other)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        replace(path, &text)
    }
}

pub fn write_new(path: &Path, text: &str) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    let result = file
        .write_all(text.as_bytes())
        .and_then(|_| file.sync_all());
    drop(file);
    if result.is_err() {
        let _ = fs::remove_file(path);
    }
    result
}

pub fn replace(path: &Path, text: &str) -> io::Result<()> {
    let mut id = [0; 8];
    getrandom::fill(&mut id).map_err(|error| io::Error::other(error.to_string()))?;
    let temporary = path.with_file_name(format!(".eipmaster-{:016x}.tmp", u64::from_le_bytes(id)));
    write_new(&temporary, text)?;
    let result = fs::rename(&temporary, path);
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}
