//! Path dialects: the one place that knows the SOVD tree prefixes.
//!
//! A server speaks one or both of two trees, and a client picks one with a [`Dialect`]:
//!
//! - [`Dialect::Legacy`] (the default): `/vehicle/v1`, flat. Every component is
//!   `/vehicle/v1/components/{id}`, the node-level resources are `/vehicle/v1/{status|updates|..}`.
//!   This is the tree every client spoke before dialects existed; its requests are unchanged.
//! - [`Dialect::V1`]: the ISO 17978-3 tree `/v1`. A server that presents a node (a node server,
//!   see SOVDd `ARCHITECTURE.md` §7.1) serves `/v1` hierarchically only, and the client then uses
//!   the node form for everything: a member is `/v1/components/{node}/subcomponents/{id}`, a
//!   node-level resource `/v1/components/{node}/{status|updates|operations|data}...`. A server
//!   without a node serves `/v1` flat: `/v1/components/{id}` and `/v1/{status|...}`. A client never
//!   uses the flat member paths on a node server.
//!
//! Which of the two `/v1` shapes applies is not a setting: [`SovdClient::with_dialect`] and
//! [`FlashClient::with_dialect`] discover the node once (`GET /v1/components`, then
//! `GET {item}/subcomponents`, see [`SovdClient::discover_node`]) and keep the result in the
//! client's [`Paths`]. A resolved `Paths` can be handed to further clients
//! ([`SovdClient::with_paths`], [`FlashConfigBuilder::paths`]) without discovering again.
//!
//! [`SovdClient::with_dialect`]: crate::SovdClient::with_dialect
//! [`SovdClient::with_paths`]: crate::SovdClient::with_paths
//! [`SovdClient::discover_node`]: crate::SovdClient::discover_node
//! [`FlashClient::with_dialect`]: crate::FlashClient::with_dialect
//! [`FlashConfigBuilder::paths`]: crate::flash::FlashConfigBuilder::paths

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use url::Url;

use crate::types::ComponentList;

/// Base of the legacy tree.
const LEGACY_BASE: &str = "/vehicle/v1";
/// Base of the ISO 17978-3 tree.
const V1_BASE: &str = "/v1";

/// The SOVD tree a client speaks (see the [module docs](self)).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Dialect {
    /// `/vehicle/v1`, flat — the default.
    #[default]
    Legacy,
    /// `/v1`, in the node form on a node server, flat otherwise.
    V1,
}

impl fmt::Display for Dialect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Dialect::Legacy => "legacy",
            Dialect::V1 => "v1",
        })
    }
}

impl FromStr for Dialect {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "legacy" => Ok(Dialect::Legacy),
            "v1" => Ok(Dialect::V1),
            other => Err(format!(
                "unknown SOVD dialect '{other}' (expected 'legacy' or 'v1')"
            )),
        }
    }
}

/// The resolved path map of one connection: a [`Dialect`] and, on `/v1`, the node the server
/// presents (if any). Every request path a client of this crate sends comes from here.
///
/// `rest` arguments are paths relative to the entity, starting with `/` (`"/status/restart"`,
/// `"/data/x-update-state"`, `"/operations/x-update-commit/executions"`), or empty.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Paths {
    #[serde(default)]
    dialect: Dialect,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    node: Option<String>,
}

impl Paths {
    /// The legacy map (`/vehicle/v1`, flat) — the default.
    pub fn legacy() -> Self {
        Self::default()
    }

    /// The `/v1` map: `node` is the node the server presents (node form), `None` for a server
    /// without one (flat). Normally obtained by discovery (see the [module docs](self)).
    pub fn v1(node: Option<String>) -> Self {
        Self {
            dialect: Dialect::V1,
            node,
        }
    }

    /// The dialect of this map.
    pub fn dialect(&self) -> Dialect {
        self.dialect
    }

    /// The node on `/v1`, if the server presents one.
    pub fn node(&self) -> Option<&str> {
        self.node.as_deref()
    }

    /// Whether this map speaks `/v1`. The behaviour differences of the `/v1` tree are keyed on it.
    pub fn is_v1(&self) -> bool {
        self.dialect == Dialect::V1
    }

    /// The tree's base: `/vehicle/v1` or `/v1`.
    pub fn base(&self) -> &'static str {
        match self.dialect {
            Dialect::Legacy => LEGACY_BASE,
            Dialect::V1 => V1_BASE,
        }
    }

    /// The listing of the server's components: `{base}/components`; on a node server the
    /// node's members, `/v1/components/{node}/subcomponents`.
    pub fn components(&self) -> String {
        match &self.node {
            Some(node) => format!("{V1_BASE}/components/{node}/subcomponents"),
            None => format!("{}/components", self.base()),
        }
    }

    /// Component `id`: `{base}/components/{id}`; on a node server
    /// `/v1/components/{node}/subcomponents/{id}`.
    pub fn member(&self, id: &str) -> String {
        format!("{}/{id}", self.components())
    }

    /// `rest` under component `id` (see [`member`](Self::member)).
    pub fn member_res(&self, id: &str, rest: &str) -> String {
        format!("{}{rest}", self.member(id))
    }

    /// A node-level resource: `{base}{rest}`; on a node server `/v1/components/{node}{rest}`.
    /// The node form serves `/status`, `/status/restart`, `/updates...`, `/operations...` and
    /// `/data...` only.
    pub fn node_level(&self, rest: &str) -> String {
        match &self.node {
            Some(node) => format!("{V1_BASE}/components/{node}{rest}"),
            None => format!("{}{rest}", self.base()),
        }
    }

    /// Sub-entity `app` behind gateway `component`: `{member(component)}/apps/{app}`.
    pub fn app(&self, component: &str, app: &str) -> String {
        format!("{}/apps/{app}", self.member(component))
    }

    /// Whether this is the default (legacy) map, for `skip_serializing_if`.
    pub(crate) fn is_legacy(&self) -> bool {
        *self == Self::legacy()
    }
}

/// Why node discovery failed, before each client maps it into its own error type.
#[derive(Debug)]
pub(crate) enum DiscoveryError {
    Http(reqwest::Error),
    Url(url::ParseError),
    Status { status: u16, message: String },
    Parse(String),
}

/// Discover the node a `/v1` server presents: `GET /v1/components`; when it lists exactly one
/// item, `GET {item.href}/subcomponents` answering 2xx makes that item the node. Anything else
/// (several items, none, or a single component without subcomponents) is a server without a node.
/// Independent of the item's `type`. `get` builds an authenticated GET for a URL.
pub(crate) async fn discover_node(
    base_url: &Url,
    get: impl Fn(Url) -> reqwest::RequestBuilder,
) -> Result<Option<String>, DiscoveryError> {
    let listing = base_url
        .join(&format!("{V1_BASE}/components"))
        .map_err(DiscoveryError::Url)?;
    let response = get(listing).send().await.map_err(DiscoveryError::Http)?;
    let status = response.status();
    if !status.is_success() {
        return Err(DiscoveryError::Status {
            status: status.as_u16(),
            message: response.text().await.unwrap_or_default(),
        });
    }
    let list: ComponentList = response
        .json()
        .await
        .map_err(|e| DiscoveryError::Parse(e.to_string()))?;
    let [item] = list.items.as_slice() else {
        return Ok(None);
    };
    let href = item
        .href
        .clone()
        .unwrap_or_else(|| format!("{V1_BASE}/components/{}", item.id));
    let subcomponents = base_url
        .join(&format!("{}/subcomponents", href.trim_end_matches('/')))
        .map_err(DiscoveryError::Url)?;
    let response = get(subcomponents)
        .send()
        .await
        .map_err(DiscoveryError::Http)?;
    Ok(response.status().is_success().then(|| item.id.clone()))
}

impl From<DiscoveryError> for crate::SovdClientError {
    fn from(e: DiscoveryError) -> Self {
        match e {
            DiscoveryError::Http(e) => Self::HttpError(e),
            DiscoveryError::Url(e) => Self::InvalidUrl(e),
            DiscoveryError::Status { status, message } => Self::server_error(status, message),
            DiscoveryError::Parse(m) => Self::ParseError(m),
        }
    }
}

impl From<DiscoveryError> for crate::FlashError {
    fn from(e: DiscoveryError) -> Self {
        match e {
            DiscoveryError::Http(e) => Self::Http(e),
            DiscoveryError::Url(e) => Self::Url(e),
            DiscoveryError::Status { status, message } => Self::Server { status, message },
            DiscoveryError::Parse(m) => Self::Parse(m),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shapes() -> [Paths; 3] {
        [
            Paths::legacy(),
            Paths::v1(None),
            Paths::v1(Some("cvc".into())),
        ]
    }

    /// A path built from a map, and its legacy, v1-flat and v1-node result.
    type Row<'a> = (&'a dyn Fn(&Paths) -> String, [&'a str; 3]);

    fn check(rows: &[Row]) {
        for (build, expected) in rows {
            for (paths, want) in shapes().iter().zip(expected) {
                assert_eq!(build(paths), *want, "{paths:?}");
            }
        }
    }

    #[test]
    fn base_and_listing() {
        check(&[
            (&|p| p.base().to_string(), ["/vehicle/v1", "/v1", "/v1"]),
            (
                &|p| p.components(),
                [
                    "/vehicle/v1/components",
                    "/v1/components",
                    "/v1/components/cvc/subcomponents",
                ],
            ),
        ]);
    }

    #[test]
    fn member_resources() {
        check(&[
            (
                &|p| p.member("vm1"),
                [
                    "/vehicle/v1/components/vm1",
                    "/v1/components/vm1",
                    "/v1/components/cvc/subcomponents/vm1",
                ],
            ),
            (
                &|p| p.member_res("vm1", "/status"),
                [
                    "/vehicle/v1/components/vm1/status",
                    "/v1/components/vm1/status",
                    "/v1/components/cvc/subcomponents/vm1/status",
                ],
            ),
            (
                &|p| p.member_res("vm1", "/status/restart"),
                [
                    "/vehicle/v1/components/vm1/status/restart",
                    "/v1/components/vm1/status/restart",
                    "/v1/components/cvc/subcomponents/vm1/status/restart",
                ],
            ),
            (
                &|p| p.member_res("vm1", "/data/x-ota-update-mode"),
                [
                    "/vehicle/v1/components/vm1/data/x-ota-update-mode",
                    "/v1/components/vm1/data/x-ota-update-mode",
                    "/v1/components/cvc/subcomponents/vm1/data/x-ota-update-mode",
                ],
            ),
            (
                &|p| p.member_res("vm1", "/updates/u1/bulk-data/p1"),
                [
                    "/vehicle/v1/components/vm1/updates/u1/bulk-data/p1",
                    "/v1/components/vm1/updates/u1/bulk-data/p1",
                    "/v1/components/cvc/subcomponents/vm1/updates/u1/bulk-data/p1",
                ],
            ),
            (
                &|p| p.member_res("vm1", "/updates/u1/execute"),
                [
                    "/vehicle/v1/components/vm1/updates/u1/execute",
                    "/v1/components/vm1/updates/u1/execute",
                    "/v1/components/cvc/subcomponents/vm1/updates/u1/execute",
                ],
            ),
            (
                &|p| p.member_res("vm1", "/x-ota-force-rollback"),
                [
                    "/vehicle/v1/components/vm1/x-ota-force-rollback",
                    "/v1/components/vm1/x-ota-force-rollback",
                    "/v1/components/cvc/subcomponents/vm1/x-ota-force-rollback",
                ],
            ),
            (
                &|p| p.member_res("hsm", "/x-ecu-id"),
                [
                    "/vehicle/v1/components/hsm/x-ecu-id",
                    "/v1/components/hsm/x-ecu-id",
                    "/v1/components/cvc/subcomponents/hsm/x-ecu-id",
                ],
            ),
            (
                &|p| p.member_res("hsm", "/operations/x-csr/executions"),
                [
                    "/vehicle/v1/components/hsm/operations/x-csr/executions",
                    "/v1/components/hsm/operations/x-csr/executions",
                    "/v1/components/cvc/subcomponents/hsm/operations/x-csr/executions",
                ],
            ),
            (
                &|p| p.member_res("ecu", "/cyclic-subscriptions/s1"),
                [
                    "/vehicle/v1/components/ecu/cyclic-subscriptions/s1",
                    "/v1/components/ecu/cyclic-subscriptions/s1",
                    "/v1/components/cvc/subcomponents/ecu/cyclic-subscriptions/s1",
                ],
            ),
        ]);
    }

    #[test]
    fn gateway_apps() {
        check(&[
            (
                &|p| p.app("uds_gw", "engine_ecu"),
                [
                    "/vehicle/v1/components/uds_gw/apps/engine_ecu",
                    "/v1/components/uds_gw/apps/engine_ecu",
                    "/v1/components/cvc/subcomponents/uds_gw/apps/engine_ecu",
                ],
            ),
            (
                &|p| format!("{}/updates", p.app("uds_gw", "engine_ecu")),
                [
                    "/vehicle/v1/components/uds_gw/apps/engine_ecu/updates",
                    "/v1/components/uds_gw/apps/engine_ecu/updates",
                    "/v1/components/cvc/subcomponents/uds_gw/apps/engine_ecu/updates",
                ],
            ),
        ]);
    }

    #[test]
    fn node_level_resources() {
        check(&[
            (
                &|p| p.node_level("/status"),
                [
                    "/vehicle/v1/status",
                    "/v1/status",
                    "/v1/components/cvc/status",
                ],
            ),
            (
                &|p| p.node_level("/status/restart"),
                [
                    "/vehicle/v1/status/restart",
                    "/v1/status/restart",
                    "/v1/components/cvc/status/restart",
                ],
            ),
            (
                &|p| p.node_level("/status/x-boot-id"),
                [
                    "/vehicle/v1/status/x-boot-id",
                    "/v1/status/x-boot-id",
                    // Not served by SOVDd's node form (only /status, /status/restart).
                    "/v1/components/cvc/status/x-boot-id",
                ],
            ),
            (
                &|p| p.node_level("/updates"),
                [
                    "/vehicle/v1/updates",
                    "/v1/updates",
                    "/v1/components/cvc/updates",
                ],
            ),
            (
                &|p| p.node_level("/data/x-update-state"),
                [
                    "/vehicle/v1/data/x-update-state",
                    "/v1/data/x-update-state",
                    "/v1/components/cvc/data/x-update-state",
                ],
            ),
            (
                &|p| p.node_level("/operations/x-update-commit/executions"),
                [
                    "/vehicle/v1/operations/x-update-commit/executions",
                    "/v1/operations/x-update-commit/executions",
                    "/v1/components/cvc/operations/x-update-commit/executions",
                ],
            ),
            (
                &|p| p.node_level("/operations/factory-reset/executions"),
                [
                    "/vehicle/v1/operations/factory-reset/executions",
                    "/v1/operations/factory-reset/executions",
                    "/v1/components/cvc/operations/factory-reset/executions",
                ],
            ),
        ]);
    }

    #[test]
    fn dialect_names_round_trip() {
        assert_eq!(Dialect::default(), Dialect::Legacy);
        for d in [Dialect::Legacy, Dialect::V1] {
            assert_eq!(d.to_string().parse::<Dialect>().unwrap(), d);
        }
        assert_eq!("legacy".parse::<Dialect>().unwrap(), Dialect::Legacy);
        assert_eq!("v1".parse::<Dialect>().unwrap(), Dialect::V1);
        assert!("V1".parse::<Dialect>().is_err());
        assert!("auto".parse::<Dialect>().is_err());
    }

    #[test]
    fn accessors() {
        assert!(!Paths::legacy().is_v1());
        assert!(Paths::legacy().is_legacy());
        assert_eq!(Paths::legacy().dialect(), Dialect::Legacy);
        let node = Paths::v1(Some("cvc".into()));
        assert!(node.is_v1() && !node.is_legacy());
        assert_eq!(node.node(), Some("cvc"));
        assert_eq!(Paths::v1(None).node(), None);
    }
}
