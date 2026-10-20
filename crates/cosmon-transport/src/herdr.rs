// SPDX-License-Identifier: AGPL-3.0-only

//! Herdr protocol-22 worker hosting. Terminal identities are observation only;
//! ADR-188 supervision is the sole stop authority. No request here closes a pane,
//! replaces a tab, registers resume argv, or stops a server.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use cosmon_core::id::WorkerId;
use cosmon_core::terminal::{TerminalBackend, TerminalIdentity, TerminalUnavailable};
use cosmon_core::transport::{
    AgentDefinition, RuntimeConfig, SessionInfo, SpawnHandle, TransportBackend, TransportError,
};
use serde_json::{json, Value};

/// Protocol qualified against Herdr v0.9.5. Other generations fail closed.
pub const PROTOCOL: u64 = 22;
const MAX_REPLY: u64 = 8 * 1024 * 1024;

fn io(error: impl std::fmt::Display) -> TransportError {
    TransportError::Io(error.to_string())
}
fn unsupported() -> TransportError {
    TransportError::Unsupported(
        "Herdr terminals have no stop authority; use the ADR-188 supervisor channel".into(),
    )
}
fn field<'a>(value: &'a Value, name: &str) -> Result<&'a Value, TransportError> {
    value
        .get(name)
        .ok_or_else(|| io(format!("Herdr protocol 22 missing field `{name}`")))
}
fn string(value: &Value, name: &str) -> Result<String, TransportError> {
    field(value, name)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| io(format!("Herdr field `{name}` must be a string")))
}

/// Injectable JSON boundary, including the kernel-observed server incarnation.
pub trait HerdrClient {
    /// Send one bounded newline JSON request; errors grant no retry or rollback authority.
    ///
    /// # Errors
    /// Reports transport, malformed response and remote method errors.
    fn request(&self, method: &str, params: Value) -> Result<Value, TransportError>;
    /// Read the socket peer's PID and kernel birth fingerprint, never a label or saved PID.
    ///
    /// # Errors
    /// Refuses platforms or sockets without a trustworthy peer fingerprint.
    fn fingerprint(&self) -> Result<(u32, u64), TransportError>;
}

/// One connection per request, bounded reads and no dependency on Herdr crates.
pub struct SocketClient {
    path: String,
}
impl SocketClient {
    /// Address an already running, explicitly selected Herdr session socket.
    pub fn new(path: impl Into<String>) -> Self {
        Self { path: path.into() }
    }
    fn connect(&self) -> Result<UnixStream, TransportError> {
        let stream = UnixStream::connect(&self.path).map_err(io)?;
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .map_err(io)?;
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .map_err(io)?;
        Ok(stream)
    }
}
impl HerdrClient for SocketClient {
    fn request(&self, method: &str, params: Value) -> Result<Value, TransportError> {
        let mut stream = self.connect()?;
        serde_json::to_writer(
            &mut stream,
            &json!({"id":"cosmon", "method":method,"params":params}),
        )
        .map_err(io)?;
        stream.write_all(b"\n").map_err(io)?;
        let mut response = String::new();
        BufReader::new(stream.take(MAX_REPLY))
            .read_line(&mut response)
            .map_err(io)?;
        if !response.ends_with('\n') {
            return Err(io("Herdr reply exceeded limit or was truncated"));
        }
        let response: Value = serde_json::from_str(&response).map_err(io)?;
        if response.get("id").and_then(Value::as_str) != Some("cosmon") {
            return Err(io("Herdr reply id mismatch"));
        }
        if let Some(error) = response.get("error") {
            return Err(io(format!("Herdr {method}: {error}")));
        }
        field(&response, "result").cloned()
    }
    fn fingerprint(&self) -> Result<(u32, u64), TransportError> {
        let stream = self.connect()?;
        #[cfg(target_os = "macos")]
        let raw = nix::sys::socket::getsockopt(&stream, nix::sys::socket::sockopt::LocalPeerPid)
            .map_err(io)?;
        #[cfg(target_os = "linux")]
        let raw = nix::sys::socket::getsockopt(&stream, nix::sys::socket::sockopt::PeerCredentials)
            .map_err(io)?
            .pid();
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let raw: i32 = {
            return Err(unsupported());
        };
        let pid = u32::try_from(raw).map_err(io)?;
        let start = cosmon_process_witness::process_start_time(pid)
            .ok_or_else(|| io("Herdr peer birth fingerprint unavailable"))?;
        Ok((pid, start))
    }
}

/// Herdr host with launch-bound observations. Names are in-memory dispatch keys,
/// never identities recovered from live workspace labels.
pub struct HerdrBackend<C = SocketClient> {
    socket: String,
    client: C,
    identities: RefCell<BTreeMap<String, TerminalIdentity>>,
}
impl HerdrBackend<SocketClient> {
    /// Bind the exact JSON socket. This never installs or starts Herdr.
    pub fn new(socket: impl Into<String>) -> Self {
        let socket = socket.into();
        Self::with_client(socket.clone(), SocketClient::new(socket))
    }
}
impl<C: HerdrClient> HerdrBackend<C> {
    /// Inject a protocol peer for conformance tests without real terminal effects.
    pub fn with_client(socket: String, client: C) -> Self {
        Self {
            socket,
            client,
            identities: RefCell::new(BTreeMap::new()),
        }
    }
    /// Reconstitute only the recorded witness; never consult current configuration.
    pub fn bound(socket: String, client: C, worker: String, identity: TerminalIdentity) -> Self {
        let backend = Self::with_client(socket, client);
        backend.identities.borrow_mut().insert(worker, identity);
        backend
    }
    /// Verify protocol and required read-side fields before any creation effect.
    /// The JSON API exposes no per-method negotiation: protocol 22 is the method contract.
    ///
    /// # Errors
    /// Refuses a different protocol or malformed/missing snapshot fields.
    pub fn preflight(&self) -> Result<(), TransportError> {
        let result = self.client.request("session.snapshot", json!({}))?;
        let snapshot = field(&result, "snapshot")?;
        if field(snapshot, "protocol")?.as_u64() != Some(PROTOCOL) {
            return Err(io("Herdr protocol mismatch: require protocol 22 (v0.9.5)"));
        }
        string(snapshot, "version")?;
        let panes = field(snapshot, "panes")?
            .as_array()
            .ok_or_else(|| io("Herdr panes must be an array"))?;
        for pane in panes {
            Self::pane_fields(pane)?;
        }
        // Verify the read methods before creating any dispatch resources.
        let listing = self.client.request("pane.list", json!({}))?;
        let listed = field(&listing, "panes")?
            .as_array()
            .ok_or_else(|| io("Herdr panes must be an array"))?;
        for pane in listed {
            Self::pane_fields(pane)?;
        }
        Ok(())
    }
    fn pane_fields(pane: &Value) -> Result<(String, String, String), TransportError> {
        Ok((
            string(pane, "workspace_id")?,
            string(pane, "pane_id")?,
            string(pane, "terminal_id")?,
        ))
    }
    fn validate(&self, identity: &TerminalIdentity) -> Result<(), TransportError> {
        let TerminalIdentity::Herdr {
            socket_path,
            server_pid,
            server_start,
            workspace_id,
            pane_id,
            terminal_id,
        } = identity
        else {
            return Err(io("not a Herdr witness"));
        };
        if socket_path != &self.socket || self.client.fingerprint()? != (*server_pid, *server_start)
        {
            return Err(io("Herdr server incarnation changed"));
        }
        self.preflight()?;
        let result = self
            .client
            .request("pane.list", json!({"workspace_id":workspace_id}))?;
        let panes = field(&result, "panes")?
            .as_array()
            .ok_or_else(|| io("Herdr panes must be an array"))?;
        let mut found = 0;
        for pane in panes {
            let (workspace, id, terminal) = Self::pane_fields(pane)?;
            if workspace == *workspace_id && id == *pane_id && terminal == *terminal_id {
                found += 1;
            }
        }
        if found != 1 || self.client.fingerprint()? != (*server_pid, *server_start) {
            return Err(io("Herdr terminal incarnation changed or worker exited"));
        }
        Ok(())
    }
}
impl<C: HerdrClient> TransportBackend for HerdrBackend<C> {
    fn supports_command_sessions(&self) -> bool {
        self.preflight().is_ok()
    }
    fn spawn_command(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: &[(String, String)],
    ) -> Result<Option<TerminalIdentity>, TransportError> {
        Err(TransportError::Unsupported(
            "Herdr requires literal argv, never a shell command".into(),
        ))
    }
    fn spawn_argv(
        &self,
        session: &str,
        directory: &str,
        argv: &[String],
        environment: &[(String, String)],
    ) -> Result<Option<TerminalIdentity>, TransportError> {
        if argv.is_empty() {
            return Err(io("empty Herdr argv"));
        }
        self.preflight()?;
        let fingerprint = self.client.fingerprint()?;
        let workspace = self.client.request(
            "workspace.create",
            json!({"cwd":directory,"label":session,"focus":false}),
        )?;
        let workspace_id = string(field(&workspace, "workspace")?, "workspace_id")?;
        let env: BTreeMap<_, _> = environment.iter().cloned().collect();
        // Never tab_id: layout.apply would replace (and destroy) that tab.
        let result = self.client.request("layout.apply", json!({"workspace_id":workspace_id,"focus":false,"root":{"type":"pane","command":argv,"cwd":directory,"env":env}}))?;
        let layout = field(&result, "layout")?;
        if string(layout, "workspace_id")? != workspace_id {
            return Err(io("Herdr launch workspace mismatch; unresolved launch"));
        }
        let pane_id = string(layout, "focused_pane_id")?;
        // Protocol 22's layout response has no terminal_id. Bind its returned
        // pane to an exact read on the unchanged server; never repeat creation.
        let listing = self
            .client
            .request("pane.list", json!({"workspace_id":workspace_id}))?;
        let panes = field(&listing, "panes")?
            .as_array()
            .ok_or_else(|| io("Herdr panes must be an array"))?;
        let mut terminal_id = None;
        let mut matches = 0;
        for pane in panes {
            let (workspace, id, terminal) = Self::pane_fields(pane)?;
            if workspace == workspace_id && id == pane_id {
                terminal_id = Some(terminal);
                matches += 1;
            }
        }
        if matches != 1 {
            return Err(io(
                "Herdr launch requires exactly one matching pane; unresolved launch",
            ));
        }
        let terminal_id = terminal_id.ok_or_else(|| {
            io("Herdr launch pane unavailable; retain receipt and unresolved launch, never retry")
        })?;
        if self.client.fingerprint()? != fingerprint {
            return Err(io("Herdr server changed during launch; unresolved launch"));
        }
        let identity = TerminalIdentity::Herdr {
            socket_path: self.socket.clone(),
            server_pid: fingerprint.0,
            server_start: fingerprint.1,
            workspace_id,
            pane_id,
            terminal_id,
        };
        self.identities
            .borrow_mut()
            .insert(session.into(), identity.clone());
        Ok(Some(identity))
    }
    fn install_exit_hook(&self, _: &str, _: &str) -> Result<(), TransportError> {
        Ok(())
    }
    fn spawn(&self, _: &AgentDefinition, _: &RuntimeConfig) -> Result<SpawnHandle, TransportError> {
        Err(unsupported())
    }
    fn terminate(&self, _: &WorkerId) -> Result<(), TransportError> {
        Err(unsupported())
    }
    fn terminate_session(&self, _: &str) -> Result<(), TransportError> {
        Err(unsupported())
    }
    fn is_alive(&self, id: &WorkerId) -> Result<bool, TransportError> {
        let identities = self.identities.borrow();
        let identity = identities
            .get(id.as_str())
            .ok_or_else(|| io("no bound Herdr worker identity"))?;
        self.validate(identity).map(|()| true)
    }
    fn send_input(&self, _: &WorkerId, _: &str) -> Result<(), TransportError> {
        Err(unsupported())
    }
    fn capture_output(&self, id: &WorkerId, lines: usize) -> Result<String, TransportError> {
        let identities = self.identities.borrow();
        let identity = identities
            .get(id.as_str())
            .ok_or_else(|| io("no bound Herdr worker identity"))?;
        self.validate(identity)?;
        let TerminalIdentity::Herdr { pane_id, .. } = identity else {
            return Err(unsupported());
        };
        let result = self.client.request("pane.read", json!({"pane_id":pane_id,"source":"visible","format":"text","lines":lines,"strip_ansi":true}))?;
        let text = string(field(&result, "read")?, "text")?;
        self.validate(identity)?;
        Ok(text)
    }
    fn list_sessions(&self) -> Result<Vec<SessionInfo>, TransportError> {
        let mut sessions = Vec::new();
        for (name, identity) in self.identities.borrow().iter() {
            if self.validate(identity).is_ok() {
                sessions.push(SessionInfo {
                    worker_id: WorkerId::new(name).map_err(io)?,
                    session_name: name.clone(),
                });
            }
        }
        Ok(sessions)
    }
    fn session_exists(&self, session: &str) -> Result<bool, TransportError> {
        Ok(self.identities.borrow().contains_key(session))
    }
    fn graceful_exit(&self, _: &WorkerId, _: Duration) -> Result<bool, TransportError> {
        Err(unsupported())
    }
}
impl<C: HerdrClient> TerminalBackend for HerdrBackend<C> {
    fn validate_terminal(&self, identity: &TerminalIdentity) -> Result<(), TerminalUnavailable> {
        self.validate(identity)
            .map_err(|_| TerminalUnavailable::IdentityChanged)
    }
    fn attach_read_only(&self, _: &TerminalIdentity) -> Result<(), TerminalUnavailable> {
        Err(TerminalUnavailable::BackendUnavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    #[derive(Clone)]
    struct Fake {
        calls: Rc<RefCell<Vec<(String, Value)>>>,
        protocol: Rc<RefCell<u64>>,
        fingerprint: Rc<RefCell<(u32, u64)>>,
        terminal: Rc<RefCell<Option<String>>>,
        omit: Rc<RefCell<bool>>,
    }
    impl Fake {
        fn new() -> Self {
            Self {
                calls: Default::default(),
                protocol: Rc::new(RefCell::new(22)),
                fingerprint: Rc::new(RefCell::new((42, 100))),
                terminal: Rc::new(RefCell::new(Some("A".into()))),
                omit: Default::default(),
            }
        }
        fn pane(&self) -> Value {
            let mut pane = json!({"workspace_id":"w1","pane_id":"w1:p2","terminal_id":self.terminal.borrow().as_deref().unwrap_or("")});
            if *self.omit.borrow() {
                pane.as_object_mut()
                    .map(|object| object.remove("terminal_id"));
            }
            pane
        }
        fn panes(&self) -> Value {
            if self.terminal.borrow().is_some() {
                json!([self.pane()])
            } else {
                json!([])
            }
        }
    }
    impl HerdrClient for Fake {
        fn request(&self, method: &str, params: Value) -> Result<Value, TransportError> {
            self.calls.borrow_mut().push((method.into(), params));
            match method {
                "session.snapshot" => Ok(
                    json!({"snapshot":{"protocol":*self.protocol.borrow(),"version":"0.9.5","panes":self.panes()}}),
                ),
                "workspace.create" => Ok(json!({"workspace":{"workspace_id":"w1"}})),
                "layout.apply" => {
                    Ok(json!({"layout":{"workspace_id":"w1","focused_pane_id":"w1:p2"}}))
                }
                "pane.list" => Ok(json!({"panes":self.panes()})),
                "pane.read" => Ok(json!({"read":{"text":"OpenAI Codex\nready"}})),
                // This is intentionally accepted: an unsafe mutation must be
                // caught by the no-close assertion, not a fake rejecting it.
                "pane.close" => Ok(json!({})),
                _ => Err(io("missing method")),
            }
        }
        fn fingerprint(&self) -> Result<(u32, u64), TransportError> {
            Ok(*self.fingerprint.borrow())
        }
    }
    fn launch(fake: Fake) -> Result<(HerdrBackend<Fake>, TerminalIdentity), TransportError> {
        let backend = HerdrBackend::with_client("/tmp/private.sock".into(), fake);
        let identity = backend
            .spawn_argv(
                "same-name",
                "/tmp",
                &["worker".into(), "literal ; $HOME ' argument".into()],
                &[("LITERAL".into(), "$HOME;".into())],
            )?
            .ok_or_else(|| io("no witness"))?;
        Ok((backend, identity))
    }
    #[test]
    fn used_protocol_projection_names_only_qualified_methods(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let schema: Value = serde_json::from_str(include_str!("herdr-protocol-22.json"))?;
        assert_eq!(schema["protocol"], 22);
        let fake = Fake::new();
        let (_backend, _identity) = launch(fake.clone())?;
        for (method, _) in fake.calls.borrow().iter() {
            assert!(
                schema["schemas"]["request"]["oneOf"]
                    .as_array()
                    .ok_or("missing methods")?
                    .iter()
                    .any(|entry| entry["properties"]["method"]["const"] == *method),
                "unvendored method: {method}"
            );
        }
        Ok(())
    }
    #[test]
    fn stop_a_leaves_same_name_b_intact_and_records_zero_close_calls(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let fake = Fake::new();
        let (backend, identity) = launch(fake.clone())?;
        *fake.terminal.borrow_mut() = Some("B".into());
        let before = fake.terminal.borrow().clone();
        let calls_before = fake.calls.borrow().len();
        let worker = WorkerId::new("same-name")?;
        assert!(matches!(
            backend.terminate(&worker),
            Err(TransportError::Unsupported(_))
        ));
        assert!(matches!(
            backend.terminate_session("same-name"),
            Err(TransportError::Unsupported(_))
        ));
        assert!(backend.graceful_exit(&worker, Duration::ZERO).is_err());
        assert_eq!(
            fake.calls.borrow().len(),
            calls_before,
            "stop must make zero Herdr calls"
        );
        assert_eq!(*fake.terminal.borrow(), before);
        assert_eq!(
            fake.request("pane.list", json!({}))?["panes"][0]["terminal_id"],
            "B"
        );
        assert!(backend.validate_terminal(&identity).is_err());
        for (method, params) in fake.calls.borrow().iter() {
            assert!(![
                "pane.close",
                "tab.close",
                "workspace.close",
                "server.stop",
                "pane.report_agent",
                "pane.report_agent_session"
            ]
            .contains(&method.as_str()));
            if method == "layout.apply" {
                assert!(params.get("tab_id").is_none());
            }
        }
        Ok(())
    }
    #[test]
    fn protocol_or_missing_read_fields_refuse_before_creation() {
        for missing in [false, true] {
            let fake = Fake::new();
            if missing {
                *fake.omit.borrow_mut() = true;
            } else {
                *fake.protocol.borrow_mut() = 23;
            }
            assert!(launch(fake.clone()).is_err());
            assert_eq!(fake.calls.borrow().len(), 1);
            assert_eq!(fake.calls.borrow()[0].0, "session.snapshot");
        }
    }
    #[test]
    fn literal_argv_and_environment_never_pass_through_a_shell(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let fake = Fake::new();
        let (_backend, _identity) = launch(fake.clone())?;
        let calls = fake.calls.borrow();
        let (_, params) = calls
            .iter()
            .find(|(method, _)| method == "layout.apply")
            .ok_or("missing launch")?;
        assert_eq!(params["workspace_id"], "w1");
        assert_eq!(params["focus"], false);
        assert_eq!(
            params["root"]["command"],
            json!(["worker", "literal ; $HOME ' argument"])
        );
        assert_eq!(params["root"]["cwd"], "/tmp");
        assert_eq!(params["root"]["env"]["LITERAL"], "$HOME;");
        assert!(!serde_json::to_string(params)?.contains("resume_argv"));
        Ok(())
    }
    #[test]
    fn server_restart_or_missing_terminal_never_adopts_or_replays(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let fake = Fake::new();
        let (backend, identity) = launch(fake.clone())?;
        *fake.fingerprint.borrow_mut() = (42, 101);
        *fake.terminal.borrow_mut() = Some("restored-shell".into());
        assert!(backend.validate_terminal(&identity).is_err());
        assert!(backend.is_alive(&WorkerId::new("same-name")?).is_err());
        assert!(backend.terminate_session("same-name").is_err());
        assert_eq!(fake.terminal.borrow().as_deref(), Some("restored-shell"));
        assert_eq!(
            fake.calls
                .borrow()
                .iter()
                .filter(|(m, _)| m == "layout.apply")
                .count(),
            1
        );
        Ok(())
    }
}
