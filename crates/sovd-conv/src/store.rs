//! DID Store - the main container for definitions
//!
//! Provides lookup by DID (u16) and conversion operations.

use std::collections::HashMap;
use std::path::Path;
use std::sync::RwLock;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::decode;
use crate::definition::DidDefinition;
use crate::encode;
use crate::error::{parse_did, ConvError, ConvResult};

/// Thread-safe store for DID definitions
///
/// Supports multiple definitions per DID to allow different ECUs to have
/// the same DID with different configurations (e.g., both body_ecu and vtx_ecm
/// can have DID 0xF405 for temperature).
#[derive(Debug, Default)]
pub struct DidStore {
    /// Map of DID (u16) → list of definitions (one per component)
    definitions: RwLock<HashMap<u16, Vec<DidDefinition>>>,
    /// Reverse index: semantic id → DID (for SOVD-compliant name lookup)
    /// Stores both "param_name" → DID and "component/param_name" → DID
    name_index: RwLock<HashMap<String, u16>>,
    /// Metadata about the store
    meta: RwLock<StoreMeta>,
}

/// Metadata about the store
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StoreMeta {
    /// Name of the ECU/definition set
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Component ID this definition file belongs to
    /// All DIDs in this file will be associated with this component
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component_id: Option<String>,
    /// Version string
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl DidStore {
    /// Create a new empty store
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a store with metadata
    pub fn with_meta(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            definitions: RwLock::new(HashMap::new()),
            name_index: RwLock::new(HashMap::new()),
            meta: RwLock::new(StoreMeta {
                name: Some(name.into()),
                component_id: None,
                version: Some(version.into()),
                description: None,
            }),
        }
    }

    /// Create a store for a specific component
    pub fn for_component(component_id: impl Into<String>) -> Self {
        let component_id = component_id.into();
        Self {
            definitions: RwLock::new(HashMap::new()),
            name_index: RwLock::new(HashMap::new()),
            meta: RwLock::new(StoreMeta {
                name: None,
                component_id: Some(component_id),
                version: None,
                description: None,
            }),
        }
    }

    /// Load definitions from a YAML file
    pub fn from_file(path: impl AsRef<Path>) -> ConvResult<Self> {
        let content = std::fs::read_to_string(path)?;
        Self::from_yaml(&content)
    }

    /// Load definitions from YAML string
    pub fn from_yaml(yaml: &str) -> ConvResult<Self> {
        let file: DefinitionFile = serde_yaml::from_str(yaml)?;
        let store = Self::new();

        // Get component_id from meta - all DIDs in this file belong to this component
        let file_component_id = file.meta.as_ref().and_then(|m| m.component_id.clone());

        // Set metadata
        if let Some(meta) = file.meta {
            *store.meta.write().unwrap() = meta;
        }

        // Load definitions
        if let Some(dids) = file.dids {
            for (did_str, mut def) in dids {
                let did = parse_did(&did_str)?;

                // Set component_id from file meta
                def.component_id = file_component_id.clone();

                // Use register to properly handle storage and indexing
                store.register(did, def);
            }
        }

        Ok(store)
    }

    /// Register a definition for a DID
    ///
    /// Multiple definitions can be registered for the same DID if they have
    /// different component_ids. This supports multi-ECU gateways where different
    /// ECUs may have the same DID with different configurations.
    pub fn register(&self, did: u16, def: DidDefinition) {
        // Index by semantic id if present
        if let Some(ref id) = def.id {
            let mut names = self.name_index.write().unwrap();
            // Always index by plain name (last one wins for global lookup)
            names.insert(id.clone(), did);
            // Also index by "component/name" for component-specific lookup
            if let Some(ref comp_id) = def.component_id {
                names.insert(format!("{}/{}", comp_id, id), did);
            }
        }

        let mut defs = self.definitions.write().unwrap();
        let entries = defs.entry(did).or_insert_with(Vec::new);

        // Replace existing definition for same component, or add new one
        if let Some(ref comp_id) = def.component_id {
            if let Some(pos) = entries
                .iter()
                .position(|d| d.component_id.as_ref() == Some(comp_id))
            {
                entries[pos] = def;
            } else {
                entries.push(def);
            }
        } else {
            // Global definition (no component_id) - replace any existing global
            if let Some(pos) = entries.iter().position(|d| d.component_id.is_none()) {
                entries[pos] = def;
            } else {
                entries.push(def);
            }
        }
    }

    /// Register using string DID (for convenience)
    pub fn register_str(&self, did: &str, def: DidDefinition) -> ConvResult<()> {
        let did = parse_did(did)?;
        self.register(did, def);
        Ok(())
    }

    /// Get a definition by DID (returns first matching definition)
    ///
    /// For multi-component scenarios, use `get_for_component` instead.
    pub fn get(&self, did: u16) -> Option<DidDefinition> {
        let defs = self.definitions.read().unwrap();
        defs.get(&did).and_then(|v| v.first().cloned())
    }

    /// Get a definition by DID for a specific component
    pub fn get_for_component(&self, did: u16, component_id: &str) -> Option<DidDefinition> {
        let defs = self.definitions.read().unwrap();
        defs.get(&did).and_then(|entries| {
            entries
                .iter()
                .find(|d| d.is_available_for(component_id))
                .cloned()
        })
    }

    /// Get a definition by string DID
    pub fn get_str(&self, did: &str) -> ConvResult<Option<DidDefinition>> {
        let did = parse_did(did)?;
        Ok(self.get(did))
    }

    /// Remove all definitions for a DID
    pub fn remove(&self, did: u16) -> Option<Vec<DidDefinition>> {
        let mut defs = self.definitions.write().unwrap();
        let removed = defs.remove(&did);
        // Remove from name index if present
        if let Some(ref entries) = removed {
            let mut names = self.name_index.write().unwrap();
            for def in entries {
                if let Some(ref id) = def.id {
                    names.remove(id);
                    if let Some(ref comp_id) = def.component_id {
                        names.remove(&format!("{}/{}", comp_id, id));
                    }
                }
            }
        }
        removed
    }

    /// Look up DID by semantic name (returns first matching definition)
    pub fn get_by_name(&self, name: &str) -> Option<(u16, DidDefinition)> {
        let names = self.name_index.read().unwrap();
        if let Some(&did) = names.get(name) {
            drop(names);
            let defs = self.definitions.read().unwrap();
            defs.get(&did)
                .and_then(|v| v.first().cloned())
                .map(|def| (did, def))
        } else {
            None
        }
    }

    /// Resolve a parameter identifier - tries semantic name first, then DID hex format
    /// Returns (did, definition) or None if not found
    pub fn resolve(&self, identifier: &str) -> Option<(u16, DidDefinition)> {
        // Try semantic name first (SOVD-compliant)
        if let Some(result) = self.get_by_name(identifier) {
            return Some(result);
        }

        // Fall back to DID hex parsing
        if let Ok(did) = parse_did(identifier) {
            let defs = self.definitions.read().unwrap();
            return defs
                .get(&did)
                .and_then(|v| v.first().cloned())
                .map(|def| (did, def));
        }

        None
    }

    /// Resolve a parameter identifier, returning just the DID
    /// Useful when definition is optional (e.g., raw DID access)
    pub fn resolve_did(&self, identifier: &str) -> Option<u16> {
        // Try semantic name first
        {
            let names = self.name_index.read().unwrap();
            if let Some(&did) = names.get(identifier) {
                return Some(did);
            }
        }

        // Fall back to DID hex parsing
        parse_did(identifier).ok()
    }

    /// Check if a DID is registered
    pub fn contains(&self, did: u16) -> bool {
        let defs = self.definitions.read().unwrap();
        defs.contains_key(&did)
    }

    /// Check if a global (non-component-scoped) definition exists for a DID.
    /// Component-scoped definitions (from YAML files with component_id) are not counted.
    pub fn contains_global(&self, did: u16) -> bool {
        let defs = self.definitions.read().unwrap();
        defs.get(&did)
            .map(|entries| entries.iter().any(|d| d.component_id.is_none()))
            .unwrap_or(false)
    }

    /// Get all registered DIDs
    pub fn list(&self) -> Vec<u16> {
        let defs = self.definitions.read().unwrap();
        defs.keys().copied().collect()
    }

    /// Get all definitions (returns first definition for each DID)
    pub fn list_all(&self) -> HashMap<u16, DidDefinition> {
        let defs = self.definitions.read().unwrap();
        defs.iter()
            .filter_map(|(&did, entries)| entries.first().cloned().map(|def| (did, def)))
            .collect()
    }

    /// Check if any DID definitions are explicitly registered for a specific component
    /// (not global definitions). Used to distinguish ECU backends (which have config-defined
    /// DIDs) from proxy backends (which get parameters from a remote server).
    pub fn has_component_specific_dids(&self, component_id: &str) -> bool {
        let defs = self.definitions.read().unwrap();
        defs.values().any(|entries| {
            entries
                .iter()
                .any(|def| def.component_id.as_deref() == Some(component_id))
        })
    }

    /// Get definitions filtered by component ID
    /// Only returns DIDs that are available for the specified component
    pub fn list_for_component(&self, component_id: &str) -> HashMap<u16, DidDefinition> {
        let defs = self.definitions.read().unwrap();
        defs.iter()
            .filter_map(|(&did, entries)| {
                entries
                    .iter()
                    .find(|def| def.is_available_for(component_id))
                    .cloned()
                    .map(|def| (did, def))
            })
            .collect()
    }

    /// Get the number of unique DIDs registered
    pub fn len(&self) -> usize {
        let defs = self.definitions.read().unwrap();
        defs.len()
    }

    /// Get total number of definitions (including multiple per DID)
    pub fn total_definitions(&self) -> usize {
        let defs = self.definitions.read().unwrap();
        defs.values().map(|v| v.len()).sum()
    }

    /// Check if the store is empty
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Clear all definitions
    pub fn clear(&self) {
        let mut defs = self.definitions.write().unwrap();
        defs.clear();
        let mut names = self.name_index.write().unwrap();
        names.clear();
    }

    /// Merge definitions from another store into this one
    /// Useful for loading multiple component-specific YAML files into a global store
    pub fn merge(&self, other: &DidStore) {
        let other_defs = other.definitions.read().unwrap();

        for (&did, entries) in other_defs.iter() {
            for def in entries {
                // Use register to properly handle deduplication
                self.register(did, def.clone());
            }
        }
    }

    /// Load and merge a YAML file into this store
    pub fn load_file(&self, path: impl AsRef<Path>) -> ConvResult<()> {
        let other = Self::from_file(path)?;
        self.merge(&other);
        Ok(())
    }

    /// Load and merge YAML string into this store
    pub fn load_yaml(&self, yaml: &str) -> ConvResult<()> {
        let other = Self::from_yaml(yaml)?;
        self.merge(&other);
        Ok(())
    }

    /// Get metadata
    pub fn meta(&self) -> StoreMeta {
        self.meta.read().unwrap().clone()
    }

    /// Set metadata
    pub fn set_meta(&self, meta: StoreMeta) {
        *self.meta.write().unwrap() = meta;
    }

    // =========================================================================
    // Decode/Encode Operations
    // =========================================================================

    /// Decode raw bytes for a DID
    pub fn decode(&self, did: u16, data: &[u8]) -> ConvResult<Value> {
        let def = self.get(did).ok_or(ConvError::UnknownDid(did))?;
        decode::decode(&def, data)
    }

    /// Decode raw bytes for a DID (string version)
    pub fn decode_str(&self, did: &str, data: &[u8]) -> ConvResult<Value> {
        let did = parse_did(did)?;
        self.decode(did, data)
    }

    /// Decode raw bytes, returning raw hex if DID is not registered
    pub fn decode_or_raw(&self, did: u16, data: &[u8]) -> Value {
        if let Some(def) = self.get(did) {
            decode::decode(&def, data).unwrap_or_else(|_| decode::decode_bytes(data))
        } else {
            decode::decode_bytes(data)
        }
    }

    /// Encode a value for a DID
    pub fn encode(&self, did: u16, value: &Value) -> ConvResult<Vec<u8>> {
        let def = self.get(did).ok_or(ConvError::UnknownDid(did))?;
        encode::encode(&def, value)
    }

    /// Encode a value for a DID (string version)
    pub fn encode_str(&self, did: &str, value: &Value) -> ConvResult<Vec<u8>> {
        let did = parse_did(did)?;
        self.encode(did, value)
    }

    // =========================================================================
    // Export
    // =========================================================================

    /// Export definitions to YAML (exports first definition per DID)
    pub fn to_yaml(&self) -> ConvResult<String> {
        let defs = self.definitions.read().unwrap();
        let meta = self.meta.read().unwrap();

        let mut dids: HashMap<String, DidDefinition> = HashMap::new();
        for (&did, entries) in defs.iter() {
            if let Some(def) = entries.first() {
                dids.insert(format!("0x{:04X}", did), def.clone());
            }
        }

        let file = DefinitionFile {
            meta: Some(meta.clone()),
            dids: Some(dids),
        };

        Ok(serde_yaml::to_string(&file)?)
    }
}

/// YAML file structure for definitions
#[derive(Debug, Serialize, Deserialize)]
struct DefinitionFile {
    #[serde(skip_serializing_if = "Option::is_none")]
    meta: Option<StoreMeta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dids: Option<HashMap<String, DidDefinition>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::DataType;
    use serde_json::json;

    #[test]
    fn test_store_register_and_get() {
        let store = DidStore::new();

        let def = DidDefinition::scaled(DataType::Uint8, 1.0, -40.0)
            .with_name("Coolant Temp")
            .with_unit("°C");

        store.register(0xF405, def);

        assert!(store.contains(0xF405));
        assert!(!store.contains(0xFFFF));

        let retrieved = store.get(0xF405).unwrap();
        assert_eq!(retrieved.name, Some("Coolant Temp".to_string()));
    }

    #[test]
    fn test_store_decode() {
        let store = DidStore::new();
        store.register(0xF405, DidDefinition::scaled(DataType::Uint8, 1.0, -40.0));

        let value = store.decode(0xF405, &[132]).unwrap();
        assert_eq!(value, json!(92));
    }

    #[test]
    fn test_store_encode() {
        let store = DidStore::new();
        store.register(0xF40C, DidDefinition::scaled(DataType::Uint16, 0.25, 0.0));

        let bytes = store.encode(0xF40C, &json!(1800)).unwrap();
        assert_eq!(bytes, vec![0x1C, 0x20]);
    }

    #[test]
    fn test_store_from_yaml() {
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
    min: -40
    max: 215

  0xF40C:
    name: Engine RPM
    type: uint16
    scale: 0.25
    unit: rpm
"#;

        let store = DidStore::from_yaml(yaml).unwrap();

        assert_eq!(store.meta().name, Some("Test ECU".to_string()));
        assert_eq!(store.len(), 2);

        let value = store.decode(0xF405, &[132]).unwrap();
        assert_eq!(value, json!(92));

        let value = store.decode(0xF40C, &[0x1C, 0x20]).unwrap();
        assert_eq!(value, json!(1800));
    }

    #[test]
    fn test_store_unknown_did() {
        let store = DidStore::new();

        let result = store.decode(0xFFFF, &[1, 2, 3]);
        assert!(matches!(result, Err(ConvError::UnknownDid(0xFFFF))));
    }

    #[test]
    fn test_store_decode_or_raw() {
        let store = DidStore::new();
        store.register(0xF405, DidDefinition::scaled(DataType::Uint8, 1.0, -40.0));

        // Known DID - decoded
        let value = store.decode_or_raw(0xF405, &[132]);
        assert_eq!(value, json!(92));

        // Unknown DID - raw hex
        let value = store.decode_or_raw(0xFFFF, &[0xAB, 0xCD]);
        assert_eq!(value, json!("abcd"));
    }

    #[test]
    fn test_store_roundtrip_yaml() {
        let store = DidStore::with_meta("Test ECU", "1.0");
        store.register(
            0xF405,
            DidDefinition::scaled(DataType::Uint8, 1.0, -40.0)
                .with_name("Coolant Temp")
                .with_unit("°C"),
        );

        let yaml = store.to_yaml().unwrap();
        let store2 = DidStore::from_yaml(&yaml).unwrap();

        assert_eq!(store2.len(), 1);
        assert!(store2.contains(0xF405));
    }

    #[test]
    fn test_store_list_for_component() {
        let store = DidStore::new();

        // Global DID (no component_id)
        store.register(
            0xF190,
            DidDefinition::scalar(DataType::Bytes)
                .with_id("vin")
                .with_name("VIN"),
        );

        // Engine-specific DIDs (set component_id directly)
        let mut engine_rpm = DidDefinition::scaled(DataType::Uint16, 0.25, 0.0)
            .with_id("engine_rpm")
            .with_name("Engine RPM");
        engine_rpm.component_id = Some("engine_ecu".to_string());
        store.register(0xF40C, engine_rpm);

        let mut coolant_temp = DidDefinition::scaled(DataType::Uint8, 1.0, -40.0)
            .with_id("coolant_temp")
            .with_name("Coolant Temperature");
        coolant_temp.component_id = Some("engine_ecu".to_string());
        store.register(0xF405, coolant_temp);

        // Transmission-specific DID
        let mut gear_pos = DidDefinition::scalar(DataType::Uint8)
            .with_id("gear_position")
            .with_name("Gear Position");
        gear_pos.component_id = Some("transmission_ecu".to_string());
        store.register(0xF401, gear_pos);

        // Engine ECU should see: VIN (global), engine_rpm, coolant_temp
        let engine_dids = store.list_for_component("engine_ecu");
        assert_eq!(engine_dids.len(), 3);
        assert!(engine_dids.contains_key(&0xF190)); // VIN (global)
        assert!(engine_dids.contains_key(&0xF40C)); // engine_rpm
        assert!(engine_dids.contains_key(&0xF405)); // coolant_temp
        assert!(!engine_dids.contains_key(&0xF401)); // NOT gear_position

        // Transmission ECU should see: VIN (global), gear_position
        let trans_dids = store.list_for_component("transmission_ecu");
        assert_eq!(trans_dids.len(), 2);
        assert!(trans_dids.contains_key(&0xF190)); // VIN (global)
        assert!(trans_dids.contains_key(&0xF401)); // gear_position
        assert!(!trans_dids.contains_key(&0xF40C)); // NOT engine_rpm

        // Unknown ECU should only see global DIDs
        let unknown_dids = store.list_for_component("unknown_ecu");
        assert_eq!(unknown_dids.len(), 1);
        assert!(unknown_dids.contains_key(&0xF190)); // VIN (global)
    }

    #[test]
    fn test_store_component_from_meta() {
        // Load engine ECU definitions with component_id in meta
        let engine_yaml = r#"
meta:
  name: Engine ECU
  component_id: engine_ecu
  version: "1.0"

dids:
  0xF40C:
    id: engine_rpm
    name: Engine RPM
    type: uint16
    scale: 0.25
    unit: rpm

  0xF405:
    id: coolant_temp
    name: Coolant Temperature
    type: uint8
"#;

        // Load transmission ECU definitions
        let trans_yaml = r#"
meta:
  name: Transmission ECU
  component_id: transmission_ecu

dids:
  0xF401:
    id: gear_position
    name: Gear Position
    type: uint8
"#;

        // Load shared/global definitions (no component_id)
        let global_yaml = r#"
meta:
  name: Shared Definitions

dids:
  0xF190:
    id: vin
    name: VIN
    type: string
    length: 17
"#;

        // Create a global store and merge all files
        let store = DidStore::new();
        store.load_yaml(engine_yaml).unwrap();
        store.load_yaml(trans_yaml).unwrap();
        store.load_yaml(global_yaml).unwrap();

        assert_eq!(store.len(), 4);

        // Engine ECU sees its DIDs + global
        let engine_dids = store.list_for_component("engine_ecu");
        assert_eq!(engine_dids.len(), 3); // engine_rpm, coolant_temp, vin
        assert!(engine_dids.contains_key(&0xF40C));
        assert!(engine_dids.contains_key(&0xF405));
        assert!(engine_dids.contains_key(&0xF190));

        // Transmission ECU sees its DIDs + global
        let trans_dids = store.list_for_component("transmission_ecu");
        assert_eq!(trans_dids.len(), 2); // gear_position, vin
        assert!(trans_dids.contains_key(&0xF401));
        assert!(trans_dids.contains_key(&0xF190));

        // Some other ECU only sees global
        let other_dids = store.list_for_component("body_ecu");
        assert_eq!(other_dids.len(), 1); // just vin
        assert!(other_dids.contains_key(&0xF190));
    }
}
