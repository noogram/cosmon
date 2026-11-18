// SPDX-License-Identifier: AGPL-3.0-only

//! Local custody of pilot checkpoints through the shipped CLI: size and
//! evidence-count bounds, idempotent publication, and refusal of client paths
//! as evidence (`docs/specs/cross-machine-collaboration.md` §4, S-CKPUT and
//! S-CKGET).

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Output};

const MISSION: &str = "task-20261117-c0de";

struct Galaxy {
    dir: tempfile::TempDir,
}

impl Galaxy {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("isolated state directory"),
        }
    }

    fn cs(&self, args: &[&str]) -> Output {
        Command::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../scripts/no-pilot-env.sh"
        ))
        .arg("env")
        .arg("COSMON_SESSION_ID=custody-pilot")
        .arg(env!("CARGO_BIN_EXE_cs"))
        .arg("--config")
        .arg(self.dir.path())
        .arg("sessions")
        .args(args)
        .current_dir(self.dir.path())
        .output()
        .expect("run cs")
    }

    fn publish(&self, id: &str, extra: &[&str]) -> Output {
        let mut args = vec!["checkpoint", "publish", "--mission", MISSION, "--id", id];
        args.extend_from_slice(extra);
        self.cs(&args)
    }

    fn files(&self) -> BTreeMap<String, Vec<u8>> {
        fn visit(root: &Path, path: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
            let Ok(entries) = std::fs::read_dir(path) else {
                return;
            };
            for entry in entries {
                let path = entry.expect("entry").path();
                if path.is_dir() {
                    visit(root, &path, out);
                } else {
                    out.insert(
                        path.strip_prefix(root)
                            .expect("relative")
                            .display()
                            .to_string(),
                        std::fs::read(&path).expect("bytes"),
                    );
                }
            }
        }
        let mut files = BTreeMap::new();
        visit(self.dir.path(), self.dir.path(), &mut files);
        files
    }

    fn record_path(&self, id: &str) -> std::path::PathBuf {
        self.dir
            .path()
            .join("pilot/checkpoints")
            .join(MISSION)
            .join(format!("{id}.json"))
    }
}

fn refused(output: &Output) -> String {
    assert!(!output.status.success(), "expected a refusal: {output:?}");
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn success(output: &Output) -> String {
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout.clone()).expect("UTF-8 output")
}

#[test]
fn republishing_the_same_key_and_content_succeeds_without_writing() {
    let g = Galaxy::new();
    let args = ["--hypothesis", "cursor=byte offsets", "--risk", "none"];
    success(&g.publish("cp-idem", &args));
    let before = g.files();
    assert_eq!(
        before.len(),
        1,
        "one record is written: {:?}",
        before.keys()
    );

    let again = success(&g.publish("cp-idem", &args));
    assert!(again.contains("already published"), "{again}");
    assert_eq!(before, g.files(), "a retry must leave every byte unchanged");
}

#[test]
fn republishing_the_same_key_with_different_content_is_refused_and_keeps_the_record() {
    let g = Galaxy::new();
    success(&g.publish("cp-collide", &["--hypothesis", "cursor=byte offsets"]));
    let before = g.files();

    let error = refused(&g.publish("cp-collide", &["--hypothesis", "cursor=line numbers"]));
    assert!(error.contains("cp-collide"), "{error}");
    assert_eq!(before, g.files(), "the first record stays as published");
}

#[test]
fn a_hundred_evidence_references_are_accepted_and_a_hundred_and_one_are_refused() {
    let g = Galaxy::new();
    let digest = "a".repeat(64);
    let refs = |n: usize| -> Vec<String> {
        (0..n)
            .flat_map(|i| {
                [
                    "--checkpoint-evidence".to_owned(),
                    format!("artifact-{i}#{digest}"),
                ]
            })
            .collect()
    };
    let hundred = refs(100);
    let hundred: Vec<&str> = hundred.iter().map(String::as_str).collect();
    success(&g.publish("cp-100", &hundred));

    let before = g.files();
    let over = refs(101);
    let over: Vec<&str> = over.iter().map(String::as_str).collect();
    let error = refused(&g.publish("cp-101", &over));
    assert!(error.contains("100"), "the limit is named: {error}");
    assert_eq!(before, g.files(), "a refused checkpoint writes nothing");
}

#[test]
fn evidence_on_claims_counts_toward_the_same_hundred() {
    let g = Galaxy::new();
    let digest = "b".repeat(64);
    let mut args: Vec<String> = vec!["--hypothesis".into(), "cursor=byte offsets".into()];
    for i in 0..101 {
        args.push("--evidence".into());
        args.push(format!("cursor=artifact-{i}#{digest}"));
    }
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let error = refused(&g.publish("cp-claims", &args));
    assert!(error.contains("100"), "{error}");
    assert!(g.files().is_empty(), "nothing is written");
}

#[test]
fn a_checkpoint_over_262144_bytes_is_refused_and_one_within_it_is_kept() {
    let g = Galaxy::new();
    // Linux caps ONE argument at 128 KiB (MAX_ARG_STRLEN), so a large record
    // is built from several `--risk` flags of 100 000 bytes each, never one.
    let chunk = "x".repeat(100_000);
    success(&g.publish("cp-small", &["--risk", &chunk, "--risk", &chunk]));

    let before = g.files();
    let error = refused(&g.publish(
        "cp-big",
        &["--risk", &chunk, "--risk", &chunk, "--risk", &chunk],
    ));
    assert!(error.contains("262144"), "the limit is named: {error}");
    assert_eq!(before, g.files(), "a refused checkpoint writes nothing");
}

#[test]
fn showing_a_stored_record_over_262144_bytes_is_refused_before_it_is_read() {
    let g = Galaxy::new();
    success(&g.publish("cp-grown", &["--risk", "small"]));
    let path = g.record_path("cp-grown");
    let mut record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).expect("record")).expect("json");
    record["open_risks"] = serde_json::json!(["y".repeat(262_144)]);
    std::fs::write(&path, serde_json::to_vec(&record).expect("bytes")).expect("grow record");

    let error = refused(&g.cs(&[
        "checkpoint",
        "show",
        "--mission",
        MISSION,
        "--id",
        "cp-grown",
    ]));
    assert!(error.contains("262144"), "the limit is named: {error}");
}

#[test]
fn evidence_naming_a_client_path_is_refused_and_a_digest_bound_reference_is_kept() {
    let g = Galaxy::new();
    let digest = "c".repeat(64);
    for locator in [
        "/Users/someone/notes.md",
        "~/notes.md",
        "../outside.md",
        "docs/../../outside.md",
        "file:///etc/hosts",
        "C:\\Users\\someone\\notes.md",
    ] {
        let before = g.files();
        let error = refused(&g.publish(
            "cp-path",
            &["--checkpoint-evidence", &format!("{locator}#{digest}")],
        ));
        assert!(error.contains(locator), "{locator}: {error}");
        assert_eq!(before, g.files(), "{locator}: nothing is written");
    }
    success(&g.publish(
        "cp-ref",
        &["--checkpoint-evidence", &format!("artifact-7#{digest}")],
    ));
}
