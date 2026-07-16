// SPDX-License-Identifier: Apache-2.0

//! Claudion: the probe — measures Claude Code session energy.
//!
//! Parses Claude Code session JSONL logs and computes token consumption,
//! cost estimates, and context usage metrics. Part of the cosmon particle
//! ecosystem:
//!
//! | Particle | Role |
//! |----------|------|
//! | **cosmon** | The universe — orchestration |
//! | **neurion** | The nervous system — registry |
//! | **claudion** | The probe — session measurement |
//! | **topon** | The topology — code structure |
//!
//! # Quick start
//!
//! ```no_run
//! use claudion::{discover_sessions, parse_session, compute_metrics, PricingModel};
//!
//! let base = claudion::default_base_path();
//! let sessions = discover_sessions(&base).unwrap();
//!
//! let pricing = PricingModel::opus();
//! for sp in &sessions {
//!     let log = parse_session(&sp.path).unwrap();
//!     let metrics = compute_metrics(&log, &pricing);
//!     println!("{}: {} turns, {}", log.session_id, metrics.turn_count, metrics.total_cost);
//! }
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod discover;
pub mod energy;
pub mod error;
pub mod metrics;
pub mod parse;
pub mod pricing;
pub mod types;

// Re-exports for convenience.
pub use discover::{default_base_path, discover_project_sessions, discover_sessions};
pub use energy::{SessionId, TokenCost, TokenCount};
pub use error::ClaudionError;
pub use metrics::{aggregate_project, compute_metrics, ProjectMetrics, SessionMetrics};
pub use parse::parse_session;
pub use pricing::PricingModel;
pub use types::{SessionLog, SessionPath, Turn};
