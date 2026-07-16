// SPDX-License-Identifier: AGPL-3.0-only

//! Cluster topology config — parsed from `cluster.toml`.
//!
//! A cluster is the set of devices that share one cosmon galaxy tree:
//! the operator's primary Mac, the iPad, an optional AWS Synapse, a
//! collaborator's iPad, and the surfaces they expose (cs-api,
//! matrix-echo-tick, Mac/iOS apps). `cluster.toml` is the **single
//! source of truth for the shape of the cluster** — every surface
//! reads it so that adding a new device (a second Mac, a new
//! collaborator) never requires rebuilding code.
//!
//! This module is zero-I/O: it knows how to deserialize the TOML
//! structure and validate the schema. The actual read/write lives in
//! `cosmon-filestore` (for the CLI) and in `cosmon-api` (for the
//! HTTP endpoint).
//!
//! See [ADR-066](../../../docs/adr/066-surfaces-cluster-config.md) for
//! the full design and the three invariants:
//!
//! 1. **References, not secrets** — `credentials_file` is a path, the
//!    credential itself never appears in the TOML.
//! 2. **Read-only for surfaces** — only the operator edits;
//!    surfaces read and cache.
//! 3. **Versioned** — `schema_version` begins at 1 and bumps on any
//!    breaking change; older readers log a warning and proceed.
//!
//! ```toml
//! schema_version = 1
//!
//! [cluster]
//! name = "you-local"
//! owner_nucleon_id = "you"
//! tailnet_domain = "tail-XXXX.ts.net"
//!
//! [host.mbp]
//! tailscale_ip = "192.0.2.10"
//! tailscale_hostname = "host.example"
//! role = "primary"
//!
//! [surfaces.cs_api]
//! host = "mbp"
//! port = 4222
//! launchagent = "dev.noogram.cosmon.cs-api"
//! ```

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The current schema version understood by this crate. Readers tolerate
/// files stamped with an older version (simply ignore newer fields).
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// Top-level cluster configuration. See module docs for the TOML shape.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClusterConfig {
    /// Schema version. Begins at 1; any breaking change bumps it.
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,

    /// Cluster-wide identity + metadata.
    #[serde(default)]
    pub cluster: ClusterMetadata,

    /// Devices in the cluster, keyed by short local name (e.g. `mbp`,
    /// `ipad-operator-demo`). Surface references resolve their `host` field
    /// into one of these keys.
    #[serde(default)]
    pub host: BTreeMap<String, Host>,

    /// Surfaces exposed by the cluster (cs-api, matrix-echo-tick,
    /// future homeservers …). Each names a host by short key and
    /// whatever service-specific attributes it needs.
    #[serde(default)]
    pub surfaces: Surfaces,

    /// App bundle identifiers (Mac + iOS + future). Not strictly
    /// topology, but a common place to record "the iPad should install
    /// the bundle named X" for pairing flows.
    #[serde(default)]
    pub apps: AppBundles,
}

impl Default for ClusterConfig {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            cluster: ClusterMetadata::default(),
            host: BTreeMap::new(),
            surfaces: Surfaces::default(),
            apps: AppBundles::default(),
        }
    }
}

fn default_schema_version() -> u32 {
    CURRENT_SCHEMA_VERSION
}

/// Cluster-wide identity fields.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClusterMetadata {
    /// Human-readable cluster name (`you-local`, `tenant-demo-prod`, …).
    #[serde(default)]
    pub name: Option<String>,

    /// Operator nucleon id — the continuous cognitive field that owns
    /// this cluster (ADR-061).
    #[serde(default)]
    pub owner_nucleon_id: Option<String>,

    /// Tailnet domain for `MagicDNS` resolution (e.g.
    /// `tail-XXXX.ts.net`). Optional — the IP fallback always works.
    #[serde(default)]
    pub tailnet_domain: Option<String>,

    /// ISO-8601 timestamp of the last operator edit. Self-documenting;
    /// the file is still authoritative even when stale.
    #[serde(default)]
    pub updated_at: Option<String>,
}

/// One device in the cluster.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Host {
    /// Tailscale IPv4 address. Always present in practice; the whole
    /// point is that the cluster is Tailscale-reachable.
    #[serde(default)]
    pub tailscale_ip: Option<String>,

    /// Tailscale `MagicDNS` hostname (short form, no `.ts.net`).
    #[serde(default)]
    pub tailscale_hostname: Option<String>,

    /// Role hint: `primary`, `secondary`, `collaborator`, `worker`,
    /// etc. Free-form; not enforced. Helps a human reader.
    #[serde(default)]
    pub role: Option<String>,
}

/// The `[surfaces.*]` tables.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Surfaces {
    /// `cs-api` HTTP adapter.
    #[serde(default)]
    pub cs_api: Option<CsApiSurface>,

    /// `matrix-echo-tick` bridge.
    #[serde(default)]
    pub matrix_echo_tick: Option<MatrixEchoTickSurface>,
}

/// `[surfaces.cs_api]` — the HTTP endpoint native pilots hit.
///
/// The `host` field must name a key in the top-level `host` map.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CsApiSurface {
    /// Host key (must match a `[host.*]` entry).
    pub host: String,
    /// TCP port (default 4222).
    #[serde(default = "default_cs_api_port")]
    pub port: u16,
    /// Optional `LaunchAgent` label for bookkeeping.
    #[serde(default)]
    pub launchagent: Option<String>,
}

fn default_cs_api_port() -> u16 {
    4222
}

/// `[surfaces.matrix_echo_tick]` — Matrix bridge for the
/// `cosmon-whispers` room.
///
/// Carries the room id inline so the shared topology artifact can
/// point every device at the same room without re-reading the
/// per-host credential file.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MatrixEchoTickSurface {
    /// Host key (must match a `[host.*]` entry).
    pub host: String,
    /// Optional `LaunchAgent` label.
    #[serde(default)]
    pub launchagent: Option<String>,
    /// **Path** to the credentials file. Never the secret itself
    /// (invariant §1 from ADR-066).
    #[serde(default)]
    pub credentials_file: Option<String>,
    /// Matrix room id (`!xxx:server.tld`) for `cosmon-whispers`.
    #[serde(default)]
    pub room_id: Option<String>,
}

/// `[apps]` — app-bundle identifiers shared by the cluster.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppBundles {
    /// macOS pilot bundle id.
    #[serde(default)]
    pub mac_pilot_bundle_id: Option<String>,
    /// iOS / iPadOS pilot bundle id.
    #[serde(default)]
    pub ios_pilot_bundle_id: Option<String>,
}

impl ClusterConfig {
    /// Parse a TOML string into a `ClusterConfig`. Unknown top-level
    /// fields are tolerated (see `#[serde(default)]` on every section)
    /// so a future v2 file still opens under a v1 reader.
    ///
    /// # Errors
    ///
    /// Returns a `toml::de::Error` if the input is not valid TOML or
    /// if a required field on a declared section has the wrong type.
    pub fn from_toml_str(s: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(s)
    }

    /// Render this config as a TOML string. Used by `cs cluster edit`
    /// when seeding the file and by integration tests.
    ///
    /// # Errors
    ///
    /// Returns a `toml::ser::Error` if any field fails to serialize —
    /// in practice this only happens if a string contains a byte
    /// sequence TOML cannot represent.
    pub fn to_toml_string(&self) -> Result<String, toml::ser::Error> {
        toml::to_string_pretty(self)
    }

    /// Resolve the cs-api base URL as `http://<ip>:<port>` using the
    /// host lookup. Returns `None` when no `cs_api` surface is
    /// declared, the host key is missing from `[host.*]`, or the host
    /// has no `tailscale_ip`.
    #[must_use]
    pub fn cs_api_base_url(&self) -> Option<String> {
        let cs_api = self.surfaces.cs_api.as_ref()?;
        let host = self.host.get(&cs_api.host)?;
        let ip = host.tailscale_ip.as_ref()?;
        Some(format!("http://{ip}:{}", cs_api.port))
    }
}

/// Placeholder string used in generated templates for values the
/// operator must edit before the file is useful.
pub const TEMPLATE_PLACEHOLDER: &str = "<TO_FILL>";

/// Produce a commented TOML template suitable for `cs cluster edit`
/// when the file does not exist yet. The comments document the schema
/// inline so the operator does not need to read the ADR first.
#[must_use]
pub fn template_toml() -> String {
    let schema = CURRENT_SCHEMA_VERSION;
    let placeholder = TEMPLATE_PLACEHOLDER;
    format!(
        r#"# ~/.config/cosmon/cluster.toml — cosmon cluster topology (ADR-066)
#
# This file describes the shape of your cosmon cluster: which devices
# are in the Tailscale orbit, which surfaces they expose, which app
# bundles are deployed. Every surface (cs-api, matrix-echo-tick, iOS
# pilot) reads this file so that adding a new device never requires
# rebuilding code.

schema_version = {schema}

[cluster]
name = "{placeholder}"
owner_nucleon_id = "{placeholder}"
tailnet_domain = ""  # optional; fill if you use MagicDNS
# updated_at is written by `cs cluster edit` when you save.

# --- Hosts ---------------------------------------------------------
# One table per device. The key (e.g. `mbp`) is what surfaces below
# reference in their `host = "..."` field.

[host.mbp]
tailscale_ip = "{placeholder}"
tailscale_hostname = "{placeholder}"
role = "primary"

# --- Surfaces ------------------------------------------------------

[surfaces.cs_api]
host = "mbp"
port = 4222
launchagent = "dev.noogram.cosmon.cs-api"

[surfaces.matrix_echo_tick]
host = "mbp"
launchagent = "dev.noogram.cosmon.matrix-tick"
# Reference only — never inline the secret. (ADR-066 §2.1 invariant #1)
credentials_file = "~/.config/cosmon-matrix-tick/credentials.toml"
room_id = "{placeholder}"

# --- Apps ----------------------------------------------------------

[apps]
mac_pilot_bundle_id = "dev.noogram.cosmon.mac-pilot"
ios_pilot_bundle_id = "dev.noogram.cosmon.ios-pilot"
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_round_trips_through_toml() {
        let default = ClusterConfig::default();
        let encoded = default.to_toml_string().expect("serialize");
        let decoded = ClusterConfig::from_toml_str(&encoded).expect("deserialize");
        assert_eq!(default, decoded);
    }

    #[test]
    fn parses_a_minimal_config_with_only_host_and_cs_api() {
        let input = r#"
schema_version = 1

[cluster]
name = "you-local"
owner_nucleon_id = "you"

[host.mbp]
tailscale_ip = "192.0.2.10"
tailscale_hostname = "mbp"
role = "primary"

[surfaces.cs_api]
host = "mbp"
port = 4222
"#;
        let parsed = ClusterConfig::from_toml_str(input).expect("parse");
        assert_eq!(parsed.schema_version, 1);
        assert_eq!(parsed.cluster.name.as_deref(), Some("you-local"));
        assert_eq!(
            parsed
                .host
                .get("mbp")
                .and_then(|h| h.tailscale_ip.as_deref()),
            Some("192.0.2.10")
        );
        assert_eq!(
            parsed.cs_api_base_url().as_deref(),
            Some("http://192.0.2.10:4222")
        );
    }

    #[test]
    fn cs_api_base_url_is_none_when_host_missing() {
        let input = r#"
[surfaces.cs_api]
host = "ghost"
port = 4222
"#;
        let parsed = ClusterConfig::from_toml_str(input).expect("parse");
        assert_eq!(parsed.cs_api_base_url(), None);
    }

    #[test]
    fn cs_api_base_url_is_none_when_no_cs_api_surface() {
        let parsed = ClusterConfig::default();
        assert_eq!(parsed.cs_api_base_url(), None);
    }

    #[test]
    fn schema_version_defaults_to_current_when_missing() {
        let input = "[cluster]\nname = \"minimal\"\n";
        let parsed = ClusterConfig::from_toml_str(input).expect("parse");
        assert_eq!(parsed.schema_version, CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn template_parses_as_a_valid_cluster_config() {
        let tmpl = template_toml();
        ClusterConfig::from_toml_str(&tmpl).expect("template parses as v1 cluster config");
    }

    #[test]
    fn matrix_surface_carries_room_id_and_credentials_path_only() {
        let input = r#"
[surfaces.matrix_echo_tick]
host = "mbp"
credentials_file = "~/.config/cosmon-matrix-tick/credentials.toml"
room_id = "!room:matrix.org"
"#;
        let parsed = ClusterConfig::from_toml_str(input).expect("parse");
        let s = parsed.surfaces.matrix_echo_tick.expect("surface present");
        assert_eq!(s.host, "mbp");
        assert_eq!(s.room_id.as_deref(), Some("!room:matrix.org"));
        assert!(s
            .credentials_file
            .as_deref()
            .unwrap_or("")
            .ends_with("credentials.toml"));
    }
}
