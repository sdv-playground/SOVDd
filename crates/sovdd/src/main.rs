//! sovdd - SOVD Server Daemon
//!
//! REST API gateway for vehicle diagnostics (UDS/CAN, DoIP, etc.)
//!
//! Usage:
//!   sovdd [OPTIONS] [config.toml]
//!
//! Options:
//!   --did-definitions <path>  Load DID definitions from file or directory
//!                             Supports .yaml/.json files (sovd-conv format)
//!
//! If no config file is provided, uses mock transport for demo purposes.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use sovd_api::{create_router, AppState};
use sovd_conv::DidStore;
use sovd_gateway::GatewayBackend;
use sovd_proxy::SovdProxyBackend;
use sovd_uds::{
    config::{
        FlashCommitConfig, IsoTpConfig, MockConfig, OperationConfig, OutputConfig,
        ServiceOverrides, SessionConfig, SocketCanConfig, TransportConfig, UdsBackendConfig,
    },
    DiagnosticBackend, UdsBackend,
};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

/// Parsed command-line arguments
struct Args {
    /// Server config file (TOML)
    config_path: Option<String>,
    /// DID definition files/directories (sovd-conv format)
    did_definitions: Vec<String>,
}

fn parse_args() -> Args {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut result = Args {
        config_path: None,
        did_definitions: Vec::new(),
    };

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--did-definitions" | "-d" => {
                if i + 1 < args.len() {
                    result.did_definitions.push(args[i + 1].clone());
                    i += 2;
                } else {
                    tracing::error!("Missing argument for --did-definitions");
                    i += 1;
                }
            }
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            arg if !arg.starts_with('-') => {
                // Positional argument = config file
                result.config_path = Some(arg.to_string());
                i += 1;
            }
            _ => {
                tracing::warn!("Unknown argument: {}", args[i]);
                i += 1;
            }
        }
    }

    result
}

fn print_help() {
    eprintln!(
        r#"sovdd - SOVD Server Daemon

Usage: sovdd [OPTIONS] [config.toml]

Options:
  -d, --did-definitions <path>  Load DID definitions from file or directory
                                Supports .yaml/.json files (sovd-conv format)
                                Can be specified multiple times
  -h, --help                    Print this help message

Examples:
  # Run with mock transport
  sovdd

  # Run with config file
  sovdd config.toml

  # Run with DID definitions
  sovdd --did-definitions config/did-definitions/ config.toml

  # Multiple definition files
  sovdd -d engine.did.yaml -d transmission.did.yaml config.toml
"#
    );
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize logging
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                "sovdd=info,sovd_api=info,sovd_uds=debug,sovd_gateway=info".into()
            }),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    tracing::info!("Starting sovdd (SOVD Server Daemon)");

    // Parse command-line arguments
    let args = parse_args();

    // Load DID definitions from files
    let did_store = load_did_definitions(&args)?;
    let did_count = did_store.len();
    if did_count > 0 {
        tracing::info!("Loaded {} DID definitions from files", did_count);
    }

    // Load backend configuration (also registers inline params in DidStore)
    let (backends, port, output_configs) = if let Some(ref path) = args.config_path {
        tracing::info!("Loading config from: {}", path);
        load_config_file(path, &did_store).await?
    } else {
        tracing::info!("No config file provided, using mock transport");
        let (backends, output_configs) = create_mock_backends().await?;
        (backends, 18081, output_configs)
    };

    let final_count = did_store.len();
    if final_count > did_count {
        tracing::info!(
            "Registered {} additional DIDs from config",
            final_count - did_count
        );
    }

    // Register standard identification DIDs as global (available to all components).
    // These are spec-defined strings (ISO 14229-1 Annex C) — no YAML entry needed.
    // Per-component YAML entries take precedence if present.
    register_standard_dids(&did_store);

    // Create the app state with DID store and output configs
    let state = AppState::with_output_configs(backends, Arc::new(did_store), output_configs);

    // Create the router
    let app = create_router(state);

    // Bind to address
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    tracing::info!("Listening on http://{}", addr);

    // Run the server
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

/// Load DID definitions from CLI arguments
fn load_did_definitions(args: &Args) -> anyhow::Result<DidStore> {
    let mut store = DidStore::new();

    for path_str in &args.did_definitions {
        let path = Path::new(path_str);
        if path.is_dir() {
            load_did_definitions_from_directory(&mut store, path)?;
        } else if path.is_file() {
            load_did_definition_file(&mut store, path)?;
        } else {
            tracing::warn!("DID definitions path not found: {}", path_str);
        }
    }

    Ok(store)
}

/// Load DID definitions from a directory
fn load_did_definitions_from_directory(store: &mut DidStore, dir: &Path) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_file() {
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if ext == "yaml" || ext == "yml" || ext == "json" {
                if let Err(e) = load_did_definition_file(store, &path) {
                    tracing::warn!("Failed to load {}: {}", path.display(), e);
                }
            }
        }
    }
    Ok(())
}

/// Load a single DID definition file (sovd-conv format)
fn load_did_definition_file(store: &mut DidStore, path: &Path) -> anyhow::Result<()> {
    let content = std::fs::read_to_string(path)?;
    let loaded = DidStore::from_yaml(&content)?;

    let count = loaded.len();
    tracing::info!("Loaded {} DIDs from {}", count, path.display());

    // Merge into main store
    for did in loaded.list() {
        if let Some(def) = loaded.get(did) {
            store.register(did, def);
        }
    }

    // Update metadata if present
    let meta = loaded.meta();
    if meta.name.is_some() || meta.version.is_some() {
        store.set_meta(meta);
    }

    Ok(())
}

/// Load configuration from TOML file
async fn load_config_file(
    path: &str,
    did_store: &DidStore,
) -> anyhow::Result<(
    HashMap<String, Arc<dyn DiagnosticBackend>>,
    u16,
    HashMap<String, Vec<OutputConfig>>,
)> {
    let content = std::fs::read_to_string(path)?;
    let config: toml::Value = toml::from_str(&content)?;

    let port = config
        .get("server")
        .and_then(|s| s.get("port"))
        .and_then(|p| p.as_integer())
        .unwrap_or(18081) as u16;

    let mut backends: HashMap<String, Arc<dyn DiagnosticBackend>> = HashMap::new();
    let mut output_configs: HashMap<String, Vec<OutputConfig>> = HashMap::new();

    // Check if gateway mode is enabled
    let gateway_enabled = config
        .get("gateway")
        .and_then(|g| g.get("enabled"))
        .and_then(|e| e.as_bool())
        .unwrap_or(false);

    // Load ECU configs
    tracing::info!(keys = ?config.as_table().map(|t| t.keys().collect::<Vec<_>>()), "Config root keys");
    if let Some(ecus) = config.get("ecu").and_then(|e| e.as_table()) {
        tracing::info!(ecu_count = ecus.len(), "Loading ECU configs");
        for (ecu_id, ecu_config) in ecus {
            tracing::info!(ecu_id = %ecu_id, "Processing ECU config");
            // Register inline params from config into DidStore
            register_inline_params(ecu_id, ecu_config, did_store)?;

            // Collect output configs for this ECU
            let outputs = load_outputs(ecu_config)?;
            if !outputs.is_empty() {
                output_configs.insert(ecu_id.clone(), outputs);
            }

            let backend = create_ecu_backend(ecu_id, ecu_config, &config).await?;
            let backend: Arc<dyn DiagnosticBackend> = Arc::new(backend);

            if gateway_enabled {
                // Will be added to gateway below
            } else {
                backends.insert(ecu_id.clone(), backend.clone());
            }

            // Store for gateway if needed
            if gateway_enabled {
                backends.insert(format!("_ecu_{}", ecu_id), backend);
            }
        }
    }

    // Load proxy configs (SOVD-over-HTTP backends)
    if let Some(proxies) = config.get("proxy").and_then(|p| p.as_table()) {
        tracing::info!(proxy_count = proxies.len(), "Loading proxy configs");
        for (proxy_id, proxy_config) in proxies {
            let name = proxy_config
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or(proxy_id);
            let url = proxy_config
                .get("url")
                .and_then(|u| u.as_str())
                .ok_or_else(|| anyhow::anyhow!("Proxy '{}' missing 'url' field", proxy_id))?;
            let component_id = proxy_config
                .get("component_id")
                .and_then(|c| c.as_str())
                .ok_or_else(|| {
                    anyhow::anyhow!("Proxy '{}' missing 'component_id' field", proxy_id)
                })?;

            let auth_token = proxy_config.get("auth_token").and_then(|t| t.as_str());

            tracing::info!(
                proxy_id = %proxy_id,
                name = %name,
                url = %url,
                component_id = %component_id,
                auth = auth_token.is_some(),
                "Creating proxy backend"
            );

            let backend = SovdProxyBackend::with_auth(proxy_id, url, component_id, auth_token)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to create proxy '{}': {}", proxy_id, e))?;
            let backend: Arc<dyn DiagnosticBackend> = Arc::new(backend);

            if gateway_enabled {
                backends.insert(format!("_proxy_{}", proxy_id), backend);
            } else {
                backends.insert(proxy_id.clone(), backend);
            }
        }
    }

    // Create gateway if enabled
    if gateway_enabled {
        let gw_section = config.get("gateway");
        let gw_id_val = gw_section.and_then(|g| g.get("id"));
        tracing::info!(?gw_section, ?gw_id_val, "Gateway config debug");

        let gateway_id = gw_id_val
            .and_then(|i| i.as_str())
            .unwrap_or("vehicle_gateway");

        let gateway_name = gw_section
            .and_then(|g| g.get("name"))
            .and_then(|n| n.as_str())
            .unwrap_or("Vehicle Gateway");

        tracing::info!(gateway_id = %gateway_id, gateway_name = %gateway_name, "Creating gateway");

        let mut gateway = GatewayBackend::new(gateway_id, gateway_name, None);

        // Register all ECU backends with gateway
        let ecu_keys: Vec<String> = backends
            .keys()
            .filter(|k| k.starts_with("_ecu_"))
            .cloned()
            .collect();

        for key in &ecu_keys {
            if let Some(backend) = backends.remove(key) {
                gateway.register_backend(backend);
            }
        }

        // Register proxy backends with gateway
        let proxy_keys: Vec<String> = backends
            .keys()
            .filter(|k| k.starts_with("_proxy_"))
            .cloned()
            .collect();

        for key in &proxy_keys {
            if let Some(backend) = backends.remove(key) {
                gateway.register_backend(backend);
            }
        }

        // CAN bus auto-discovery: scan for ECUs not explicitly configured
        #[cfg(target_os = "linux")]
        if let Some(scan_config) = gw_section.and_then(|g| g.get("scan")) {
            let scan_interface = scan_config
                .get("interface")
                .and_then(|i| i.as_str())
                .unwrap_or("can0")
                .to_string();
            let scan_timeout = scan_config
                .get("timeout_ms")
                .and_then(|t| t.as_integer())
                .unwrap_or(2000) as u64;

            // Collect CAN ID pairs of already-configured ECUs to avoid duplicates
            let configured_ids: std::collections::HashSet<(u32, u32)> =
                collect_configured_can_ids(&config);

            tracing::info!(
                interface = %scan_interface,
                timeout_ms = scan_timeout,
                configured_ecus = configured_ids.len(),
                "Running CAN bus ECU auto-discovery"
            );

            let cfg = sovd_uds::scanner::ScanConfig {
                interface: scan_interface,
                timeout_ms: scan_timeout,
            };

            match sovd_uds::scanner::scan_can_bus(&cfg).await {
                Ok(discovered) => {
                    let session_config = if let Some(s) = config.get("session") {
                        parse_session_config(s).unwrap_or_default()
                    } else {
                        SessionConfig::default()
                    };

                    // Parse default flash config for discovered ECUs from [gateway.scan.flash]
                    let scan_flash_config =
                        load_flash_commit_config(scan_config).unwrap_or_default();

                    for ecu in discovered {
                        // Skip if this ECU's CAN IDs are already explicitly configured
                        if configured_ids.contains(&(ecu.tx_can_id, ecu.rx_can_id)) {
                            tracing::info!(
                                address = format!("0x{:02X}", ecu.address),
                                "Skipping discovered ECU (already configured)"
                            );
                            continue;
                        }

                        let ecu_id = format!("ecu_0x{:02x}", ecu.address);
                        let ecu_name = ecu
                            .part_number
                            .as_deref()
                            .map(|pn| format!("Discovered ECU {} (0x{:02X})", pn, ecu.address))
                            .unwrap_or_else(|| format!("Discovered ECU 0x{:02X}", ecu.address));

                        let backend_config = UdsBackendConfig {
                            id: ecu_id.clone(),
                            name: ecu_name.clone(),
                            description: Some(format!(
                                "Auto-discovered on {} at address 0x{:02X}",
                                ecu.interface, ecu.address
                            )),
                            transport: TransportConfig::SocketCan(SocketCanConfig {
                                interface: ecu.interface.clone(),
                                bitrate: 500000,
                                isotp: IsoTpConfig {
                                    tx_id: format!("0x{:08X}", ecu.tx_can_id),
                                    rx_id: format!("0x{:08X}", ecu.rx_can_id),
                                    tx_padding: 0xCC,
                                    rx_padding: 0xCC,
                                    block_size: 0,
                                    st_min_us: 0,
                                    tx_dl: 8,
                                },
                            }),
                            operations: vec![],
                            outputs: vec![],
                            service_overrides: Default::default(),
                            sessions: session_config.clone(),
                            flash_commit: scan_flash_config.clone(),
                        };

                        match UdsBackend::new(backend_config).await {
                            Ok(backend) => {
                                tracing::info!(
                                    ecu_id = %ecu_id,
                                    name = %ecu_name,
                                    address = format!("0x{:02X}", ecu.address),
                                    vin = ?ecu.vin,
                                    "Registered auto-discovered ECU"
                                );

                                // Register identification DIDs for this ECU
                                register_discovered_ecu_dids(&ecu_id, &ecu, did_store);

                                gateway.register_backend(Arc::new(backend));
                            }
                            Err(e) => {
                                tracing::warn!(
                                    address = format!("0x{:02X}", ecu.address),
                                    error = %e,
                                    "Failed to create backend for discovered ECU"
                                );
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "CAN bus scan failed, continuing without discovery");
                }
            }
        }

        backends.insert(gateway_id.to_string(), Arc::new(gateway));
    }

    let keys: Vec<&String> = backends.keys().collect();
    tracing::info!(?keys, "Final backend keys");

    Ok((backends, port, output_configs))
}

/// Create an ECU backend from config
async fn create_ecu_backend(
    ecu_id: &str,
    ecu_config: &toml::Value,
    root_config: &toml::Value,
) -> anyhow::Result<UdsBackend> {
    let name = ecu_config
        .get("name")
        .and_then(|n| n.as_str())
        .unwrap_or(ecu_id);

    let description = ecu_config
        .get("description")
        .and_then(|d| d.as_str())
        .map(|s| s.to_string());

    // Get transport config (shared or per-ECU)
    let transport = if let Some(t) = ecu_config.get("transport") {
        parse_transport_config(t)?
    } else if let Some(t) = root_config.get("transport") {
        parse_transport_config(t)?
    } else {
        TransportConfig::Mock(MockConfig { latency_ms: 10 })
    };

    // Load operations
    let operations = load_operations(ecu_config)?;

    // Session config (per-ECU first, then root-level fallback)
    let mut sessions = if let Some(s) = ecu_config.get("session") {
        parse_session_config(s)?
    } else if let Some(s) = root_config.get("session") {
        parse_session_config(s)?
    } else {
        SessionConfig::default()
    };

    // Per-ECU [ecu.*.security] section (gateway config pattern) — merge into session config
    if let Some(sec) = ecu_config.get("security") {
        let level = sec.get("level").and_then(|v| v.as_integer()).unwrap_or(
            sessions
                .security
                .as_ref()
                .map(|s| s.level as i64)
                .unwrap_or(1),
        ) as u8;
        let enabled = sec.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
        sessions.security = Some(sovd_uds::config::SecurityConfig { enabled, level });
    }

    // Parse per-ECU service overrides (for OEM variants like Vortex Motors)
    let service_overrides = if let Some(so) = ecu_config.get("service_overrides") {
        parse_service_overrides(so)?
    } else if let Some(so) = root_config.get("service_overrides") {
        parse_service_overrides(so)?
    } else {
        Default::default()
    };

    let outputs = load_outputs(ecu_config)?;

    // Load flash commit/rollback config
    let flash_commit = load_flash_commit_config(ecu_config)?;

    let config = UdsBackendConfig {
        id: ecu_id.to_string(),
        name: name.to_string(),
        description,
        transport,
        operations,
        outputs,
        service_overrides,
        sessions,
        flash_commit,
    };

    tracing::info!(ecu_id = %ecu_id, "Creating UDS backend");
    UdsBackend::new(config)
        .await
        .map_err(|e| anyhow::anyhow!("{}", e))
}

fn parse_transport_config(config: &toml::Value) -> anyhow::Result<TransportConfig> {
    let transport_type = config
        .get("type")
        .and_then(|t| t.as_str())
        .unwrap_or("mock");

    match transport_type {
        "socketcan" => {
            let interface = config
                .get("interface")
                .and_then(|i| i.as_str())
                .unwrap_or("vcan0")
                .to_string();

            let bitrate = config
                .get("bitrate")
                .and_then(|b| b.as_integer())
                .unwrap_or(500000) as u32;

            let isotp = config.get("isotp").ok_or_else(|| {
                anyhow::anyhow!("SocketCAN transport requires isotp configuration")
            })?;

            let tx_id = isotp
                .get("tx_id")
                .and_then(|t| t.as_str())
                .unwrap_or("0x7E0")
                .to_string();

            let rx_id = isotp
                .get("rx_id")
                .and_then(|r| r.as_str())
                .unwrap_or("0x7E8")
                .to_string();

            let tx_padding = isotp
                .get("tx_padding")
                .and_then(|p| p.as_integer())
                .unwrap_or(0xCC) as u8;

            let rx_padding = isotp
                .get("rx_padding")
                .and_then(|p| p.as_integer())
                .unwrap_or(0xCC) as u8;

            let block_size = isotp
                .get("block_size")
                .and_then(|b| b.as_integer())
                .unwrap_or(0) as u8;

            let st_min_us = isotp
                .get("st_min_us")
                .and_then(|s| s.as_integer())
                .unwrap_or(0) as u32;

            let tx_dl = isotp.get("tx_dl").and_then(|t| t.as_integer()).unwrap_or(8) as u8;

            Ok(TransportConfig::SocketCan(SocketCanConfig {
                interface,
                bitrate,
                isotp: IsoTpConfig {
                    tx_id,
                    rx_id,
                    tx_padding,
                    rx_padding,
                    block_size,
                    st_min_us,
                    tx_dl,
                },
            }))
        }
        _ => Ok(TransportConfig::Mock(MockConfig {
            latency_ms: config
                .get("latency_ms")
                .and_then(|l| l.as_integer())
                .unwrap_or(10) as u64,
        })),
    }
}

fn parse_session_config(config: &toml::Value) -> anyhow::Result<SessionConfig> {
    let default_session = config
        .get("default_session")
        .and_then(|d| d.as_integer())
        .unwrap_or(0x01) as u8;
    let programming_session = config
        .get("programming_session")
        .and_then(|p| p.as_integer())
        .unwrap_or(0x02) as u8;
    let extended_session = config
        .get("extended_session")
        .and_then(|e| e.as_integer())
        .unwrap_or(0x03) as u8;
    let engineering_session = config
        .get("engineering_session")
        .and_then(|e| e.as_integer())
        .unwrap_or(0x60) as u8;
    let transfer_data_block_counter_start = config
        .get("transfer_data_block_counter_start")
        .and_then(|v| v.as_integer())
        .unwrap_or(0) as u8;
    let transfer_data_block_counter_wrap = config
        .get("transfer_data_block_counter_wrap")
        .and_then(|v| v.as_integer())
        .unwrap_or(0) as u8;

    tracing::info!(
        "parse_session_config: default={:#x}, programming={:#x}, extended={:#x}, engineering={:#x}, block_counter_start={}, block_counter_wrap={}",
        default_session, programming_session, extended_session, engineering_session,
        transfer_data_block_counter_start, transfer_data_block_counter_wrap
    );

    // Parse [session.security] sub-table if present
    let security = config.get("security").and_then(|sec| {
        let enabled = sec
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let level = sec.get("level").and_then(|v| v.as_integer()).unwrap_or(1) as u8;
        if enabled {
            Some(sovd_uds::config::SecurityConfig { enabled, level })
        } else {
            None
        }
    });

    Ok(SessionConfig {
        default_session,
        programming_session,
        extended_session,
        engineering_session,
        transfer_data_block_counter_start,
        transfer_data_block_counter_wrap,
        security,
        ..Default::default()
    })
}

fn parse_service_overrides(config: &toml::Value) -> anyhow::Result<ServiceOverrides> {
    Ok(ServiceOverrides {
        diagnostic_session_control: config
            .get("diagnostic_session_control")
            .and_then(|v| v.as_integer())
            .map(|v| v as u8),
        ecu_reset: config
            .get("ecu_reset")
            .and_then(|v| v.as_integer())
            .map(|v| v as u8),
        clear_diagnostic_info: config
            .get("clear_diagnostic_info")
            .and_then(|v| v.as_integer())
            .map(|v| v as u8),
        read_dtc_info: config
            .get("read_dtc_info")
            .and_then(|v| v.as_integer())
            .map(|v| v as u8),
        read_data_by_id: config
            .get("read_data_by_id")
            .and_then(|v| v.as_integer())
            .map(|v| v as u8),
        security_access: config
            .get("security_access")
            .and_then(|v| v.as_integer())
            .map(|v| v as u8),
        read_data_by_periodic_id: config
            .get("read_data_by_periodic_id")
            .and_then(|v| v.as_integer())
            .map(|v| v as u8),
        dynamically_define_data_id: config
            .get("dynamically_define_data_id")
            .and_then(|v| v.as_integer())
            .map(|v| v as u8),
        write_data_by_id: config
            .get("write_data_by_id")
            .and_then(|v| v.as_integer())
            .map(|v| v as u8),
        io_control_by_id: config
            .get("io_control_by_id")
            .and_then(|v| v.as_integer())
            .map(|v| v as u8),
        routine_control: config
            .get("routine_control")
            .and_then(|v| v.as_integer())
            .map(|v| v as u8),
        request_download: config
            .get("request_download")
            .and_then(|v| v.as_integer())
            .map(|v| v as u8),
        request_upload: config
            .get("request_upload")
            .and_then(|v| v.as_integer())
            .map(|v| v as u8),
        transfer_data: config
            .get("transfer_data")
            .and_then(|v| v.as_integer())
            .map(|v| v as u8),
        request_transfer_exit: config
            .get("request_transfer_exit")
            .and_then(|v| v.as_integer())
            .map(|v| v as u8),
        tester_present: config
            .get("tester_present")
            .and_then(|v| v.as_integer())
            .map(|v| v as u8),
        link_control: config
            .get("link_control")
            .and_then(|v| v.as_integer())
            .map(|v| v as u8),
    })
}

fn load_operations(ecu_config: &toml::Value) -> anyhow::Result<Vec<OperationConfig>> {
    let mut operations = Vec::new();

    if let Some(ops) = ecu_config.get("operations").and_then(|o| o.as_array()) {
        for op in ops {
            operations.push(OperationConfig {
                id: op
                    .get("id")
                    .and_then(|i| i.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Operation missing id"))?
                    .to_string(),
                name: op
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string(),
                rid: op
                    .get("rid")
                    .and_then(|r| r.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Operation missing rid"))?
                    .to_string(),
                description: op
                    .get("description")
                    .and_then(|d| d.as_str())
                    .map(|s| s.to_string()),
                security_level: op
                    .get("security_level")
                    .and_then(|s| s.as_integer())
                    .unwrap_or(0) as u8,
            });
        }
    }

    Ok(operations)
}

fn load_flash_commit_config(ecu_config: &toml::Value) -> anyhow::Result<FlashCommitConfig> {
    let flash = match ecu_config.get("flash") {
        Some(f) => f,
        None => return Ok(FlashCommitConfig::default()),
    };

    let supports_rollback = flash
        .get("supports_rollback")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let commit_routine = flash
        .get("commit_routine")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let rollback_routine = flash
        .get("rollback_routine")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    if supports_rollback {
        tracing::info!(
            commit_routine = ?commit_routine,
            rollback_routine = ?rollback_routine,
            "Flash commit/rollback enabled"
        );
    }

    Ok(FlashCommitConfig {
        supports_rollback,
        commit_routine,
        rollback_routine,
    })
}

fn load_outputs(ecu_config: &toml::Value) -> anyhow::Result<Vec<OutputConfig>> {
    use sovd_uds::config::DataType;

    let mut outputs = Vec::new();

    if let Some(outs) = ecu_config.get("outputs").and_then(|o| o.as_array()) {
        for out in outs {
            let data_type = out
                .get("data_type")
                .and_then(|t| t.as_str())
                .map(|s| match s {
                    "uint8" => DataType::Uint8,
                    "uint16" => DataType::Uint16,
                    "uint32" => DataType::Uint32,
                    "int8" => DataType::Int8,
                    "int16" => DataType::Int16,
                    "int32" => DataType::Int32,
                    "float" => DataType::Float,
                    "string" => DataType::String,
                    "bytes" => DataType::Bytes,
                    _ => DataType::Uint8,
                });

            let allowed = out
                .get("allowed")
                .and_then(|a| a.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();

            outputs.push(OutputConfig {
                id: out
                    .get("id")
                    .and_then(|i| i.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Output missing id"))?
                    .to_string(),
                name: out
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string(),
                ioid: out
                    .get("ioid")
                    .and_then(|i| i.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Output missing ioid"))?
                    .to_string(),
                default_value: out
                    .get("default_value")
                    .and_then(|d| d.as_str())
                    .unwrap_or("00")
                    .to_string(),
                description: out
                    .get("description")
                    .and_then(|d| d.as_str())
                    .map(|s| s.to_string()),
                security_level: out
                    .get("security_level")
                    .and_then(|s| s.as_integer())
                    .unwrap_or(0) as u8,
                data_type,
                unit: out
                    .get("unit")
                    .and_then(|u| u.as_str())
                    .map(|s| s.to_string()),
                scale: out.get("scale").and_then(|s| s.as_float()).unwrap_or(1.0),
                offset: out.get("offset").and_then(|o| o.as_float()).unwrap_or(0.0),
                min: out.get("min").and_then(|m| m.as_float()),
                max: out.get("max").and_then(|m| m.as_float()),
                allowed,
            });
        }
    }

    Ok(outputs)
}

/// Register standard UDS identification DIDs as global definitions.
///
/// Standard DIDs (ISO 14229-1 Annex C) are always strings and read-only.
/// Registering them globally means every component can decode them without
/// needing explicit YAML entries. Per-component YAML entries still take
/// precedence for display name, length, etc.
fn register_standard_dids(did_store: &DidStore) {
    use sovd_conv::{DataType, DidDefinition};
    use sovd_uds::uds::standard_did;

    let mut count = 0;
    for &(did, key, label) in standard_did::IDENTIFICATION_DIDS {
        // Skip if a global (non-component-scoped) definition already exists.
        // Component-scoped definitions (from YAML files with component_id) should
        // NOT prevent the global fallback from being registered, since other ECUs
        // still need a definition to decode these standard DIDs.
        if did_store.contains_global(did) {
            continue;
        }

        let def = DidDefinition::scalar(DataType::String)
            .with_id(key)
            .with_name(label);

        did_store.register(did, def);
        count += 1;
    }

    if count > 0 {
        tracing::info!("Registered {} standard identification DIDs", count);
    }
}

/// Register inline params from TOML config into DidStore
fn register_inline_params(
    ecu_id: &str,
    ecu_config: &toml::Value,
    did_store: &DidStore,
) -> anyhow::Result<()> {
    use sovd_conv::types::DataType;
    use sovd_conv::DidDefinition;

    let params = match ecu_config
        .get("params")
        .or_else(|| ecu_config.get("parameters"))
    {
        Some(p) => match p.as_array() {
            Some(arr) => {
                tracing::info!(ecu = %ecu_id, count = arr.len(), "Found inline params");
                arr
            }
            None => {
                tracing::warn!(ecu = %ecu_id, "params is not an array: {:?}", p);
                return Ok(());
            }
        },
        None => {
            tracing::info!(ecu = %ecu_id, keys = ?ecu_config.as_table().map(|t| t.keys().collect::<Vec<_>>()), "No params section found");
            return Ok(());
        }
    };

    for param in params {
        // Parse DID hex value (required)
        let did_str = param
            .get("did")
            .and_then(|d| d.as_str())
            .ok_or_else(|| anyhow::anyhow!("Param missing 'did' field"))?;
        let did_u16 = parse_hex_u16(did_str)?;

        // Parse data type
        let data_type = param
            .get("data_type")
            .and_then(|t| t.as_str())
            .map(|s| match s {
                "uint8" => DataType::Uint8,
                "uint16" => DataType::Uint16,
                "uint32" => DataType::Uint32,
                "int8" => DataType::Int8,
                "int16" => DataType::Int16,
                "int32" => DataType::Int32,
                "float32" => DataType::Float32,
                "float64" => DataType::Float64,
                "string" => DataType::String,
                _ => DataType::Bytes,
            })
            .unwrap_or(DataType::Bytes);

        // Build DidDefinition
        let mut def = DidDefinition::scalar(data_type);

        // Set semantic ID
        if let Some(id) = param.get("id").and_then(|i| i.as_str()) {
            def.id = Some(id.to_string());
        }

        // Set display name
        if let Some(name) = param.get("name").and_then(|n| n.as_str()) {
            def.name = Some(name.to_string());
        }

        // Set unit
        if let Some(unit) = param.get("unit").and_then(|u| u.as_str()) {
            def.unit = Some(unit.to_string());
        }

        // Set scale/offset
        if let Some(scale) = param.get("scale").and_then(|s| s.as_float()) {
            def.scale = scale;
        }
        if let Some(offset) = param.get("offset").and_then(|o| o.as_float()) {
            def.offset = offset;
        }

        // Set byte length for string/bytes types
        if let Some(length) = param.get("byte_length").and_then(|l| l.as_integer()) {
            def.length = Some(length as usize);
        }

        // Set writable flag
        if let Some(writable) = param.get("writable").and_then(|w| w.as_bool()) {
            def.writable = writable;
        }

        // Set component_id so this DID is associated with this ECU
        def.component_id = Some(ecu_id.to_string());

        // Register in DidStore
        let param_id = param
            .get("id")
            .and_then(|i| i.as_str())
            .map(|s| s.to_string());
        tracing::info!(
            ecu = %ecu_id,
            did = format!("{:04X}", did_u16),
            id = ?param_id,
            "Registering inline param"
        );
        did_store.register(did_u16, def);
    }

    Ok(())
}

/// Parse a hex string like "0xF190" or "F190" to u16
fn parse_hex_u16(s: &str) -> anyhow::Result<u16> {
    let s = s.trim_start_matches("0x").trim_start_matches("0X");
    u16::from_str_radix(s, 16).map_err(|e| anyhow::anyhow!("Invalid hex '{}': {}", s, e))
}

/// Collect (tx_id, rx_id) CAN ID pairs from explicitly configured ECUs.
/// Used to skip already-configured ECUs during auto-discovery.
#[cfg(target_os = "linux")]
fn collect_configured_can_ids(config: &toml::Value) -> std::collections::HashSet<(u32, u32)> {
    let mut ids = std::collections::HashSet::new();

    if let Some(ecus) = config.get("ecu").and_then(|e| e.as_table()) {
        for (_ecu_id, ecu_config) in ecus {
            if let Some(transport) = ecu_config.get("transport") {
                if let Some(isotp) = transport.get("isotp") {
                    let tx = isotp
                        .get("tx_id")
                        .and_then(|t| t.as_str())
                        .and_then(|s| parse_hex_u32(s).ok());
                    let rx = isotp
                        .get("rx_id")
                        .and_then(|r| r.as_str())
                        .and_then(|s| parse_hex_u32(s).ok());

                    if let (Some(tx_id), Some(rx_id)) = (tx, rx) {
                        ids.insert((tx_id, rx_id));
                    }
                }
            }
        }
    }

    ids
}

#[cfg(target_os = "linux")]
fn parse_hex_u32(s: &str) -> anyhow::Result<u32> {
    let s = s.trim_start_matches("0x").trim_start_matches("0X");
    u32::from_str_radix(s, 16).map_err(|e| anyhow::anyhow!("Invalid hex '{}': {}", s, e))
}

/// Register identification DIDs discovered during CAN bus scan.
#[cfg(target_os = "linux")]
fn register_discovered_ecu_dids(
    ecu_id: &str,
    ecu: &sovd_uds::scanner::DiscoveredEcu,
    did_store: &DidStore,
) {
    use sovd_conv::{DataType, DidDefinition};

    let dids: &[(u16, &str, &str, &Option<String>)] = &[
        (0xF190, "vin", "VIN", &ecu.vin),
        (0xF187, "part_number", "Part Number", &ecu.part_number),
        (0xF18C, "serial_number", "Serial Number", &ecu.serial_number),
        (
            0xF195,
            "sw_version",
            "Software Version",
            &ecu.software_version,
        ),
    ];

    for &(did, key, label, value) in dids {
        if value.is_some() {
            let mut def = DidDefinition::scalar(DataType::String)
                .with_id(key)
                .with_name(label);
            def.component_id = Some(ecu_id.to_string());
            did_store.register(did, def);
        }
    }
}

/// Create mock backends for demo/testing
async fn create_mock_backends() -> anyhow::Result<(
    HashMap<String, Arc<dyn DiagnosticBackend>>,
    HashMap<String, Vec<OutputConfig>>,
)> {
    use sovd_uds::config::DataType;

    let mock_outputs = vec![
        OutputConfig {
            id: "throttle_position".to_string(),
            name: "Throttle Position".to_string(),
            ioid: "0xF000".to_string(),
            default_value: "00".to_string(),
            description: Some("Electronic throttle body position control".to_string()),
            security_level: 1,
            data_type: Some(DataType::Uint8),
            unit: Some("%".to_string()),
            scale: 0.392157,
            offset: 0.0,
            min: Some(0.0),
            max: Some(100.0),
            allowed: Vec::new(),
        },
        OutputConfig {
            id: "fuel_injector_1".to_string(),
            name: "Fuel Injector #1".to_string(),
            ioid: "0xF001".to_string(),
            default_value: "00".to_string(),
            description: Some("Cylinder 1 fuel injector actuation".to_string()),
            security_level: 1,
            data_type: None,
            unit: None,
            scale: 1.0,
            offset: 0.0,
            min: None,
            max: None,
            allowed: Vec::new(),
        },
        OutputConfig {
            id: "check_engine_light".to_string(),
            name: "Check Engine Light (MIL)".to_string(),
            ioid: "0xF010".to_string(),
            default_value: "00".to_string(),
            description: Some("Malfunction Indicator Lamp control".to_string()),
            security_level: 0,
            data_type: Some(DataType::Uint8),
            unit: None,
            scale: 1.0,
            offset: 0.0,
            min: None,
            max: None,
            allowed: vec!["off".to_string(), "on".to_string()],
        },
        OutputConfig {
            id: "cooling_fan".to_string(),
            name: "Cooling Fan Relay".to_string(),
            ioid: "0xF020".to_string(),
            default_value: "00".to_string(),
            description: Some("Engine cooling fan relay control".to_string()),
            security_level: 0,
            data_type: Some(DataType::Uint8),
            unit: None,
            scale: 1.0,
            offset: 0.0,
            min: None,
            max: None,
            allowed: vec!["off".to_string(), "on".to_string()],
        },
    ];

    let ecu_config = UdsBackendConfig {
        id: "engine_ecu".to_string(),
        name: "Engine Control Module".to_string(),
        description: Some("Main engine ECU (mock)".to_string()),
        transport: TransportConfig::Mock(MockConfig { latency_ms: 10 }),
        operations: vec![OperationConfig {
            id: "self_test".to_string(),
            name: "Run Self Test".to_string(),
            rid: "0x0203".to_string(),
            description: Some("Execute ECU self-test routine".to_string()),
            security_level: 0,
        }],
        outputs: mock_outputs.clone(),
        service_overrides: Default::default(),
        sessions: SessionConfig::default(),
        flash_commit: Default::default(),
    };

    let backend = UdsBackend::new(ecu_config).await?;
    let backend: Arc<dyn DiagnosticBackend> = Arc::new(backend);

    let mut backends = HashMap::new();
    backends.insert("engine_ecu".to_string(), backend);

    let mut output_configs = HashMap::new();
    output_configs.insert("engine_ecu".to_string(), mock_outputs);

    Ok((backends, output_configs))
}
