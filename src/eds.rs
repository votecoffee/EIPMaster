use crate::model::{Settings, hex};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub fields: BTreeMap<String, String>,
    pub settings: Settings,
    pub unsupported: Vec<String>,
}

pub fn load(path: &Path) -> Result<Vec<Profile>, String> {
    let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    parse(&String::from_utf8_lossy(&bytes))
}
fn fields(text: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    for c in text.chars() {
        match c {
            '"' => quoted = !quoted,
            ',' if !quoted => result.push(std::mem::take(&mut field).trim().to_owned()),
            _ => field.push(c),
        }
    }
    result.push(field.trim().to_owned());
    result
}
fn number(text: &str) -> Option<u32> {
    if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16).ok()
    } else {
        text.parse().ok()
    }
}
fn path_bytes(text: &str, param_byte: impl Fn(&str) -> Option<u8>) -> Option<Vec<u8>> {
    text.split_whitespace()
        .map(|word| {
            if let Some(parameter) = word
                .strip_prefix('[')
                .and_then(|word| word.strip_suffix(']'))
            {
                return param_byte(parameter);
            }
            let word = word.strip_prefix("0x").unwrap_or(word);
            if word.len() != 2 {
                return None;
            }
            u8::from_str_radix(word, 16).ok()
        })
        .collect()
}

fn assembly_path(bytes: &[u8]) -> Option<(Option<u16>, u16, u16)> {
    let mut rest = bytes.strip_prefix(&[0x20, 4])?;
    let mut next = |tag: u8| -> Option<u16> {
        match rest {
            [code, n, tail @ ..] if *code == tag => {
                rest = tail;
                Some((*n).into())
            }
            [code, 0, lo, hi, tail @ ..] if *code == (tag | 1) => {
                rest = tail;
                Some(u16::from_le_bytes([*lo, *hi]))
            }
            _ => None,
        }
    };
    let config = next(0x24);
    let output = next(0x2c)?;
    let input = next(0x2c)?;
    rest.is_empty().then_some((config, output, input))
}
pub fn parse(text: &str) -> Result<Vec<Profile>, String> {
    let mut cleaned = String::new();
    let mut quoted = false;
    let mut comment = false;
    for c in text.trim_start_matches('\u{feff}').chars() {
        if c == '\n' {
            comment = false;
        }
        if !quoted && c == '$' {
            comment = true;
        }
        if comment {
            continue;
        }
        if c == '"' {
            quoted = !quoted;
        }
        cleaned.push(c);
    }
    if quoted {
        return Err("EDS has an unterminated quoted string".into());
    }
    let mut section = String::new();
    let mut statement = String::new();
    let mut entries = HashMap::new();
    let mut connections = Vec::new();
    for line in cleaned.lines() {
        let line = line.trim();
        if statement.trim().is_empty() && line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].to_ascii_lowercase();
            continue;
        }
        for c in line.chars() {
            if c == '"' {
                quoted = !quoted;
            }
            if c == ';' && !quoted {
                if let Some((name, value)) = statement.split_once('=') {
                    let key = name.trim().to_ascii_lowercase();
                    let value = fields(value);
                    if section == "connection manager"
                        && key
                            .strip_prefix("connection")
                            .is_some_and(|n| n.parse::<u32>().is_ok())
                    {
                        connections.push((name.trim().to_string(), value));
                    } else {
                        entries.insert((section.clone(), key), value);
                    }
                }
                statement.clear();
            } else {
                statement.push(c);
            }
        }
        if !statement.is_empty() {
            statement.push(' ');
        }
    }
    if connections.is_empty() {
        return Err("No ConnectionN profiles found in [Connection Manager]".into());
    }

    let parameter = |text: &str| entries.get(&("params".into(), text.to_ascii_lowercase()));
    let resolve = |text: &str| number(text).or_else(|| number(parameter(text)?.get(11)?));
    let parameter_byte = |name: &str| {
        let fields = parameter(name)?;
        let name = name.to_ascii_lowercase();
        name.strip_prefix("param")?.parse::<u32>().ok()?;
        if number(fields.get(5)?)? != 1 {
            return None;
        }
        u8::try_from(number(fields.get(11)?)?).ok()
    };
    let assembly_size = |text: &str| {
        entries
            .get(&("assembly".into(), text.to_ascii_lowercase()))
            .and_then(|p| p.get(2))
            .and_then(|s| number(s))
    };
    Ok(connections
        .into_iter()
        .map(|(id, f)| {
            let get = |idx: usize| f.get(idx).map(String::as_str).unwrap_or("");
            let name = get(12).to_string();
            let mut p = Profile {
                id,
                name,
                fields: BTreeMap::new(),
                unsupported: Vec::new(),
                settings: Settings {
                    input_instance: 0,
                    output_instance: 0,
                    config_instance: None,
                    input_size: 0,
                    output_size: 0,
                    input_rpi_us: 0,
                    output_rpi_us: 0,
                    input_only: false,
                    ..Settings::default()
                },
            };
            let s = &mut p.settings;
            let origin = &mut p.fields;
            let mut issues = Vec::new();
            let trigger = number(get(0)).unwrap_or(0);
            if trigger & 0x0001_0002 != 0x0001_0002 {
                issues.push("Profile must support cyclic Class 1".into());
            }
            if trigger & 0x0200_0000 != 0 {
                s.input_only = true;
                origin.insert("Ownership".into(), "Input-only".into());
            } else if trigger & 0x0400_0000 != 0 {
                origin.insert("Ownership".into(), "Exclusive owner".into());
            } else {
                issues.push("Profile lacks input-only or exclusive-owner support".into());
            }

            let mask = number(get(1)).unwrap_or(0);
            if mask & 0x0044_0005 != 0x0044_0005 {
                issues.push(
                    "Profile must support fixed-size point-to-point I/O in both directions".into(),
                );
            }
            for (shift, direction, header, priority) in [
                (0, "O->T", &mut s.output_run_idle, &mut s.output_priority),
                (4, "T->O", &mut s.input_run_idle, &mut s.input_priority),
            ] {
                let format = (mask >> (8 + shift)) & 7;
                let header_label = if shift == 0 {
                    "O->T header"
                } else {
                    "T->O header"
                };
                let priority_label = if shift == 0 {
                    "O->T priority"
                } else {
                    "T->O priority"
                };
                match format {
                    0 => {
                        origin.insert(header_label.into(), "Modeless".into());
                    }
                    3 if shift == 0 && s.input_only => {
                        origin.insert(header_label.into(), "Heartbeat".into());
                    }
                    4 => {
                        *header = true;
                        origin.insert(header_label.into(), "32-bit Run/Idle".into());
                    }
                    _ => issues.push(format!(
                        "{direction} transfer format {format} needs manual review"
                    )),
                }
                let supported = (mask >> (24 + shift)) & 15;
                if supported != 0 {
                    *priority = supported.trailing_zeros() as u8;
                    origin.insert(priority_label.into(), priority.to_string());
                } else {
                    issues.push(format!("{direction} priority missing"));
                }
            }
            if let Some(bytes) = path_bytes(get(14), parameter_byte)
                && let Some((config, output, input)) = assembly_path(&bytes)
            {
                s.config_instance = config;
                s.output_instance = output;
                s.input_instance = input;
                origin.insert(
                    "Config assembly".into(),
                    config.map_or_else(|| "None".into(), |v| v.to_string()),
                );
                origin.insert("O->T assembly".into(), output.to_string());
                origin.insert("T->O assembly".into(), input.to_string());
                if get(14).contains('[') {
                    origin.insert(
                        "Assembly path".into(),
                        format!("{} => {}", get(14), hex(&bytes)),
                    );
                }
            } else {
                issues.push("Unresolved or unsupported assembly path".into());
            }

            for (idx, label, value) in [
                (2, "O->T RPI", &mut s.output_rpi_us),
                (5, "T->O RPI", &mut s.input_rpi_us),
            ] {
                if let Some(n) = resolve(get(idx)) {
                    *value = n;
                    origin.insert(label.into(), format!("{n} µs ({})", get(idx)));
                }
            }
            for (idx, label, value) in [
                (3, "O->T size", &mut s.output_size),
                (6, "T->O size", &mut s.input_size),
            ] {
                let size = resolve(get(idx)).or_else(|| {
                    get(idx)
                        .is_empty()
                        .then(|| assembly_size(get(idx + 1)))
                        .flatten()
                });
                if let Some(n) = size.and_then(|n| u16::try_from(n).ok()) {
                    *value = n;
                    origin.insert(
                        label.into(),
                        format!(
                            "{n} bytes ({})",
                            if get(idx).is_empty() {
                                get(idx + 1)
                            } else {
                                get(idx)
                            }
                        ),
                    );
                }
            }
            if !origin.contains_key("O->T size") {
                issues.push("Unresolved O->T size; verify it in manual mode".into());
            }
            let size1 = if get(8).is_empty() && get(9).is_empty() {
                Some(0)
            } else {
                resolve(get(8))
            };
            let size2 = if get(10).is_empty() && get(11).is_empty() {
                Some(0)
            } else if get(10).is_empty() {
                assembly_size(get(11))
            } else {
                resolve(get(10))
            };
            match (size1, size2) {
                (Some(0), Some(n)) if n <= 490 => {
                    s.config_size = n as u16;
                    origin.insert("Config size".into(), n.to_string());
                }
                _ => issues.push("Configuration format or size requires manual review".into()),
            }
            p.unsupported = issues;
            p
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sample_and_unresolved_path() {
        let sample = include_str!("../samples/loopback.eds");
        let profiles = parse(sample).unwrap();
        let p = &profiles[0];
        assert!(p.unsupported.is_empty(), "{:?}", p.unsupported);
        assert!(p.settings.input_only);
        assert_eq!(
            (
                p.settings.input_instance,
                p.settings.input_size,
                p.settings.input_rpi_us
            ),
            (101, 4, 1000)
        );
        let variant = sample.replace("2C 65", "2D 00 [Param2]");
        assert!(!parse(&variant).unwrap()[0].unsupported.is_empty());
    }

    #[test]
    fn resolves_path_byte_parameters_without_truncation() {
        let sample = include_str!("../samples/loopback.eds")
            .replace("[Assembly]", "Param5 = 0,,,0,0xC6,1,\"Input\",\"\",\"\",101,101,101;\nParam7 = 0,,,0,0xC6,1,\"Heartbeat\",\"\",\"\",254,254,254;\n[Assembly]")
            .replace("0x22440305", "0x44244305")
            .replace("2C FE 2C 65", "2C [Param7] 2C [Param5]");
        let profiles = parse(&sample).unwrap();
        let p = &profiles[0];
        // This test profile advertises multicast T->O. Keep the decoded
        // path available for review without treating it as a unicast profile.
        assert_eq!(p.settings.config_instance, Some(1));
        assert_eq!(p.settings.output_instance, 254);
        assert_eq!(p.settings.input_instance, 101);
        assert_eq!(p.unsupported.len(), 1);
        assert!(p.unsupported[0].contains("point-to-point"));
        assert_eq!(
            p.fields["Assembly path"],
            "20 04 24 01 2C [Param7] 2C [Param5] => 20 04 24 01 2C FE 2C 65"
        );
        let too_wide = sample.replace("254,254,254", "254,254,510");
        assert!(
            parse(&too_wide).unwrap()[0]
                .unsupported
                .iter()
                .any(|e| e.contains("assembly path"))
        );
    }
}
