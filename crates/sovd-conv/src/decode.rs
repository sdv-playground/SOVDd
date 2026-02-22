//! Decoding raw bytes to values
//!
//! Converts UDS response bytes to JSON values based on DID definitions.

use serde_json::{json, Value};

use crate::definition::DidDefinition;
use crate::error::{ConvError, ConvResult};
use crate::precision::to_json_number;
use crate::types::{ByteOrder, DataType};

/// Decode raw bytes according to definition
pub fn decode(def: &DidDefinition, data: &[u8]) -> ConvResult<Value> {
    // Handle string type
    if matches!(def.data_type, DataType::String) {
        return decode_string(def, data);
    }

    // Handle raw bytes type
    if matches!(def.data_type, DataType::Bytes) {
        return Ok(decode_bytes(data));
    }

    // Handle bit fields specially
    if def.is_bitfield() {
        return decode_bitfield(def, data);
    }

    // Handle enum mapping
    if def.is_enum() && def.array.is_none() && def.map.is_none() {
        return decode_enum(def, data);
    }

    // Handle histogram
    if def.is_histogram() {
        return decode_histogram(def, data);
    }

    // Handle map (2D)
    if def.is_map() {
        return decode_map(def, data);
    }

    // Handle array (1D)
    if def.is_array() {
        return decode_array(def, data);
    }

    // Scalar
    decode_scalar(def, data)
}

/// Decode a single scalar value
fn decode_scalar(def: &DidDefinition, data: &[u8]) -> ConvResult<Value> {
    let raw = read_raw_value(def, data, 0)?;
    let physical = raw * def.scale + def.offset;
    Ok(to_json_number(physical, def.scale))
}

/// Decode a 1D array
fn decode_array(def: &DidDefinition, data: &[u8]) -> ConvResult<Value> {
    let length = def
        .array
        .ok_or_else(|| ConvError::InvalidData("Not an array".to_string()))?;
    let elem_size = def
        .data_type
        .byte_size()
        .ok_or_else(|| ConvError::InvalidData("Variable-length type in array".to_string()))?;

    let mut values = Vec::with_capacity(length);

    for i in 0..length {
        let offset = i * elem_size;
        if offset + elem_size <= data.len() {
            let raw = read_raw_value(def, data, offset)?;
            let physical = raw * def.scale + def.offset;
            values.push(to_json_number(physical, def.scale));
        } else {
            values.push(Value::Null);
        }
    }

    // If labels are provided, return as object
    if let Some(labels) = &def.labels {
        if labels.len() == length {
            let mut obj = serde_json::Map::new();
            for (i, label) in labels.iter().enumerate() {
                obj.insert(label.clone(), values[i].clone());
            }
            return Ok(Value::Object(obj));
        }
    }

    Ok(Value::Array(values))
}

/// Decode a 2D map
fn decode_map(def: &DidDefinition, data: &[u8]) -> ConvResult<Value> {
    let map_def = def
        .map
        .as_ref()
        .ok_or_else(|| ConvError::InvalidData("Not a map".to_string()))?;

    let elem_size = def
        .data_type
        .byte_size()
        .ok_or_else(|| ConvError::InvalidData("Variable-length type in map".to_string()))?;

    let mut matrix = Vec::with_capacity(map_def.rows);

    for row in 0..map_def.rows {
        let mut row_values = Vec::with_capacity(map_def.cols);
        for col in 0..map_def.cols {
            let idx = row * map_def.cols + col;
            let offset = idx * elem_size;

            if offset + elem_size <= data.len() {
                let raw = read_raw_value(def, data, offset)?;
                let physical = raw * def.scale + def.offset;
                row_values.push(to_json_number(physical, def.scale));
            } else {
                row_values.push(Value::Null);
            }
        }
        matrix.push(Value::Array(row_values));
    }

    // Build result with axis information if available
    let mut result = serde_json::Map::new();
    result.insert("values".to_string(), Value::Array(matrix));

    if let Some(row_axis) = &map_def.row_axis {
        result.insert(
            "row_axis".to_string(),
            json!({
                "name": row_axis.name,
                "unit": row_axis.unit,
                "breakpoints": row_axis.breakpoints,
            }),
        );
    }

    if let Some(col_axis) = &map_def.col_axis {
        result.insert(
            "col_axis".to_string(),
            json!({
                "name": col_axis.name,
                "unit": col_axis.unit,
                "breakpoints": col_axis.breakpoints,
            }),
        );
    }

    Ok(Value::Object(result))
}

/// Decode histogram data
fn decode_histogram(def: &DidDefinition, data: &[u8]) -> ConvResult<Value> {
    let hist_def = def
        .histogram
        .as_ref()
        .ok_or_else(|| ConvError::InvalidData("Not a histogram".to_string()))?;

    let elem_size = def
        .data_type
        .byte_size()
        .ok_or_else(|| ConvError::InvalidData("Variable-length type in histogram".to_string()))?;

    let num_bins = hist_def.bins.len();
    let mut counts = Vec::with_capacity(num_bins);

    for i in 0..num_bins {
        let offset = i * elem_size;
        if offset + elem_size <= data.len() {
            let raw = read_raw_value(def, data, offset)?;
            let physical = raw * def.scale + def.offset;
            counts.push(to_json_number(physical, def.scale));
        } else {
            counts.push(json!(0));
        }
    }

    let mut result = serde_json::Map::new();
    result.insert("counts".to_string(), Value::Array(counts));
    result.insert("bins".to_string(), json!(hist_def.bins));

    if let Some(labels) = &hist_def.labels {
        result.insert("labels".to_string(), json!(labels));
    }

    if let Some(axis_name) = &hist_def.axis_name {
        result.insert("axis_name".to_string(), json!(axis_name));
    }

    if let Some(axis_unit) = &hist_def.axis_unit {
        result.insert("axis_unit".to_string(), json!(axis_unit));
    }

    Ok(Value::Object(result))
}

/// Decode enum value
fn decode_enum(def: &DidDefinition, data: &[u8]) -> ConvResult<Value> {
    let enum_map = def
        .enum_map
        .as_ref()
        .ok_or_else(|| ConvError::InvalidData("Not an enum".to_string()))?;

    let raw = read_raw_value(def, data, 0)?;
    let raw_int = raw.round() as u32;

    if let Some(label) = enum_map.get(&raw_int) {
        Ok(json!({
            "value": raw_int,
            "label": label
        }))
    } else {
        Ok(json!({
            "value": raw_int,
            "label": null
        }))
    }
}

/// Decode bit fields
fn decode_bitfield(def: &DidDefinition, data: &[u8]) -> ConvResult<Value> {
    let bits = def
        .bits
        .as_ref()
        .ok_or_else(|| ConvError::InvalidData("No bit fields defined".to_string()))?;

    // Read the raw value (usually 1-4 bytes)
    let raw = read_raw_value(def, data, 0)?;
    let raw_int = raw.round() as u32;

    let mut result = serde_json::Map::new();
    result.insert("raw".to_string(), json!(format!("0x{:02X}", raw_int)));

    for field in bits {
        let mask = (1u32 << field.width) - 1;
        let field_value = (raw_int >> field.bit) & mask;

        if field.width == 1 {
            // Boolean field
            result.insert(field.name.clone(), json!(field_value == 1));
        } else if let Some(enum_map) = &field.enum_map {
            // Multi-bit with enum
            if let Some(label) = enum_map.get(&field_value) {
                result.insert(
                    field.name.clone(),
                    json!({
                        "value": field_value,
                        "label": label
                    }),
                );
            } else {
                result.insert(
                    field.name.clone(),
                    json!({
                        "value": field_value,
                        "label": null
                    }),
                );
            }
        } else {
            // Multi-bit without enum
            result.insert(field.name.clone(), json!(field_value));
        }
    }

    Ok(Value::Object(result))
}

/// Read a raw numeric value from data at the given byte offset
fn read_raw_value(def: &DidDefinition, data: &[u8], offset: usize) -> ConvResult<f64> {
    let byte_order = def.byte_order;

    match def.data_type {
        DataType::Uint8 => {
            check_length(data, offset, 1)?;
            let mut raw = data[offset] as u32;
            if let Some(mask) = def.bit_mask {
                raw &= mask;
            }
            if let Some(shift) = def.bit_shift {
                raw >>= shift;
            }
            Ok(raw as f64)
        }
        DataType::Uint16 => {
            check_length(data, offset, 2)?;
            let bytes = [data[offset], data[offset + 1]];
            let mut raw = match byte_order {
                ByteOrder::Big => u16::from_be_bytes(bytes) as u32,
                ByteOrder::Little => u16::from_le_bytes(bytes) as u32,
            };
            if let Some(mask) = def.bit_mask {
                raw &= mask;
            }
            if let Some(shift) = def.bit_shift {
                raw >>= shift;
            }
            Ok(raw as f64)
        }
        DataType::Uint32 => {
            check_length(data, offset, 4)?;
            let bytes = [
                data[offset],
                data[offset + 1],
                data[offset + 2],
                data[offset + 3],
            ];
            let mut raw = match byte_order {
                ByteOrder::Big => u32::from_be_bytes(bytes),
                ByteOrder::Little => u32::from_le_bytes(bytes),
            };
            if let Some(mask) = def.bit_mask {
                raw &= mask;
            }
            if let Some(shift) = def.bit_shift {
                raw >>= shift;
            }
            Ok(raw as f64)
        }
        DataType::Int8 => {
            check_length(data, offset, 1)?;
            Ok(data[offset] as i8 as f64)
        }
        DataType::Int16 => {
            check_length(data, offset, 2)?;
            let bytes = [data[offset], data[offset + 1]];
            let raw = match byte_order {
                ByteOrder::Big => i16::from_be_bytes(bytes),
                ByteOrder::Little => i16::from_le_bytes(bytes),
            };
            Ok(raw as f64)
        }
        DataType::Int32 => {
            check_length(data, offset, 4)?;
            let bytes = [
                data[offset],
                data[offset + 1],
                data[offset + 2],
                data[offset + 3],
            ];
            let raw = match byte_order {
                ByteOrder::Big => i32::from_be_bytes(bytes),
                ByteOrder::Little => i32::from_le_bytes(bytes),
            };
            Ok(raw as f64)
        }
        DataType::Float32 => {
            check_length(data, offset, 4)?;
            let bytes = [
                data[offset],
                data[offset + 1],
                data[offset + 2],
                data[offset + 3],
            ];
            let raw = match byte_order {
                ByteOrder::Big => f32::from_be_bytes(bytes),
                ByteOrder::Little => f32::from_le_bytes(bytes),
            };
            Ok(raw as f64)
        }
        DataType::Float64 => {
            check_length(data, offset, 8)?;
            let bytes = [
                data[offset],
                data[offset + 1],
                data[offset + 2],
                data[offset + 3],
                data[offset + 4],
                data[offset + 5],
                data[offset + 6],
                data[offset + 7],
            ];
            let raw = match byte_order {
                ByteOrder::Big => f64::from_be_bytes(bytes),
                ByteOrder::Little => f64::from_le_bytes(bytes),
            };
            Ok(raw)
        }
        DataType::String | DataType::Bytes => {
            // For strings/bytes, return 0 (these are handled separately)
            Ok(0.0)
        }
    }
}

/// Decode string data
pub fn decode_string(def: &DidDefinition, data: &[u8]) -> ConvResult<Value> {
    let len = def.length.unwrap_or(data.len()).min(data.len());
    let s = String::from_utf8_lossy(&data[..len])
        .trim_end_matches('\0')
        .to_string();
    Ok(json!(s))
}

/// Decode raw bytes as hex string
pub fn decode_bytes(data: &[u8]) -> Value {
    json!(hex::encode(data))
}

fn check_length(data: &[u8], offset: usize, required: usize) -> ConvResult<()> {
    if offset + required > data.len() {
        Err(ConvError::DataTooShort {
            expected: offset + required,
            actual: data.len(),
        })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_decode_uint8_temperature() {
        let def = DidDefinition::scaled(DataType::Uint8, 1.0, -40.0)
            .with_name("Coolant Temp")
            .with_unit("°C");

        let value = decode(&def, &[132]).unwrap();
        assert_eq!(value, json!(92));
    }

    #[test]
    fn test_decode_uint16_rpm() {
        let def = DidDefinition::scaled(DataType::Uint16, 0.25, 0.0)
            .with_name("Engine RPM")
            .with_unit("rpm");

        let value = decode(&def, &[0x1C, 0x20]).unwrap();
        assert_eq!(value, json!(1800));
    }

    #[test]
    fn test_decode_array_with_labels() {
        let mut def = DidDefinition::array(DataType::Uint16, 4).with_scale(0.01, 0.0);
        def.labels = Some(vec![
            "FL".to_string(),
            "FR".to_string(),
            "RL".to_string(),
            "RR".to_string(),
        ]);

        let data = [
            0x27, 0x10, // 10000 → 100.0
            0x27, 0x42, // 10050 → 100.5
            0x26, 0xFC, // 9980 → 99.8
            0x27, 0x24, // 10020 → 100.2
        ];

        let value = decode(&def, &data).unwrap();

        assert!(value.is_object());
        // Note: clean integers come out without decimals (100 not 100.0)
        assert_eq!(value["FL"], json!(100));
        assert_eq!(value["FR"], json!(100.5));
        assert_eq!(value["RL"], json!(99.8));
        assert_eq!(value["RR"], json!(100.2));
    }

    #[test]
    fn test_decode_enum() {
        let mut def = DidDefinition::scalar(DataType::Uint8);
        def.enum_map = Some(HashMap::from([
            (0, "Off".to_string()),
            (1, "Cranking".to_string()),
            (2, "Running".to_string()),
        ]));

        let value = decode(&def, &[2]).unwrap();
        assert_eq!(value["value"], json!(2));
        assert_eq!(value["label"], json!("Running"));
    }

    #[test]
    fn test_decode_bitfield() {
        let mut def = DidDefinition::scalar(DataType::Uint8);
        def.bits = Some(vec![
            crate::definition::BitFieldDef {
                name: "engine_running".to_string(),
                bit: 0,
                width: 1,
                enum_map: None,
            },
            crate::definition::BitFieldDef {
                name: "ac_on".to_string(),
                bit: 1,
                width: 1,
                enum_map: None,
            },
            crate::definition::BitFieldDef {
                name: "gear".to_string(),
                bit: 4,
                width: 3,
                enum_map: Some(HashMap::from([
                    (0, "P".to_string()),
                    (1, "R".to_string()),
                    (2, "N".to_string()),
                    (3, "D".to_string()),
                ])),
            },
        ]);

        // 0b00110001 = engine running, ac off, gear = 3 (D)
        let value = decode(&def, &[0b00110001]).unwrap();

        assert_eq!(value["engine_running"], json!(true));
        assert_eq!(value["ac_on"], json!(false));
        assert_eq!(value["gear"]["value"], json!(3));
        assert_eq!(value["gear"]["label"], json!("D"));
    }

    #[test]
    fn test_decode_little_endian() {
        let mut def = DidDefinition::scaled(DataType::Uint16, 1.0, 0.0);
        def.byte_order = ByteOrder::Little;

        // Little-endian: 0x1234 stored as [0x34, 0x12]
        let value = decode(&def, &[0x34, 0x12]).unwrap();
        assert_eq!(value, json!(0x1234));
    }

    #[test]
    fn test_decode_map_2x2() {
        let def = DidDefinition::map(DataType::Uint8, 2, 2).with_scale(1.0, 0.0);

        let value = decode(&def, &[1, 2, 3, 4]).unwrap();

        assert_eq!(value["values"], json!([[1, 2], [3, 4]]));
    }
}
