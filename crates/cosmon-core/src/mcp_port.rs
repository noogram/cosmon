// SPDX-License-Identifier: AGPL-3.0-only

//! MCP client port — hexagonal port for invoking tools on remote MCP servers.
//!
//! This module defines the [`McpClient`] trait (a hexagonal port) that
//! abstracts how Cosmon calls tools exposed by agent MCP servers. This is
//! the "inward" direction of bidirectional MCP: Cosmon reaches into an
//! agent's MCP server to discover capabilities, call tools, and read
//! resources.
//!
//! Concrete adapters (e.g. `StdioMcpClient`) live outside this crate.
//! The domain layer programs against the trait without knowing the MCP
//! transport mechanism (stdio, SSE, WebSocket).
//!
//! See ADR-COS-007 for the full bidirectional MCP design.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::id::WorkerId;

// ---------------------------------------------------------------------------
// ToolInfo — discovered tool metadata
// ---------------------------------------------------------------------------

/// Metadata about a tool exposed by a remote MCP server.
///
/// Returned by [`McpClient::list_tools`] during capability discovery.
/// The `input_schema` is the JSON Schema describing the tool's parameters,
/// exactly as declared by the remote server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolInfo {
    /// The tool's name (e.g. "context/snapshot").
    pub name: String,
    /// Human-readable description of what the tool does.
    pub description: String,
    /// JSON Schema for the tool's input parameters.
    pub input_schema: serde_json::Value,
}

// ---------------------------------------------------------------------------
// ResourceInfo — discovered resource metadata
// ---------------------------------------------------------------------------

/// Metadata about a resource exposed by a remote MCP server.
///
/// Returned by [`McpClient::list_resources`] during capability discovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceInfo {
    /// The resource URI (e.g. `cosmon://fleet`).
    pub uri: String,
    /// Human-readable name for the resource.
    pub name: String,
    /// Optional description of the resource contents.
    pub description: Option<String>,
    /// MIME type of the resource content (e.g. "application/json").
    pub mime_type: Option<String>,
}

// ---------------------------------------------------------------------------
// McpClientError
// ---------------------------------------------------------------------------

/// Errors from MCP client operations.
#[derive(Debug, thiserror::Error)]
pub enum McpClientError {
    /// The target worker's MCP server is not reachable.
    #[error("MCP server unreachable for worker {0}: {1}")]
    Unreachable(WorkerId, String),

    /// The tool was not found on the remote server.
    #[error("tool {tool} not found on worker {worker}")]
    ToolNotFound {
        /// The worker that was queried.
        worker: WorkerId,
        /// The tool name that was not found.
        tool: String,
    },

    /// The resource was not found on the remote server.
    #[error("resource {uri} not found on worker {worker}")]
    ResourceNotFound {
        /// The worker that was queried.
        worker: WorkerId,
        /// The resource URI that was not found.
        uri: String,
    },

    /// The remote tool returned an error.
    #[error("tool {tool} on worker {worker} returned error: {message}")]
    ToolError {
        /// The worker that was called.
        worker: WorkerId,
        /// The tool that failed.
        tool: String,
        /// The error message from the remote tool.
        message: String,
    },

    /// The MCP protocol exchange failed (malformed JSON-RPC, timeout, etc.).
    #[error("MCP protocol error for worker {0}: {1}")]
    ProtocolError(WorkerId, String),
}

// ---------------------------------------------------------------------------
// McpClient trait (hexagonal port)
// ---------------------------------------------------------------------------

/// Hexagonal port for invoking tools on remote MCP servers (agents).
///
/// This is the "inward" direction of bidirectional MCP: Cosmon reaches
/// into an agent's MCP server to discover capabilities, call tools,
/// and read resources. The transport mechanism (stdio, SSE, WebSocket)
/// is an implementation detail of the adapter.
///
/// # Convention-based tool discovery
///
/// Cosmon does not prescribe which tools an agent must expose. However,
/// certain tool names are conventional and enable deeper integration:
///
/// | Tool name | Purpose | Used by |
/// |-----------|---------|---------|
/// | `context/snapshot` | Measure context window state | `ContextManager` |
/// | `context/should_compact` | Evaluate compaction need | `ContextManager` |
/// | `context/compact` | Execute compaction | `ContextManager` |
///
/// Agents that expose these tools can be context-managed by Cosmon.
/// Agents that don't expose them simply can't be context-managed — no
/// error, no fallback, just a capability gap that the orchestrator
/// must handle.
pub trait McpClient {
    /// Discover tools exposed by the target agent's MCP server.
    ///
    /// # Errors
    ///
    /// Returns [`McpClientError::Unreachable`] if the agent's MCP server
    /// cannot be contacted.
    /// Returns [`McpClientError::ProtocolError`] if the response is malformed.
    fn list_tools(&self, worker: &WorkerId) -> Result<Vec<ToolInfo>, McpClientError>;

    /// Discover resources exposed by the target agent's MCP server.
    ///
    /// # Errors
    ///
    /// Returns [`McpClientError::Unreachable`] if the agent's MCP server
    /// cannot be contacted.
    /// Returns [`McpClientError::ProtocolError`] if the response is malformed.
    fn list_resources(&self, worker: &WorkerId) -> Result<Vec<ResourceInfo>, McpClientError>;

    /// Call a tool on the target agent's MCP server.
    ///
    /// # Errors
    ///
    /// Returns [`McpClientError::Unreachable`] if the agent cannot be contacted.
    /// Returns [`McpClientError::ToolNotFound`] if the tool does not exist.
    /// Returns [`McpClientError::ToolError`] if the tool returns an error.
    /// Returns [`McpClientError::ProtocolError`] on transport failures.
    fn call_tool(
        &self,
        worker: &WorkerId,
        tool: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, McpClientError>;

    /// Read a resource from the target agent's MCP server.
    ///
    /// # Errors
    ///
    /// Returns [`McpClientError::Unreachable`] if the agent cannot be contacted.
    /// Returns [`McpClientError::ResourceNotFound`] if the resource does not exist.
    /// Returns [`McpClientError::ProtocolError`] on transport failures.
    fn read_resource(
        &self,
        worker: &WorkerId,
        uri: &str,
    ) -> Result<serde_json::Value, McpClientError>;
}

impl fmt::Display for ToolInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.name, self.description)
    }
}

impl fmt::Display for ResourceInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.description {
            Some(desc) => write!(f, "{}: {}", self.uri, desc),
            None => write!(f, "{}", self.uri),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_worker() -> WorkerId {
        WorkerId::new("topaz").unwrap()
    }

    // -- ToolInfo --

    #[test]
    fn test_tool_info_serde_roundtrip() {
        let tool = ToolInfo {
            name: "context/snapshot".to_owned(),
            description: "Measure context window state".to_owned(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
        };
        let json = serde_json::to_string(&tool).unwrap();
        let back: ToolInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(tool, back);
    }

    #[test]
    fn test_tool_info_display() {
        let tool = ToolInfo {
            name: "cosmon_nucleate".to_owned(),
            description: "Create a new molecule".to_owned(),
            input_schema: serde_json::Value::Null,
        };
        assert_eq!(tool.to_string(), "cosmon_nucleate: Create a new molecule");
    }

    // -- ResourceInfo --

    #[test]
    fn test_resource_info_serde_roundtrip() {
        let resource = ResourceInfo {
            uri: "cosmon://fleet".to_owned(),
            name: "Fleet state".to_owned(),
            description: Some("Full fleet snapshot".to_owned()),
            mime_type: Some("application/json".to_owned()),
        };
        let json = serde_json::to_string(&resource).unwrap();
        let back: ResourceInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(resource, back);
    }

    #[test]
    fn test_resource_info_display_with_description() {
        let resource = ResourceInfo {
            uri: "cosmon://fleet".to_owned(),
            name: "Fleet".to_owned(),
            description: Some("Full fleet snapshot".to_owned()),
            mime_type: None,
        };
        assert_eq!(resource.to_string(), "cosmon://fleet: Full fleet snapshot");
    }

    #[test]
    fn test_resource_info_display_without_description() {
        let resource = ResourceInfo {
            uri: "cosmon://fleet".to_owned(),
            name: "Fleet".to_owned(),
            description: None,
            mime_type: None,
        };
        assert_eq!(resource.to_string(), "cosmon://fleet");
    }

    // -- McpClientError --

    #[test]
    fn test_error_unreachable_display() {
        let err = McpClientError::Unreachable(test_worker(), "connection refused".to_owned());
        assert!(err.to_string().contains("topaz"));
        assert!(err.to_string().contains("connection refused"));
    }

    #[test]
    fn test_error_tool_not_found_display() {
        let err = McpClientError::ToolNotFound {
            worker: test_worker(),
            tool: "context/snapshot".to_owned(),
        };
        assert!(err.to_string().contains("topaz"));
        assert!(err.to_string().contains("context/snapshot"));
    }

    #[test]
    fn test_error_resource_not_found_display() {
        let err = McpClientError::ResourceNotFound {
            worker: test_worker(),
            uri: "cosmon://fleet".to_owned(),
        };
        assert!(err.to_string().contains("topaz"));
        assert!(err.to_string().contains("cosmon://fleet"));
    }

    #[test]
    fn test_error_tool_error_display() {
        let err = McpClientError::ToolError {
            worker: test_worker(),
            tool: "cosmon_evolve".to_owned(),
            message: "molecule not found".to_owned(),
        };
        assert!(err.to_string().contains("topaz"));
        assert!(err.to_string().contains("cosmon_evolve"));
        assert!(err.to_string().contains("molecule not found"));
    }

    #[test]
    fn test_error_protocol_error_display() {
        let err = McpClientError::ProtocolError(test_worker(), "malformed JSON-RPC".to_owned());
        assert!(err.to_string().contains("topaz"));
        assert!(err.to_string().contains("malformed JSON-RPC"));
    }

    // -- Trait object safety --

    #[test]
    fn test_trait_is_object_safe() {
        fn _accepts_dyn(_: &dyn McpClient) {}
    }
}
