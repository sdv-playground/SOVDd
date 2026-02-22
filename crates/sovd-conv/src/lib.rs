//! sovd-conv - DID Conversion Library for Automotive Diagnostics
//!
//! A library for encoding and decoding automotive diagnostic data (DIDs)
//! with support for scalars, arrays, matrices, enums, and bit fields.
//!
//! # Features
//!
//! - **Type-safe DID handling** - DIDs are `u16`, not strings
//! - **Precision-aware floating point** - no ugly `13.000000001` values
//! - **YAML definition files** - human-readable, like DBC for CAN
//! - **Rich data types** - scalars, arrays, maps, enums, bitfields, histograms
//! - **Axis metadata** - breakpoints for map visualization/interpolation
//!
//! # Quick Start
//!
//! ```rust
//! use sovd_conv::{DidStore, DidDefinition, DataType};
//! use serde_json::json;
//!
//! // Create a store and register definitions
//! let store = DidStore::new();
//!
//! // Coolant temperature: raw 132 → physical = 132 - 40 = 92°C
//! store.register(0xF405, DidDefinition::scaled(DataType::Uint8, 1.0, -40.0)
//!     .with_name("Coolant Temperature")
//!     .with_unit("°C"));
//!
//! // Decode raw bytes
//! let value = store.decode(0xF405, &[132]).unwrap();
//! assert_eq!(value, json!(92));
//!
//! // Encode physical value to bytes
//! let bytes = store.encode(0xF405, &json!(92)).unwrap();
//! assert_eq!(bytes, vec![132]);
//! ```
//!
//! # YAML Definition Files
//!
//! ```yaml
//! meta:
//!   name: Engine ECU
//!   version: "1.0"
//!
//! dids:
//!   0xF405:
//!     id: coolant_temperature     # SOVD-compliant semantic name
//!     name: Coolant Temperature   # Human-readable display name
//!     type: uint8
//!     scale: 1.0
//!     offset: -40.0
//!     unit: °C
//!     min: -40
//!     max: 215
//!
//!   0xF500:
//!     id: fuel_injection_map
//!     name: Fuel Injection Map
//!     type: uint16
//!     scale: 0.01
//!     unit: ms
//!     map:
//!       rows: 16
//!       cols: 16
//!       row_axis:
//!         name: RPM
//!         unit: rpm
//!         breakpoints: [800, 1200, 1600, ...]
//!       col_axis:
//!         name: Load
//!         unit: "%"
//!         breakpoints: [0, 10, 20, ...]
//! ```
//!
//! # Data Types
//!
//! | Type | Description | Example |
//! |------|-------------|---------|
//! | Scalar | Single value | Temperature, RPM |
//! | Array | 1D with optional labels | Wheel speeds (FL, FR, RL, RR) |
//! | Map | 2D with axis breakpoints | Fuel injection map |
//! | Enum | Discrete states | Gear position (P, R, N, D) |
//! | Bitfield | Packed boolean/multi-bit | Status byte |
//! | Histogram | Binned counts | Operating time distribution |

pub mod decode;
pub mod definition;
pub mod encode;
pub mod error;
pub mod precision;
pub mod store;
pub mod types;

// Re-export main types
pub use definition::{BitFieldDef, DidDefinition, HistogramDefinition, MapDefinition};
pub use error::{format_did, parse_did, ConvError, ConvResult};
pub use precision::{precision_from_scale, round_for_scale, to_json_number};
pub use store::{DidStore, StoreMeta};
pub use types::{Axis, BitField, ByteOrder, DataType, Shape};

/// Prelude module for convenient imports
pub mod prelude {
    pub use crate::definition::DidDefinition;
    pub use crate::error::{ConvError, ConvResult};
    pub use crate::store::DidStore;
    pub use crate::types::{ByteOrder, DataType};
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_full_workflow() {
        // Load from YAML
        let yaml = r#"
meta:
  name: Test ECU
  version: "1.0"

dids:
  0xF405:
    name: Coolant Temperature
    type: uint8
    scale: 1.0
    offset: -40.0
    unit: °C

  0xF40C:
    name: Engine RPM
    type: uint16
    scale: 0.25
    unit: rpm

  0xF421:
    name: Wheel Speeds
    type: uint16
    scale: 0.01
    unit: km/h
    array: 4
    labels: [FL, FR, RL, RR]

  0xF404:
    name: Gear Position
    type: uint8
    enum:
      0: P
      1: R
      2: N
      3: D

  0xF410:
    name: Engine Status
    type: uint8
    bits:
      - name: engine_running
        bit: 0
      - name: ac_on
        bit: 1
      - name: check_engine
        bit: 7
"#;

        let store = DidStore::from_yaml(yaml).unwrap();
        assert_eq!(store.len(), 5);

        // Test scalar decode
        let temp = store.decode(0xF405, &[132]).unwrap();
        assert_eq!(temp, json!(92));

        // Test scalar encode
        let bytes = store.encode(0xF405, &json!(92)).unwrap();
        assert_eq!(bytes, vec![132]);

        // Test RPM with fractional scale
        let rpm = store.decode(0xF40C, &[0x1C, 0x20]).unwrap();
        assert_eq!(rpm, json!(1800));

        // Test labeled array
        let wheels = store
            .decode(
                0xF421,
                &[
                    0x27, 0x10, // FL: 10000 → 100
                    0x27, 0x42, // FR: 10050 → 100.5
                    0x26, 0xFC, // RL: 9980 → 99.8
                    0x27, 0x24, // RR: 10020 → 100.2
                ],
            )
            .unwrap();
        // Note: clean integers come out without decimals (100 not 100.0)
        assert_eq!(wheels["FL"], json!(100));
        assert_eq!(wheels["FR"], json!(100.5));

        // Test enum
        let gear = store.decode(0xF404, &[3]).unwrap();
        assert_eq!(gear["label"], json!("D"));

        // Test bitfield
        let status = store.decode(0xF410, &[0b10000001]).unwrap();
        assert_eq!(status["engine_running"], json!(true));
        assert_eq!(status["ac_on"], json!(false));
        assert_eq!(status["check_engine"], json!(true));
    }

    #[test]
    fn test_precision_handling() {
        let store = DidStore::new();
        store.register(0xF500, DidDefinition::scaled(DataType::Uint16, 0.01, 0.0));

        // 140 * 0.01 should be 1.4, not 1.4000000000000001
        let value = store.decode(0xF500, &[0x00, 0x8C]).unwrap(); // 140
        assert_eq!(value, json!(1.4));
    }

    #[test]
    fn test_map_with_axes() {
        let yaml = r#"
dids:
  0xF500:
    name: Fuel Map
    type: uint8
    scale: 0.1
    unit: ms
    map:
      rows: 2
      cols: 2
      row_axis:
        name: RPM
        unit: rpm
        breakpoints: [1000, 2000]
      col_axis:
        name: Load
        unit: "%"
        breakpoints: [0, 50]
"#;

        let store = DidStore::from_yaml(yaml).unwrap();
        let value = store.decode(0xF500, &[10, 11, 12, 13]).unwrap();

        // Check values (clean integers like 1.0 become 1)
        assert_eq!(value["values"], json!([[1, 1.1], [1.2, 1.3]]));

        // Check axes (breakpoints come from YAML as floats)
        assert_eq!(value["row_axis"]["name"], json!("RPM"));
        assert_eq!(value["row_axis"]["breakpoints"], json!([1000.0, 2000.0]));
        assert_eq!(value["col_axis"]["name"], json!("Load"));
    }
}
