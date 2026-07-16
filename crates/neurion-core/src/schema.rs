// SPDX-License-Identifier: Apache-2.0

//! Neurion registry schema.
//!
//! The SQL DDL for creating a neurion registry database. Exposed as a
//! constant so that both the MCP server and `cs init` can create
//! identical databases without duplicating the schema.

/// The SQL DDL for creating all neurion registry tables.
///
/// This schema is versioned by convention: adding tables is always
/// backward compatible (CREATE TABLE IF NOT EXISTS). Structural
/// changes require a migration.
pub const SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS mcp_servers (
    name        TEXT PRIMARY KEY,
    command     TEXT NOT NULL,
    args        TEXT DEFAULT '[]',
    port        INTEGER,
    transport   TEXT DEFAULT 'stdio',
    config_files TEXT DEFAULT '[]',
    description TEXT,
    updated_at  TEXT DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS services (
    name        TEXT PRIMARY KEY,
    plist       TEXT,
    port        INTEGER,
    binary_path TEXT,
    restart     TEXT,
    description TEXT,
    updated_at  TEXT DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS databases (
    name        TEXT PRIMARY KEY,
    path        TEXT NOT NULL,
    engine      TEXT NOT NULL,
    size_bytes  INTEGER,
    description TEXT,
    access_via  TEXT,
    updated_at  TEXT DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS config_files (
    path        TEXT PRIMARY KEY,
    scope       TEXT,
    controls    TEXT,
    description TEXT,
    updated_at  TEXT DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS binaries (
    name        TEXT PRIMARY KEY,
    path        TEXT NOT NULL,
    binary_type TEXT,
    description TEXT,
    updated_at  TEXT DEFAULT (datetime('now'))
);

-- `galaxy_kind` (nullable) classifies the repo per the four-family taxonomy
-- (delib-20260419-5168 synthesis §5). Closed enum:
--   'infra' | 'project' | 'social-hub' | 'editorial' | NULL (nascent).
-- Validation of the accepted token set is enforced by `upsert_entry` in
-- neurion-mcp; SQLite stores it as free text so that the migration path is
-- a single ADD COLUMN with no CHECK rewrite.
CREATE TABLE IF NOT EXISTS repos (
    name         TEXT PRIMARY KEY,
    local_path   TEXT NOT NULL,
    remote_url   TEXT,
    branch       TEXT DEFAULT 'main',
    description  TEXT,
    galaxy_kind  TEXT,
    updated_at   TEXT DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS agents (
    name        TEXT PRIMARY KEY,
    role        TEXT,
    clearance   TEXT,
    model       TEXT,
    source_path TEXT,
    deployed_to TEXT,
    description TEXT,
    updated_at  TEXT DEFAULT (datetime('now'))
);

-- First-class GitHub orgs / domain owners (task-20260424-4908). Until
-- now an org lived only as the 'user/' prefix in `repos.remote_url`,
-- so the same handle could not be cross-referenced across repos,
-- domains, and vault notes without text-matching. This table makes
-- the org an addressable entity. The hypergraph mirror trigger below
-- emits nodes with the short kind 'orgs' (not 'organizations') so
-- that pre-existing 'orgs:<name>' nodes — added by hand before this
-- table existed — stay stable instead of forking into two ids.
--
-- Columns:
--   name           TEXT PK   -- short slug, e.g. 'noogram-labs'
--   github_handle  TEXT      -- the literal '<handle>' in
--                               github.com/<handle>; usually equal to
--                               name but kept distinct so the slug
--                               can drift (rename) without losing the
--                               GitHub identity.
--   domains        TEXT JSON -- array of owned DNS domains, e.g.
--                               an SQL JSON array literal.
--   description    TEXT
--   vault_note_path TEXT     -- absolute path or '~'-rooted path to
--                               an Obsidian note describing the org.
--   created_at     TEXT      -- first-seen timestamp (separate from
--                               updated_at so the org's age is preserved
--                               across upserts).
--   updated_at     TEXT      -- last write.
CREATE TABLE IF NOT EXISTS organizations (
    name            TEXT PRIMARY KEY,
    github_handle   TEXT,
    domains         TEXT DEFAULT '[]',
    description     TEXT,
    vault_note_path TEXT,
    created_at      TEXT DEFAULT (datetime('now')),
    updated_at      TEXT DEFAULT (datetime('now'))
);

-- Semantic layer: knowledge domains (referents)
CREATE TABLE IF NOT EXISTS referents (
    name        TEXT PRIMARY KEY,
    description TEXT,
    updated_at  TEXT DEFAULT (datetime('now'))
);

-- Semantic layer: access paths / materializations (reaches)
CREATE TABLE IF NOT EXISTS reaches (
    referent       TEXT NOT NULL REFERENCES referents(name),
    bearer         TEXT NOT NULL,
    bearer_table   TEXT NOT NULL,
    kind           TEXT NOT NULL,
    tool           TEXT DEFAULT '',
    latency_ms     INTEGER,
    coverage       REAL DEFAULT 0.5,
    queryability   REAL DEFAULT 0.5,
    fidelity       REAL DEFAULT 0.5,
    freshness_sec  INTEGER DEFAULT 3600,
    format         TEXT,
    auth           INTEGER DEFAULT 0,
    derived_from   TEXT,
    description    TEXT,
    capabilities   TEXT DEFAULT 'read',
    updated_at     TEXT DEFAULT (datetime('now')),
    PRIMARY KEY (referent, bearer, tool)
);

-- Infrastructure dependencies (bearer-to-bearer edges)
CREATE TABLE IF NOT EXISTS dependencies (
    source_table TEXT NOT NULL,
    source_key   TEXT NOT NULL,
    target_table TEXT NOT NULL,
    target_key   TEXT NOT NULL,
    relation     TEXT NOT NULL,
    description  TEXT,
    updated_at   TEXT DEFAULT (datetime('now')),
    PRIMARY KEY (source_table, source_key, target_table, target_key, relation)
);

-- Surfaces (Mailroom synthesis §C1, §C3, §C7; delib-20260417-02f3).
-- A surface is a contact point with the Real: person, institution, deal, or
-- commitment. Shannon columns (H_s, lambda_s, I_s, D_s, U_s_fn) parameterise
-- per-surface information physics. container_of is a hypergraph containment
-- pointer (e.g., a member is contained_in an org) — honest sheaf-over-surfaces
-- base category without pretending H^1 = 0 (Gödel §D3).
-- DESCRIBE-ONLY: neurion describes surfaces; it does not act on self.
CREATE TABLE IF NOT EXISTS surfaces (
    name            TEXT PRIMARY KEY,
    owner           TEXT,
    kind            TEXT,
    H_s             REAL,
    lambda_s        REAL,
    U_s_fn          TEXT,
    I_s             REAL,
    D_s             REAL,
    container_of    TEXT,
    last_signal_at  TEXT,
    capacity        REAL,
    updated_at      TEXT DEFAULT (datetime('now'))
);

-- Surface signals (V(m) edges). Each row attributes one observed signal to
-- one surface with its computed value. The same signal may touch multiple
-- surfaces — this table is the sheaf restriction map. DESCRIBE-ONLY.
CREATE TABLE IF NOT EXISTS surface_signals (
    surface_name  TEXT NOT NULL REFERENCES surfaces(name),
    signal_id     TEXT NOT NULL,
    channel       TEXT NOT NULL,
    V_m           REAL,
    observed_at   TEXT,
    updated_at    TEXT DEFAULT (datetime('now')),
    PRIMARY KEY (surface_name, signal_id, channel)
);

-- Chronicles (SYZYGIE child #4, delib-20260417-7e31). Index of chronicle
-- artifacts across galaxies — pointers only, not content. Enables
-- `how_to_access(\"principle:<name>\")` as pull-discovery: a galaxy
-- registers each chronicle with its principle sentence + relative path;
-- neurion indexes chronicles, not principles. The body stays in the
-- originating galaxy. Wheeler: neurion is the only registry; unregistering
-- is one SQL DELETE. Einstein: neurion indexe les chronicles, pas les
-- principes. von Neumann: (id, date, principle-sentence, origin, citations).
CREATE TABLE IF NOT EXISTS chronicles (
    id                   TEXT PRIMARY KEY,
    origin_galaxy        TEXT,
    relative_path        TEXT,
    absolute_path_hint   TEXT,
    date                 TEXT,
    title                TEXT,
    principle_sentence   TEXT,
    citations_out        TEXT DEFAULT '[]',
    tags                 TEXT DEFAULT '[]',
    updated_at           TEXT DEFAULT (datetime('now'))
);

-- Chronicle citations (chronicle-to-chronicle edges). Captures the
-- three-verdict rule (inherit | adapt | refuse) when one chronicle
-- cites a prior principle from another. NULL verdict = plain citation,
-- not a principle-inheritance claim.
CREATE TABLE IF NOT EXISTS chronicle_citations (
    from_chronicle  TEXT NOT NULL REFERENCES chronicles(id),
    to_chronicle    TEXT NOT NULL REFERENCES chronicles(id),
    verdict         TEXT,
    verdict_reason  TEXT,
    updated_at      TEXT DEFAULT (datetime('now')),
    PRIMARY KEY (from_chronicle, to_chronicle)
);
";

/// The SQL DDL for the typed-hypergraph shadow layer (ADR-004).
///
/// Split from `SCHEMA_SQL` so that consumers that already emit the
/// legacy inventory DDL (e.g. `neurion-mcp`'s inline `migrate()`) can
/// append the hypergraph DDL without duplicating table definitions.
///
/// Idempotent: every statement is `CREATE ... IF NOT EXISTS`. Safe to
/// apply repeatedly and safe to apply after the legacy DDL.
///
/// Shadow mode contract: the legacy tables remain authoritative. This
/// triplet populates in parallel via `AFTER INSERT` / `AFTER DELETE`
/// triggers on each legacy edge table + node table.
pub const HYPERGRAPH_SQL: &str = "
-- ============================================================
-- Typed hypergraph (ADR-004, idea-20260419-5913). Three tables
-- (nodes, edges, edge_endpoints) express every registry entity
-- and every relation as a single uniform shape so that
-- cross-kind reachability (repo -> service -> chronicle -> galaxy)
-- and N-ary relations (a listening session with 5 participants)
-- become a single recursive-CTE query instead of bespoke joins.
--
-- Shadow mode: the legacy tables above remain authoritative.
-- The hypergraph populates in parallel via the mirror triggers
-- declared at the bottom of this DDL. A follow-up ADR proposes
-- the cut-over (legacy tables become VIEWs over the hypergraph,
-- then are deprecated) once the shadow has run clean for ~2 weeks.
--
-- Composite node id convention: '<kind>:<local_id>' where <kind>
-- is the source-table name (plural; chosen so that the id is
-- trivially derivable from the table without any per-table case
-- mapping). Example: 'repos:showroom', 'chronicles:2026-04-17-syzygie'.
-- ============================================================

CREATE TABLE IF NOT EXISTS nodes (
    id          TEXT PRIMARY KEY,
    kind        TEXT NOT NULL,
    ref_table   TEXT,
    ref_id      TEXT,
    updated_at  TEXT DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS nodes_kind_idx ON nodes(kind);
CREATE INDEX IF NOT EXISTS nodes_ref_idx  ON nodes(ref_table, ref_id);

CREATE TABLE IF NOT EXISTS edges (
    id             TEXT PRIMARY KEY,
    relation       TEXT NOT NULL,
    verdict        TEXT,
    verdict_reason TEXT,
    ref_table      TEXT,
    ref_pk         TEXT,
    updated_at     TEXT DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS edges_relation_idx ON edges(relation);
CREATE INDEX IF NOT EXISTS edges_ref_idx      ON edges(ref_table, ref_pk);

CREATE TABLE IF NOT EXISTS edge_endpoints (
    edge_id  TEXT NOT NULL REFERENCES edges(id) ON DELETE CASCADE ON UPDATE CASCADE,
    node_id  TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE ON UPDATE CASCADE,
    role     TEXT NOT NULL,
    ord      INTEGER,
    PRIMARY KEY (edge_id, node_id, role)
);
CREATE INDEX IF NOT EXISTS edge_endpoints_node_idx ON edge_endpoints(node_id);
CREATE INDEX IF NOT EXISTS edge_endpoints_edge_idx ON edge_endpoints(edge_id);

-- ============================================================
-- Mirror triggers: node-like tables -> nodes.
-- Two triggers per table (AFTER INSERT, AFTER DELETE). The
-- INSERT variant uses INSERT OR REPLACE so upserts keep nodes
-- in sync. DELETE cascades to edge_endpoints via the FK.
-- Rename of the PK column is not mirrored here; rename is
-- handled by an explicit UPDATE on nodes (rare operation;
-- documented in ADR-004 §Rename).
-- ============================================================

CREATE TRIGGER IF NOT EXISTS nodes_mirror_mcp_servers_ai
AFTER INSERT ON mcp_servers BEGIN
    INSERT OR REPLACE INTO nodes (id, kind, ref_table, ref_id, updated_at)
    VALUES ('mcp_servers:' || NEW.name, 'mcp_servers', 'mcp_servers', NEW.name, datetime('now'));
END;
CREATE TRIGGER IF NOT EXISTS nodes_mirror_mcp_servers_ad
AFTER DELETE ON mcp_servers BEGIN
    DELETE FROM nodes WHERE id = 'mcp_servers:' || OLD.name;
END;

CREATE TRIGGER IF NOT EXISTS nodes_mirror_services_ai
AFTER INSERT ON services BEGIN
    INSERT OR REPLACE INTO nodes (id, kind, ref_table, ref_id, updated_at)
    VALUES ('services:' || NEW.name, 'services', 'services', NEW.name, datetime('now'));
END;
CREATE TRIGGER IF NOT EXISTS nodes_mirror_services_ad
AFTER DELETE ON services BEGIN
    DELETE FROM nodes WHERE id = 'services:' || OLD.name;
END;

CREATE TRIGGER IF NOT EXISTS nodes_mirror_databases_ai
AFTER INSERT ON databases BEGIN
    INSERT OR REPLACE INTO nodes (id, kind, ref_table, ref_id, updated_at)
    VALUES ('databases:' || NEW.name, 'databases', 'databases', NEW.name, datetime('now'));
END;
CREATE TRIGGER IF NOT EXISTS nodes_mirror_databases_ad
AFTER DELETE ON databases BEGIN
    DELETE FROM nodes WHERE id = 'databases:' || OLD.name;
END;

CREATE TRIGGER IF NOT EXISTS nodes_mirror_config_files_ai
AFTER INSERT ON config_files BEGIN
    INSERT OR REPLACE INTO nodes (id, kind, ref_table, ref_id, updated_at)
    VALUES ('config_files:' || NEW.path, 'config_files', 'config_files', NEW.path, datetime('now'));
END;
CREATE TRIGGER IF NOT EXISTS nodes_mirror_config_files_ad
AFTER DELETE ON config_files BEGIN
    DELETE FROM nodes WHERE id = 'config_files:' || OLD.path;
END;

CREATE TRIGGER IF NOT EXISTS nodes_mirror_binaries_ai
AFTER INSERT ON binaries BEGIN
    INSERT OR REPLACE INTO nodes (id, kind, ref_table, ref_id, updated_at)
    VALUES ('binaries:' || NEW.name, 'binaries', 'binaries', NEW.name, datetime('now'));
END;
CREATE TRIGGER IF NOT EXISTS nodes_mirror_binaries_ad
AFTER DELETE ON binaries BEGIN
    DELETE FROM nodes WHERE id = 'binaries:' || OLD.name;
END;

CREATE TRIGGER IF NOT EXISTS nodes_mirror_repos_ai
AFTER INSERT ON repos BEGIN
    INSERT OR REPLACE INTO nodes (id, kind, ref_table, ref_id, updated_at)
    VALUES ('repos:' || NEW.name, 'repos', 'repos', NEW.name, datetime('now'));
END;
CREATE TRIGGER IF NOT EXISTS nodes_mirror_repos_ad
AFTER DELETE ON repos BEGIN
    DELETE FROM nodes WHERE id = 'repos:' || OLD.name;
END;

CREATE TRIGGER IF NOT EXISTS nodes_mirror_agents_ai
AFTER INSERT ON agents BEGIN
    INSERT OR REPLACE INTO nodes (id, kind, ref_table, ref_id, updated_at)
    VALUES ('agents:' || NEW.name, 'agents', 'agents', NEW.name, datetime('now'));
END;
CREATE TRIGGER IF NOT EXISTS nodes_mirror_agents_ad
AFTER DELETE ON agents BEGIN
    DELETE FROM nodes WHERE id = 'agents:' || OLD.name;
END;

CREATE TRIGGER IF NOT EXISTS nodes_mirror_referents_ai
AFTER INSERT ON referents BEGIN
    INSERT OR REPLACE INTO nodes (id, kind, ref_table, ref_id, updated_at)
    VALUES ('referents:' || NEW.name, 'referents', 'referents', NEW.name, datetime('now'));
END;
CREATE TRIGGER IF NOT EXISTS nodes_mirror_referents_ad
AFTER DELETE ON referents BEGIN
    DELETE FROM nodes WHERE id = 'referents:' || OLD.name;
END;

CREATE TRIGGER IF NOT EXISTS nodes_mirror_surfaces_ai
AFTER INSERT ON surfaces BEGIN
    INSERT OR REPLACE INTO nodes (id, kind, ref_table, ref_id, updated_at)
    VALUES ('surfaces:' || NEW.name, 'surfaces', 'surfaces', NEW.name, datetime('now'));
END;
CREATE TRIGGER IF NOT EXISTS nodes_mirror_surfaces_ad
AFTER DELETE ON surfaces BEGIN
    DELETE FROM nodes WHERE id = 'surfaces:' || OLD.name;
END;

CREATE TRIGGER IF NOT EXISTS nodes_mirror_chronicles_ai
AFTER INSERT ON chronicles BEGIN
    INSERT OR REPLACE INTO nodes (id, kind, ref_table, ref_id, updated_at)
    VALUES ('chronicles:' || NEW.id, 'chronicles', 'chronicles', NEW.id, datetime('now'));
END;
CREATE TRIGGER IF NOT EXISTS nodes_mirror_chronicles_ad
AFTER DELETE ON chronicles BEGIN
    DELETE FROM nodes WHERE id = 'chronicles:' || OLD.id;
END;

-- organizations -> nodes. Asymmetric naming on purpose: the table is
-- spelled in full ('organizations') but the node kind is the short
-- 'orgs' (and ids are 'orgs:<name>'), so any node added by hand with
-- the conventional short prefix before this table existed remains
-- valid and points back to its row via ref_table / ref_id.
CREATE TRIGGER IF NOT EXISTS nodes_mirror_organizations_ai
AFTER INSERT ON organizations BEGIN
    INSERT OR REPLACE INTO nodes (id, kind, ref_table, ref_id, updated_at)
    VALUES ('orgs:' || NEW.name, 'orgs', 'organizations', NEW.name, datetime('now'));
END;
CREATE TRIGGER IF NOT EXISTS nodes_mirror_organizations_ad
AFTER DELETE ON organizations BEGIN
    DELETE FROM nodes WHERE id = 'orgs:' || OLD.name;
END;

-- ============================================================
-- Mirror triggers: edge-like tables -> edges + edge_endpoints.
-- Each trigger first guarantees node shadows exist (INSERT OR
-- IGNORE) so the FK on edge_endpoints holds even when the
-- dependency row arrives before the source/target rows.
-- DELETE on the legacy row removes the synthesized edge; the
-- ON DELETE CASCADE on edge_endpoints.edge_id sweeps endpoints.
-- ============================================================

-- chronicle_citations -> 'cites' edge (verdict passes through).
CREATE TRIGGER IF NOT EXISTS edges_mirror_chronicle_citations_ai
AFTER INSERT ON chronicle_citations BEGIN
    INSERT OR IGNORE INTO nodes (id, kind, ref_table, ref_id)
    VALUES ('chronicles:' || NEW.from_chronicle, 'chronicles', 'chronicles', NEW.from_chronicle);
    INSERT OR IGNORE INTO nodes (id, kind, ref_table, ref_id)
    VALUES ('chronicles:' || NEW.to_chronicle,   'chronicles', 'chronicles', NEW.to_chronicle);

    INSERT OR REPLACE INTO edges (id, relation, verdict, verdict_reason, ref_table, ref_pk, updated_at)
    VALUES (
        'cites:' || NEW.from_chronicle || '->' || NEW.to_chronicle,
        'cites',
        NEW.verdict,
        NEW.verdict_reason,
        'chronicle_citations',
        NEW.from_chronicle || '|' || NEW.to_chronicle,
        datetime('now')
    );

    INSERT OR REPLACE INTO edge_endpoints (edge_id, node_id, role, ord)
    VALUES ('cites:' || NEW.from_chronicle || '->' || NEW.to_chronicle,
            'chronicles:' || NEW.from_chronicle, 'citer', 1);
    INSERT OR REPLACE INTO edge_endpoints (edge_id, node_id, role, ord)
    VALUES ('cites:' || NEW.from_chronicle || '->' || NEW.to_chronicle,
            'chronicles:' || NEW.to_chronicle,   'cited', 2);
END;

CREATE TRIGGER IF NOT EXISTS edges_mirror_chronicle_citations_ad
AFTER DELETE ON chronicle_citations BEGIN
    DELETE FROM edges
    WHERE id = 'cites:' || OLD.from_chronicle || '->' || OLD.to_chronicle;
END;

-- dependencies -> edge (relation column carries the predicate).
-- The source/target nodes are synthesized using the stored
-- (source_table, source_key) / (target_table, target_key) tuples
-- with table name as kind (plural, same convention as the node
-- mirror triggers).
CREATE TRIGGER IF NOT EXISTS edges_mirror_dependencies_ai
AFTER INSERT ON dependencies BEGIN
    INSERT OR IGNORE INTO nodes (id, kind, ref_table, ref_id)
    VALUES (NEW.source_table || ':' || NEW.source_key, NEW.source_table, NEW.source_table, NEW.source_key);
    INSERT OR IGNORE INTO nodes (id, kind, ref_table, ref_id)
    VALUES (NEW.target_table || ':' || NEW.target_key, NEW.target_table, NEW.target_table, NEW.target_key);

    INSERT OR REPLACE INTO edges (id, relation, ref_table, ref_pk, updated_at)
    VALUES (
        'dep:' || NEW.relation || ':' || NEW.source_table || '/' || NEW.source_key
              || '->' || NEW.target_table || '/' || NEW.target_key,
        NEW.relation,
        'dependencies',
        NEW.source_table || '|' || NEW.source_key || '|'
            || NEW.target_table || '|' || NEW.target_key || '|' || NEW.relation,
        datetime('now')
    );

    INSERT OR REPLACE INTO edge_endpoints (edge_id, node_id, role, ord)
    VALUES (
        'dep:' || NEW.relation || ':' || NEW.source_table || '/' || NEW.source_key
              || '->' || NEW.target_table || '/' || NEW.target_key,
        NEW.source_table || ':' || NEW.source_key, 'src', 1
    );
    INSERT OR REPLACE INTO edge_endpoints (edge_id, node_id, role, ord)
    VALUES (
        'dep:' || NEW.relation || ':' || NEW.source_table || '/' || NEW.source_key
              || '->' || NEW.target_table || '/' || NEW.target_key,
        NEW.target_table || ':' || NEW.target_key, 'dst', 2
    );
END;

CREATE TRIGGER IF NOT EXISTS edges_mirror_dependencies_ad
AFTER DELETE ON dependencies BEGIN
    DELETE FROM edges
    WHERE id = 'dep:' || OLD.relation || ':' || OLD.source_table || '/' || OLD.source_key
            || '->' || OLD.target_table || '/' || OLD.target_key;
END;

-- reaches -> 'reaches' edge (referent -> bearer, bearer_table
-- tells us which node kind the bearer lives in).
CREATE TRIGGER IF NOT EXISTS edges_mirror_reaches_ai
AFTER INSERT ON reaches BEGIN
    INSERT OR IGNORE INTO nodes (id, kind, ref_table, ref_id)
    VALUES ('referents:' || NEW.referent, 'referents', 'referents', NEW.referent);
    INSERT OR IGNORE INTO nodes (id, kind, ref_table, ref_id)
    VALUES (NEW.bearer_table || ':' || NEW.bearer, NEW.bearer_table, NEW.bearer_table, NEW.bearer);

    INSERT OR REPLACE INTO edges (id, relation, ref_table, ref_pk, updated_at)
    VALUES (
        'reach:' || NEW.referent || '->' || NEW.bearer_table || '/' || NEW.bearer
              || '#' || NEW.tool,
        'reaches',
        'reaches',
        NEW.referent || '|' || NEW.bearer || '|' || NEW.tool,
        datetime('now')
    );

    INSERT OR REPLACE INTO edge_endpoints (edge_id, node_id, role, ord)
    VALUES (
        'reach:' || NEW.referent || '->' || NEW.bearer_table || '/' || NEW.bearer
              || '#' || NEW.tool,
        'referents:' || NEW.referent, 'referent', 1
    );
    INSERT OR REPLACE INTO edge_endpoints (edge_id, node_id, role, ord)
    VALUES (
        'reach:' || NEW.referent || '->' || NEW.bearer_table || '/' || NEW.bearer
              || '#' || NEW.tool,
        NEW.bearer_table || ':' || NEW.bearer, 'bearer', 2
    );
END;

CREATE TRIGGER IF NOT EXISTS edges_mirror_reaches_ad
AFTER DELETE ON reaches BEGIN
    DELETE FROM edges
    WHERE id = 'reach:' || OLD.referent || '->' || OLD.bearer_table || '/' || OLD.bearer
            || '#' || OLD.tool;
END;
";
