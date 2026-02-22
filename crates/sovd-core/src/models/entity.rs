//! Entity (component) models

use serde::{Deserialize, Serialize};

/// Information about a diagnostic entity (ECU, HPC, container, etc.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityInfo {
    /// Unique identifier for this entity
    pub id: String,
    /// Human-readable name
    pub name: String,
    /// Entity type (ecu, hpc, app, etc.)
    #[serde(rename = "type")]
    pub entity_type: String,
    /// Description of this entity
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Link to this entity's resources
    pub href: String,
    /// Current status (e.g., "running", "stopped")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

/// Capabilities of a diagnostic entity
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Capabilities {
    /// Can read data parameters
    pub read_data: bool,
    /// Can write data parameters
    pub write_data: bool,
    /// Supports faults/DTCs
    pub faults: bool,
    /// Supports clearing faults
    pub clear_faults: bool,
    /// Supports logs (typically HPC only)
    pub logs: bool,
    /// Supports operations/routines
    pub operations: bool,
    /// Supports software update
    pub software_update: bool,
    /// Supports I/O control
    pub io_control: bool,
    /// Supports session management
    pub sessions: bool,
    /// Supports security access
    pub security: bool,
    /// Has sub-entities (containers for HPC)
    pub sub_entities: bool,
    /// Supports periodic data subscriptions
    pub subscriptions: bool,
}

impl Capabilities {
    /// Create capabilities for a typical UDS ECU
    pub fn uds_ecu() -> Self {
        Self {
            read_data: true,
            write_data: true,
            faults: true,
            clear_faults: true,
            logs: false,
            operations: true,
            software_update: true,
            io_control: true,
            sessions: true,
            security: true,
            sub_entities: false,
            subscriptions: true,
        }
    }

    /// Create capabilities for an HPC node
    pub fn hpc() -> Self {
        Self {
            read_data: true,
            write_data: false,
            faults: true,
            clear_faults: false,
            logs: true,
            operations: true,
            software_update: true,
            io_control: false,
            sessions: false,
            security: false,
            sub_entities: true,
            subscriptions: true,
        }
    }

    /// Create capabilities for a container/app
    pub fn container() -> Self {
        Self {
            read_data: true,
            write_data: false,
            faults: true,
            clear_faults: false,
            logs: true,
            operations: true,
            software_update: true,
            io_control: false,
            sessions: false,
            security: false,
            sub_entities: false,
            subscriptions: true,
        }
    }

    /// Create capabilities for a gateway (empty until backends are registered)
    pub fn gateway() -> Self {
        Self {
            read_data: false,
            write_data: false,
            faults: false,
            clear_faults: false,
            logs: false,
            operations: false,
            software_update: false,
            io_control: false,
            sessions: false,
            security: false,
            sub_entities: true, // Gateway always has sub-entities
            subscriptions: false,
        }
    }
}
