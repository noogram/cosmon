// SPDX-License-Identifier: AGPL-3.0-only

//! Cosmon emoji vocabulary — visual glyphs for terminal, docs, and commits.
//!
//! Each concept in the cosmon universe has a distinct emoji for instant
//! recognition in CLI output, STATUS.md, and git commit messages.

/// Cosmon framework emoji.
pub const COSMON: &str = "🧪";

/// Emoji for molecule kinds.
pub mod kind {
    /// 💡 Idea — unstructured insight.
    pub const IDEA: &str = "💡";
    /// 🔧 Task — actionable work.
    pub const TASK: &str = "🔧";
    /// 📐 Decision — architecture record.
    pub const DECISION: &str = "📐";
    /// 🐛 Issue — tracked problem.
    pub const ISSUE: &str = "🐛";
    /// ⚡ Signal — ephemeral observation.
    pub const SIGNAL: &str = "⚡";
}

/// Emoji for molecule statuses.
pub mod status {
    /// ⏳ Pending — created, no worker.
    pub const PENDING: &str = "⏳";
    /// 📋 Queued — assigned, waiting.
    pub const QUEUED: &str = "📋";
    /// ▶️ Running — actively worked on.
    pub const RUNNING: &str = "▶️";
    /// ❄️ Frozen — paused.
    pub const FROZEN: &str = "❄️";
    /// ✅ Completed — done.
    pub const COMPLETED: &str = "✅";
    /// 💥 Collapsed — failed.
    pub const COLLAPSED: &str = "💥";
}

/// Emoji for interactions.
pub mod interaction {
    /// 💫 Decay — 1 → N.
    pub const DECAY: &str = "💫";
    /// 🔀 Merge — N → 1.
    pub const MERGE: &str = "🔀";
    /// 🔄 Transform — kind change.
    pub const TRANSFORM: &str = "🔄";
}

/// Emoji for fleet operations.
pub mod fleet {
    /// 🚀 Deploy.
    pub const DEPLOY: &str = "🚀";
    /// 🛑 Teardown.
    pub const TEARDOWN: &str = "🛑";
    /// 🔁 Rolling restart.
    pub const ROLLING_RESTART: &str = "🔁";
}
