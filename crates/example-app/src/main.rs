//! example-app - Example diagnostic app entity
//!
//! An SOVD "app" entity that:
//! - Authenticates requests via bearer token
//! - Exposes synthetic computed parameters (engine health score, maintenance hours)
//! - Contains a managed ECU sub-entity that proxies diagnostics, intercepts OTA,
//!   and manages flash transfers to an upstream ECU
//!
//! Usage:
//!   example-app --port 4001 --upstream-url http://localhost:4002 --upstream-component vtx_vx500
//!   example-app --port 4001 --auth-token my-secret -u http://localhost:4002 -c vtx_vx500

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::middleware;
use example_app::auth::{auth_middleware, AuthToken};
use example_app::backend::ExampleAppBackend;
use example_app::config::ExampleAppConfig;
use example_app::managed_ecu::ManagedEcuBackend;
use sovd_api::{create_router, AppState, DidStore};
use sovd_proxy::SovdProxyBackend;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

struct Args {
    port: u16,
    id: String,
    name: String,
    upstream_url: String,
    upstream_component: String,
    upstream_gateway: Option<String>,
    auth_token: Option<String>,
    config_path: Option<String>,
}

fn parse_args() -> anyhow::Result<Args> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut port = 4001u16;
    let mut id = String::from("vortex_engine");
    let mut name = String::from("Vortex Motors Engine App");
    let mut upstream_url = String::from("http://localhost:4002");
    let mut upstream_component = String::from("vtx_vx500");
    let mut upstream_gateway: Option<String> = None;
    let mut auth_token: Option<String> = None;
    let mut config_path: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--port" | "-p" => {
                if i + 1 < args.len() {
                    port = args[i + 1].parse()?;
                    i += 2;
                } else {
                    anyhow::bail!("Missing argument for --port");
                }
            }
            "--id" => {
                if i + 1 < args.len() {
                    id = args[i + 1].clone();
                    i += 2;
                } else {
                    anyhow::bail!("Missing argument for --id");
                }
            }
            "--name" => {
                if i + 1 < args.len() {
                    name = args[i + 1].clone();
                    i += 2;
                } else {
                    anyhow::bail!("Missing argument for --name");
                }
            }
            "--upstream-url" | "-u" => {
                if i + 1 < args.len() {
                    upstream_url = args[i + 1].clone();
                    i += 2;
                } else {
                    anyhow::bail!("Missing argument for --upstream-url");
                }
            }
            "--upstream-component" | "-c" => {
                if i + 1 < args.len() {
                    upstream_component = args[i + 1].clone();
                    i += 2;
                } else {
                    anyhow::bail!("Missing argument for --upstream-component");
                }
            }
            "--upstream-gateway" | "-g" => {
                if i + 1 < args.len() {
                    upstream_gateway = Some(args[i + 1].clone());
                    i += 2;
                } else {
                    anyhow::bail!("Missing argument for --upstream-gateway");
                }
            }
            "--auth-token" => {
                if i + 1 < args.len() {
                    auth_token = Some(args[i + 1].clone());
                    i += 2;
                } else {
                    anyhow::bail!("Missing argument for --auth-token");
                }
            }
            "--config" | "-f" => {
                if i + 1 < args.len() {
                    config_path = Some(args[i + 1].clone());
                    i += 2;
                } else {
                    anyhow::bail!("Missing argument for --config");
                }
            }
            "--help" | "-h" => {
                eprintln!(
                    r#"example-app - Example diagnostic app entity

Usage: example-app [OPTIONS]

Options:
  -p, --port <PORT>                    Listen port (default: 4001)
      --id <ID>                        App component ID (default: vortex_engine)
      --name <NAME>                    App display name (default: Vortex Motors Engine App)
  -u, --upstream-url <URL>             Upstream SOVD server URL (default: http://localhost:4002)
  -c, --upstream-component <ID>        Component ID on upstream server (default: vtx_vx500)
  -g, --upstream-gateway <ID>          Gateway ID if component is a sub-entity (optional)
      --auth-token <TOKEN>             Bearer token for authentication (disabled if not set)
  -f, --config <PATH>                TOML config file for output definitions (optional)
  -h, --help                           Print this help message
"#
                );
                std::process::exit(0);
            }
            _ => {
                tracing::warn!("Unknown argument: {}", args[i]);
                i += 1;
            }
        }
    }

    Ok(Args {
        port,
        id,
        name,
        upstream_url,
        upstream_component,
        upstream_gateway,
        auth_token,
        config_path,
    })
}

/// Try to connect to the upstream SOVD server and build a ManagedEcuBackend.
///
/// This is extracted so it can be called both at startup and in the background
/// retry loop.
#[allow(clippy::too_many_arguments)]
async fn try_connect_upstream(
    upstream_component: &str,
    upstream_url: &str,
    upstream_gateway: Option<&str>,
    ecu_id: &str,
    ecu_name: &str,
    app_id: &str,
    ecu_secret_hex: Option<&str>,
    output_defs: &[sovd_uds::config::OutputConfig],
    param_defs: &[example_app::config::ParameterDef],
    op_defs: &[sovd_uds::config::OperationConfig],
) -> anyhow::Result<ManagedEcuBackend> {
    let proxy = SovdProxyBackend::with_options(
        upstream_component,
        upstream_url,
        upstream_component,
        None,
        upstream_gateway,
    )
    .await
    .map_err(|e| anyhow::anyhow!("Failed to connect to upstream: {}", e))?;

    let ecu = ManagedEcuBackend::new(
        ecu_id,
        ecu_name,
        app_id,
        proxy,
        upstream_url,
        output_defs.to_vec(),
        param_defs.to_vec(),
        op_defs.to_vec(),
        ecu_secret_hex,
    )
    .map_err(|e| anyhow::anyhow!("Failed to create managed ECU backend: {}", e))?;

    Ok(ecu)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "example_app=info,sovd_proxy=info,sovd_api=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let args = parse_args()?;

    tracing::info!(
        port = args.port,
        id = %args.id,
        upstream_url = %args.upstream_url,
        upstream_component = %args.upstream_component,
        auth = args.auth_token.is_some(),
        "Starting example-app"
    );

    // 1. Load optional config file and normalize for backward compatibility
    let mut config = if let Some(ref path) = args.config_path {
        tracing::info!(config = %path, "Loading supplier app config");
        ExampleAppConfig::load(path).map_err(|e| anyhow::anyhow!("{}", e))?
    } else {
        ExampleAppConfig::default()
    };
    config.normalize(&args.upstream_component);

    // 2. Resolve ECU identity and config-driven definitions
    let (ecu_id, ecu_name, ecu_secret, output_defs, param_defs, op_defs) =
        if let Some(ref ecu_config) = config.managed_ecu {
            (
                ecu_config.id.clone(),
                ecu_config.name.clone(),
                ecu_config.secret.clone(),
                ecu_config.outputs.clone(),
                ecu_config.parameters.clone(),
                ecu_config.operations.clone(),
            )
        } else {
            (
                args.upstream_component.clone(),
                format!("Managed ECU ({})", args.upstream_component),
                None,
                Vec::new(),
                Vec::new(),
                Vec::new(),
            )
        };

    if !output_defs.is_empty() {
        tracing::info!(
            count = output_defs.len(),
            "Loaded output definitions from config"
        );
    }
    if !param_defs.is_empty() {
        tracing::info!(
            count = param_defs.len(),
            "Loaded parameter definitions from config"
        );
    }
    if !op_defs.is_empty() {
        tracing::info!(
            count = op_defs.len(),
            "Loaded operation definitions from config"
        );
    }

    // 3. Try to connect to the upstream SOVD server once.
    //    If it fails, start the HTTP server anyway and retry in the background.
    let managed_ecu = match try_connect_upstream(
        &args.upstream_component,
        &args.upstream_url,
        args.upstream_gateway.as_deref(),
        &ecu_id,
        &ecu_name,
        &args.id,
        ecu_secret.as_deref(),
        &output_defs,
        &param_defs,
        &op_defs,
    )
    .await
    {
        Ok(ecu) => {
            tracing::info!(
                ecu_id = %ecu_id,
                ecu_name = %ecu_name,
                "Managed ECU sub-entity created"
            );
            Some(Arc::new(ecu))
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "Failed to connect to upstream — server will start without ECU, retrying in background"
            );
            None
        }
    };

    // 4. Build ExampleAppBackend wrapping the managed ECU (or None)
    let backend = ExampleAppBackend::new(
        &args.id,
        &args.name,
        &ecu_id,
        &ecu_name,
        managed_ecu.clone(),
    );
    let ecu_slot = backend.managed_ecu_slot();

    // Build AppState with output configs for the enrichment pipeline
    let mut output_configs_map = HashMap::new();
    if let Some(ref ecu_config) = config.managed_ecu {
        if !ecu_config.outputs.is_empty() {
            output_configs_map.insert(args.id.clone(), ecu_config.outputs.clone());
        }
    }
    let did_store = Arc::new(DidStore::new());

    let mut backends: HashMap<String, Arc<dyn sovd_core::DiagnosticBackend>> = HashMap::new();
    backends.insert(args.id.clone(), Arc::new(backend));
    let state = AppState::with_output_configs(backends, did_store, output_configs_map);
    let mut app = create_router(state);

    // Apply auth middleware if token is configured
    if let Some(token) = args.auth_token {
        tracing::info!("Bearer token authentication enabled");
        app = app
            .layer(middleware::from_fn(auth_middleware))
            .layer(axum::Extension(AuthToken(token)));
    }

    // 5. If the upstream was not reachable, spawn a background retry task
    if managed_ecu.is_none() {
        let upstream_component = args.upstream_component.clone();
        let upstream_url = args.upstream_url.clone();
        let upstream_gateway = args.upstream_gateway.clone();
        let app_id = args.id.clone();
        let ecu_id_bg = ecu_id.clone();
        let ecu_name_bg = ecu_name.clone();
        let ecu_secret_bg = ecu_secret.clone();
        let output_defs_bg = output_defs;
        let param_defs_bg = param_defs;
        let op_defs_bg = op_defs;

        tokio::spawn(async move {
            const RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);
            let mut attempt = 0u64;
            loop {
                attempt += 1;
                tokio::time::sleep(RETRY_INTERVAL).await;

                tracing::info!(attempt, "Retrying upstream connection");

                match try_connect_upstream(
                    &upstream_component,
                    &upstream_url,
                    upstream_gateway.as_deref(),
                    &ecu_id_bg,
                    &ecu_name_bg,
                    &app_id,
                    ecu_secret_bg.as_deref(),
                    &output_defs_bg,
                    &param_defs_bg,
                    &op_defs_bg,
                )
                .await
                {
                    Ok(ecu) => {
                        let mut slot = ecu_slot.write().await;
                        *slot = Some(Arc::new(ecu));
                        tracing::info!(
                            attempt,
                            ecu_id = %ecu_id_bg,
                            "Upstream connection established — managed ECU is now available"
                        );
                        break;
                    }
                    Err(e) => {
                        tracing::warn!(
                            attempt,
                            error = %e,
                            "Upstream still unreachable, will retry"
                        );
                    }
                }
            }
        });
    }

    // 6. Start the HTTP server (always, regardless of upstream connectivity)
    let addr = SocketAddr::from(([0, 0, 0, 0], args.port));
    tracing::info!("Listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
