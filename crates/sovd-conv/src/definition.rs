//! DID definition structures
//!
//! Represents the complete specification for how to decode/encode a DID.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::types::{Axis, BitField, ByteOrder, DataType};

/// Complete definition for a single DID
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DidDefinition {
    /// SOVD-compliant semantic identifier (e.g., "coolant_temperature", "engine_rpm")
    /// Used for SOVD API routing: /data/{id}
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,

    /// Human-readable display name (e.g., "Coolant Temperature")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// Description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Primitive data type
    #[serde(rename = "type", default)]
    pub data_type: DataType,

    /// Byte order (default: big-endian for UDS)
    #[serde(default)]
    pub byte_order: ByteOrder,

    /// Scale factor: physical = raw * scale + offset
    #[serde(default = "default_scale")]
    pub scale: f64,

    /// Offset: physical = raw * scale + offset
    #[serde(default)]
    pub offset: f64,

    /// Unit string (e.g., "°C", "rpm", "kPa")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,

    /// Minimum valid value (for validation/display)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,

    /// Maximum valid value (for validation/display)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,

    /// Fixed byte length (for strings, bytes)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub length: Option<usize>,

    /// Array length for 1D arrays
    #[serde(skip_serializing_if = "Option::is_none")]
    pub array: Option<usize>,

    /// Labels for array elements (e.g., ["FL", "FR", "RL", "RR"] for wheels)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub labels: Option<Vec<String>>,

    /// Map configuration for 2D data
    #[serde(skip_serializing_if = "Option::is_none")]
    pub map: Option<MapDefinition>,

    /// Histogram configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub histogram: Option<HistogramDefinition>,

    /// Enum mapping for discrete values
    #[serde(rename = "enum", skip_serializing_if = "Option::is_none")]
    pub enum_map: Option<HashMap<u32, String>>,

    /// Bit field definitions (for status bytes)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bits: Option<Vec<BitFieldDef>>,

    /// Explicit precision override (decimal places)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub precision: Option<u8>,

    /// Bit mask to apply before scaling
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bit_mask: Option<u32>,

    /// Bit shift to apply after masking (right shift)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bit_shift: Option<u8>,

    /// Whether this DID supports WriteDataByIdentifier.
    /// Defaults to false — only explicitly writable DIDs can be edited.
    #[serde(default)]
    pub writable: bool,

    /// Component ID this DID belongs to (set automatically from file meta)
    /// None = global (available to all components)
    #[serde(skip)]
    pub component_id: Option<String>,
}

fn default_scale() -> f64 {
    1.0
}

impl Default for DidDefinition {
    fn default() -> Self {
        Self {
            id: None,
            name: None,
            description: None,
            data_type: DataType::Bytes,
            byte_order: ByteOrder::Big,
            scale: 1.0,
            offset: 0.0,
            unit: None,
            min: None,
            max: None,
            length: None,
            array: None,
            labels: None,
            map: None,
            histogram: None,
            enum_map: None,
            bits: None,
            precision: None,
            bit_mask: None,
            bit_shift: None,
            writable: false,
            component_id: None,
        }
    }
}

impl DidDefinition {
    /// Create a simple scalar definition
    pub fn scalar(data_type: DataType) -> Self {
        Self {
            data_type,
            ..Default::default()
        }
    }

    /// Create a scalar with scale/offset
    pub fn scaled(data_type: DataType, scale: f64, offset: f64) -> Self {
        Self {
            data_type,
            scale,
            offset,
            ..Default::default()
        }
    }

    /// Create an array definition
    pub fn array(data_type: DataType, length: usize) -> Self {
        Self {
            data_type,
            array: Some(length),
            ..Default::default()
        }
    }

    /// Create a map (2D matrix) definition
    pub fn map(data_type: DataType, rows: usize, cols: usize) -> Self {
        Self {
            data_type,
            map: Some(MapDefinition {
                rows,
                cols,
                row_axis: None,
                col_axis: None,
            }),
            ..Default::default()
        }
    }

    /// Add a semantic identifier (SOVD-compliant, e.g., "coolant_temperature")
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    /// Add a display name
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Add a unit
    pub fn with_unit(mut self, unit: impl Into<String>) -> Self {
        self.unit = Some(unit.into());
        self
    }

    /// Add scale/offset
    pub fn with_scale(mut self, scale: f64, offset: f64) -> Self {
        self.scale = scale;
        self.offset = offset;
        self
    }

    /// Add min/max bounds
    pub fn with_bounds(mut self, min: f64, max: f64) -> Self {
        self.min = Some(min);
        self.max = Some(max);
        self
    }

    /// Check if this DID is available for a specific component
    /// Returns true if:
    /// - DID has no component_id (global/shared)
    /// - DID's component_id matches the requested component
    pub fn is_available_for(&self, component_id: &str) -> bool {
        match &self.component_id {
            None => true, // Global - available to all
            Some(cid) => cid == component_id,
        }
    }

    /// Check if this is an array type
    pub fn is_array(&self) -> bool {
        self.array.is_some()
    }

    /// Check if this is a map (2D) type
    pub fn is_map(&self) -> bool {
        self.map.is_some()
    }

    /// Check if this is a histogram type
    pub fn is_histogram(&self) -> bool {
        self.histogram.is_some()
    }

    /// Check if this has bit field definitions
    pub fn is_bitfield(&self) -> bool {
        self.bits.is_some() && !self.bits.as_ref().unwrap().is_empty()
    }

    /// Check if this has enum mapping
    pub fn is_enum(&self) -> bool {
        self.enum_map.is_some() && !self.enum_map.as_ref().unwrap().is_empty()
    }

    /// Get the precision to use (explicit or derived from scale)
    pub fn get_precision(&self) -> u8 {
        self.precision
            .unwrap_or_else(|| crate::precision::precision_from_scale(self.scale))
    }

    /// Calculate expected byte length
    pub fn expected_byte_length(&self) -> Option<usize> {
        // For variable-length types
        if let Some(len) = self.length {
            return Some(len);
        }

        let elem_size = self.data_type.byte_size()?;

        if let Some(map) = &self.map {
            Some(map.rows * map.cols * elem_size)
        } else if let Some(arr_len) = self.array {
            Some(arr_len * elem_size)
        } else if let Some(hist) = &self.histogram {
            Some(hist.bins.len() * elem_size)
        } else {
            Some(elem_size)
        }
    }
}

/// Map (2D matrix) configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MapDefinition {
    /// Number of rows
    pub rows: usize,
    /// Number of columns
    pub cols: usize,
    /// Row axis definition (e.g., RPM breakpoints)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub row_axis: Option<Axis>,
    /// Column axis definition (e.g., Load breakpoints)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub col_axis: Option<Axis>,
}

/// Histogram configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistogramDefinition {
    /// Bin edges (N+1 edges for N bins, or N edges if overflow bin)
    pub bins: Vec<f64>,
    /// Whether there's an overflow bin for values above last edge
    #[serde(default)]
    pub overflow: bool,
    /// Optional labels for each bin
    #[serde(skip_serializing_if = "Option::is_none")]
    pub labels: Option<Vec<String>>,
    /// Axis name (what the bins represent)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub axis_name: Option<String>,
    /// Axis unit
    #[serde(skip_serializing_if = "Option::is_none")]
    pub axis_unit: Option<String>,
}

/// Bit field definition (for YAML parsing)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BitFieldDef {
    /// Field name
    pub name: String,
    /// Bit position (0 = LSB)
    pub bit: u8,
    /// Number of bits (default: 1)
    #[serde(default = "default_width")]
    pub width: u8,
    /// Enum mapping for multi-bit fields
    #[serde(rename = "enum", skip_serializing_if = "Option::is_none")]
    pub enum_map: Option<HashMap<u32, String>>,
}

fn default_width() -> u8 {
    1
}

impl From<BitFieldDef> for BitField {
    fn from(def: BitFieldDef) -> Self {
        BitField {
            name: def.name,
            bit: def.bit,
            width: def.width,
            enum_map: def.enum_map,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scalar_definition() {
        let def = DidDefinition::scaled(DataType::Uint8, 1.0, -40.0)
            .with_name("Coolant Temperature")
            .with_unit("°C")
            .with_bounds(-40.0, 215.0);

        assert_eq!(def.name, Some("Coolant Temperature".to_string()));
        assert_eq!(def.data_type, DataType::Uint8);
        assert_eq!(def.scale, 1.0);
        assert_eq!(def.offset, -40.0);
        assert_eq!(def.expected_byte_length(), Some(1));
    }

    #[test]
    fn test_array_definition() {
        let def = DidDefinition::array(DataType::Uint16, 4)
            .with_name("Wheel Speeds")
            .with_scale(0.01, 0.0)
            .with_unit("km/h");

        assert!(def.is_array());
        assert_eq!(def.expected_byte_length(), Some(8)); // 4 * 2 bytes
    }

    #[test]
    fn test_map_definition() {
        let def = DidDefinition::map(DataType::Uint16, 16, 16)
            .with_name("Fuel Map")
            .with_scale(0.01, 0.0)
            .with_unit("ms");

        assert!(def.is_map());
        assert_eq!(def.expected_byte_length(), Some(512)); // 16 * 16 * 2 bytes
    }

    #[test]
    fn test_precision() {
        let def = DidDefinition::scaled(DataType::Uint16, 0.01, 0.0);
        assert_eq!(def.get_precision(), 2);

        let def = DidDefinition::scaled(DataType::Uint8, 1.0, 0.0);
        assert_eq!(def.get_precision(), 0);

        let mut def = DidDefinition::scaled(DataType::Uint16, 0.01, 0.0);
        def.precision = Some(3); // Override
        assert_eq!(def.get_precision(), 3);
    }

    #[test]
    fn test_component_availability() {
        // No component_id - global, available to all
        let def = DidDefinition::scalar(DataType::Uint8);
        assert!(def.is_available_for("engine_ecu"));
        assert!(def.is_available_for("transmission_ecu"));
        assert!(def.is_available_for("any_ecu"));

        // With component_id - only available to that component
        let mut def = DidDefinition::scalar(DataType::Uint8);
        def.component_id = Some("engine_ecu".to_string());
        assert!(def.is_available_for("engine_ecu"));
        assert!(!def.is_available_for("transmission_ecu"));
        assert!(!def.is_available_for("body_ecu"));
    }
}
