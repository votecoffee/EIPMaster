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
pub const MAX_ASSEMBLY_BYTES: usize = 509;
pub const MAX_MAPPED_FIELDS: usize = 256;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureMode {
    #[default]
    EveryPacket,
    OnChange,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    #[serde(
        default,
        rename = "input_fields",
        alias = "capture_fields",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub capture_fields: Vec<CaptureField>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_fields: Vec<CaptureField>,
    pub log_fields: Option<Vec<String>>,
    #[serde(default)]
    pub capture_mode: CaptureMode,
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
            output_fields: Vec::new(),
            log_fields: None,
            capture_mode: CaptureMode::EveryPacket,
            output_rpi_us: 1000,
            input_rpi_us: 1000,
            selection: Selection::default(),
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
            || self.input_size as usize + 4 * self.input_run_idle as usize > MAX_ASSEMBLY_BYTES
            || self.output_size as usize + 4 * self.output_run_idle as usize > MAX_ASSEMBLY_BYTES
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
        if !self.input_only {
            validate_fields(&self.output_fields, self.output_size as usize, false)?;
        }
        self.selected_fields()?;
        Ok(())
    }

    pub fn csv_fields(&self) -> Result<Vec<CaptureField>, String> {
        let fields = self.selected_fields()?;
        if fields.is_empty() {
            return Err("Select at least one input field for CSV capture".into());
        }
        Ok(fields)
    }

    fn selected_fields(&self) -> Result<Vec<CaptureField>, String> {
        validate_fields(&self.capture_fields, self.input_size as usize, true)?;
        let mapped = if self.capture_fields.is_empty() {
            self.selection.validate(self.input_size as usize)?;
            vec![CaptureField {
                name: "value".into(),
                selection: self.selection,
            }]
        } else {
            self.capture_fields.clone()
        };
        let Some(names) = &self.log_fields else {
            return Ok(mapped);
        };
        for (i, name) in names.iter().enumerate() {
            if !mapped.iter().any(|field| field.name == *name) {
                return Err(format!("Unknown CSV input field {name:?}"));
            }
            if names[..i].contains(name) {
                return Err(format!("Duplicate CSV input field {name:?}"));
            }
        }
        Ok(mapped
            .into_iter()
            .filter(|field| names.contains(&field.name))
            .collect())
    }

    pub fn output(&self) -> Result<Vec<u8>, String> {
        let data = parse_hex(&self.output_hex)?;
        if data.len() != self.output_size as usize {
            return Err("Output bytes do not match the declared output size".into());
        }
        Ok(data)
    }

    /// Encode one field into the staged buffer without sending any data.
    pub fn stage_output(&mut self, index: usize, value: &str) -> Result<(), String> {
        if self.input_only {
            return Err("The input-only profile has no writable outputs".into());
        }
        let field = self
            .output_fields
            .get(index)
            .ok_or("Unknown output field")?;
        let mut bytes = self.output()?;
        field.selection.encode(value, &mut bytes)?;
        self.output_hex = hex(&bytes);
        Ok(())
    }
}

fn validate_fields(fields: &[CaptureField], size: usize, csv_names: bool) -> Result<(), String> {
    if fields.len() > MAX_MAPPED_FIELDS {
        return Err(format!(
            "Use at most {MAX_MAPPED_FIELDS} mapped fields per assembly"
        ));
    }
    for (index, field) in fields.iter().enumerate() {
        let mut chars = field.name.chars();
        if !chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            return Err(format!("Invalid capture field name {:?}", field.name));
        }
        if (csv_names
            && CAPTURE_METADATA
                .iter()
                .any(|name| name.eq_ignore_ascii_case(&field.name)))
            || fields[..index]
                .iter()
                .any(|other| other.name.eq_ignore_ascii_case(&field.name))
        {
            return Err(format!("Duplicate capture field name {:?}", field.name));
        }
        field
            .selection
            .validate(size)
            .map_err(|error| format!("{}: {error}", field.name))?;
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    Bool,
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
    Utf8,
}
impl Kind {
    pub const ALL: [Self; 12] = [
        Self::Bool,
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
        Self::Utf8,
    ];
    fn width(self) -> usize {
        match self {
            Self::Bool | Self::U8 | Self::I8 => 1,
            Self::U16 | Self::I16 => 2,
            Self::U32 | Self::I32 | Self::F32 => 4,
            Self::U64 | Self::I64 | Self::F64 => 8,
            Self::Utf8 => 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ByteSwap {
    #[default]
    None,
    BytesInWords,
    WordOrder,
}

impl ByteSwap {
    pub const ALL: [Self; 3] = [Self::None, Self::BytesInWords, Self::WordOrder];

    pub fn label(self) -> &'static str {
        match self {
            Self::None => "No swap",
            Self::BytesInWords => "Swap bytes within words",
            Self::WordOrder => "Reverse word order",
        }
    }

    fn apply(self, bytes: &mut [u8]) {
        if self == Self::WordOrder {
            bytes.reverse();
        }
        if self != Self::None {
            for pair in bytes.chunks_exact_mut(2) {
                pair.swap(0, 1);
            }
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub offset: usize,
    pub kind: Kind,
    #[serde(default)]
    pub big_endian: bool,
    pub bit: Option<u8>,
    #[serde(default, skip_serializing_if = "is_no_swap")]
    pub swap: ByteSwap,
    pub string_length: Option<usize>,
}

fn is_no_swap(swap: &ByteSwap) -> bool {
    *swap == ByteSwap::None
}

impl Default for Selection {
    fn default() -> Self {
        Self {
            offset: 0,
            kind: Kind::U32,
            big_endian: false,
            bit: None,
            swap: ByteSwap::None,
            string_length: None,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureField {
    pub name: String,
    pub selection: Selection,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Value<T = Vec<u8>> {
    Bool(bool),
    Unsigned(u64),
    Signed(i64),
    Float32(f32),
    Float64(f64),
    Text(T),
}
impl<T: AsRef<[u8]>> std::fmt::Display for Value<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bool(v) => v.fmt(f),
            Self::Unsigned(v) => v.fmt(f),
            Self::Signed(v) => v.fmt(f),
            Self::Float32(v) => v.fmt(f),
            Self::Float64(v) => v.fmt(f),
            Self::Text(v) => String::from_utf8_lossy(v.as_ref()).fmt(f),
        }
    }
}
impl Value<&[u8]> {
    pub fn into_owned(self) -> Value {
        match self {
            Self::Bool(v) => Value::Bool(v),
            Self::Unsigned(v) => Value::Unsigned(v),
            Self::Signed(v) => Value::Signed(v),
            Self::Float32(v) => Value::Float32(v),
            Self::Float64(v) => Value::Float64(v),
            Self::Text(v) => Value::Text(v.to_vec()),
        }
    }
}
impl Selection {
    pub fn validate(self, size: usize) -> Result<(), String> {
        if self.kind == Kind::Utf8 && self.bit.is_some() {
            return Err("UTF-8 fields cannot select a single bit".into());
        }
        let width = self.width()?;
        if self.swap == ByteSwap::BytesInWords
            && (!width.is_multiple_of(2) || self.kind == Kind::Utf8)
            || self.swap == ByteSwap::WordOrder
                && (width < 4 || !width.is_multiple_of(2) || self.kind == Kind::Utf8)
        {
            return Err("This swap needs a numeric field with at least two bytes per word".into());
        }
        if size
            .checked_sub(self.offset)
            .is_none_or(|left| left < width)
        {
            return Err("Selected value extends past the assembly".into());
        }
        Ok(())
    }

    fn width(self) -> Result<usize, String> {
        if let Some(bit) = self.bit {
            if bit >= 8 {
                return Err("Bit index must be 0–7".into());
            }
            return Ok(1);
        }
        if self.kind == Kind::Utf8 {
            return self
                .string_length
                .filter(|&size| (1..=MAX_ASSEMBLY_BYTES).contains(&size))
                .ok_or_else(|| "UTF-8 needs a fixed byte length between 1 and 509".into());
        }
        Ok(self.kind.width())
    }

    pub fn decode(self, bytes: &[u8]) -> Option<Value<&[u8]>> {
        self.validate(bytes.len()).ok()?;
        if let Some(bit) = self.bit {
            let set = (bytes[self.offset] >> bit) & 1;
            return Some(if self.kind == Kind::Bool {
                Value::Bool(set != 0)
            } else {
                Value::Unsigned(set.into())
            });
        }
        let width = self.width().ok()?;
        let source = &bytes[self.offset..self.offset + width];
        if self.kind == Kind::Utf8 {
            return Some(Value::Text(
                source.split(|&byte| byte == 0).next().unwrap_or_default(),
            ));
        }
        let mut bits = [0; 8];
        bits[..width].copy_from_slice(source);
        self.swap.apply(&mut bits[..width]);
        if self.big_endian {
            bits[..width].reverse();
        }
        let value = u64::from_le_bytes(bits);
        Some(match self.kind {
            Kind::Bool => Value::Bool(value != 0),
            Kind::I8 | Kind::I16 | Kind::I32 | Kind::I64 => {
                let shift = 64 - width * 8;
                Value::Signed(((value << shift) as i64) >> shift)
            }
            Kind::F32 => Value::Float32(f32::from_bits(value as u32)),
            Kind::F64 => Value::Float64(f64::from_bits(value)),
            Kind::U8 | Kind::U16 | Kind::U32 | Kind::U64 => Value::Unsigned(value),
            Kind::Utf8 => unreachable!(),
        })
    }

    pub fn encode(self, value: &str, bytes: &mut [u8]) -> Result<(), String> {
        self.validate(bytes.len())?;
        let target = &mut bytes[self.offset..self.offset + self.width()?];
        if let Some(bit) = self.bit {
            let mask = 1 << bit;
            let set = parse_bool(value)?;
            target[0] = (target[0] & !mask) | if set { mask } else { 0 };
            return Ok(());
        }
        if self.kind == Kind::Utf8 {
            if value.len() > target.len() || value.contains('\0') {
                return Err("Text must fit the fixed byte length and cannot contain NUL".into());
            }
            target.fill(0);
            target[..value.len()].copy_from_slice(value.as_bytes());
            return Ok(());
        }
        let value = value.trim();
        let invalid = || format!("Enter a valid {:?} value", self.kind);
        macro_rules! parse {
            ($ty:ty) => {
                value.parse::<$ty>().map_err(|_| invalid())?.to_le_bytes()
            };
        }
        match self.kind {
            Kind::Bool => target[0] = u8::from(parse_bool(value)?),
            Kind::U8 => target.copy_from_slice(&parse!(u8)),
            Kind::I8 => target.copy_from_slice(&parse!(i8)),
            Kind::U16 => target.copy_from_slice(&parse!(u16)),
            Kind::I16 => target.copy_from_slice(&parse!(i16)),
            Kind::U32 => target.copy_from_slice(&parse!(u32)),
            Kind::I32 => target.copy_from_slice(&parse!(i32)),
            Kind::U64 => target.copy_from_slice(&parse!(u64)),
            Kind::I64 => target.copy_from_slice(&parse!(i64)),
            Kind::F32 => target.copy_from_slice(&parse!(f32)),
            Kind::F64 => target.copy_from_slice(&parse!(f64)),
            Kind::Utf8 => unreachable!(),
        }
        if self.big_endian {
            target.reverse();
        }
        self.swap.apply(target);
        Ok(())
    }
}

fn parse_bool(value: &str) -> Result<bool, String> {
    let value = value.trim();
    if value == "1" || value.eq_ignore_ascii_case("true") {
        Ok(true)
    } else if value == "0" || value.eq_ignore_ascii_case("false") {
        Ok(false)
    } else {
        Err("Enter true, false, 1, or 0".into())
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
            ..Selection::default()
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

    #[test]
    fn mapped_output_encoding_and_swap_round_trip() {
        let mut bytes = [0xaa; 8];
        let bit = Selection {
            kind: Kind::Bool,
            bit: Some(2),
            ..Selection::default()
        };
        bit.encode("true", &mut bytes).unwrap();
        assert_eq!(bytes[0], 0xae);
        assert_eq!(bit.decode(&bytes), Some(Value::Bool(true)));
        assert!(bit.encode("2", &mut bytes).is_err());
        assert_eq!(bytes[0], 0xae);

        let mut float = Selection {
            offset: 1,
            kind: Kind::F32,
            big_endian: true,
            swap: ByteSwap::WordOrder,
            ..Selection::default()
        };
        float.encode("1.5", &mut bytes).unwrap();
        assert_eq!(&bytes[..6], &[0xae, 0, 0, 0x3f, 0xc0, 0xaa]);
        assert_eq!(float.decode(&bytes), Some(Value::Float32(1.5)));
        float.swap = ByteSwap::BytesInWords;
        float.encode("1.5", &mut bytes).unwrap();
        assert_eq!(&bytes[1..5], &[0xc0, 0x3f, 0, 0]);
        assert_eq!(float.decode(&bytes), Some(Value::Float32(1.5)));

        let text = Selection {
            offset: 5,
            kind: Kind::Utf8,
            string_length: Some(3),
            ..Selection::default()
        };
        text.encode("é", &mut bytes).unwrap();
        assert_eq!(&bytes[5..], &[0xc3, 0xa9, 0]);
        assert_eq!(text.decode(&bytes), Some(Value::Text("é".as_bytes())));
        assert!(text.encode("abcd", &mut bytes).is_err());
        assert!(text.encode("\0", &mut bytes).is_err());
        assert_eq!(&bytes[5..], &[0xc3, 0xa9, 0]);
    }

    #[test]
    fn mapped_profile_round_trips_with_only_selected_columns() {
        let settings: Settings =
            toml::from_str(include_str!("../samples/mapped-loopback.toml")).unwrap();
        settings.validate().unwrap();
        let saved = toml::to_string_pretty(&settings).unwrap();
        let reloaded: Settings = toml::from_str(&saved).unwrap();
        assert_eq!(reloaded.capture_mode, CaptureMode::OnChange);
        assert_eq!(reloaded.capture_fields.len(), 3);
        assert_eq!(
            reloaded
                .csv_fields()
                .unwrap()
                .into_iter()
                .map(|field| field.name)
                .collect::<Vec<_>>(),
            ["counter", "low_bit"]
        );
    }
}
