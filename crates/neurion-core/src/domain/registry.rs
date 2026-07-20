// SPDX-License-Identifier: Apache-2.0

use serde_json::Value;

/// Category of inventory entries (infrastructure classifier).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Mcp,
    Service,
    Database,
    Config,
    Binary,
    Repo,
    Agent,
    /// First-class GitHub orgs / domain owners (table `organizations`,
    /// hypergraph kind `orgs`). The table holds one row per org with
    /// its github_handle, owned domains (JSON array), and an optional
    /// vault_note_path pointer; the hypergraph mirror produces nodes
    /// with the short kind `orgs` so existing `orgs:<name>` ids stay
    /// stable.
    Organization,
}

impl Category {
    pub fn all() -> &'static [Category] {
        &[
            Self::Mcp,
            Self::Service,
            Self::Database,
            Self::Config,
            Self::Binary,
            Self::Repo,
            Self::Agent,
            Self::Organization,
        ]
    }

    pub fn table_name(&self) -> &'static str {
        match self {
            Self::Mcp => "mcp_servers",
            Self::Service => "services",
            Self::Database => "databases",
            Self::Config => "config_files",
            Self::Binary => "binaries",
            Self::Repo => "repos",
            Self::Agent => "agents",
            Self::Organization => "organizations",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "mcp" => Some(Self::Mcp),
            "launchagent" | "service" => Some(Self::Service),
            "database" => Some(Self::Database),
            "config" => Some(Self::Config),
            "binary" => Some(Self::Binary),
            "repo" => Some(Self::Repo),
            "agent" => Some(Self::Agent),
            // Accept the short hypergraph kind ("orgs"), the singular
            // ("org"), and the full table name ("organizations") so
            // that callers can use whichever spelling is most natural
            // at the call site.
            "orgs" | "org" | "organization" | "organizations" => Some(Self::Organization),
            _ => None,
        }
    }
}

// -- Semantic layer types (ADR-003) --

/// Ranked access profile for a knowledge domain.
pub struct ReachProfile {
    pub referent: String,
    pub description: Option<String>,
    pub reaches: Vec<RankedReach>,
    pub health: HealthStatus,
}

/// What the consumer intends to do with the knowledge.
/// Filters which reaches are eligible and re-weights the score.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    Read,   // retrieve/view information
    Write,  // create/modify information at the source
    Search, // find specific items by query
    Verify, // confirm freshness/existence
}

impl Intent {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "read" => Some(Self::Read),
            "write" => Some(Self::Write),
            "search" => Some(Self::Search),
            "verify" => Some(Self::Verify),
            _ => None,
        }
    }
}

/// A single access path with channel properties and computed score.
pub struct RankedReach {
    pub bearer: String,
    pub bearer_table: String,
    pub kind: String,
    pub tool: String,
    pub latency_ms: Option<i64>,
    pub coverage: f64,
    pub queryability: f64,
    pub fidelity: f64,
    pub freshness_sec: Option<i64>,
    pub format: Option<String>,
    pub auth: bool,
    pub derived_from: Option<String>,
    pub description: Option<String>,
    pub capabilities: Vec<String>, // ["read", "search", "write", "verify"]
    pub score: f64,
}

/// A pointer to an authoritative `kind=person` surface — neurion's answer
/// to *"where is the fiche for this person?"*.
///
/// **Pointer-only by construction (ADR-005).** A `PersonSurface` carries
/// the surface primary key (which encodes the alias/slug), the `kind`
/// discriminant, and the `container_of` path to the fiche on disk. It
/// **never** carries a byte of the fiche *body* — no DOB, no domicile, no
/// nationalité. Neurion indexes the *path*, not the facts: update the
/// fiche and the answer updates for free, because the registry stores a
/// pointer that cannot rot. The complementary pointer-only PII enforcement
/// (schema column whitelist + sentinel-leak test) lives in the resolver's
/// `surfaces` query.
///
/// This is the structural reason a person is resolved by *pointer lookup*
/// rather than by a `[[reaches]]` row: channel properties
/// (`coverage`/`freshness` of *one* Jordan?) do not type-check on an
/// individual. `people`/`contacts` is a **domain** (a reach); `jordan` is
/// an **instance** (a surface).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonSurface {
    /// Surface primary key, e.g. `person:jordan-noog`. Encodes the slug.
    pub name: String,
    /// Surface kind — always `person` for rows returned by the resolver.
    pub kind: String,
    /// Authoritative pointer to the fiche on disk (the `container_of`
    /// column), e.g. `~/galaxies/knowledge/wiki/people/jordan-noog.md`.
    /// `None` only for a malformed surface row with no container.
    pub container_of: Option<String>,
}

/// Health status of a referent based on its reach topology.
/// Ordered from worst to best: Gap < Fragile < Shallow < Slow < Stale < Healthy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthStatus {
    Gap,     // no bearers at all
    Fragile, // single point of failure (1 bearer)
    Shallow, // bearers exist but max coverage < 0.1
    Slow,    // all bearers have latency > 10s
    Stale,   // all bearers stale > 24h
    Healthy, // redundant access with meaningful coverage
}

impl std::fmt::Display for HealthStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Gap => write!(f, "GAP"),
            Self::Fragile => write!(f, "FRAGILE"),
            Self::Shallow => write!(f, "SHALLOW"),
            Self::Slow => write!(f, "SLOW"),
            Self::Stale => write!(f, "STALE"),
            Self::Healthy => write!(f, "HEALTHY"),
        }
    }
}

/// Compute health status from a set of reaches.
pub fn compute_health(reaches: &[RankedReach]) -> HealthStatus {
    if reaches.is_empty() {
        return HealthStatus::Gap;
    }
    if reaches.len() == 1 {
        return HealthStatus::Fragile;
    }
    if reaches.iter().all(|r| r.coverage < 0.1) {
        return HealthStatus::Shallow;
    }
    if reaches
        .iter()
        .all(|r| r.latency_ms.unwrap_or(1000) > 10_000)
    {
        return HealthStatus::Slow;
    }
    if reaches
        .iter()
        .all(|r| r.freshness_sec.unwrap_or(3600) > 86400)
    {
        return HealthStatus::Stale;
    }
    HealthStatus::Healthy
}

/// Kind weight: primary > cache > index > materialization > unknown.
fn kind_weight(kind: &str) -> f64 {
    match kind {
        "primary" => 1.0,
        "cache" => 0.9,
        "index" => 0.7,
        "materialization" => 0.5,
        _ => 0.3,
    }
}

/// Default scoring function for ranking reaches.
/// Weighted: coverage 35%, queryability 25%, fidelity 15%, freshness 15%, latency 10%.
pub fn default_score(reach: &RankedReach) -> f64 {
    let kw = kind_weight(&reach.kind);
    let freshness_penalty = (reach.freshness_sec.unwrap_or(3600) as f64 / 86400.0).clamp(0.0, 1.0);
    kw * (0.35 * reach.coverage
        + 0.25 * reach.queryability
        + 0.15 * reach.fidelity
        + 0.15 * (1.0 - freshness_penalty)
        + 0.1 * (1.0 - (reach.latency_ms.unwrap_or(1000) as f64 / 10000.0).clamp(0.0, 1.0)))
}

/// Intent-aware scoring: re-weights channel properties based on what the consumer needs.
pub fn intent_score(reach: &RankedReach, intent: Intent) -> f64 {
    let kw = kind_weight(&reach.kind);
    let freshness_penalty = (reach.freshness_sec.unwrap_or(3600) as f64 / 86400.0).clamp(0.0, 1.0);
    let latency_norm = (reach.latency_ms.unwrap_or(1000) as f64 / 10000.0).clamp(0.0, 1.0);

    // Intent-dependent weights: (coverage, queryability, fidelity, freshness, latency)
    let (wc, wq, wf, wfr, wl) = match intent {
        Intent::Read => (0.30, 0.15, 0.30, 0.10, 0.15), // fidelity matters most
        Intent::Write => (0.10, 0.10, 0.10, 0.10, 0.60), // latency dominates (interactive)
        Intent::Search => (0.25, 0.45, 0.10, 0.10, 0.10), // queryability dominates
        Intent::Verify => (0.10, 0.10, 0.10, 0.50, 0.20), // freshness dominates
    };

    kw * (wc * reach.coverage
        + wq * reach.queryability
        + wf * reach.fidelity
        + wfr * (1.0 - freshness_penalty)
        + wl * (1.0 - latency_norm))
}

// -- Port trait --

/// The core port -- all data access goes through this trait.
/// Provides both inventory (CRUD) and semantic layer (graph) operations.
pub trait RegistryPort: Send + Sync {
    // -- Inventory operations --

    /// Execute a read-only SQL query and return rows as JSON.
    fn query_readonly(&self, sql: &str, params: &[&str]) -> anyhow::Result<Vec<Value>>;

    /// List inventory entries by category.
    fn list_by_category(&self, categories: &[Category]) -> anyhow::Result<Vec<Value>>;

    /// Search inventory tables by keyword (LIKE pattern). Legacy fallback.
    fn search_inventory(&self, pattern: &str) -> anyhow::Result<InventorySearchResult>;

    /// Insert or update an entry. Validates table name and column names.
    fn upsert(
        &self,
        table: &str,
        data: &serde_json::Map<String, Value>,
    ) -> anyhow::Result<Vec<Value>>;

    /// Delete an entry by primary key. Validates table name.
    fn delete(&self, table: &str, key: &str) -> anyhow::Result<usize>;

    // -- Semantic layer operations (the five algebraic operations) --

    /// Op 1: Fan-out — find referents matching a name and return ranked reaches.
    fn lookup_referent(&self, name: &str) -> anyhow::Result<Vec<ReachProfile>>;

    /// Op 1b: Person resolution — find authoritative `kind=person` surfaces
    /// whose name/slug matches `query`, returned as pointer-only
    /// [`PersonSurface`] values (ADR-005).
    ///
    /// This is the *missing JOIN*, not a new feature: the answer already
    /// exists in the auto-populated `surfaces` table with `container_of`
    /// → the fiche. The resolver teaches `how_to_access` to also probe
    /// `surfaces`, so a *person query* resolves to the fiche pointer
    /// instead of stale free-text inventory matches. Implementations MUST
    /// read only the pointer-level columns (`name`, `kind`,
    /// `container_of`) — never the fiche body.
    fn resolve_person_surfaces(&self, query: &str) -> anyhow::Result<Vec<PersonSurface>>;

    /// Op 3: Tool lookup — what tools does a bearer expose?
    fn tools_for(&self, bearer: &str) -> anyhow::Result<Vec<String>>;

    /// Op 4: Reverse traversal — what referents does a tool give access to?
    fn referents_via(&self, tool: &str) -> anyhow::Result<Vec<String>>;

    /// Op 2: Optimal reach — find the best reach for a referent given an intent.
    /// Filters reaches by capability, re-scores by intent, returns top match.
    fn optimal_reach(&self, referent: &str, intent: Intent) -> anyhow::Result<Option<RankedReach>>;

    /// Op 5: Gap analysis — referents with problematic health.
    fn health_report(&self) -> anyhow::Result<Vec<(String, HealthStatus)>>;

    // -- Hypergraph operations (ADR-004) --

    /// Insert or update a node. `ref_table`/`ref_id` are back-pointers
    /// into the legacy inventory tables (nullable for synthetic nodes
    /// that don't mirror a legacy row).
    fn graph_add_node(
        &self,
        id: &str,
        kind: &str,
        ref_table: Option<&str>,
        ref_id: Option<&str>,
    ) -> anyhow::Result<Value>;

    /// Insert an edge together with its endpoints in a single
    /// transaction. All referenced `endpoints[*].node_id` values must
    /// already exist in `nodes`; callers are expected to create them
    /// first via `graph_add_node`.
    fn graph_add_edge(
        &self,
        id: &str,
        relation: &str,
        verdict: Option<&str>,
        verdict_reason: Option<&str>,
        endpoints: &[GraphEndpoint],
    ) -> anyhow::Result<Value>;

    /// Return a node row plus every incident edge (with its full
    /// endpoint list). Shape:
    /// `{ "node": {...}, "edges": [{ "edge": {...}, "endpoints": [...] }, ...] }`.
    fn graph_describe_node(&self, id: &str) -> anyhow::Result<Value>;

    /// Bounded reachability walk from `start`. Traverses any edge that
    /// touches the current node, optionally filtered to a single
    /// `relation`. Returns rows `{ node_id, depth, relation, path }`
    /// sorted by depth then node id. Cycles are suppressed via a
    /// simple path-visited check.
    fn graph_query(
        &self,
        start: &str,
        relation: Option<&str>,
        max_depth: u32,
    ) -> anyhow::Result<Vec<Value>>;
}

/// Legacy inventory search result -- flat LIKE matches grouped by table.
pub struct InventorySearchResult {
    pub sections: Vec<(String, Vec<Value>)>,
}

impl InventorySearchResult {
    pub fn is_empty(&self) -> bool {
        self.sections.iter().all(|(_, rows)| rows.is_empty())
    }
}

// -- Hypergraph (ADR-004) --

/// One endpoint of an `edges` row: the node attached, its role, and
/// an optional ordinal for ordered N-ary relations.
pub struct GraphEndpoint {
    pub node_id: String,
    pub role: String,
    pub ord: Option<i64>,
}
