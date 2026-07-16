// SPDX-License-Identifier: AGPL-3.0-only

//! `cs fleet` — fleet template discovery and initialization.
//!
//! Provides two subcommands:
//! - `cs fleet list-templates` — list available fleet templates compiled into the binary.
//! - `cs fleet init <template>` — copy a template to `.cosmon/fleet.toml`.
//!
//! Templates are embedded at compile time via `include_str!` because the
//! `examples/` directory is absent at install time — the binary must be
//! self-contained.

use std::fs;
use std::path::{Path, PathBuf};

use cosmon_core::fleet::{
    find_agent_line_tag, DuplicateAgentAcrossFleetsDetails, FleetInclude, FleetSpec, FleetSpecError,
};

use super::Context;

/// A fleet template embedded at compile time.
struct FleetTemplate {
    /// Short identifier used on the CLI (e.g. `code-sprint`).
    name: &'static str,
    /// One-line description shown in `list-templates`.
    description: &'static str,
    /// Full TOML content of the template file.
    content: &'static str,
}

/// All fleet templates compiled into the `cs` binary.
///
/// Each entry corresponds to an `examples/*.fleet.toml` file in the
/// repository. The content is resolved at compile time so the binary
/// is self-contained — `examples/` need not be present at runtime.
const FLEET_TEMPLATES: &[FleetTemplate] = &[
    FleetTemplate {
        name: "agentic-survey",
        description: "Survey of agentic workflow designs + fleet replica generation",
        content: include_str!("../../../../examples/agentic-survey.fleet.toml"),
    },
    FleetTemplate {
        name: "code-sprint",
        description: "Code development fleet — architect, coder, reviewer, tester",
        content: include_str!("../../../../examples/code-sprint.fleet.toml"),
    },
    FleetTemplate {
        name: "cosmopedia",
        description: "Wikipedia-style knowledge production fleet",
        content: include_str!("../../../../examples/cosmopedia.fleet.toml"),
    },
    FleetTemplate {
        name: "cosmopedia-full",
        description: "Wikipedia-faithful organization for knowledge production (full)",
        content: include_str!("../../../../examples/cosmopedia-full.fleet.toml"),
    },
    FleetTemplate {
        name: "k8s-research",
        description: "Research fleet studying Kubernetes patterns for cosmon",
        content: include_str!("../../../../examples/k8s-research.fleet.toml"),
    },
];

/// Arguments for the `fleet` subcommand.
#[derive(clap::Args)]
pub struct Args {
    #[command(subcommand)]
    pub command: FleetCommand,
}

/// Fleet subcommands.
#[derive(clap::Subcommand)]
pub enum FleetCommand {
    /// List available fleet templates
    ListTemplates,
    /// Initialize a fleet from a template (copies to .cosmon/fleet.toml)
    Init(InitArgs),
    /// Resolve a fleet.toml (follow `[[fleet.include]]`) and print the flattened fleet
    Resolve(ResolveArgs),
}

/// Arguments for `cs fleet resolve`.
#[derive(clap::Args)]
pub(crate) struct ResolveArgs {
    /// Path to the master fleet.toml (default: .cosmon/fleet.toml via walk-up)
    path: Option<PathBuf>,
}

/// Arguments for `cs fleet init`.
#[derive(clap::Args)]
pub(crate) struct InitArgs {
    /// Template name (from `cs fleet list-templates`)
    template: String,

    /// Output path (default: .cosmon/fleet.toml)
    #[arg(long, short, value_name = "PATH")]
    output: Option<PathBuf>,
}

/// Execute the `fleet` command.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    match &args.command {
        FleetCommand::ListTemplates => run_list_templates(ctx),
        FleetCommand::Init(init_args) => run_init(ctx, init_args),
        FleetCommand::Resolve(resolve_args) => run_resolve(ctx, resolve_args),
    }
}

/// List all embedded fleet templates.
#[allow(clippy::unnecessary_wraps)]
fn run_list_templates(ctx: &Context) -> anyhow::Result<()> {
    if ctx.json {
        let templates: Vec<serde_json::Value> = FLEET_TEMPLATES
            .iter()
            .map(|t| {
                serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                })
            })
            .collect();
        let output = serde_json::json!({
            "command": "fleet list-templates",
            "templates": templates,
        });
        println!("{output}");
    } else {
        println!("Available fleet templates:");
        println!();
        for t in FLEET_TEMPLATES {
            println!("  {:<22} {}", t.name, t.description);
        }
        println!();
        println!("Use `cs fleet init <template>` to copy one to .cosmon/fleet.toml");
    }
    Ok(())
}

/// Copy a template to `.cosmon/fleet.toml` (or a custom path).
fn run_init(ctx: &Context, args: &InitArgs) -> anyhow::Result<()> {
    let template = FLEET_TEMPLATES
        .iter()
        .find(|t| t.name == args.template)
        .ok_or_else(|| {
            let names: Vec<&str> = FLEET_TEMPLATES.iter().map(|t| t.name).collect();
            anyhow::anyhow!(
                "unknown template '{}'. Available: {}",
                args.template,
                names.join(", ")
            )
        })?;

    let output_path = if let Some(ref path) = args.output {
        path.clone()
    } else {
        // resolve_state_dir returns .cosmon/state/ — parent is .cosmon/
        let state_dir = cosmon_filestore::resolve_state_dir(None);
        let cosmon_dir = state_dir
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or(state_dir);
        cosmon_dir.join("fleet.toml")
    };

    if output_path.exists() {
        return Err(anyhow::anyhow!(
            "{} already exists — remove it first or use --output to write elsewhere",
            output_path.display()
        ));
    }

    // Ensure parent directory exists.
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)?;
    }

    // Write the template content. Fleet templates ship with a
    // `<CHANGE_ME: ...>` placeholder for `workdir` so that copying a
    // template cannot silently inherit a path from the author's machine
    // (see the `wiki2` incident: cosmopedia-full pointed at
    // `~/knowledge/cosmopedia-full/`, which did not exist in the new
    // project and surfaced as a cryptic FileNotFound deep in a worker).
    fs::write(&output_path, template.content)?;

    // Parse back the freshly-written file to detect the placeholder
    // and warn the user loudly before they try to deploy.
    let has_placeholder = template_has_changeme_workdir(template.content);

    if ctx.json {
        let output = serde_json::json!({
            "command": "fleet init",
            "template": template.name,
            "path": output_path.display().to_string(),
            "placeholder_workdir": has_placeholder,
        });
        println!("{output}");
    } else {
        println!(
            "Initialized fleet from '{}' template → {}",
            template.name,
            output_path.display()
        );
        println!();
        println!("Next steps:");
        println!(
            "  1. Edit {} to adjust workdir and agent prompts",
            output_path.display()
        );
        println!("  2. Deploy: cs deploy {}", output_path.display());
    }

    if has_placeholder {
        eprintln!();
        eprintln!(
            "⚠ fleet template has placeholder workdir — edit {} to set a real workdir before deploying",
            output_path.display()
        );
    }

    Ok(())
}

/// Return `true` if the given fleet.toml content still contains the
/// `<CHANGE_ME: ...>` placeholder string for a top-level `workdir` key.
///
/// We deliberately do *not* parse the TOML here: we want the detection to
/// work on partially-edited or syntactically-broken files so the warning
/// still fires. Substring match on the sentinel is sufficient because
/// `<CHANGE_ME` never appears in a legitimate path.
pub(crate) fn template_has_changeme_workdir(content: &str) -> bool {
    content.contains("<CHANGE_ME")
}

// ---------------------------------------------------------------------------
// `cs fleet resolve` — load-time flattening (ADR-038)
// ---------------------------------------------------------------------------

/// Execute `cs fleet resolve`: read the master fleet.toml, walk every
/// `[[fleet.include]]`, parse each child, hard-fail on duplicate agent ids,
/// and print the flattened fleet.
///
/// This is the CLI-side I/O layer; all parsing and composition is pure and
/// lives in [`cosmon_core::fleet`].
fn run_resolve(ctx: &Context, args: &ResolveArgs) -> anyhow::Result<()> {
    let master_path = if let Some(p) = args.path.clone() {
        p
    } else {
        let state_dir = cosmon_filestore::resolve_state_dir(None);
        state_dir.parent().map_or_else(
            || PathBuf::from(".cosmon/fleet.toml"),
            |d| d.join("fleet.toml"),
        )
    };

    let flat = resolve_fleet_at(&master_path)?;

    // Emit FleetTyped for IFBDD instrumentation when the resolved fleet
    // carries an advisory `organization_type` (delib-20260509-18df §D-C).
    // Best-effort: a missing state dir or a write failure does not break
    // the resolve.
    let state_dir = cosmon_filestore::resolve_state_dir(None);
    cosmon_state::event_log::emit_fleet_typed(
        &state_dir,
        &flat.name,
        flat.organization_type.as_deref(),
    );

    if ctx.json {
        let v = fleet_to_json(&flat, &master_path);
        println!("{v}");
    } else {
        render_resolved_plain(&flat, &master_path);
    }
    Ok(())
}

/// Resolve a master fleet path into a flattened [`FleetSpec`].
///
/// Pure wrapper around [`cosmon_core::fleet::FleetSpec::compose`] that
/// supplies the filesystem I/O: each `file:` include is read from disk
/// relative to the master file's directory.
pub(crate) fn resolve_fleet_at(master_path: &Path) -> anyhow::Result<FleetSpec> {
    let master_text = fs::read_to_string(master_path)
        .map_err(|e| anyhow::anyhow!("failed to read fleet file {}: {e}", master_path.display()))?;
    let master = FleetSpec::parse(&master_text)
        .map_err(|e| anyhow::anyhow!("failed to parse {}: {e}", master_path.display()))?;

    if master.includes.is_empty() {
        return Ok(master);
    }

    let master_dir = master_path.parent().unwrap_or_else(|| Path::new("."));

    let mut children: Vec<(FleetInclude, FleetSpec, String)> =
        Vec::with_capacity(master.includes.len());
    let mut child_texts: Vec<String> = Vec::with_capacity(master.includes.len());

    for inc in &master.includes {
        if inc.scheme != "file" {
            return Err(anyhow::anyhow!(FleetSpecError::IncludeSchemeUnsupported {
                scheme: inc.scheme.clone(),
                uri: inc.source.clone(),
            }));
        }
        let child_path = master_dir.join(&inc.path);
        let child_text = fs::read_to_string(&child_path).map_err(|e| {
            anyhow::anyhow!(
                "failed to read included fleet {}: {e} (declared as `{}` in {})",
                child_path.display(),
                inc.source,
                master_path.display()
            )
        })?;
        let child = FleetSpec::parse(&child_text).map_err(|e| {
            anyhow::anyhow!(
                "failed to parse included fleet {}: {e}",
                child_path.display()
            )
        })?;
        // Tolnay's forbidden-fields guard: included children must not
        // ship their own operational knobs.
        if !child.includes.is_empty() {
            return Err(anyhow::anyhow!(
                "included fleet `{}` declares its own `[[fleet.include]]` — \
                 transitive includes are not supported in v0",
                child.name
            ));
        }
        let source_label = child_path.display().to_string();
        children.push((inc.clone(), child, source_label));
        child_texts.push(child_text);
    }

    // Compose, then enrich duplicate-agent errors with line tags read from
    // the child TOML texts.
    match FleetSpec::compose(master, children.clone()) {
        Ok(flat) => Ok(flat),
        Err(FleetSpecError::DuplicateAgentAcrossFleets(details)) => {
            let DuplicateAgentAcrossFleetsDetails {
                agent,
                fleet_a,
                source_a,
                fleet_b,
                source_b,
                ..
            } = *details;

            // Strip optional namespace prefix before searching children.
            let bare_name = agent
                .split_once(':')
                .map_or_else(|| agent.clone(), |(_, n)| n.to_string());

            let mut line_a = String::new();
            let mut line_b = String::new();
            let mut lookups: Vec<(&str, &str)> = vec![(source_a.as_str(), master_text.as_str())];
            for ((_, _, child_source), text) in children.iter().zip(child_texts.iter()) {
                lookups.push((child_source.as_str(), text.as_str()));
            }
            for (src_label, text) in &lookups {
                let tag = find_agent_line_tag(text, &bare_name);
                if !tag.is_empty() {
                    if *src_label == source_a.as_str() {
                        line_a.clone_from(&tag);
                    } else if *src_label == source_b.as_str() {
                        line_b.clone_from(&tag);
                    }
                }
            }

            Err(anyhow::anyhow!(FleetSpecError::DuplicateAgentAcrossFleets(
                Box::new(DuplicateAgentAcrossFleetsDetails {
                    agent,
                    fleet_a,
                    source_a,
                    line_a,
                    fleet_b,
                    source_b,
                    line_b,
                })
            )))
        }
        Err(other) => Err(anyhow::anyhow!(other)),
    }
}

fn fleet_to_json(fleet: &FleetSpec, master_path: &Path) -> serde_json::Value {
    let agents: Vec<serde_json::Value> = fleet
        .agents
        .iter()
        .map(|a| {
            serde_json::json!({
                "name": a.name.as_str(),
                "role": a.role.to_string(),
                "clearance": a.clearance.to_string(),
                "origin_fleet_id": a.origin_fleet_id,
                "prompt": a.prompt,
                "model": a.model,
            })
        })
        .collect();
    serde_json::json!({
        "command": "fleet resolve",
        "master_path": master_path.display().to_string(),
        "fleet": {
            "id": fleet.name,
            "schema_version": fleet.schema_version,
            "agents": agents,
        },
    })
}

fn render_resolved_plain(fleet: &FleetSpec, master_path: &Path) {
    println!("Resolved fleet: {}", fleet.name);
    println!("  master: {}", master_path.display());
    println!("  schema_version: {}", fleet.schema_version);
    println!("  agents: {}", fleet.agents.len());
    println!();
    for a in &fleet.agents {
        let origin = a.origin_fleet_id.as_deref().unwrap_or("-");
        println!(
            "  {:<24} role={:<14} clearance={:<6} origin={}",
            a.name.as_str(),
            a.role.to_string(),
            a.clearance.to_string(),
            origin
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_templates_are_non_empty_and_valid_toml() {
        for t in FLEET_TEMPLATES {
            assert!(!t.content.is_empty(), "template '{}' is empty", t.name);
            assert!(
                t.content.contains("fleet = "),
                "template '{}' missing `fleet = ` declaration",
                t.name
            );
            assert!(
                t.content.contains("[[agents]]"),
                "template '{}' has no [[agents]]",
                t.name
            );
        }
    }

    #[test]
    fn template_names_are_unique() {
        let mut names: Vec<&str> = FLEET_TEMPLATES.iter().map(|t| t.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(
            names.len(),
            FLEET_TEMPLATES.len(),
            "duplicate template names detected"
        );
    }

    #[test]
    fn list_templates_json_output() {
        let ctx = Context {
            verbose: false,
            json: true,
            config: None,
        };
        // Should not error.
        run_list_templates(&ctx).unwrap();
    }

    #[test]
    fn init_unknown_template_fails() {
        let ctx = Context {
            verbose: false,
            json: false,
            config: None,
        };
        let args = InitArgs {
            template: "nonexistent".to_string(),
            output: Some(PathBuf::from("/tmp/test-fleet-init.toml")),
        };
        let err = run_init(&ctx, &args).unwrap_err();
        assert!(err.to_string().contains("unknown template"));
    }

    #[test]
    fn init_writes_template_to_output_path() {
        let tmp = tempfile::tempdir().unwrap();
        let output = tmp.path().join("fleet.toml");

        let ctx = Context {
            verbose: false,
            json: true,
            config: None,
        };
        let args = InitArgs {
            template: "code-sprint".to_string(),
            output: Some(output.clone()),
        };

        run_init(&ctx, &args).unwrap();

        let content = fs::read_to_string(&output).unwrap();
        assert!(content.contains("fleet = \"code-sprint\""));
        assert!(content.contains("[[agents]]"));
    }

    #[test]
    fn all_bundled_templates_use_changeme_workdir_placeholder() {
        // Invariant: no bundled template may ship with a hardcoded
        // author-specific workdir. Every `workdir = ` line must use the
        // `<CHANGE_ME: ...>` placeholder so users are forced to supply
        // their own path.
        for t in FLEET_TEMPLATES {
            let has_workdir_line = t
                .content
                .lines()
                .any(|l| l.trim_start().starts_with("workdir ="));
            assert!(
                has_workdir_line,
                "template '{}' missing workdir = line",
                t.name
            );
            assert!(
                template_has_changeme_workdir(t.content),
                "template '{}' must use <CHANGE_ME: ...> placeholder for workdir",
                t.name
            );
        }
    }

    #[test]
    fn template_has_changeme_workdir_detects_placeholder() {
        let good = "workdir = \"<CHANGE_ME: path>\"\n";
        let bad = "workdir = \"/home/me/stuff\"\n";
        assert!(template_has_changeme_workdir(good));
        assert!(!template_has_changeme_workdir(bad));
    }

    #[test]
    fn init_refuses_to_overwrite_existing() {
        let tmp = tempfile::tempdir().unwrap();
        let output = tmp.path().join("fleet.toml");
        fs::write(&output, "existing").unwrap();

        let ctx = Context {
            verbose: false,
            json: false,
            config: None,
        };
        let args = InitArgs {
            template: "code-sprint".to_string(),
            output: Some(output),
        };

        let err = run_init(&ctx, &args).unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }
}
