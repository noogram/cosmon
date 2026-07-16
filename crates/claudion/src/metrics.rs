// SPDX-License-Identifier: Apache-2.0

//! Session and project metrics computation.
//!
//! Pure functions that transform parsed session data into aggregated metrics.
//! No I/O — all inputs are in-memory types, all outputs are serializable values.

use crate::energy::{SessionId, TokenCost, TokenCount};
use serde::{Deserialize, Serialize};

use crate::pricing::PricingModel;
use crate::types::SessionLog;

/// Aggregated metrics for a single session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionMetrics {
    /// Session identifier.
    pub session_id: SessionId,
    /// Number of assistant turns.
    pub turn_count: usize,
    /// Total fresh input tokens.
    pub total_input: TokenCount,
    /// Total output tokens.
    pub total_output: TokenCount,
    /// Total cache-creation tokens.
    pub total_cache_creation: TokenCount,
    /// Total cache-read tokens.
    pub total_cache_read: TokenCount,
    /// Grand total across all categories.
    pub total_tokens: TokenCount,
    /// Estimated cost based on the pricing model.
    pub total_cost: TokenCost,
    /// Average total input tokens per turn (context size proxy).
    pub avg_context_per_turn: f64,
    /// `cache_read / (cache_read + fresh_input)` — cache effectiveness.
    pub cache_hit_ratio: f64,
    /// Session wall-clock duration in seconds (if timestamps are available).
    pub duration_secs: Option<f64>,
}

/// Aggregated metrics across multiple sessions for a project.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProjectMetrics {
    /// Encoded project directory name.
    pub project: String,
    /// Number of sessions.
    pub session_count: usize,
    /// Total assistant turns across all sessions.
    pub total_turns: usize,
    /// Grand total tokens.
    pub total_tokens: TokenCount,
    /// Grand total cost.
    pub total_cost: TokenCost,
    /// Average cost per session.
    pub avg_cost_per_session: TokenCost,
    /// Average turns per session.
    pub avg_turns_per_session: f64,
}

/// Compute metrics for a single session.
///
/// This is a pure function — no I/O, no side effects. The pricing model
/// determines how token counts convert to dollar estimates.
#[must_use]
pub fn compute_metrics(session: &SessionLog, pricing: &PricingModel) -> SessionMetrics {
    let mut total_input = TokenCount::new(0);
    let mut total_output = TokenCount::new(0);
    let mut total_cache_creation = TokenCount::new(0);
    let mut total_cache_read = TokenCount::new(0);
    let mut total_cost = TokenCost::new(0.0);

    for turn in &session.turns {
        total_input = total_input + turn.input_tokens;
        total_output = total_output + turn.output_tokens;
        total_cache_creation = total_cache_creation + turn.cache_creation_input_tokens;
        total_cache_read = total_cache_read + turn.cache_read_input_tokens;
        total_cost = total_cost + pricing.cost_of_turn(turn);
    }

    let total_all_input = total_input + total_cache_creation + total_cache_read;
    let total_tokens = total_all_input + total_output;

    let turn_count = session.turns.len();

    #[allow(clippy::cast_precision_loss)]
    let avg_context_per_turn = if turn_count > 0 {
        total_all_input.get() as f64 / turn_count as f64
    } else {
        0.0
    };

    let denominator = total_cache_read.get() + total_input.get();
    #[allow(clippy::cast_precision_loss)]
    let cache_hit_ratio = if denominator > 0 {
        total_cache_read.get() as f64 / denominator as f64
    } else {
        0.0
    };

    let duration_secs = match (session.start_time, session.end_time) {
        (Some(start), Some(end)) => {
            let dur = end - start;
            #[allow(clippy::cast_precision_loss)]
            Some(dur.num_milliseconds() as f64 / 1000.0)
        }
        _ => None,
    };

    SessionMetrics {
        session_id: session.session_id.clone(),
        turn_count,
        total_input,
        total_output,
        total_cache_creation,
        total_cache_read,
        total_tokens,
        total_cost,
        avg_context_per_turn,
        cache_hit_ratio,
        duration_secs,
    }
}

/// Aggregate metrics across multiple sessions for a project.
///
/// Sessions should all belong to the same project. The `project` name
/// is passed explicitly rather than inferred from the sessions.
#[must_use]
pub fn aggregate_project(
    project: &str,
    sessions: &[SessionLog],
    pricing: &PricingModel,
) -> ProjectMetrics {
    let mut total_turns = 0usize;
    let mut total_tokens = TokenCount::new(0);
    let mut total_cost = TokenCost::new(0.0);

    for session in sessions {
        let m = compute_metrics(session, pricing);
        total_turns += m.turn_count;
        total_tokens = total_tokens + m.total_tokens;
        total_cost = total_cost + m.total_cost;
    }

    let session_count = sessions.len();

    #[allow(clippy::cast_precision_loss)]
    let avg_cost_per_session = if session_count > 0 {
        TokenCost::new(total_cost.get() / session_count as f64)
    } else {
        TokenCost::new(0.0)
    };

    #[allow(clippy::cast_precision_loss)]
    let avg_turns_per_session = if session_count > 0 {
        total_turns as f64 / session_count as f64
    } else {
        0.0
    };

    ProjectMetrics {
        project: project.to_string(),
        session_count,
        total_turns,
        total_tokens,
        total_cost,
        avg_cost_per_session,
        avg_turns_per_session,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    use crate::types::Turn;

    fn make_session(turns: Vec<Turn>) -> SessionLog {
        let start_time = turns.first().map(|t| t.timestamp);
        let end_time = turns.last().map(|t| t.timestamp);
        SessionLog {
            session_id: SessionId::new("test-session".to_string()).unwrap(),
            project: "test-project".to_string(),
            slug: None,
            start_time,
            end_time,
            turns,
            total_lines: 10,
        }
    }

    fn make_turn(index: usize, input: u64, cache_read: u64, output: u64) -> Turn {
        Turn {
            index,
            timestamp: Utc
                .with_ymd_and_hms(2026, 4, 6, 10, index as u32, 0)
                .unwrap(),
            model: Some("claude-opus-4-6".to_string()),
            input_tokens: TokenCount::new(input),
            cache_creation_input_tokens: TokenCount::new(0),
            cache_read_input_tokens: TokenCount::new(cache_read),
            output_tokens: TokenCount::new(output),
        }
    }

    #[test]
    fn test_compute_metrics_basic() {
        let session = make_session(vec![
            make_turn(0, 100, 5000, 200),
            make_turn(1, 150, 8000, 300),
        ]);
        let pricing = PricingModel::opus();
        let m = compute_metrics(&session, &pricing);

        assert_eq!(m.turn_count, 2);
        assert_eq!(m.total_input, TokenCount::new(250));
        assert_eq!(m.total_output, TokenCount::new(500));
        assert_eq!(m.total_cache_read, TokenCount::new(13000));
        // total = (250 + 0 + 13000) + 500 = 13750
        assert_eq!(m.total_tokens, TokenCount::new(13750));
    }

    #[test]
    fn test_cache_hit_ratio() {
        let session = make_session(vec![make_turn(0, 100, 900, 50)]);
        let pricing = PricingModel::opus();
        let m = compute_metrics(&session, &pricing);
        // cache_read / (cache_read + input) = 900 / (900 + 100) = 0.9
        assert!((m.cache_hit_ratio - 0.9).abs() < 0.001);
    }

    #[test]
    fn test_empty_session() {
        let session = make_session(vec![]);
        let pricing = PricingModel::opus();
        let m = compute_metrics(&session, &pricing);
        assert_eq!(m.turn_count, 0);
        assert_eq!(m.total_tokens, TokenCount::new(0));
        assert!((m.avg_context_per_turn - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_aggregate_project() {
        let s1 = make_session(vec![make_turn(0, 100, 1000, 50)]);
        let s2 = make_session(vec![
            make_turn(0, 200, 2000, 100),
            make_turn(1, 300, 3000, 150),
        ]);
        let pricing = PricingModel::opus();
        let pm = aggregate_project("test", &[s1, s2], &pricing);

        assert_eq!(pm.session_count, 2);
        assert_eq!(pm.total_turns, 3);
        assert!((pm.avg_turns_per_session - 1.5).abs() < 0.001);
    }

    #[test]
    fn test_duration_secs() {
        let session = make_session(vec![
            make_turn(0, 10, 100, 5),
            make_turn(5, 10, 100, 5), // 5 minutes later
        ]);
        let pricing = PricingModel::opus();
        let m = compute_metrics(&session, &pricing);
        // 5 minutes = 300 seconds
        assert!((m.duration_secs.unwrap() - 300.0).abs() < 0.001);
    }
}
