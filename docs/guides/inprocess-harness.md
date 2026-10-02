# In-process harness: tool-shell environment

Direct-API adapters run the agent loop inside the `cs tackle` process. The
`exec_command` tool starts a persistent `bash` for the model. This page records
what that shell can see.

## Environment

The shell does not inherit the harness process's environment. It is spawned
with `env_clear` and receives only the set built by
`crates/cosmon-agent-harness/src/tools/shell_environment.rs`, on first spawn
and on every respawn after the shell died or timed out. The parent environment
is not re-imported on respawn, so exports made inside a dead shell are gone.

| Group | Variables | Reason |
|---|---|---|
| Fixed | `PS1`, `PS2`, `HISTFILE`, `TERM` | Keep the sentinel protocol deterministic. |
| Toolchain, locale | `PATH`, `HOME`, `USER`, `LOGNAME`, `LANG`, `LC_ALL`, `LC_CTYPE`, `TMPDIR`, `CARGO_HOME`, `RUSTUP_HOME`, `RUSTUP_TOOLCHAIN` | Commands need to find tools. Copied by exact name when set. |
| Lifecycle | `COSMON_MOL_DIR`, `COSMON_PARENT_MOL_ID`, `CB_DEPTH`, `CB_SESSION_ROLE`, `COSMON_ARTIFACT_DIR`, `COSMON_EGRESS_REQUIRE_NETNS`, `COSMON_API_REQUEST` | The worker's `cs` resolves its own molecule and the spawn-depth guard still applies. |
| Egress | `COSMON_EGRESS_POLICY` | Emitted as the resolved policy token. An unset or corrupt parent value reaches the shell as `deny-external`. |

Everything else is absent: provider credentials under any name (the configured
variable name is never consulted), shell-startup variables such as `BASH_ENV`,
loader injection variables, agent-socket variables, and the Claude account and
model pins. The list is an allowlist, so a variable nobody has thought of is
excluded by default.

## What this does not change

The shell runs as the same user with the same filesystem access. It can read
files a credential lives in, and `cd /` works. Reducing the environment removes
the credentials the process holds in memory from the shell's reach; it is not
isolation. The only enforced boundary is the egress network namespace, which
blocks the wire, not file reads. See the module documentation of
`exec_command` for the full statement.

## Test

`crates/cosmon-cli/tests/harness_shell_environment.rs` seeds a default-named
credential variable, a custom-named one and `BASH_ENV` in the parent, then
probes by name (never by value) from the tool shell, before and after a
respawn.
