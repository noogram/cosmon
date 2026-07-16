# ADR-012: Hybrid Query Routing — SQLite + Parquet + DuckDB

## Status
Accepted

## Context

ADR-001 established a phased storage strategy (JSON → SQLite → distributed).
As the system grows, different query patterns emerge with conflicting
optimization needs:

- **OLTP** (current state, lookups, mutations) needs low-latency indexed reads
  and ACID writes → row-oriented storage (SQLite)
- **OLAP** (aggregations, time-series, trend analysis) needs columnar scans
  with predicate pushdown → columnar storage (Parquet + DuckDB)
- **Bulk export** (data pipelines, external analytics) needs zero-copy
  columnar reads → direct Parquet file access

A single backend forces a trade-off. The archive-service prototype confirmed this:
SQLite handled operational queries well but choked on analytical aggregations
over historical data, while Parquet excelled at analytics but couldn't serve
point lookups or mutations.

## Decision

### Intent-based query routing

Introduce a `QueryRouter` in `cosmon-core` that routes queries to the optimal
storage backend based on the query's declared *intent* (`QueryIntent`), not
its SQL text or API shape. The router is a pure function — zero I/O.

### Three-tier topology

| Tier | Backend | Role | Access |
|------|---------|------|--------|
| **Operational** | SQLite | Current state, mutations, search | Read-write |
| **Columnar** | Parquet | Historical snapshots, bulk export | Read-only |
| **Analytical** | DuckDB | Aggregations, time-series, cross-joins | Read-only (over Parquet) |

### Routing rules

1. **Mutations** → always SQLite (only mutable backend)
2. **Point lookups, search** → SQLite (indexed)
3. **Filtered lists, counts** → SQLite (current state)
4. **Aggregations, time-series** → DuckDB over Parquet
5. **Bulk export** → Parquet direct read

### Graceful degradation

If analytical backends are unavailable (not configured, not installed),
the router falls back to SQLite for all queries. This preserves ADR-001's
"JSON/SQLite first" principle — the analytical tier is additive, not required.

### Compaction pipeline (future)

A periodic compaction process will snapshot operational state from SQLite
into Parquet files. DuckDB queries these snapshots. The compaction boundary
is a natural point for retention policies and archival.

## Consequences

### Positive
- Each query type uses the backend optimized for its access pattern
- Analytical queries don't compete with operational mutations for locks
- Parquet files are immutable, portable, and efficiently compressed
- Graceful degradation means SQLite-only deployments still work
- Pure domain types — the router is testable without any storage backend

### Negative
- Three backends to configure in production deployments
- Compaction pipeline adds operational complexity
- Query callers must declare intent (minor API overhead)

### Risks
- If compaction lags, analytical queries return stale data.
  Mitigation: staleness is explicit (Parquet files carry timestamps).
- DuckDB is a C library with FFI overhead in Rust.
  Mitigation: DuckDB tier is optional; SQLite handles everything at lower scale.

## References
- ADR-001: State Storage — JSON First (phased backend strategy)
- ADR-011: Content-Identity Principle (companion-note pattern for external sources)
- Thesis P15: Morphological Plasticity — backend swaps without identity change
- archive-service prototype: validated hybrid pattern with real workload
