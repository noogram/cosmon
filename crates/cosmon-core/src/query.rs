// SPDX-License-Identifier: AGPL-3.0-only

//! Hybrid query routing — `SQLite` + Parquet + `DuckDB` backend selection.
//!
//! Implements the **archive-service pattern**: a query router that directs each query
//! to the optimal storage backend based on the query's *intent* (point lookup,
//! scan, aggregation, time-series analytics). The routing decision is a pure
//! function of the query characteristics — no I/O in this module.
//!
//! # Architecture
//!
//! Three storage tiers, each with different strengths:
//!
//! | Backend | Strength | Use case |
//! |---------|----------|----------|
//! | **File/`SQLite`** | Low-latency point reads, ACID mutations | Current state, lookups |
//! | **Parquet** | Columnar compression, predicate pushdown | Historical snapshots, bulk export |
//! | **`DuckDB`** | Analytical SQL over Parquet | Aggregations, time-series, cross-join |
//!
//! ```text
//!                ┌──────────────┐
//!   Query ──────▶│ QueryRouter  │
//!                └──────┬───────┘
//!                       │ route()
//!          ┌────────────┼────────────┐
//!          ▼            ▼            ▼
//!    ┌──────────┐ ┌──────────┐ ┌──────────┐
//!    │ SQLite   │ │ Parquet  │ │ DuckDB   │
//!    │ (OLTP)   │ │ (column) │ │ (OLAP)   │
//!    └──────────┘ └──────────┘ └──────────┘
//! ```
//!
//! # Design rationale (from archive-service study)
//!
//! The archive-service prototype demonstrated that a single storage backend forces a
//! trade-off between write latency and analytical throughput. The hybrid pattern
//! eliminates this by routing queries to the backend whose data layout matches
//! the access pattern. The router is a pure function — adapters (in separate
//! crates) implement the actual storage.
//!
//! See ADR-012 for the full decision record.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::CosmonError;

// ---------------------------------------------------------------------------
// StorageBackend — which storage tier to use
// ---------------------------------------------------------------------------

/// A storage backend in the hybrid query routing topology.
///
/// Each backend is optimized for a different access pattern. The query router
/// selects the appropriate backend based on the query's [`QueryIntent`].
///
/// # Examples
///
/// ```
/// use cosmon_core::query::StorageBackend;
///
/// let backend = StorageBackend::Sqlite;
/// assert_eq!(backend.to_string(), "sqlite");
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageBackend {
    /// Row-oriented store for current state — low-latency point reads and
    /// ACID mutations. Phase 1 uses `FileStore` (JSON); Phase 2 upgrades
    /// to `SQLite` when concurrent writes or indexing are needed (ADR-001).
    Sqlite,
    /// Columnar store for historical snapshots — optimized for predicate
    /// pushdown, projection, and bulk export. Written periodically by a
    /// compaction process; never mutated in place.
    Parquet,
    /// Analytical query engine over Parquet files — handles aggregations,
    /// time-series analysis, and cross-source joins. Read-only; operates
    /// on Parquet files produced by the compaction tier.
    DuckDb,
}

impl fmt::Display for StorageBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite => write!(f, "sqlite"),
            Self::Parquet => write!(f, "parquet"),
            Self::DuckDb => write!(f, "duckdb"),
        }
    }
}

impl std::str::FromStr for StorageBackend {
    type Err = CosmonError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "sqlite" => Ok(Self::Sqlite),
            "parquet" => Ok(Self::Parquet),
            "duckdb" => Ok(Self::DuckDb),
            other => Err(CosmonError::Runtime {
                reason: format!("unknown storage backend: {other}"),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// QueryIntent — the shape of a query
// ---------------------------------------------------------------------------

/// Describes what a query wants to do, independent of how it will be executed.
///
/// The router inspects the intent to choose the optimal backend. This is the
/// key abstraction: callers declare *what* they need, the router decides *where*.
///
/// # Examples
///
/// ```
/// use cosmon_core::query::QueryIntent;
///
/// let intent = QueryIntent::PointLookup;
/// assert!(intent.is_oltp());
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryIntent {
    /// Fetch a single entity by ID (molecule, worker, agent).
    /// Optimal backend: `SQLite` (indexed primary key lookup).
    PointLookup,
    /// Fetch current state of multiple entities with simple filters.
    /// Optimal backend: `SQLite` (indexed scans).
    FilteredList,
    /// Count entities matching a predicate.
    /// Optimal backend: `SQLite` (COUNT with WHERE).
    Count,
    /// Mutation: create, update, or delete an entity.
    /// Required backend: `SQLite` (only mutable backend).
    Mutate,
    /// Aggregate over historical data (sum, avg, min, max, percentiles).
    /// Optimal backend: `DuckDB` over Parquet (columnar aggregation).
    Aggregate,
    /// Time-series analysis over historical snapshots (trends, rates, windows).
    /// Optimal backend: `DuckDB` over Parquet (window functions).
    TimeSeries,
    /// Bulk export of historical or current data.
    /// Optimal backend: Parquet (direct file read, zero deserialization).
    BulkExport,
    /// Full-text or fuzzy search across entities.
    /// Optimal backend: `SQLite` (FTS5 extension).
    Search,
}

impl QueryIntent {
    /// Returns `true` if this intent is best served by an OLTP backend (`SQLite`).
    #[must_use]
    pub fn is_oltp(self) -> bool {
        matches!(
            self,
            Self::PointLookup | Self::FilteredList | Self::Count | Self::Mutate | Self::Search
        )
    }

    /// Returns `true` if this intent is best served by an OLAP backend (`DuckDB`/Parquet).
    #[must_use]
    pub fn is_olap(self) -> bool {
        matches!(self, Self::Aggregate | Self::TimeSeries | Self::BulkExport)
    }

    /// Returns `true` if this intent requires write access (mutation).
    #[must_use]
    pub fn is_mutation(self) -> bool {
        matches!(self, Self::Mutate)
    }
}

impl fmt::Display for QueryIntent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PointLookup => write!(f, "point_lookup"),
            Self::FilteredList => write!(f, "filtered_list"),
            Self::Count => write!(f, "count"),
            Self::Mutate => write!(f, "mutate"),
            Self::Aggregate => write!(f, "aggregate"),
            Self::TimeSeries => write!(f, "time_series"),
            Self::BulkExport => write!(f, "bulk_export"),
            Self::Search => write!(f, "search"),
        }
    }
}

// ---------------------------------------------------------------------------
// BackendCapability — what a backend can do
// ---------------------------------------------------------------------------

/// Declares the capabilities of a storage backend.
///
/// Used by the router to verify that a backend can actually handle a routed
/// query. Backends self-report their capabilities at registration time.
///
/// # Examples
///
/// ```
/// use cosmon_core::query::{BackendCapability, StorageBackend};
///
/// let cap = BackendCapability::for_backend(StorageBackend::Sqlite);
/// assert!(cap.supports_mutation);
/// assert!(cap.supports_point_lookup);
/// assert!(!cap.supports_columnar_scan);
/// ```
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendCapability {
    /// Which backend this describes.
    pub backend: StorageBackend,
    /// Can perform point lookups by primary key.
    pub supports_point_lookup: bool,
    /// Can perform filtered list scans.
    pub supports_filtered_list: bool,
    /// Can perform count queries.
    pub supports_count: bool,
    /// Can perform mutations (insert/update/delete).
    pub supports_mutation: bool,
    /// Can perform columnar aggregations.
    pub supports_aggregation: bool,
    /// Can perform time-series window queries.
    pub supports_time_series: bool,
    /// Can perform columnar scans (predicate pushdown).
    pub supports_columnar_scan: bool,
    /// Can perform full-text search.
    pub supports_search: bool,
}

impl BackendCapability {
    /// Returns the canonical capability profile for a given backend.
    ///
    /// These profiles encode the archive-service study findings: `SQLite` excels at
    /// OLTP, Parquet at columnar reads, `DuckDB` at analytical queries.
    #[must_use]
    pub fn for_backend(backend: StorageBackend) -> Self {
        match backend {
            StorageBackend::Sqlite => Self {
                backend,
                supports_point_lookup: true,
                supports_filtered_list: true,
                supports_count: true,
                supports_mutation: true,
                supports_aggregation: false,
                supports_time_series: false,
                supports_columnar_scan: false,
                supports_search: true,
            },
            StorageBackend::Parquet => Self {
                backend,
                supports_point_lookup: false,
                supports_filtered_list: true,
                supports_count: true,
                supports_mutation: false,
                supports_aggregation: false,
                supports_time_series: false,
                supports_columnar_scan: true,
                supports_search: false,
            },
            StorageBackend::DuckDb => Self {
                backend,
                supports_point_lookup: false,
                supports_filtered_list: true,
                supports_count: true,
                supports_mutation: false,
                supports_aggregation: true,
                supports_time_series: true,
                supports_columnar_scan: true,
                supports_search: false,
            },
        }
    }

    /// Check if this backend can handle the given query intent.
    #[must_use]
    pub fn can_handle(&self, intent: QueryIntent) -> bool {
        match intent {
            QueryIntent::PointLookup => self.supports_point_lookup,
            QueryIntent::FilteredList => self.supports_filtered_list,
            QueryIntent::Count => self.supports_count,
            QueryIntent::Mutate => self.supports_mutation,
            QueryIntent::Aggregate => self.supports_aggregation,
            QueryIntent::TimeSeries => self.supports_time_series,
            QueryIntent::BulkExport => self.supports_columnar_scan,
            QueryIntent::Search => self.supports_search,
        }
    }
}

// ---------------------------------------------------------------------------
// QueryRoute — the routing decision
// ---------------------------------------------------------------------------

/// A routing decision: which backend should handle this query, and why.
///
/// The `reason` field is for observability — it explains the routing logic
/// to operators debugging query performance.
///
/// # Examples
///
/// ```
/// use cosmon_core::query::{QueryRoute, StorageBackend, QueryIntent};
///
/// let route = QueryRoute::new(
///     StorageBackend::DuckDb,
///     QueryIntent::Aggregate,
///     "aggregation over historical energy records",
/// );
/// assert_eq!(route.backend(), &StorageBackend::DuckDb);
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryRoute {
    /// The selected backend.
    backend: StorageBackend,
    /// The intent that was routed.
    intent: QueryIntent,
    /// Human-readable explanation of why this backend was selected.
    reason: String,
}

impl QueryRoute {
    /// Create a new routing decision.
    #[must_use]
    pub fn new(backend: StorageBackend, intent: QueryIntent, reason: impl Into<String>) -> Self {
        Self {
            backend,
            intent,
            reason: reason.into(),
        }
    }

    /// The selected storage backend.
    #[must_use]
    pub fn backend(&self) -> &StorageBackend {
        &self.backend
    }

    /// The query intent that was routed.
    #[must_use]
    pub fn intent(&self) -> &QueryIntent {
        &self.intent
    }

    /// Explanation of the routing decision.
    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

impl fmt::Display for QueryRoute {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} → {} ({})", self.intent, self.backend, self.reason)
    }
}

// ---------------------------------------------------------------------------
// QueryRouter — the routing function
// ---------------------------------------------------------------------------

/// Routes queries to the optimal storage backend based on intent.
///
/// This is the core of the hybrid pattern: a pure function that maps
/// [`QueryIntent`] to [`StorageBackend`]. No I/O — adapters in separate
/// crates implement the actual storage.
///
/// # Routing rules (from archive-service study)
///
/// 1. **Mutations** → always `SQLite` (only mutable backend)
/// 2. **Point lookups** → `SQLite` (indexed primary key)
/// 3. **Search** → `SQLite` (FTS5)
/// 4. **Filtered lists / counts** → `SQLite` (indexed scans, current state)
/// 5. **Aggregations** → `DuckDB` over Parquet (columnar engine)
/// 6. **Time-series** → `DuckDB` over Parquet (window functions)
/// 7. **Bulk export** → Parquet direct (zero-copy columnar read)
///
/// # Fallback
///
/// If the preferred backend is unavailable, the router checks the fallback
/// chain. `SQLite` is always the terminal fallback for read queries.
///
/// # Examples
///
/// ```
/// use cosmon_core::query::{QueryRouter, QueryIntent, StorageBackend};
///
/// let router = QueryRouter::new();
/// let route = router.route(QueryIntent::Aggregate);
/// assert_eq!(route.backend(), &StorageBackend::DuckDb);
///
/// let route = router.route(QueryIntent::Mutate);
/// assert_eq!(route.backend(), &StorageBackend::Sqlite);
/// ```
#[derive(Clone, Debug)]
pub struct QueryRouter {
    /// Whether the `DuckDB` backend is available.
    duckdb_available: bool,
    /// Whether the Parquet backend is available.
    parquet_available: bool,
}

impl QueryRouter {
    /// Create a router with all backends available.
    #[must_use]
    pub fn new() -> Self {
        Self {
            duckdb_available: true,
            parquet_available: true,
        }
    }

    /// Create a router with specific backend availability.
    ///
    /// `SQLite` is always available (it's the base tier). This method controls
    /// whether the analytical backends (Parquet, `DuckDB`) are configured.
    #[must_use]
    pub fn with_availability(parquet_available: bool, duckdb_available: bool) -> Self {
        Self {
            duckdb_available,
            parquet_available,
        }
    }

    /// Route a query intent to the optimal backend.
    ///
    /// Returns a [`QueryRoute`] with the selected backend and explanation.
    #[must_use]
    pub fn route(&self, intent: QueryIntent) -> QueryRoute {
        match intent {
            // Mutations always go to SQLite — only mutable backend
            QueryIntent::Mutate => QueryRoute::new(
                StorageBackend::Sqlite,
                intent,
                "mutations require ACID — SQLite only",
            ),

            // Point lookups → SQLite (indexed PK)
            QueryIntent::PointLookup => QueryRoute::new(
                StorageBackend::Sqlite,
                intent,
                "point lookup by primary key — SQLite indexed",
            ),

            // Search → SQLite (FTS5)
            QueryIntent::Search => QueryRoute::new(
                StorageBackend::Sqlite,
                intent,
                "full-text search — SQLite FTS5",
            ),

            // Filtered list / count → SQLite (current state, indexed)
            QueryIntent::FilteredList => QueryRoute::new(
                StorageBackend::Sqlite,
                intent,
                "filtered list over current state — SQLite indexed scan",
            ),
            QueryIntent::Count => QueryRoute::new(
                StorageBackend::Sqlite,
                intent,
                "count with predicate — SQLite COUNT(*)",
            ),

            // Aggregations → DuckDB (columnar engine), fallback to SQLite
            QueryIntent::Aggregate => {
                if self.duckdb_available {
                    QueryRoute::new(
                        StorageBackend::DuckDb,
                        intent,
                        "columnar aggregation — DuckDB over Parquet",
                    )
                } else {
                    QueryRoute::new(
                        StorageBackend::Sqlite,
                        intent,
                        "aggregation fallback — DuckDB unavailable, using SQLite",
                    )
                }
            }

            // Time-series → DuckDB (window functions), fallback to SQLite
            QueryIntent::TimeSeries => {
                if self.duckdb_available {
                    QueryRoute::new(
                        StorageBackend::DuckDb,
                        intent,
                        "time-series window functions — DuckDB over Parquet",
                    )
                } else {
                    QueryRoute::new(
                        StorageBackend::Sqlite,
                        intent,
                        "time-series fallback — DuckDB unavailable, using SQLite",
                    )
                }
            }

            // Bulk export → Parquet direct, fallback to DuckDB, then SQLite
            QueryIntent::BulkExport => {
                if self.parquet_available {
                    QueryRoute::new(
                        StorageBackend::Parquet,
                        intent,
                        "bulk export — direct Parquet columnar read",
                    )
                } else if self.duckdb_available {
                    QueryRoute::new(
                        StorageBackend::DuckDb,
                        intent,
                        "bulk export fallback — Parquet unavailable, using DuckDB",
                    )
                } else {
                    QueryRoute::new(
                        StorageBackend::Sqlite,
                        intent,
                        "bulk export fallback — analytical backends unavailable, using SQLite",
                    )
                }
            }
        }
    }

    /// Check if a specific backend is available for routing.
    #[must_use]
    pub fn is_available(&self, backend: StorageBackend) -> bool {
        match backend {
            StorageBackend::Sqlite => true, // always available
            StorageBackend::Parquet => self.parquet_available,
            StorageBackend::DuckDb => self.duckdb_available,
        }
    }
}

impl Default for QueryRouter {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- StorageBackend ---

    #[test]
    fn test_storage_backend_display_roundtrip() {
        for (s, expected) in [
            ("sqlite", StorageBackend::Sqlite),
            ("parquet", StorageBackend::Parquet),
            ("duckdb", StorageBackend::DuckDb),
        ] {
            let parsed: StorageBackend = s.parse().unwrap();
            assert_eq!(parsed, expected);
            assert_eq!(parsed.to_string(), s);
        }
    }

    #[test]
    fn test_storage_backend_rejects_unknown() {
        let err = "mongodb".parse::<StorageBackend>().unwrap_err();
        assert!(err.to_string().contains("unknown storage backend"));
    }

    #[test]
    fn test_storage_backend_serde_roundtrip() {
        let backend = StorageBackend::DuckDb;
        let json = serde_json::to_string(&backend).unwrap();
        assert_eq!(json, "\"duck_db\"");
        let back: StorageBackend = serde_json::from_str(&json).unwrap();
        assert_eq!(back, backend);
    }

    // --- QueryIntent ---

    #[test]
    fn test_query_intent_oltp_classification() {
        assert!(QueryIntent::PointLookup.is_oltp());
        assert!(QueryIntent::FilteredList.is_oltp());
        assert!(QueryIntent::Count.is_oltp());
        assert!(QueryIntent::Mutate.is_oltp());
        assert!(QueryIntent::Search.is_oltp());
        assert!(!QueryIntent::Aggregate.is_oltp());
        assert!(!QueryIntent::TimeSeries.is_oltp());
        assert!(!QueryIntent::BulkExport.is_oltp());
    }

    #[test]
    fn test_query_intent_olap_classification() {
        assert!(QueryIntent::Aggregate.is_olap());
        assert!(QueryIntent::TimeSeries.is_olap());
        assert!(QueryIntent::BulkExport.is_olap());
        assert!(!QueryIntent::PointLookup.is_olap());
        assert!(!QueryIntent::Mutate.is_olap());
    }

    #[test]
    fn test_query_intent_mutation() {
        assert!(QueryIntent::Mutate.is_mutation());
        assert!(!QueryIntent::PointLookup.is_mutation());
        assert!(!QueryIntent::Aggregate.is_mutation());
    }

    #[test]
    fn test_query_intent_display() {
        assert_eq!(QueryIntent::PointLookup.to_string(), "point_lookup");
        assert_eq!(QueryIntent::TimeSeries.to_string(), "time_series");
        assert_eq!(QueryIntent::BulkExport.to_string(), "bulk_export");
    }

    // --- BackendCapability ---

    #[test]
    fn test_sqlite_capability_profile() {
        let cap = BackendCapability::for_backend(StorageBackend::Sqlite);
        assert!(cap.supports_point_lookup);
        assert!(cap.supports_filtered_list);
        assert!(cap.supports_count);
        assert!(cap.supports_mutation);
        assert!(cap.supports_search);
        assert!(!cap.supports_aggregation);
        assert!(!cap.supports_time_series);
        assert!(!cap.supports_columnar_scan);
    }

    #[test]
    fn test_parquet_capability_profile() {
        let cap = BackendCapability::for_backend(StorageBackend::Parquet);
        assert!(!cap.supports_point_lookup);
        assert!(cap.supports_filtered_list);
        assert!(cap.supports_count);
        assert!(!cap.supports_mutation);
        assert!(!cap.supports_search);
        assert!(!cap.supports_aggregation);
        assert!(!cap.supports_time_series);
        assert!(cap.supports_columnar_scan);
    }

    #[test]
    fn test_duckdb_capability_profile() {
        let cap = BackendCapability::for_backend(StorageBackend::DuckDb);
        assert!(!cap.supports_point_lookup);
        assert!(cap.supports_filtered_list);
        assert!(cap.supports_count);
        assert!(!cap.supports_mutation);
        assert!(!cap.supports_search);
        assert!(cap.supports_aggregation);
        assert!(cap.supports_time_series);
        assert!(cap.supports_columnar_scan);
    }

    #[test]
    fn test_capability_can_handle() {
        let sqlite = BackendCapability::for_backend(StorageBackend::Sqlite);
        assert!(sqlite.can_handle(QueryIntent::Mutate));
        assert!(sqlite.can_handle(QueryIntent::PointLookup));
        assert!(!sqlite.can_handle(QueryIntent::Aggregate));

        let duckdb = BackendCapability::for_backend(StorageBackend::DuckDb);
        assert!(!duckdb.can_handle(QueryIntent::Mutate));
        assert!(duckdb.can_handle(QueryIntent::Aggregate));
        assert!(duckdb.can_handle(QueryIntent::TimeSeries));
    }

    // --- QueryRoute ---

    #[test]
    fn test_query_route_display() {
        let route = QueryRoute::new(
            StorageBackend::DuckDb,
            QueryIntent::Aggregate,
            "columnar aggregation",
        );
        let display = route.to_string();
        assert!(display.contains("aggregate"));
        assert!(display.contains("duckdb"));
        assert!(display.contains("columnar aggregation"));
    }

    #[test]
    fn test_query_route_serde_roundtrip() {
        let route = QueryRoute::new(
            StorageBackend::Sqlite,
            QueryIntent::PointLookup,
            "indexed PK lookup",
        );
        let json = serde_json::to_string(&route).unwrap();
        let back: QueryRoute = serde_json::from_str(&json).unwrap();
        assert_eq!(route, back);
    }

    // --- QueryRouter ---

    #[test]
    fn test_router_mutations_always_sqlite() {
        let router = QueryRouter::new();
        let route = router.route(QueryIntent::Mutate);
        assert_eq!(route.backend(), &StorageBackend::Sqlite);
    }

    #[test]
    fn test_router_point_lookup_sqlite() {
        let router = QueryRouter::new();
        let route = router.route(QueryIntent::PointLookup);
        assert_eq!(route.backend(), &StorageBackend::Sqlite);
    }

    #[test]
    fn test_router_search_sqlite() {
        let router = QueryRouter::new();
        let route = router.route(QueryIntent::Search);
        assert_eq!(route.backend(), &StorageBackend::Sqlite);
    }

    #[test]
    fn test_router_aggregate_duckdb() {
        let router = QueryRouter::new();
        let route = router.route(QueryIntent::Aggregate);
        assert_eq!(route.backend(), &StorageBackend::DuckDb);
    }

    #[test]
    fn test_router_time_series_duckdb() {
        let router = QueryRouter::new();
        let route = router.route(QueryIntent::TimeSeries);
        assert_eq!(route.backend(), &StorageBackend::DuckDb);
    }

    #[test]
    fn test_router_bulk_export_parquet() {
        let router = QueryRouter::new();
        let route = router.route(QueryIntent::BulkExport);
        assert_eq!(route.backend(), &StorageBackend::Parquet);
    }

    #[test]
    fn test_router_fallback_when_duckdb_unavailable() {
        let router = QueryRouter::with_availability(true, false);

        let route = router.route(QueryIntent::Aggregate);
        assert_eq!(
            route.backend(),
            &StorageBackend::Sqlite,
            "aggregate should fall back to SQLite"
        );
        assert!(route.reason().contains("fallback"));

        let route = router.route(QueryIntent::TimeSeries);
        assert_eq!(
            route.backend(),
            &StorageBackend::Sqlite,
            "time-series should fall back to SQLite"
        );
    }

    #[test]
    fn test_router_bulk_export_fallback_chain() {
        // Parquet available → Parquet
        let router = QueryRouter::with_availability(true, true);
        assert_eq!(
            router.route(QueryIntent::BulkExport).backend(),
            &StorageBackend::Parquet
        );

        // Parquet unavailable, DuckDB available → DuckDB
        let router = QueryRouter::with_availability(false, true);
        assert_eq!(
            router.route(QueryIntent::BulkExport).backend(),
            &StorageBackend::DuckDb
        );

        // Both unavailable → SQLite
        let router = QueryRouter::with_availability(false, false);
        assert_eq!(
            router.route(QueryIntent::BulkExport).backend(),
            &StorageBackend::Sqlite
        );
    }

    #[test]
    fn test_router_sqlite_always_available() {
        let router = QueryRouter::with_availability(false, false);
        assert!(router.is_available(StorageBackend::Sqlite));
        assert!(!router.is_available(StorageBackend::Parquet));
        assert!(!router.is_available(StorageBackend::DuckDb));
    }

    #[test]
    fn test_router_oltp_queries_unaffected_by_availability() {
        let router = QueryRouter::with_availability(false, false);

        // All OLTP queries should still route to SQLite regardless
        for intent in [
            QueryIntent::PointLookup,
            QueryIntent::FilteredList,
            QueryIntent::Count,
            QueryIntent::Mutate,
            QueryIntent::Search,
        ] {
            assert_eq!(
                router.route(intent).backend(),
                &StorageBackend::Sqlite,
                "OLTP intent {intent} should always route to SQLite"
            );
        }
    }

    #[test]
    fn test_router_default() {
        let router = QueryRouter::default();
        assert!(router.is_available(StorageBackend::DuckDb));
        assert!(router.is_available(StorageBackend::Parquet));
    }

    #[test]
    fn test_every_intent_routes_to_capable_backend() {
        let router = QueryRouter::new();
        for intent in [
            QueryIntent::PointLookup,
            QueryIntent::FilteredList,
            QueryIntent::Count,
            QueryIntent::Mutate,
            QueryIntent::Aggregate,
            QueryIntent::TimeSeries,
            QueryIntent::BulkExport,
            QueryIntent::Search,
        ] {
            let route = router.route(intent);
            let cap = BackendCapability::for_backend(*route.backend());
            assert!(
                cap.can_handle(intent),
                "router sent {intent} to {} which cannot handle it",
                route.backend()
            );
        }
    }
}
