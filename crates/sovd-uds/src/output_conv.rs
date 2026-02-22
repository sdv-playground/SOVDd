//! Typed value conversion for I/O control outputs
//!
//! Converts between typed JSON values (booleans, enums, numbers) and raw UDS bytes.
//! Operates on `OutputConfig` type metadata to determine encoding/decoding strategy.

use crate::config::{DataType, OutputConfig};
use anyhow::{anyhow, Result};
use serde_json::Value;

/// Encode a typed JSON value into raw bytes for UDS I/O control.
///
/// The allowed labels and data_type are UI hints — a tester that knows about
/// them can send `"on"` or `50.0`, but a raw tester that doesn't can still
/// send `"01"` (hex) or `1` (numeric) and it will work.
///
/// Conversion strategy (each step falls through on miss):
/// 1. If `allowed` is non-empty and value is a matching label string → index
/// 2. If `data_type` is set and value is boolean → 0x00 / 0x01
/// 3. If `data_type` is set and value is numeric → apply `(value - offset) / scale`
/// 4. If value is a string → hex decode (raw tester / backwards compatible)
pub fn encode_output_value(config: &OutputConfig, value: &Value) -> Result<Vec<u8>> {
    // Enum lookup: string value → index in allowed list.
    // On miss, fall through — the string might be a raw hex value from a
    // tester that doesn't know about the allowed labels.
    if !config.allowed.is_empty() {
        if let Some(s) = value.as_str() {
            if let Some(idx) = config
                .allowed
                .iter()
                .position(|a| a.eq_ignore_ascii_case(s))
            {
                return encode_raw_integer(config, idx as u64);
            }
            // Not in allowed list — fall through to numeric/hex below
        }

        // Numeric index into the allowed list (e.g. 1 for "on")
        if let Some(i) = value.as_u64() {
            if (i as usize) < config.allowed.len() {
                return encode_raw_integer(config, i);
            }
            // Out of range — fall through to typed numeric encoding
        }
    }

    if let Some(ref dt) = config.data_type {
        // Boolean → 0x00 / 0x01
        if let Some(b) = value.as_bool() {
            let raw = if b { 1u64 } else { 0u64 };
            return encode_raw_integer(config, raw);
        }

        // Numeric → apply inverse of physical conversion: raw = (physical - offset) / scale
        if let Some(f) = value.as_f64() {
            let raw = ((f - config.offset) / config.scale).round();
            if raw < 0.0 {
                return encode_raw_signed(dt, raw as i64);
            }
            return encode_raw_integer(config, raw as u64);
        }

        // Integer from JSON (serde_json may parse as i64/u64)
        if let Some(i) = value.as_i64() {
            let f = i as f64;
            let raw = ((f - config.offset) / config.scale).round();
            if raw < 0.0 {
                return encode_raw_signed(dt, raw as i64);
            }
            return encode_raw_integer(config, raw as u64);
        }
    }

    // Fallback: treat string as hex (raw tester / backwards compatible)
    if let Some(s) = value.as_str() {
        return hex::decode(s).map_err(|e| anyhow!("Invalid hex value '{}': {}", s, e));
    }

    Err(anyhow!(
        "Cannot encode value {:?} for output '{}'",
        value,
        config.id
    ))
}

/// Decode raw bytes into a typed JSON value for API responses.
///
/// Conversion strategy by priority:
/// 1. If `allowed` is non-empty → look up string by index
/// 2. If `data_type` is set → decode as numeric, apply `raw * scale + offset`
/// 3. No type metadata → hex string (backwards compatible)
pub fn decode_output_value(config: &OutputConfig, raw: &[u8]) -> Value {
    if let Some(ref dt) = config.data_type {
        let raw_int = decode_raw_unsigned(dt, raw);

        // Enum lookup: index → string in allowed list
        if !config.allowed.is_empty() {
            if let Some(label) = config.allowed.get(raw_int as usize) {
                return Value::String(label.clone());
            }
            // Index out of range: fall through to numeric
        }

        // Signed types
        match dt {
            DataType::Int8 | DataType::Int16 | DataType::Int32 => {
                let signed = decode_raw_signed(dt, raw);
                let physical = signed as f64 * config.scale + config.offset;
                return to_json_number(physical);
            }
            DataType::Float => {
                if raw.len() >= 4 {
                    let f = f32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]);
                    let physical = f as f64 * config.scale + config.offset;
                    return to_json_number(physical);
                }
            }
            _ => {}
        }

        // Unsigned types
        let physical = raw_int as f64 * config.scale + config.offset;
        return to_json_number(physical);
    }

    // No type metadata: hex string
    Value::String(hex::encode(raw))
}

fn encode_raw_integer(config: &OutputConfig, raw: u64) -> Result<Vec<u8>> {
    let dt = config.data_type.as_ref().unwrap_or(&DataType::Uint8);
    let size = dt.byte_size().unwrap_or(1);
    match size {
        1 => Ok(vec![raw as u8]),
        2 => Ok((raw as u16).to_be_bytes().to_vec()),
        4 => Ok((raw as u32).to_be_bytes().to_vec()),
        _ => Ok(vec![raw as u8]),
    }
}

fn encode_raw_signed(dt: &DataType, raw: i64) -> Result<Vec<u8>> {
    let size = dt.byte_size().unwrap_or(1);
    match size {
        1 => Ok(vec![raw as u8]),
        2 => Ok((raw as i16).to_be_bytes().to_vec()),
        4 => Ok((raw as i32).to_be_bytes().to_vec()),
        _ => Ok(vec![raw as u8]),
    }
}

fn decode_raw_unsigned(dt: &DataType, raw: &[u8]) -> u64 {
    match dt.byte_size() {
        Some(1) if !raw.is_empty() => raw[0] as u64,
        Some(2) if raw.len() >= 2 => u16::from_be_bytes([raw[0], raw[1]]) as u64,
        Some(4) if raw.len() >= 4 => u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]) as u64,
        _ if !raw.is_empty() => raw[0] as u64,
        _ => 0,
    }
}

fn decode_raw_signed(dt: &DataType, raw: &[u8]) -> i64 {
    match dt.byte_size() {
        Some(1) if !raw.is_empty() => raw[0] as i8 as i64,
        Some(2) if raw.len() >= 2 => i16::from_be_bytes([raw[0], raw[1]]) as i64,
        Some(4) if raw.len() >= 4 => i32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]) as i64,
        _ if !raw.is_empty() => raw[0] as i8 as i64,
        _ => 0,
    }
}

/// Convert a physical f64 to a JSON number, using integer representation when possible.
fn to_json_number(v: f64) -> Value {
    if v.fract() == 0.0 && v >= i64::MIN as f64 && v <= i64::MAX as f64 {
        Value::Number(serde_json::Number::from(v as i64))
    } else {
        serde_json::Number::from_f64(v)
            .map(Value::Number)
            .unwrap_or(Value::Null)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::OutputConfig;

    fn make_config(
        data_type: Option<DataType>,
        scale: f64,
        offset: f64,
        allowed: Vec<String>,
    ) -> OutputConfig {
        OutputConfig {
            id: "test".into(),
            name: "Test".into(),
            ioid: "0xF000".into(),
            default_value: "00".into(),
            description: None,
            security_level: 0,
            data_type,
            unit: None,
            scale,
            offset,
            min: None,
            max: None,
            allowed,
        }
    }

    #[test]
    fn test_enum_encode_decode() {
        let cfg = make_config(
            Some(DataType::Uint8),
            1.0,
            0.0,
            vec!["off".into(), "slow".into(), "fast".into()],
        );

        // Encode "fast" → [0x02]
        let bytes = encode_output_value(&cfg, &Value::String("fast".into())).unwrap();
        assert_eq!(bytes, vec![0x02]);

        // Decode [0x02] → "fast"
        let val = decode_output_value(&cfg, &[0x02]);
        assert_eq!(val, Value::String("fast".into()));

        // Decode [0x00] → "off"
        let val = decode_output_value(&cfg, &[0x00]);
        assert_eq!(val, Value::String("off".into()));
    }

    #[test]
    fn test_enum_case_insensitive() {
        let cfg = make_config(
            Some(DataType::Uint8),
            1.0,
            0.0,
            vec!["off".into(), "on".into()],
        );
        let bytes = encode_output_value(&cfg, &Value::String("ON".into())).unwrap();
        assert_eq!(bytes, vec![0x01]);
    }

    #[test]
    fn test_boolean_encode_decode() {
        let cfg = make_config(Some(DataType::Uint8), 1.0, 0.0, vec![]);

        let bytes = encode_output_value(&cfg, &Value::Bool(true)).unwrap();
        assert_eq!(bytes, vec![0x01]);

        let bytes = encode_output_value(&cfg, &Value::Bool(false)).unwrap();
        assert_eq!(bytes, vec![0x00]);
    }

    #[test]
    fn test_numeric_with_scale() {
        // throttle: scale=0.392157, so 50.0% → raw = (50.0 - 0) / 0.392157 ≈ 127.5 → 127
        let cfg = make_config(Some(DataType::Uint8), 0.392157, 0.0, vec![]);

        let bytes = encode_output_value(&cfg, &serde_json::json!(50.0)).unwrap();
        assert_eq!(bytes, vec![127]); // round(127.499...) = 127

        // Decode back: 127 * 0.392157 + 0 ≈ 49.804
        let val = decode_output_value(&cfg, &[127]);
        let f = val.as_f64().unwrap();
        assert!((f - 49.804).abs() < 0.1);
    }

    #[test]
    fn test_numeric_with_offset() {
        // temperature: offset=-40, so 25°C → raw = (25 - (-40)) / 1 = 65
        let cfg = make_config(Some(DataType::Uint8), 1.0, -40.0, vec![]);

        let bytes = encode_output_value(&cfg, &serde_json::json!(25)).unwrap();
        assert_eq!(bytes, vec![65]);

        let val = decode_output_value(&cfg, &[65]);
        assert_eq!(val, serde_json::json!(-40 + 65)); // 25
    }

    #[test]
    fn test_hex_fallback() {
        // No data_type → hex string
        let cfg = make_config(None, 1.0, 0.0, vec![]);

        let bytes = encode_output_value(&cfg, &Value::String("ff".into())).unwrap();
        assert_eq!(bytes, vec![0xFF]);

        let val = decode_output_value(&cfg, &[0xFF]);
        assert_eq!(val, Value::String("ff".into()));
    }

    #[test]
    fn test_uint16_encode_decode() {
        let cfg = make_config(Some(DataType::Uint16), 1.0, 0.0, vec![]);

        let bytes = encode_output_value(&cfg, &serde_json::json!(1000)).unwrap();
        assert_eq!(bytes, vec![0x03, 0xE8]); // 1000 big-endian

        let val = decode_output_value(&cfg, &[0x03, 0xE8]);
        assert_eq!(val, serde_json::json!(1000));
    }

    #[test]
    fn test_unknown_string_falls_through_to_hex() {
        // "maybe" is not in allowed list and not valid hex → error
        let cfg = make_config(
            Some(DataType::Uint8),
            1.0,
            0.0,
            vec!["off".into(), "on".into()],
        );
        let result = encode_output_value(&cfg, &Value::String("maybe".into()));
        assert!(result.is_err());
    }

    #[test]
    fn test_raw_hex_with_allowed_list() {
        // A raw tester sends "01" instead of "on" — should work via hex fallback
        let cfg = make_config(
            Some(DataType::Uint8),
            1.0,
            0.0,
            vec!["off".into(), "on".into()],
        );
        let bytes = encode_output_value(&cfg, &Value::String("01".into())).unwrap();
        assert_eq!(bytes, vec![0x01]);
    }

    #[test]
    fn test_numeric_with_allowed_list() {
        // A raw tester sends 1 (number) instead of "on" — should work as index
        let cfg = make_config(
            Some(DataType::Uint8),
            1.0,
            0.0,
            vec!["off".into(), "on".into()],
        );
        let bytes = encode_output_value(&cfg, &serde_json::json!(1)).unwrap();
        assert_eq!(bytes, vec![0x01]);
    }

    #[test]
    fn test_boolean_with_allowed_list() {
        // A tester sends true for a uint8 with allowed labels
        let cfg = make_config(
            Some(DataType::Uint8),
            1.0,
            0.0,
            vec!["off".into(), "on".into()],
        );
        let bytes = encode_output_value(&cfg, &Value::Bool(true)).unwrap();
        assert_eq!(bytes, vec![0x01]);
    }
}
