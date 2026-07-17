// SPDX-License-Identifier: AGPL-3.0-only

//! Creativity interface: advisory panel as amplifier.
//!
//! Materializes THESIS.md Part XV — the surface where human creative cognition
//! meets agent-augmented exploration. The advisory panel externalizes the
//! internal perspectives a creator holds serially into concurrent agents that
//! respond in parallel.
//!
//! # Core concepts
//!
//! - **Panel roles**: six specialized intellectual stances (Pragmatist, Critic,
//!   Visionary, Historian, Synthesizer, Measurer).
//! - **Speed tiers**: multi-speed processing matching creative cognition
//!   (Immediate, Reflective, Deep, Dormant).
//! - **Creative temperature**: inferred from interaction patterns, bridging
//!   the energy model (Part XI) with the creator's cognitive mode.
//! - **Anti-pattern guards**: structural defenses against the six failure modes.
//!
//! # Examples
//!
//! ```
//! use cosmon_core::creativity::{PanelRole, SpeedTier, CreativeTemperature};
//!
//! // Panel roles carry distinct cognitive functions:
//! let critic = PanelRole::Critic;
//! assert_eq!(critic.cognitive_function(), "Flaw detection, stress testing, adversarial review");
//!
//! // Temperature is inferred from input characteristics:
//! let temp = CreativeTemperature::infer("quick thought");
//! assert!(matches!(temp, CreativeTemperature::Brainstorm));
//!
//! // Speed tiers match response depth to question depth:
//! let tier = SpeedTier::Immediate;
//! assert!(tier.max_tokens() < SpeedTier::Deep.max_tokens());
//! ```

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::id::{MoleculeId, SessionId};

// ---------------------------------------------------------------------------
// PanelRole — six concurrent advisory perspectives
// ---------------------------------------------------------------------------

/// An advisory panel role embodying a distinct intellectual stance.
///
/// Each role provides a cognitive function the creator cannot hold
/// simultaneously with the others. The panel externalizes serial
/// perspective-switching into concurrent responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelRole {
    /// Feasibility analysis, implementation paths.
    /// Physics analogue: ground state — lowest-energy configuration.
    Pragmatist,

    /// Flaw detection, stress testing, adversarial review.
    /// Physics analogue: perturbation analysis — what breaks under small changes?
    Critic,

    /// Possibility exploration, boundary pushing.
    /// Physics analogue: high-energy excitation — what new states are reachable?
    Visionary,

    /// Prior art, pattern matching, precedent.
    /// Physics analogue: archaeology — what configurations visited before?
    Historian,

    /// Integration of conflicting perspectives.
    /// Physics analogue: cooling — finding equilibrium after competing forces.
    Synthesizer,

    /// Quantification, evidence gathering, data analysis.
    /// Physics analogue: observable — what can actually be measured?
    Measurer,
}

impl PanelRole {
    /// All panel roles in canonical order.
    pub const ALL: [Self; 6] = [
        Self::Pragmatist,
        Self::Critic,
        Self::Visionary,
        Self::Historian,
        Self::Synthesizer,
        Self::Measurer,
    ];

    /// The cognitive function this role embodies.
    #[must_use]
    pub fn cognitive_function(self) -> &'static str {
        match self {
            Self::Pragmatist => "Feasibility analysis, implementation paths",
            Self::Critic => "Flaw detection, stress testing, adversarial review",
            Self::Visionary => "Possibility exploration, boundary pushing",
            Self::Historian => "Prior art, pattern matching, precedent",
            Self::Synthesizer => "Integration of conflicting perspectives",
            Self::Measurer => "Quantification, evidence gathering, data analysis",
        }
    }

    /// The guiding question this role asks.
    #[must_use]
    pub fn question(self) -> &'static str {
        match self {
            Self::Pragmatist => "Will it work?",
            Self::Critic => "What breaks?",
            Self::Visionary => "What if we went further?",
            Self::Historian => "Has this been tried?",
            Self::Synthesizer => "How do these connect?",
            Self::Measurer => "What can we measure?",
        }
    }
}

impl fmt::Display for PanelRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pragmatist => write!(f, "PRAGMATIST"),
            Self::Critic => write!(f, "CRITIC"),
            Self::Visionary => write!(f, "VISIONARY"),
            Self::Historian => write!(f, "HISTORIAN"),
            Self::Synthesizer => write!(f, "SYNTHESIZER"),
            Self::Measurer => write!(f, "MEASURER"),
        }
    }
}

impl FromStr for PanelRole {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "pragmatist" => Ok(Self::Pragmatist),
            "critic" => Ok(Self::Critic),
            "visionary" => Ok(Self::Visionary),
            "historian" => Ok(Self::Historian),
            "synthesizer" => Ok(Self::Synthesizer),
            "measurer" => Ok(Self::Measurer),
            _ => Err(format!("unknown panel role: {s}")),
        }
    }
}

// ---------------------------------------------------------------------------
// SpeedTier — multi-speed processing
// ---------------------------------------------------------------------------

/// Processing speed tier matching creative cognition's temporal structure.
///
/// Each tier has a distinct token cost profile and latency expectation.
/// The system routes to the appropriate tier based on the creator's
/// interaction pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeedTier {
    /// Seconds — instant feedback within the latency of human thought.
    /// Panel responds with first reactions, not deep analysis.
    Immediate,

    /// Minutes — structured research with panel dispersal.
    /// Creator continues thinking while agents work.
    Reflective,

    /// Hours — thorough exploration producing a report.
    /// Creator receives a digest in a subsequent session.
    Deep,

    /// Days — idea captured but not yet ripe.
    /// Molecule in Frozen state, preserved without consuming energy.
    Dormant,
}

impl SpeedTier {
    /// Approximate maximum tokens per interaction for this tier.
    #[must_use]
    pub fn max_tokens(self) -> u32 {
        match self {
            Self::Immediate => 2_000,
            Self::Reflective => 20_000,
            Self::Deep => 200_000,
            Self::Dormant => 0,
        }
    }
}

impl fmt::Display for SpeedTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Immediate => write!(f, "immediate"),
            Self::Reflective => write!(f, "reflective"),
            Self::Deep => write!(f, "deep"),
            Self::Dormant => write!(f, "dormant"),
        }
    }
}

// ---------------------------------------------------------------------------
// CreativeTemperature — inferred cognitive mode
// ---------------------------------------------------------------------------

/// The creator's inferred cognitive temperature.
///
/// Bridges the energy model (Part XI) with the creativity interface (Part XV).
/// Temperature is inferred from interaction patterns, not set explicitly
/// (though the creator can override).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CreativeTemperature {
    /// High: brainstorming, divergent thinking, generating possibilities.
    /// Panel offers multiple alternatives, suppresses convergent language.
    Brainstorm,

    /// Medium: exploring an idea, uncertain direction.
    /// Panel balances exploration with analysis.
    Reflective,

    /// Low: evaluating, convergent thinking, deciding.
    /// Panel provides deep analysis on few options with explicit trade-offs.
    Decision,

    /// Dormant: silence — idea parked for later.
    Dormant,
}

impl CreativeTemperature {
    /// Infer temperature from raw input text characteristics.
    ///
    /// Heuristic: short inputs → brainstorm, medium → reflective,
    /// long/detailed → decision. This matches the design doc's inference rules.
    #[must_use]
    pub fn infer(input: &str) -> Self {
        let word_count = input.split_whitespace().count();
        if word_count < 10 {
            Self::Brainstorm
        } else if word_count < 50 {
            Self::Reflective
        } else {
            Self::Decision
        }
    }

    /// Number of filled diamonds in the visual indicator (0-5 scale).
    #[must_use]
    pub fn level(self) -> u8 {
        match self {
            Self::Brainstorm => 5,
            Self::Reflective => 3,
            Self::Decision => 1,
            Self::Dormant => 0,
        }
    }

    /// Render the visual temperature indicator: `◆◆◆◇◇`.
    #[must_use]
    pub fn indicator(self) -> String {
        let filled = self.level() as usize;
        let empty = 5 - filled;
        format!("{}{}", "◆".repeat(filled), "◇".repeat(empty))
    }

    /// Human-readable label for this temperature.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Brainstorm => "brainstorm",
            Self::Reflective => "reflective",
            Self::Decision => "decision",
            Self::Dormant => "dormant",
        }
    }
}

impl fmt::Display for CreativeTemperature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.indicator(), self.label())
    }
}

impl FromStr for CreativeTemperature {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "high" | "brainstorm" => Ok(Self::Brainstorm),
            "medium" | "reflective" => Ok(Self::Reflective),
            "low" | "decision" => Ok(Self::Decision),
            "dormant" => Ok(Self::Dormant),
            _ => Err(format!(
                "unknown temperature: {s} (use high, medium, low, or dormant)"
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// PanelRouting — which roles respond to a given input
// ---------------------------------------------------------------------------

/// Routing decision: which panel members should respond to this input.
///
/// Not all panel members speak on every question. The system routes input
/// to the 2-3 most relevant perspectives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PanelRouting {
    /// The panel members selected to respond.
    pub roles: Vec<PanelRole>,
    /// Why these roles were selected.
    pub rationale: String,
}

impl PanelRouting {
    /// Default routing: Pragmatist + Critic (the minimum viable panel).
    #[must_use]
    pub fn default_routing() -> Self {
        Self {
            roles: vec![PanelRole::Pragmatist, PanelRole::Critic],
            rationale: "default: feasibility + stress test".to_owned(),
        }
    }

    /// Full panel: all six roles respond.
    #[must_use]
    pub fn full_panel() -> Self {
        Self {
            roles: PanelRole::ALL.to_vec(),
            rationale: "full panel commissioned".to_owned(),
        }
    }
}

// ---------------------------------------------------------------------------
// PanelResponse — a response from a single panel member
// ---------------------------------------------------------------------------

/// A response from one advisory panel member.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PanelResponse {
    /// Which panel role produced this response.
    pub role: PanelRole,
    /// The response text.
    pub content: String,
    /// When this response was produced.
    pub timestamp: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Exchange — one turn in the creative dialogue
// ---------------------------------------------------------------------------

/// A single exchange in the creative conversation.
///
/// Each exchange captures the creator's input, the inferred temperature,
/// the panel routing, and the panel's responses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exchange {
    /// The creator's input text.
    pub input: String,
    /// Inferred (or overridden) temperature for this exchange.
    pub temperature: CreativeTemperature,
    /// Which panel members responded.
    pub routing: PanelRouting,
    /// The panel's responses.
    pub responses: Vec<PanelResponse>,
    /// When this exchange occurred.
    pub timestamp: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// AntiPattern — the six failure modes to guard against
// ---------------------------------------------------------------------------

/// The six anti-patterns the creativity interface must prevent.
///
/// Each variant names a failure mode from THESIS.md Part XV. The system
/// uses these as structural checks during panel response generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AntiPattern {
    /// All panelists agree — suspicious consensus.
    YesManPanel,
    /// Response depth mismatches question depth.
    Firehose,
    /// System converges on a single option prematurely.
    PrematureConverger,
    /// Panel responses consume too much context window.
    ContextVampire,
    /// System silently transforms creator input.
    InvisibleHand,
    /// Synthesis smooths over genuine contradictions.
    SycophhanticSynthesizer,
}

impl AntiPattern {
    /// Human-readable description of the guard mechanism.
    #[must_use]
    pub fn guard_description(self) -> &'static str {
        match self {
            Self::YesManPanel => "Critic always gets a turn. Unusual consensus is flagged.",
            Self::Firehose => {
                "Response depth matches question depth. Short question → short reaction."
            }
            Self::PrematureConverger => {
                "System never recommends a single option. Always presents tensions."
            }
            Self::ContextVampire => "Summaries with pointers. Full reasoning available on request.",
            Self::InvisibleHand => {
                "Transformations are shown: 'I interpreted this as: [X]. Correct?'"
            }
            Self::SycophhanticSynthesizer => {
                "Synthesis preserves genuine contradictions between panel members."
            }
        }
    }
}

// ---------------------------------------------------------------------------
// CreativeSession — the state of an ongoing creative conversation
// ---------------------------------------------------------------------------

/// The state of an ongoing creative conversation with the advisory panel.
///
/// A creative session is backed by a molecule (using the `mol-creative-session`
/// formula) but presents a dialogue interface rather than a step-by-step
/// workflow. The session tracks exchanges, temperature, and active panel state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreativeSession {
    /// Unique session identifier.
    pub id: SessionId,
    /// The backing molecule (if nucleated).
    pub molecule_id: Option<MoleculeId>,
    /// One-line summary inferred from the first input.
    pub title: String,
    /// Current creative temperature (may be overridden by creator).
    pub temperature: CreativeTemperature,
    /// The speed tier for the current interaction.
    pub speed_tier: SpeedTier,
    /// The conversation history.
    pub exchanges: Vec<Exchange>,
    /// Whether the session is frozen (dormant).
    pub frozen: bool,
    /// When the session was created.
    pub created_at: DateTime<Utc>,
    /// When the session was last active.
    pub updated_at: DateTime<Utc>,
}

impl CreativeSession {
    /// Create a new creative session from an initial idea.
    ///
    /// Temperature and speed tier are inferred from the input.
    #[must_use]
    pub fn new(id: SessionId, input: &str) -> Self {
        let temperature = CreativeTemperature::infer(input);
        let title = Self::infer_title(input);
        let now = Utc::now();
        Self {
            id,
            molecule_id: None,
            title,
            temperature,
            speed_tier: SpeedTier::Immediate,
            exchanges: Vec::new(),
            frozen: false,
            created_at: now,
            updated_at: now,
        }
    }

    /// Add an exchange to the conversation history.
    pub fn add_exchange(&mut self, exchange: Exchange) {
        self.updated_at = exchange.timestamp;
        self.temperature = exchange.temperature;
        self.exchanges.push(exchange);
    }

    /// Freeze this session (park for later).
    pub fn freeze(&mut self) {
        self.frozen = true;
        self.speed_tier = SpeedTier::Dormant;
        self.temperature = CreativeTemperature::Dormant;
        self.updated_at = Utc::now();
    }

    /// Thaw a frozen session.
    pub fn thaw(&mut self) {
        self.frozen = false;
        self.speed_tier = SpeedTier::Immediate;
        self.temperature = CreativeTemperature::Reflective;
        self.updated_at = Utc::now();
    }

    /// Override the creative temperature.
    pub fn set_temperature(&mut self, temperature: CreativeTemperature) {
        self.temperature = temperature;
        self.updated_at = Utc::now();
    }

    /// Number of exchanges in this conversation.
    #[must_use]
    pub fn exchange_count(&self) -> usize {
        self.exchanges.len()
    }

    /// Check if a collapse moment should be offered.
    ///
    /// Per the design: after every 3rd exchange, the system offers to
    /// prioritize open threads.
    #[must_use]
    pub fn should_offer_collapse(&self) -> bool {
        let count = self.exchange_count();
        count > 0 && count.is_multiple_of(3)
    }

    /// Infer a short title from the first input.
    fn infer_title(input: &str) -> String {
        let words: Vec<&str> = input.split_whitespace().collect();
        if words.len() <= 6 {
            words.join(" ")
        } else {
            format!("{} ...", words[..6].join(" "))
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- PanelRole --

    #[test]
    fn test_panel_role_all_has_six_members() {
        assert_eq!(PanelRole::ALL.len(), 6);
    }

    #[test]
    fn test_panel_role_display_roundtrip() {
        for role in PanelRole::ALL {
            let displayed = role.to_string();
            let parsed: PanelRole = displayed.to_lowercase().parse().unwrap();
            assert_eq!(role, parsed);
        }
    }

    #[test]
    fn test_panel_role_cognitive_function_not_empty() {
        for role in PanelRole::ALL {
            assert!(!role.cognitive_function().is_empty());
        }
    }

    #[test]
    fn test_panel_role_question_not_empty() {
        for role in PanelRole::ALL {
            assert!(!role.question().is_empty());
        }
    }

    #[test]
    fn test_panel_role_serde_roundtrip() {
        let role = PanelRole::Critic;
        let json = serde_json::to_string(&role).unwrap();
        assert_eq!(json, "\"critic\"");
        let back: PanelRole = serde_json::from_str(&json).unwrap();
        assert_eq!(role, back);
    }

    // -- SpeedTier --

    #[test]
    fn test_speed_tier_token_ordering() {
        assert!(SpeedTier::Immediate.max_tokens() < SpeedTier::Reflective.max_tokens());
        assert!(SpeedTier::Reflective.max_tokens() < SpeedTier::Deep.max_tokens());
        assert_eq!(SpeedTier::Dormant.max_tokens(), 0);
    }

    // -- CreativeTemperature --

    #[test]
    fn test_temperature_infer_short_input() {
        let temp = CreativeTemperature::infer("What about this?");
        assert_eq!(temp, CreativeTemperature::Brainstorm);
    }

    #[test]
    fn test_temperature_infer_medium_input() {
        let input = "I keep thinking about how agent sessions lose context \
                     at boundaries. What if we had a way to distill the \
                     essential state into something smaller?";
        let temp = CreativeTemperature::infer(input);
        assert_eq!(temp, CreativeTemperature::Reflective);
    }

    #[test]
    fn test_temperature_infer_long_input() {
        let words: Vec<&str> = vec!["word"; 60];
        let input = words.join(" ");
        let temp = CreativeTemperature::infer(&input);
        assert_eq!(temp, CreativeTemperature::Decision);
    }

    #[test]
    fn test_temperature_indicator_length() {
        for temp in [
            CreativeTemperature::Brainstorm,
            CreativeTemperature::Reflective,
            CreativeTemperature::Decision,
            CreativeTemperature::Dormant,
        ] {
            let indicator = temp.indicator();
            // Each diamond is 3 bytes (UTF-8), 5 diamonds total
            let diamond_count = indicator.matches('◆').count() + indicator.matches('◇').count();
            assert_eq!(
                diamond_count, 5,
                "indicator for {temp:?} has wrong diamond count"
            );
        }
    }

    #[test]
    fn test_temperature_parse_variants() {
        assert_eq!(
            "high".parse::<CreativeTemperature>().unwrap(),
            CreativeTemperature::Brainstorm
        );
        assert_eq!(
            "brainstorm".parse::<CreativeTemperature>().unwrap(),
            CreativeTemperature::Brainstorm
        );
        assert_eq!(
            "medium".parse::<CreativeTemperature>().unwrap(),
            CreativeTemperature::Reflective
        );
        assert_eq!(
            "low".parse::<CreativeTemperature>().unwrap(),
            CreativeTemperature::Decision
        );
        assert_eq!(
            "dormant".parse::<CreativeTemperature>().unwrap(),
            CreativeTemperature::Dormant
        );
        assert!("invalid".parse::<CreativeTemperature>().is_err());
    }

    // -- PanelRouting --

    #[test]
    fn test_default_routing_includes_critic() {
        let routing = PanelRouting::default_routing();
        assert!(routing.roles.contains(&PanelRole::Critic));
    }

    #[test]
    fn test_full_panel_has_all_roles() {
        let routing = PanelRouting::full_panel();
        assert_eq!(routing.roles.len(), 6);
    }

    // -- CreativeSession --

    #[test]
    fn test_session_new_infers_temperature() {
        let id = SessionId::new("test-session-1").unwrap();
        let session = CreativeSession::new(id, "Quick thought");
        assert_eq!(session.temperature, CreativeTemperature::Brainstorm);
        assert!(!session.frozen);
    }

    #[test]
    fn test_session_title_inference_short() {
        let id = SessionId::new("test-session-2").unwrap();
        let session = CreativeSession::new(id, "Context distillation");
        assert_eq!(session.title, "Context distillation");
    }

    #[test]
    fn test_session_title_inference_long() {
        let id = SessionId::new("test-session-3").unwrap();
        let session = CreativeSession::new(
            id,
            "I keep thinking about how agent sessions lose context at boundaries",
        );
        assert!(session.title.ends_with("..."));
    }

    #[test]
    fn test_session_freeze_thaw() {
        let id = SessionId::new("test-session-4").unwrap();
        let mut session = CreativeSession::new(id, "An idea");
        assert!(!session.frozen);

        session.freeze();
        assert!(session.frozen);
        assert_eq!(session.temperature, CreativeTemperature::Dormant);
        assert_eq!(session.speed_tier, SpeedTier::Dormant);

        session.thaw();
        assert!(!session.frozen);
        assert_eq!(session.temperature, CreativeTemperature::Reflective);
    }

    #[test]
    fn test_session_collapse_moment() {
        let id = SessionId::new("test-session-5").unwrap();
        let mut session = CreativeSession::new(id, "Test");

        assert!(!session.should_offer_collapse()); // 0 exchanges

        for i in 0..3 {
            session.add_exchange(Exchange {
                input: format!("exchange {i}"),
                temperature: CreativeTemperature::Reflective,
                routing: PanelRouting::default_routing(),
                responses: vec![],
                timestamp: Utc::now(),
            });
        }

        assert!(session.should_offer_collapse()); // 3 exchanges
    }

    // -- AntiPattern --

    #[test]
    fn test_anti_pattern_guard_descriptions_not_empty() {
        let patterns = [
            AntiPattern::YesManPanel,
            AntiPattern::Firehose,
            AntiPattern::PrematureConverger,
            AntiPattern::ContextVampire,
            AntiPattern::InvisibleHand,
            AntiPattern::SycophhanticSynthesizer,
        ];
        for pattern in patterns {
            assert!(!pattern.guard_description().is_empty());
        }
    }
}
