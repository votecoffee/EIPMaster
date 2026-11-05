use serde::{Deserialize, Serialize};
use std::net::Ipv4Addr;

pub const CAPTURE_METADATA: [&str; 6] = [
    "capture_start_utc",
    "elapsed_us",
    "sample_index",
    "connection_generation",
    "packet_sequence",
    "cip_sequence",
];
pub const MAX_CAPTURE_FIELDS: usize = 3;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub device_ip: Ipv4Addr,
    pub local_ip: Ipv4Addr,
    pub local_udp_port: u16,
    pub input_instance: u16,
    pub input_size: u16,
    pub output_instance: u16,
    pub output_size: u16,
    pub config_instance: Option<u16>,
    pub config_size: u16,
    pub config_hex: String,
    pub output_hex: String,
    pub input_only: bool,
    pub output_run_idle: bool,
    pub input_run_idle: bool,
    pub output_priority: u8,
    pub input_priority: u8,
    pub output_rpi_us: u32,
    pub input_rpi_us: u32,
    pub selection: Selection,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capture_fields: Vec<CaptureField>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            device_ip: Ipv4Addr::LOCALHOST,
            local_ip: Ipv4Addr::LOCALHOST,
            local_udp_port: 0,
            input_instance: 101,
            input_size: 4,
            output_instance: 254,
            output_size: 0,
            config_instance: Some(1),
            config_size: 0,
            config_hex: String::new(),
            output_hex: String::new(),
            input_only: true,
            output_run_idle: false,
            input_run_idle: false,
            output_priority: 1,
            input_priority: 1,
            capture_fields: Vec::new(),
            output_rpi_us: 1000,
            input_rpi_us: 1000,
            selection: Selection {
                offset: 0,
                kind: Kind::U32,
                big_endian: false,
                bit: None,
            },
        }
    }
}
pub fn parse_hex(s: &str) -> Result<Vec<u8>, String> {
    s.split_whitespace()
        .map(|word| {
            let word = word.strip_prefix("0x").unwrap_or(word);
            if word.len() != 2 {
                return Err(format!("Expected a two-digit hex byte, got {word:?}"));
            }
            u8::from_str_radix(word, 16).map_err(|_| format!("Invalid hex byte {word:?}"))
        })
        .collect()
}
pub fn hex(data: &[u8]) -> String {
    data.iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}
impl Settings {
    pub fn validate(&self) -> Result<(), String> {
        if self.device_ip.is_unspecified()
            || self.device_ip.is_multicast()
            || self.device_ip.is_broadcast()
        {
            return Err("Enter a unicast adapter IPv4 address".into());
        }
        if self.local_ip.is_unspecified()
            || self.local_ip.is_multicast()
            || self.local_ip.is_broadcast()
        {
            return Err("Enter the scanner NIC's assigned IPv4 address".into());
        }
        if self.input_instance == 0 || self.output_instance == 0 {
            return Err("Set both connection point instances".into());
        }
        if self.input_size == 0
            || self.input_size as usize + 4 * self.input_run_idle as usize > 509
            || self.output_size as usize + 4 * self.output_run_idle as usize > 509
        {
            return Err("Standard Forward_Open supports up to 509 assembly and Run/Idle bytes per direction".into());
        }
        if self.input_priority > 3 || self.output_priority > 3 {
            return Err("Connection priorities must be 0–3".into());
        }
        if !(1000..=10_000_000).contains(&self.input_rpi_us)
            || !(1000..=10_000_000).contains(&self.output_rpi_us)
        {
            return Err("Both RPIs must be between 1,000 and 10,000,000 µs".into());
        }
        if self.input_only && self.output_size != 0 {
            return Err(
                "Input-only requires a heartbeat connection with zero output assembly bytes".into(),
            );
        }
        if self.config_size > 490
            || (self.config_size != 0 && self.config_instance.unwrap_or(0) == 0)
        {
            return Err(
                "Configuration data requires an assembly instance and at most 490 bytes".into(),
            );
        }
        if parse_hex(&self.config_hex)?.len() != self.config_size as usize {
            return Err("Configuration bytes do not match the declared configuration size".into());
        }
        self.output()?;
        self.selection.validate(self.input_size as usize)?;
        self.csv_fields()?;
        Ok(())
    }

    pub fn csv_fields(&self) -> Result<Vec<CaptureField>, String> {
        if self.capture_fields.is_empty() {
            self.selection.validate(self.input_size as usize)?;
            return Ok(vec![CaptureField {
                name: "value".into(),
                selection: self.selection,
            }]);
        }
        if self.capture_fields.len() > MAX_CAPTURE_FIELDS {
            return Err(format!("Use at most {MAX_CAPTURE_FIELDS} capture fields"));
        }
        for (index, field) in self.capture_fields.iter().enumerate() {
            let mut chars = field.name.chars();
            if !chars
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                return Err(format!("Invalid capture field name {:?}", field.name));
            }
            if CAPTURE_METADATA
                .iter()
                .any(|name| name.eq_ignore_ascii_case(&field.name))
                || self.capture_fields[..index]
                    .iter()
                    .any(|other| other.name.eq_ignore_ascii_case(&field.name))
            {
                return Err(format!("Duplicate capture field name {:?}", field.name));
            }
            field
                .selection
                .validate(self.input_size as usize)
                .map_err(|error| format!("{}: {error}", field.name))?;
        }
        Ok(self.capture_fields.clone())
    }
    pub fn output(&self) -> Result<Vec<u8>, String> {
        let data = parse_hex(&self.output_hex)?;
        if data.len() != self.output_size as usize {
            return Err("Output bytes do not match the declared output size".into());
        }
        Ok(data)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    U8,
    I8,
    U16,
    I16,
    U32,
    I32,
    U64,
    I64,
    F32,
    F64,
}
impl Kind {
    pub const ALL: [Self; 10] = [
        Self::U8,
        Self::I8,
        Self::U16,
        Self::I16,
        Self::U32,
        Self::I32,
        Self::U64,
        Self::I64,
        Self::F32,
        Self::F64,
    ];
    fn width(self) -> usize {
        match self {
            Self::U8 | Self::I8 => 1,
            Self::U16 | Self::I16 => 2,
            Self::U32 | Self::I32 | Self::F32 => 4,
            _ => 8,
        }
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub offset: usize,
    pub kind: Kind,
    pub big_endian: bool,
    pub bit: Option<u8>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureField {
    pub name: String,
    pub selection: Selection,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Value {
    Unsigned(u64),
    Signed(i64),
    Float32(f32),
    Float64(f64),
}
impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsigned(v) => v.fmt(f),
            Self::Signed(v) => v.fmt(f),
            Self::Float32(v) => v.fmt(f),
            Self::Float64(v) => v.fmt(f),
        }
    }
}
impl Selection {
    pub fn validate(self, size: usize) -> Result<(), String> {
        let width = if let Some(bit) = self.bit {
            if bit >= 8 {
                return Err("Bit index must be 0–7".into());
            }
            1
        } else {
            self.kind.width()
        };
        if size
            .checked_sub(self.offset)
            .is_none_or(|left| left < width)
        {
            return Err("Selected value extends past the input assembly".into());
        }
        Ok(())
    }
    pub fn decode(self, bytes: &[u8]) -> Option<Value> {
        self.validate(bytes.len()).ok()?;
        if let Some(bit) = self.bit {
            return Some(Value::Unsigned(((bytes[self.offset] >> bit) & 1).into()));
        }
        let width = self.kind.width();
        let mut bits = [0; 8];
        bits[..width].copy_from_slice(&bytes[self.offset..self.offset + width]);
        if self.big_endian {
            bits[..width].reverse();
        }
        let value = u64::from_le_bytes(bits);
        Some(match self.kind {
            Kind::I8 | Kind::I16 | Kind::I32 | Kind::I64 => {
                let shift = 64 - width * 8;
                Value::Signed(((value << shift) as i64) >> shift)
            }
            Kind::F32 => Value::Float32(f32::from_bits(value as u32)),
            Kind::F64 => Value::Float64(f64::from_bits(value)),
            _ => Value::Unsigned(value),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decode_types_endian_bit_and_bounds() {
        let mut s = Selection {
            offset: 1,
            kind: Kind::I16,
            big_endian: true,
            bit: None,
        };
        assert_eq!(s.decode(&[0, 0xff, 0xfe]), Some(Value::Signed(-2)));
        s.big_endian = false;
        assert_eq!(s.decode(&[0, 0xff, 0xfe]), Some(Value::Signed(-257)));
        assert_eq!(s.decode(&[0, 0xff]), None);
        s.bit = Some(7);
        assert_eq!(s.decode(&[0, 0xff]), Some(Value::Unsigned(1)));
        s.bit = None;
        s.kind = Kind::F32;
        assert_eq!(s.decode(&[0, 0, 0, 128, 63]), Some(Value::Float32(1.0)));
        s.offset = usize::MAX;
        assert_eq!(s.decode(&[]), None);
    }
}
