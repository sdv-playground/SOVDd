//! Encoding values to raw bytes
//!
//! Converts JSON values to UDS request bytes based on DID definitions.

use serde_json::Value;

use crate::definition::DidDefinition;
use crate::error::{ConvError, ConvResult};
use crate::types::{ByteOrder, DataType};

/// Encode a value according to definition
pub fn encode(def: &DidDefinition, value: &Value) -> ConvResult<Vec<u8>> {
    match value {
        Value::Number(n) => {
            let physical = n
                .as_f64()
                .ok_or_else(|| ConvError::InvalidData("Invalid number".to_string()))?;
            encode_scalar(def, physical)
        }
        Value::Array(arr) => {
            if def.is_map() {
                encode_map(def, arr)
            } else {
                encode_array(def, arr)
            }
        }
        Value::Object(obj) => {
            // Check if it's a labeled array
            if let Some(labels) = &def.labels {
                let mut values = Vec::with_capacity(labels.len());
                for label in labels {
                    if let Some(v) = obj.get(label) {
                        values.push(v.clone());
                    } else {
                        return Err(ConvError::InvalidData(format!("Missing label: {}", label)));
                    }
                }
                return encode_array(def, &values);
            }

            // Check if it's a map with "values" key
            if let Some(Value::Array(arr)) = obj.get("values") {
                return encode_map(def, arr);
            }

            Err(ConvError::InvalidData(
                "Cannot encode object without labels".to_string(),
            ))
        }
        Value::String(s) => {
            if matches!(def.data_type, DataType::String) {
                encode_string(def, s)
            } else {
                // Try to parse as hex
                hex::decode(s)
                    .map_err(|_| ConvError::InvalidData(format!("Invalid hex string: {}", s)))
            }
        }
        _ => Err(ConvError::InvalidData(format!(
            "Cannot encode value type: {:?}",
            value
        ))),
    }
}

/// Encode a single scalar value
fn encode_scalar(def: &DidDefinition, physical: f64) -> ConvResult<Vec<u8>> {
    // Reverse the scale/offset: raw = (physical - offset) / scale
    let raw = ((physical - def.offset) / def.scale).round();

    // Validate bounds
    if let (Some(min), Some(max)) = (def.min, def.max) {
        if physical < min || physical > max {
            return Err(ConvError::ValueOutOfRange {
                value: physical,
                min,
                max,
            });
        }
    }

    write_raw_value(def, raw)
}

/// Encode a 1D array
fn encode_array(def: &DidDefinition, values: &[Value]) -> ConvResult<Vec<u8>> {
    let mut bytes = Vec::new();

    for value in values {
        let physical = value
            .as_f64()
            .ok_or_else(|| ConvError::InvalidData("Array element not a number".to_string()))?;
        let raw = ((physical - def.offset) / def.scale).round();
        bytes.extend(write_raw_value(def, raw)?);
    }

    Ok(bytes)
}

/// Encode a 2D map
fn encode_map(def: &DidDefinition, rows: &[Value]) -> ConvResult<Vec<u8>> {
    let mut bytes = Vec::new();

    for row in rows {
        let row_arr = row
            .as_array()
            .ok_or_else(|| ConvError::InvalidData("Map row not an array".to_string()))?;

        for cell in row_arr {
            let physical = cell
                .as_f64()
                .ok_or_else(|| ConvError::InvalidData("Map cell not a number".to_string()))?;
            let raw = ((physical - def.offset) / def.scale).round();
            bytes.extend(write_raw_value(def, raw)?);
        }
    }

    Ok(bytes)
}

/// Encode a string value
fn encode_string(def: &DidDefinition, s: &str) -> ConvResult<Vec<u8>> {
    let mut bytes = s.as_bytes().to_vec();

    // Pad or truncate to fixed length if specified
    if let Some(len) = def.length {
        bytes.resize(len, 0);
    }

    Ok(bytes)
}

/// Write a raw numeric value to bytes
fn write_raw_value(def: &DidDefinition, raw: f64) -> ConvResult<Vec<u8>> {
    let byte_order = def.byte_order;

    match def.data_type {
        DataType::Uint8 => {
            let v = raw as u8;
            Ok(vec![v])
        }
        DataType::Uint16 => {
            let v = raw as u16;
            Ok(match byte_order {
                ByteOrder::Big => v.to_be_bytes().to_vec(),
                ByteOrder::Little => v.to_le_bytes().to_vec(),
            })
        }
        DataType::Uint32 => {
            let v = raw as u32;
            Ok(match byte_order {
                ByteOrder::Big => v.to_be_bytes().to_vec(),
                ByteOrder::Little => v.to_le_bytes().to_vec(),
            })
        }
        DataType::Int8 => {
            let v = raw as i8;
            Ok(vec![v as u8])
        }
        DataType::Int16 => {
            let v = raw as i16;
            Ok(match byte_order {
                ByteOrder::Big => v.to_be_bytes().to_vec(),
                ByteOrder::Little => v.to_le_bytes().to_vec(),
            })
        }
        DataType::Int32 => {
            let v = raw as i32;
            Ok(match byte_order {
                ByteOrder::Big => v.to_be_bytes().to_vec(),
                ByteOrder::Little => v.to_le_bytes().to_vec(),
            })
        }
        DataType::Float32 => {
            let v = raw as f32;
            Ok(match byte_order {
                ByteOrder::Big => v.to_be_bytes().to_vec(),
                ByteOrder::Little => v.to_le_bytes().to_vec(),
            })
        }
        DataType::Float64 => Ok(match byte_order {
            ByteOrder::Big => raw.to_be_bytes().to_vec(),
            ByteOrder::Little => raw.to_le_bytes().to_vec(),
        }),
        DataType::String | DataType::Bytes => Ok(vec![]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_encode_uint8_temperature() {
        let def = DidDefinition::scaled(DataType::Uint8, 1.0, -40.0);

        // Physical 92°C → raw = (92 - (-40)) / 1 = 132
        let bytes = encode(&def, &json!(92)).unwrap();
        assert_eq!(bytes, vec![132]);
    }

    #[test]
    fn test_encode_uint16_rpm() {
        let def = DidDefinition::scaled(DataType::Uint16, 0.25, 0.0);

        // Physical 1800 rpm → raw = 1800 / 0.25 = 7200 = 0x1C20
        let bytes = encode(&def, &json!(1800)).unwrap();
        assert_eq!(bytes, vec![0x1C, 0x20]);
    }

    #[test]
    fn test_encode_array() {
        let def = DidDefinition::array(DataType::Uint8, 4).with_scale(1.0, -40.0);

        let bytes = encode(&def, &json!([50, 60, 70, 80])).unwrap();
        // 50+40=90, 60+40=100, 70+40=110, 80+40=120
        assert_eq!(bytes, vec![90, 100, 110, 120]);
    }

    #[test]
    fn test_encode_labeled_array() {
        let mut def = DidDefinition::array(DataType::Uint16, 4).with_scale(0.01, 0.0);
        def.labels = Some(vec![
            "FL".to_string(),
            "FR".to_string(),
            "RL".to_string(),
            "RR".to_string(),
        ]);

        let bytes = encode(
            &def,
            &json!({
                "FL": 100.0,
                "FR": 100.5,
                "RL": 99.8,
                "RR": 100.2
            }),
        )
        .unwrap();

        // 100.0/0.01=10000=0x2710, etc.
        assert_eq!(
            bytes,
            vec![
                0x27, 0x10, // 10000
                0x27, 0x42, // 10050
                0x26, 0xFC, // 9980
                0x27, 0x24, // 10020
            ]
        );
    }

    #[test]
    fn test_encode_map() {
        let def = DidDefinition::map(DataType::Uint8, 2, 2).with_scale(1.0, 0.0);

        let bytes = encode(&def, &json!([[1, 2], [3, 4]])).unwrap();
        assert_eq!(bytes, vec![1, 2, 3, 4]);
    }

    #[test]
    fn test_encode_string() {
        let mut def = DidDefinition::scalar(DataType::String);
        def.length = Some(17);

        let bytes = encode(&def, &json!("WF0XXXGCDX12345")).unwrap();
        assert_eq!(bytes.len(), 17);
        assert!(bytes.starts_with(b"WF0XXXGCDX12345"));
    }

    #[test]
    fn test_encode_little_endian() {
        let mut def = DidDefinition::scaled(DataType::Uint16, 1.0, 0.0);
        def.byte_order = ByteOrder::Little;

        let bytes = encode(&def, &json!(0x1234)).unwrap();
        assert_eq!(bytes, vec![0x34, 0x12]); // Little-endian
    }

    #[test]
    fn test_encode_bounds_check() {
        let def = DidDefinition::scaled(DataType::Uint8, 1.0, -40.0).with_bounds(-40.0, 215.0);

        // Within bounds - OK
        assert!(encode(&def, &json!(100)).is_ok());

        // Out of bounds - Error
        let result = encode(&def, &json!(300));
        assert!(matches!(result, Err(ConvError::ValueOutOfRange { .. })));
    }
}
