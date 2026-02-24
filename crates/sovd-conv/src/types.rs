//! Core types for DID conversion
//!
//! Defines the fundamental data types used in automotive diagnostics.

use serde::{Deserialize, Serialize};

/// Primitive data type for raw byte interpretation
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataType {
    /// Unsigned 8-bit integer (1 byte)
    Uint8,
    /// Unsigned 16-bit integer (2 bytes, big-endian)
    Uint16,
    /// Unsigned 32-bit integer (4 bytes, big-endian)
    Uint32,
    /// Signed 8-bit integer (1 byte)
    Int8,
    /// Signed 16-bit integer (2 bytes, big-endian)
    Int16,
    /// Signed 32-bit integer (4 bytes, big-endian)
    Int32,
    /// 32-bit IEEE 754 float (4 bytes, big-endian)
    Float32,
    /// 64-bit IEEE 754 float (8 bytes, big-endian)
    Float64,
    /// ASCII/UTF-8 string
    String,
    /// Raw bytes (hex encoded in JSON)
    #[default]
    Bytes,
}

impl std::fmt::Display for DataType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            DataType::Uint8 => "uint8",
            DataType::Uint16 => "uint16",
            DataType::Uint32 => "uint32",
            DataType::Int8 => "int8",
            DataType::Int16 => "int16",
            DataType::Int32 => "int32",
            DataType::Float32 => "float32",
            DataType::Float64 => "float64",
            DataType::String => "string",
            DataType::Bytes => "bytes",
        };
        f.write_str(s)
    }
}

impl DataType {
    /// Get the byte size for a single element of this type
    /// Returns None for variable-length types (String, Bytes)
    pub fn byte_size(&self) -> Option<usize> {
        match self {
            DataType::Uint8 | DataType::Int8 => Some(1),
            DataType::Uint16 | DataType::Int16 => Some(2),
            DataType::Uint32 | DataType::Int32 | DataType::Float32 => Some(4),
            DataType::Float64 => Some(8),
            DataType::String | DataType::Bytes => None,
        }
    }

    /// Check if this type is signed
    pub fn is_signed(&self) -> bool {
        matches!(self, DataType::Int8 | DataType::Int16 | DataType::Int32)
    }

    /// Check if this type is floating point
    pub fn is_float(&self) -> bool {
        matches!(self, DataType::Float32 | DataType::Float64)
    }
}

/// Byte order for multi-byte values
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ByteOrder {
    /// Big-endian (most significant byte first) - standard for UDS
    #[default]
    Big,
    /// Little-endian (least significant byte first)
    Little,
}

/// Shape of the data (scalar, array, or matrix)
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Shape {
    /// Single value
    #[default]
    Scalar,
    /// 1D array with length
    Array { length: usize },
    /// 2D matrix with dimensions [rows, cols]
    Matrix { rows: usize, cols: usize },
}

impl Shape {
    /// Get total element count
    pub fn element_count(&self) -> usize {
        match self {
            Shape::Scalar => 1,
            Shape::Array { length } => *length,
            Shape::Matrix { rows, cols } => rows * cols,
        }
    }

    /// Check if this is a scalar
    pub fn is_scalar(&self) -> bool {
        matches!(self, Shape::Scalar)
    }

    /// Check if this is an array
    pub fn is_array(&self) -> bool {
        matches!(self, Shape::Array { .. })
    }

    /// Check if this is a matrix
    pub fn is_matrix(&self) -> bool {
        matches!(self, Shape::Matrix { .. })
    }
}

/// Axis definition for maps (breakpoints for interpolation/display)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Axis {
    /// Axis name (e.g., "RPM", "Load")
    pub name: String,
    /// Unit for breakpoint values
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// Breakpoint values
    pub breakpoints: Vec<f64>,
    /// Optional labels for each breakpoint
    #[serde(skip_serializing_if = "Option::is_none")]
    pub labels: Option<Vec<String>>,
}

/// Single bit field within a byte/word
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BitField {
    /// Field name
    pub name: String,
    /// Bit position (0 = LSB) for single bit, or start bit for multi-bit
    pub bit: u8,
    /// Number of bits (1 for boolean, >1 for multi-bit field)
    #[serde(default = "default_bit_width")]
    pub width: u8,
    /// Enum mapping for multi-bit fields
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enum_map: Option<std::collections::HashMap<u32, String>>,
}

fn default_bit_width() -> u8 {
    1
}

impl BitField {
    /// Create a single-bit boolean field
    pub fn boolean(name: impl Into<String>, bit: u8) -> Self {
        Self {
            name: name.into(),
            bit,
            width: 1,
            enum_map: None,
        }
    }

    /// Create a multi-bit field with enum mapping
    pub fn multi_bit(
        name: impl Into<String>,
        bit: u8,
        width: u8,
        enum_map: std::collections::HashMap<u32, String>,
    ) -> Self {
        Self {
            name: name.into(),
            bit,
            width,
            enum_map: Some(enum_map),
        }
    }

    /// Extract this field's value from raw data
    pub fn extract(&self, value: u32) -> u32 {
        let mask = (1u32 << self.width) - 1;
        (value >> self.bit) & mask
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_data_type_sizes() {
        assert_eq!(DataType::Uint8.byte_size(), Some(1));
        assert_eq!(DataType::Uint16.byte_size(), Some(2));
        assert_eq!(DataType::Uint32.byte_size(), Some(4));
        assert_eq!(DataType::Int8.byte_size(), Some(1));
        assert_eq!(DataType::Int16.byte_size(), Some(2));
        assert_eq!(DataType::Int32.byte_size(), Some(4));
        assert_eq!(DataType::Float32.byte_size(), Some(4));
        assert_eq!(DataType::Float64.byte_size(), Some(8));
        assert_eq!(DataType::String.byte_size(), None);
        assert_eq!(DataType::Bytes.byte_size(), None);
    }

    #[test]
    fn test_shape_element_count() {
        assert_eq!(Shape::Scalar.element_count(), 1);
        assert_eq!(Shape::Array { length: 8 }.element_count(), 8);
        assert_eq!(Shape::Matrix { rows: 4, cols: 4 }.element_count(), 16);
    }

    #[test]
    fn test_bit_field_extract() {
        let field = BitField::boolean("test", 2);
        assert_eq!(field.extract(0b00000100), 1);
        assert_eq!(field.extract(0b00000000), 0);

        let field = BitField::multi_bit("gear", 4, 3, Default::default());
        assert_eq!(field.extract(0b01010000), 5); // bits 4-6 = 101 = 5
    }
}
