// SPDX-License-Identifier: AGPL-3.0-only

//! `cs collaboration` — host-local provisioning, inspection and revocation of
//! collaboration bindings (cross-machine collaboration contract §2, issue
//! #147 W2).
//!
//! A binding ties one exact token identity to one work seat or pilot mission
//! through one attachment with its own proof. The command is the only writer
//! of those records; no network route can create, widen or revoke one. It
//! grants collaboration access only — no spawn, harvest, lease or operator
//! right — and mounts no route.

use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context as _};
use chrono::Utc;
use cosmon_core::advisory_attempt::AdvisorySeatId;
use cosmon_core::collaboration::{
    AdmittedIdentity, BindingId, BindingSpec, CollaborationBinding, CollaborationCapability,
    CollaborationScope,
};
use cosmon_core::id::MoleculeId;
use cosmon_state::collaboration::CollaborationBindingStore;

use super::Context;

/// Long help for `cs collaboration`.
pub const LONG_ABOUT: &str = "\
Provision, inspect and revoke collaboration bindings on the authoritative host.

A binding lets one exact identity (issuer, subject, audience, tenant) act as one
seat of a declared work, or as one pilot attachment of a mission, from another
machine. Each binding has its own attachment ID and a private proof. The proof
is written once to the file named by --proof-out and never stored on the host;
hand it to the client, which keeps it in its credential store.

Bindings grant only the collaboration scopes named with --scope. They confer no
spawn, harvest, lease or operator right, and a request can never change its
own binding. Revocation takes effect at the next admission and at the commit of
any write already admitted. No network route uses these bindings yet.";

/// Host-local collaboration binding administration.
#[derive(clap::Args)]
pub struct Args {
    /// Binding action to perform.
    #[command(subcommand)]
    pub action: Action,
}

/// Binding actions. Parsed once per invocation, so the size of `Bind`
/// costs nothing worth a box.
#[allow(clippy::large_enum_variant)]
#[derive(clap::Subcommand)]
pub enum Action {
    /// Provision a binding and write its attachment proof to a new private file.
    Bind {
        /// Exact token issuer.
        #[arg(long)]
        issuer: String,
        /// Exact token subject.
        #[arg(long)]
        subject: String,
        /// Exact token audience.
        #[arg(long)]
        audience: String,
        /// Tenant the identity is bound to.
        #[arg(long)]
        tenant: String,
        /// Owning molecule of the declared work (requires --seat).
        #[arg(long, requires = "seat", conflicts_with = "mission")]
        work_owner: Option<String>,
        /// Work seat the attachment acts as.
        #[arg(long, requires = "work_owner")]
        seat: Option<String>,
        /// Mission whose pilot surface the attachment joins.
        #[arg(long, conflicts_with = "work_owner")]
        mission: Option<String>,
        /// Granted scope; repeat for several (cosmon:work:read, cosmon:work:write,
        /// cosmon:sessions:read, cosmon:sessions:write).
        #[arg(long = "scope", required = true)]
        scopes: Vec<String>,
        /// New file that receives the attachment proof (created 0600, never overwritten).
        #[arg(long)]
        proof_out: PathBuf,
        /// Display label for the client machine; never used for admission.
        #[arg(long)]
        label: Option<String>,
    },
    /// List every binding, active and revoked, without verifiers or proofs.
    List,
    /// Show one binding.
    Show {
        /// Binding ID (cb-…).
        id: String,
    },
    /// Revoke one binding; earlier admissions fail at their commit.
    Revoke {
        /// Binding ID (cb-…).
        id: String,
    },
}

/// Dispatch one binding action.
///
/// # Errors
/// Returns an error for invalid input, an existing proof file or a store
/// failure.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let store = CollaborationBindingStore::new(ctx.state_dir());
    match &args.action {
        Action::Bind {
            issuer,
            subject,
            audience,
            tenant,
            work_owner,
            seat,
            mission,
            scopes,
            proof_out,
            label,
        } => {
            let capability = match (work_owner, seat, mission) {
                (Some(owner), Some(seat), None) => CollaborationCapability::WorkSeat {
                    owner: MoleculeId::new(owner.as_str())?,
                    seat: AdvisorySeatId::new(seat.as_str())?,
                },
                (None, None, Some(mission)) => CollaborationCapability::PilotMission {
                    mission: MoleculeId::new(mission.as_str())?,
                },
                _ => bail!("name either --work-owner with --seat, or --mission"),
            };
            let scopes = scopes
                .iter()
                .map(|scope| CollaborationScope::parse(scope))
                .collect::<Result<BTreeSet<_>, _>>()?;
            let spec = BindingSpec {
                identity: AdmittedIdentity::new(
                    issuer.as_str(),
                    subject.as_str(),
                    audience.as_str(),
                    tenant.as_str(),
                )?,
                capability,
                scopes,
                label: label.clone(),
            };
            // Reserve the proof file before provisioning, so a binding never
            // exists whose proof could not be handed over.
            let mut file = create_private(proof_out)?;
            let (binding, proof) = match store.provision(spec, Utc::now()) {
                Ok(provisioned) => provisioned,
                Err(error) => {
                    drop(file);
                    let _ = std::fs::remove_file(proof_out);
                    return Err(error.into());
                }
            };
            file.write_all(proof.expose_secret().as_bytes())
                .and_then(|()| file.write_all(b"\n"))
                .and_then(|()| file.sync_all())
                .with_context(|| format!("write the proof to {}", proof_out.display()))?;
            print_binding(&binding);
        }
        Action::List => {
            for binding in store.reader().load()?.iter() {
                print_binding(binding);
            }
        }
        Action::Show { id } => {
            let id = BindingId::new(id.as_str())?;
            let set = store.reader().load()?;
            let Some(binding) = set.iter().find(|binding| binding.id == id) else {
                bail!("no collaboration binding {id}");
            };
            print_binding(binding);
        }
        Action::Revoke { id } => {
            let revoked = store.revoke(&BindingId::new(id.as_str())?, Utc::now())?;
            print_binding(&revoked);
        }
    }
    Ok(())
}

fn create_private(path: &Path) -> anyhow::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).with_context(|| {
        format!(
            "create the proof file {} (it must not exist)",
            path.display()
        )
    })
}

/// One JSON line per binding. The verifier is omitted: it is host custody,
/// not information the operator needs.
fn print_binding(binding: &CollaborationBinding) {
    println!(
        "{}",
        serde_json::json!({
            "id": binding.id,
            "attachment": binding.attachment,
            "issuer": binding.identity.issuer(),
            "subject": binding.identity.subject(),
            "audience": binding.identity.audience(),
            "tenant": binding.identity.tenant(),
            "capability": binding.capability,
            "scopes": binding.scopes,
            "revision": binding.revision,
            "label": binding.label,
            "created_at": binding.created_at,
            "revoked_at": binding.revoked_at,
            "active": binding.is_active(),
        })
    );
}
