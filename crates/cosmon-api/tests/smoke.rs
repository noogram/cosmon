// SPDX-License-Identifier: AGPL-3.0-only

//! Integration smoke tests for `cs-api`.
//!
//! Each test binds a fresh `COSMON_STATE_DIR` tempdir so real
//! `~/.cosmon/state/sessions/` is never touched. The server is built
//! via `cosmon_api::router` and exercised through `reqwest` over a
//! loopback TCP listener.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use cosmon_api::{router, AppState};
use reqwest::StatusCode;
use tempfile::TempDir;

fn cs_bin() -> PathBuf {
    // The `cs` binary this crate shells out to lives alongside the
    // `cosmon-cli` integration tests. `cargo test -p cosmon-api`
    // guarantees it has been built under `target/debug/` by the time
    // the test runs because `cs-api` depends transitively on no build
    // script that rebuilds it; we rebuild it here just in case.
    let target_dir = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            // Walk up from CARGO_MANIFEST_DIR until we find `target/`.
            let mut dir: PathBuf = env!("CARGO_MANIFEST_DIR").into();
            loop {
                let cand = dir.join("target");
                if cand.exists() {
                    return cand;
                }
                if !dir.pop() {
                    return PathBuf::from("target");
                }
            }
        });
    let candidate = target_dir.join("debug").join("cs");
    if !candidate.exists() {
        let status = Command::new(env!("CARGO"))
            .args(["build", "-p", "cosmon-cli", "--bin", "cs"])
            .status()
            .expect("spawn cargo build");
        assert!(status.success(), "failed to build cs binary for tests");
    }
    candidate
}

async fn spawn_server(state_dir: &std::path::Path) -> SocketAddr {
    spawn_server_with(|s| s.with_state_dir(state_dir.to_path_buf())).await
}

async fn spawn_server_with<F>(configure: F) -> SocketAddr
where
    F: FnOnce(AppState) -> AppState,
{
    let state = configure(AppState::new(cs_bin()));
    let app = router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        axum::serve(listener, app.into_make_service())
            .await
            .expect("serve");
    });
    // Give tokio a moment to start the task.
    tokio::time::sleep(Duration::from_millis(30)).await;
    addr
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("build reqwest client")
}

#[tokio::test]
async fn healthz_returns_ok_true_and_cs_version() {
    let tmp = TempDir::new().expect("tempdir");
    let addr = spawn_server(tmp.path()).await;
    let body: serde_json::Value = client()
        .get(format!("http://{addr}/healthz"))
        .send()
        .await
        .expect("send")
        .error_for_status()
        .expect("200")
        .json()
        .await
        .expect("json");
    assert_eq!(body["ok"], serde_json::Value::Bool(true));
    assert!(body["version"].as_str().unwrap_or("").starts_with("cs "));
    assert!(!body["cs_binary"].as_str().unwrap_or("").is_empty());
}

#[tokio::test]
async fn start_twice_returns_409() {
    let tmp = TempDir::new().expect("tempdir");
    let addr = spawn_server(tmp.path()).await;
    let c = client();
    let r1 = c
        .post(format!("http://{addr}/session/start"))
        .send()
        .await
        .expect("send");
    assert_eq!(r1.status(), StatusCode::OK);
    let body: serde_json::Value = r1.json().await.expect("json");
    assert!(body["session_id"]
        .as_str()
        .unwrap_or("")
        .starts_with("session-"));

    let r2 = c
        .post(format!("http://{addr}/session/start"))
        .send()
        .await
        .expect("send");
    assert_eq!(r2.status(), StatusCode::CONFLICT);
    let body2: serde_json::Value = r2.json().await.expect("json");
    assert!(body2["error"]
        .as_str()
        .unwrap_or("")
        .contains("already open"));
}

#[tokio::test]
async fn note_without_session_returns_409() {
    let tmp = TempDir::new().expect("tempdir");
    let addr = spawn_server(tmp.path()).await;
    let resp = client()
        .post(format!("http://{addr}/session/note"))
        .json(&serde_json::json!({"text": "nope"}))
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body: serde_json::Value = resp.json().await.expect("json");
    assert!(body["error"].as_str().unwrap_or("").contains("no session"));
}

#[tokio::test]
async fn end_seals_session_with_notes() {
    let tmp = TempDir::new().expect("tempdir");
    let addr = spawn_server(tmp.path()).await;
    let c = client();

    c.post(format!("http://{addr}/session/start"))
        .send()
        .await
        .expect("start")
        .error_for_status()
        .expect("200");

    for text in ["first note", "second note"] {
        c.post(format!("http://{addr}/session/note"))
            .json(&serde_json::json!({"text": text}))
            .send()
            .await
            .expect("note")
            .error_for_status()
            .expect("200");
    }

    let end_resp = c
        .post(format!("http://{addr}/session/end"))
        .send()
        .await
        .expect("end")
        .error_for_status()
        .expect("200");
    let body: serde_json::Value = end_resp.json().await.expect("json");
    let seal = body["seal"].as_str().unwrap_or("");
    assert!(
        seal.starts_with("blake3:") && seal.len() > "blake3:".len(),
        "seal should be blake3:<hex>, got {seal}"
    );
    assert_eq!(body["note_count"].as_u64(), Some(2));
}

#[tokio::test]
async fn current_reports_open_session_notes() {
    let tmp = TempDir::new().expect("tempdir");
    let addr = spawn_server(tmp.path()).await;
    let c = client();

    // Before any session: empty.
    let before: serde_json::Value = c
        .get(format!("http://{addr}/session/current"))
        .send()
        .await
        .expect("get")
        .json()
        .await
        .expect("json");
    assert!(before["session_id"].is_null());
    assert_eq!(before["notes"].as_array().map(Vec::len), Some(0));

    c.post(format!("http://{addr}/session/start"))
        .send()
        .await
        .expect("start")
        .error_for_status()
        .expect("200");
    c.post(format!("http://{addr}/session/note"))
        .json(&serde_json::json!({"text": "hello world", "tag": "insight"}))
        .send()
        .await
        .expect("note")
        .error_for_status()
        .expect("200");

    let during: serde_json::Value = c
        .get(format!("http://{addr}/session/current"))
        .send()
        .await
        .expect("get")
        .json()
        .await
        .expect("json");
    assert!(during["session_id"]
        .as_str()
        .unwrap_or("")
        .starts_with("session-"));
    let notes = during["notes"].as_array().expect("notes array");
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0]["text"], "hello world");
    assert_eq!(notes[0]["tag"], "insight");
}

// --- /whispers, /inbox, /galaxies (v1) ------------------------------------

fn write_whisper(inbox: &std::path::Path, room: &str, id: &str, received_at: &str, body: &str) {
    let dir = inbox.join(room);
    std::fs::create_dir_all(&dir).unwrap();
    let content = format!(
        "---\n\
         event_id: \"${id}\"\n\
         sender_mxid: \"@tenant_auditor:matrix.org\"\n\
         sender_nucleon_id: \"tenant_auditor\"\n\
         room_id: \"!{room}\"\n\
         received_at: \"{received_at}\"\n\
         ---\n\n\
         {body}\n"
    );
    std::fs::write(dir.join(format!("{id}.md")), content).unwrap();
}

fn write_molecule(
    state_dir: &std::path::Path,
    fleet: &str,
    id: &str,
    status: &str,
    topic: &str,
    tags: &[&str],
    updated_at: &str,
) {
    let dir = state_dir
        .join("fleets")
        .join(fleet)
        .join("molecules")
        .join(id);
    std::fs::create_dir_all(&dir).unwrap();
    let tags_json: Vec<serde_json::Value> =
        tags.iter().map(|t| serde_json::Value::from(*t)).collect();
    let j = serde_json::json!({
        "id": id,
        "fleet_id": fleet,
        "formula_id": "task-work",
        "status": status,
        "variables": {"topic": topic},
        "tags": tags_json,
        "created_at": updated_at,
        "updated_at": updated_at,
        "assigned_worker": null,
        "total_steps": 0,
        "current_step": 0,
        "completed_steps": [],
        "links": [],
    });
    std::fs::write(dir.join("state.json"), j.to_string()).unwrap();
}

#[tokio::test]
async fn whispers_lists_files_newest_first() {
    let tmp = TempDir::new().expect("tempdir");
    let inbox = tmp.path().join("whispers/inbox");
    write_whisper(&inbox, "room-a", "1-a", "2026-04-22T10:00:00Z", "old");
    write_whisper(&inbox, "room-b", "2-b", "2026-04-22T12:00:00Z", "Salut 👋");
    write_whisper(&inbox, "room-a", "3-a", "2026-04-22T11:00:00Z", "middle");

    let addr = spawn_server_with(|s| s.with_whispers_inbox_root(inbox)).await;
    let body: serde_json::Value = client()
        .get(format!("http://{addr}/whispers"))
        .send()
        .await
        .expect("send")
        .error_for_status()
        .expect("200")
        .json()
        .await
        .expect("json");
    let arr = body["whispers"].as_array().expect("array");
    assert_eq!(arr.len(), 3);
    assert_eq!(arr[0]["id"], "2-b", "newest first");
    assert_eq!(arr[0]["body"], "Salut 👋");
    assert_eq!(arr[0]["sender_nucleon_id"], "tenant_auditor");
    assert_eq!(arr[0]["received_at"], "2026-04-22T12:00:00Z");
    assert!(arr[0]["path"].as_str().unwrap_or("").ends_with("2-b.md"));
}

#[tokio::test]
async fn whispers_respects_limit_and_empty_inbox() {
    let tmp = TempDir::new().expect("tempdir");
    let inbox = tmp.path().join("whispers/inbox");
    // Seed 3 whispers but ask for 2.
    write_whisper(&inbox, "room-a", "1-a", "2026-04-22T10:00:00Z", "a");
    write_whisper(&inbox, "room-a", "2-a", "2026-04-22T11:00:00Z", "b");
    write_whisper(&inbox, "room-a", "3-a", "2026-04-22T12:00:00Z", "c");

    let addr = spawn_server_with(|s| s.with_whispers_inbox_root(inbox.clone())).await;
    let body: serde_json::Value = client()
        .get(format!("http://{addr}/whispers?limit=2"))
        .send()
        .await
        .expect("send")
        .error_for_status()
        .expect("200")
        .json()
        .await
        .expect("json");
    assert_eq!(body["whispers"].as_array().map(Vec::len), Some(2));

    // Missing inbox → empty list, not an error.
    let empty_tmp = TempDir::new().expect("tempdir");
    let empty_inbox = empty_tmp.path().join("not-created/inbox");
    let empty_addr = spawn_server_with(|s| s.with_whispers_inbox_root(empty_inbox)).await;
    let empty: serde_json::Value = client()
        .get(format!("http://{empty_addr}/whispers"))
        .send()
        .await
        .expect("send")
        .error_for_status()
        .expect("200")
        .json()
        .await
        .expect("json");
    assert_eq!(empty["whispers"].as_array().map(Vec::len), Some(0));
}

#[tokio::test]
async fn whisper_archive_moves_file_to_archived_tree() {
    let tmp = TempDir::new().expect("tempdir");
    let inbox = tmp.path().join("whispers/inbox");
    write_whisper(
        &inbox,
        "room-a",
        "to-archive",
        "2026-04-22T10:00:00Z",
        "bye",
    );
    let original = inbox.join("room-a/to-archive.md");
    assert!(original.exists());

    let addr = spawn_server_with(|s| s.with_whispers_inbox_root(inbox.clone())).await;
    let resp = client()
        .post(format!("http://{addr}/whispers/to-archive/archive"))
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["ok"], serde_json::Value::Bool(true));
    assert_eq!(body["id"], "to-archive");

    // File has moved out of inbox.
    assert!(!original.exists(), "source should be gone");
    let archived = tmp.path().join("whispers/archived/room-a/to-archive.md");
    assert!(archived.exists(), "archived should exist at {archived:?}");

    // Missing id -> 404.
    let miss = client()
        .post(format!("http://{addr}/whispers/does-not-exist/archive"))
        .send()
        .await
        .expect("send");
    assert_eq!(miss.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn whisper_spark_nucleates_molecule() {
    let tmp = TempDir::new().expect("tempdir");
    let state_dir = tmp.path().join("state");
    let inbox = tmp.path().join("whispers/inbox");
    write_whisper(
        &inbox,
        "room-a",
        "spark-source",
        "2026-04-22T10:00:00Z",
        "fix the flaky CI",
    );
    std::fs::create_dir_all(&state_dir).unwrap();

    let addr = spawn_server_with(|s| {
        s.with_state_dir(state_dir.clone())
            .with_whispers_inbox_root(inbox.clone())
    })
    .await;

    let resp = client()
        .post(format!("http://{addr}/whispers/spark-source/spark"))
        .header("Content-Type", "application/json")
        .body("{}")
        .send()
        .await
        .expect("send");
    if !resp.status().is_success() {
        let code = resp.status();
        let body = resp.text().await.unwrap_or_default();
        panic!("unexpected {code}: {body}");
    }
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["ok"], serde_json::Value::Bool(true));
    assert_eq!(body["whisper_id"], "spark-source");
    let spark_id = body["spark"]["id"].as_str().expect("spark id").to_owned();
    assert!(spark_id.starts_with("spark-"), "got {spark_id}");

    // The spark molecule must have landed on disk under the scoped state dir.
    let mol_path = state_dir
        .join("fleets/default/molecules")
        .join(&spark_id)
        .join("state.json");
    assert!(mol_path.exists(), "missing {mol_path:?}");
}

#[tokio::test]
async fn inbox_filters_by_status() {
    let tmp = TempDir::new().expect("tempdir");
    let state_dir = tmp.path().to_path_buf();
    write_molecule(
        &state_dir,
        "default",
        "task-pending-1",
        "pending",
        "Do X",
        &["temp:hot"],
        "2026-04-22T10:00:00Z",
    );
    write_molecule(
        &state_dir,
        "default",
        "task-running-2",
        "running",
        "Do Y",
        &[],
        "2026-04-22T11:00:00Z",
    );
    write_molecule(
        &state_dir,
        "default",
        "task-done-3",
        "completed",
        "Shipped",
        &[],
        "2026-04-22T09:00:00Z",
    );

    let addr = spawn_server(&state_dir).await;
    let body: serde_json::Value = client()
        .get(format!("http://{addr}/inbox"))
        .send()
        .await
        .expect("send")
        .error_for_status()
        .expect("200")
        .json()
        .await
        .expect("json");
    let arr = body["molecules"].as_array().expect("array");
    let ids: Vec<&str> = arr.iter().map(|m| m["id"].as_str().unwrap()).collect();
    assert!(ids.contains(&"task-pending-1"));
    assert!(ids.contains(&"task-running-2"));
    assert!(
        !ids.contains(&"task-done-3"),
        "default filter excludes completed"
    );

    // status=all returns every molecule.
    let all: serde_json::Value = client()
        .get(format!("http://{addr}/inbox?status=all"))
        .send()
        .await
        .expect("send")
        .error_for_status()
        .expect("200")
        .json()
        .await
        .expect("json");
    assert_eq!(all["molecules"].as_array().map(Vec::len), Some(3));

    // First row (most recently updated) carries the expected kind + topic + tags.
    let first = &arr[0];
    assert_eq!(first["id"], "task-running-2");
    assert_eq!(first["kind"], "task");
    assert_eq!(first["status"], "running");
    assert_eq!(first["topic"], "Do Y");
    assert_eq!(first["formula"], "task-work");
}

#[tokio::test]
async fn galaxies_scans_dir_and_counts_pending() {
    let tmp = TempDir::new().expect("tempdir");
    // cosmon galaxy with 1 pending + 1 running + 1 completed
    let cosmon = tmp.path().join("cosmon").join(".cosmon/state");
    write_molecule(
        &cosmon,
        "default",
        "task-1",
        "pending",
        "A",
        &[],
        "2026-04-22T10:00:00Z",
    );
    write_molecule(
        &cosmon,
        "default",
        "task-2",
        "running",
        "B",
        &[],
        "2026-04-22T12:00:00Z",
    );
    write_molecule(
        &cosmon,
        "default",
        "task-3",
        "completed",
        "C",
        &[],
        "2026-04-22T11:00:00Z",
    );
    // workshop galaxy with nothing
    std::fs::create_dir_all(tmp.path().join("workshop/.cosmon/state")).unwrap();
    // A non-galaxy directory
    std::fs::create_dir_all(tmp.path().join("not-a-galaxy/src")).unwrap();

    let addr = spawn_server_with(|s| s.with_galaxies_root(tmp.path().to_path_buf())).await;
    let body: serde_json::Value = client()
        .get(format!("http://{addr}/galaxies"))
        .send()
        .await
        .expect("send")
        .error_for_status()
        .expect("200")
        .json()
        .await
        .expect("json");
    let arr = body["galaxies"].as_array().expect("array");
    let names: Vec<&str> = arr.iter().map(|g| g["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"cosmon"));
    assert!(names.contains(&"workshop"));
    assert!(!names.contains(&"not-a-galaxy"));

    let cosmon_row = arr.iter().find(|g| g["name"] == "cosmon").unwrap();
    assert_eq!(cosmon_row["pending_count"].as_u64(), Some(1));
    assert_eq!(cosmon_row["running_count"].as_u64(), Some(1));
    assert_eq!(
        cosmon_row["last_activity"].as_str(),
        Some("2026-04-22T12:00:00Z")
    );
}

#[tokio::test]
async fn ensemble_aggregates_workers_and_molecules_per_galaxy() {
    let tmp = TempDir::new().expect("tempdir");
    // cosmon galaxy: 1 pending + 1 running + 1 completed + 1 fleet
    let cosmon_state = tmp.path().join("cosmon").join(".cosmon/state");
    write_molecule(
        &cosmon_state,
        "default",
        "task-a",
        "pending",
        "Do X",
        &["temp:hot"],
        "2026-04-22T10:00:00Z",
    );
    write_molecule(
        &cosmon_state,
        "default",
        "task-b",
        "running",
        "Do Y",
        &[],
        "2026-04-22T12:00:00Z",
    );
    write_molecule(
        &cosmon_state,
        "default",
        "task-c",
        "completed",
        "Shipped",
        &[],
        "2026-04-22T09:00:00Z",
    );
    std::fs::write(
        cosmon_state.join("fleet.json"),
        serde_json::json!({
            "workers": {
                "ruby": {
                    "role": "implementation",
                    "desired": "running",
                    "status": "active",
                    "current_molecule": "task-b",
                    "updated_at": "2026-04-23T10:00:00Z",
                }
            }
        })
        .to_string(),
    )
    .unwrap();
    // workshop galaxy: just the .cosmon/ marker.
    std::fs::create_dir_all(tmp.path().join("workshop/.cosmon/state")).unwrap();

    let addr = spawn_server_with(|s| s.with_galaxies_root(tmp.path().to_path_buf())).await;
    let body: serde_json::Value = client()
        .get(format!("http://{addr}/ensemble"))
        .send()
        .await
        .expect("send")
        .error_for_status()
        .expect("200")
        .json()
        .await
        .expect("json");

    assert_eq!(body["scope"], "local");
    assert_eq!(body["totals"]["galaxies"], 2);
    assert_eq!(body["totals"]["workers"], 1);
    assert_eq!(body["totals"]["molecules"], 3);

    let galaxies = body["galaxies"].as_array().expect("galaxies array");
    let cosmon = galaxies
        .iter()
        .find(|g| g["name"] == "cosmon")
        .expect("cosmon present");
    assert_eq!(cosmon["worker_count"], 1);
    assert_eq!(cosmon["workers"][0]["name"], "ruby");
    assert_eq!(cosmon["workers"][0]["live"], true);
    assert_eq!(cosmon["workers"][0]["molecule_id"], "task-b");
    assert_eq!(cosmon["total_molecules"], 3);
    let groups = cosmon["molecule_groups"].as_array().expect("groups");
    assert_eq!(groups.len(), 3);
    let running = groups
        .iter()
        .find(|g| g["status"] == "running")
        .expect("running group");
    assert_eq!(running["total"], 1);
    assert_eq!(running["sample"][0]["id"], "task-b");
    assert_eq!(running["sample"][0]["galaxy"], "cosmon");
}

#[tokio::test]
async fn ensemble_allowlist_and_status_filter() {
    let tmp = TempDir::new().expect("tempdir");
    for name in ["cosmon", "mailroom", "workshop"] {
        let state = tmp.path().join(name).join(".cosmon/state");
        std::fs::create_dir_all(&state).unwrap();
        write_molecule(
            &state,
            "default",
            "task-1",
            "pending",
            "X",
            &[],
            "2026-04-22T10:00:00Z",
        );
        write_molecule(
            &state,
            "default",
            "task-2",
            "completed",
            "Y",
            &[],
            "2026-04-22T11:00:00Z",
        );
    }
    let addr = spawn_server_with(|s| s.with_galaxies_root(tmp.path().to_path_buf())).await;
    let body: serde_json::Value = client()
        .get(format!(
            "http://{addr}/ensemble?galaxies=cosmon,mailroom&statuses=pending"
        ))
        .send()
        .await
        .expect("send")
        .error_for_status()
        .expect("200")
        .json()
        .await
        .expect("json");
    let names: Vec<&str> = body["galaxies_scanned"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["cosmon", "mailroom"]);
    for g in body["galaxies"].as_array().unwrap() {
        let groups = g["molecule_groups"].as_array().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0]["status"], "pending");
    }
}

#[tokio::test]
async fn peek_returns_monospace_text_at_city_scale() {
    let tmp = TempDir::new().expect("tempdir");
    let cosmon_state = tmp.path().join("cosmon").join(".cosmon/state");
    write_molecule(
        &cosmon_state,
        "default",
        "task-running",
        "running",
        "X",
        &[],
        "2026-04-22T12:00:00Z",
    );
    write_molecule(
        &cosmon_state,
        "default",
        "task-pending",
        "pending",
        "Y",
        &[],
        "2026-04-22T11:00:00Z",
    );
    std::fs::create_dir_all(tmp.path().join("mailroom/.cosmon")).unwrap();

    let addr = spawn_server_with(|s| s.with_galaxies_root(tmp.path().to_path_buf())).await;
    let body: serde_json::Value = client()
        .get(format!("http://{addr}/peek?scale=city"))
        .send()
        .await
        .expect("send")
        .error_for_status()
        .expect("200")
        .json()
        .await
        .expect("json");
    assert_eq!(body["scale"], "city");
    let text = body["text"].as_str().expect("text");
    assert!(text.contains("CITY VIEW"));
    assert!(text.contains("cosmon"));
    assert!(text.contains("mailroom"));
}

#[tokio::test]
async fn peek_building_default_and_skin_focus() {
    let tmp = TempDir::new().expect("tempdir");
    let cosmon_state = tmp.path().join("cosmon").join(".cosmon/state");
    write_molecule(
        &cosmon_state,
        "default",
        "task-running",
        "running",
        "Ship it",
        &[],
        "2026-04-22T12:00:00Z",
    );
    let mol_dir = cosmon_state.join("fleets/default/molecules/task-running");
    std::fs::write(mol_dir.join("briefing.md"), "## step 1\n\nDo the thing.\n").unwrap();

    let addr = spawn_server_with(|s| s.with_galaxies_root(tmp.path().to_path_buf())).await;

    // default building scale
    let building: serde_json::Value = client()
        .get(format!("http://{addr}/peek"))
        .send()
        .await
        .expect("send")
        .error_for_status()
        .expect("200")
        .json()
        .await
        .expect("json");
    assert_eq!(building["scale"], "building");
    let text = building["text"].as_str().expect("text");
    assert!(text.contains("▸ cosmon"));
    assert!(text.contains("task-running"));

    // skin scale + focus
    let skin: serde_json::Value = client()
        .get(format!("http://{addr}/peek?scale=skin&focus=task-running"))
        .send()
        .await
        .expect("send")
        .error_for_status()
        .expect("200")
        .json()
        .await
        .expect("json");
    assert_eq!(skin["scale"], "skin");
    let text = skin["text"].as_str().expect("text");
    assert!(text.contains("task-running"));
    assert!(text.contains("BRIEFING.md"));
    assert!(text.contains("Do the thing"));
}

#[tokio::test]
async fn e2e_session_survives_to_disk() {
    let tmp = TempDir::new().expect("tempdir");
    let addr = spawn_server(tmp.path()).await;
    let c = client();

    c.post(format!("http://{addr}/session/start"))
        .send()
        .await
        .expect("start")
        .error_for_status()
        .expect("200");

    for (text, tag) in [
        ("one", None),
        ("two", Some("todo")),
        ("three", Some("insight")),
    ] {
        let mut body = serde_json::json!({"text": text});
        if let Some(t) = tag {
            body["tag"] = serde_json::Value::String(t.into());
        }
        c.post(format!("http://{addr}/session/note"))
            .json(&body)
            .send()
            .await
            .expect("note")
            .error_for_status()
            .expect("200");
    }

    let end: serde_json::Value = c
        .post(format!("http://{addr}/session/end"))
        .send()
        .await
        .expect("end")
        .error_for_status()
        .expect("200")
        .json()
        .await
        .expect("json");
    assert_eq!(end["note_count"].as_u64(), Some(3));

    // File on disk is sealed and carries the three notes.
    let sessions_dir = tmp.path().join("sessions");
    let mut files: Vec<_> = std::fs::read_dir(&sessions_dir)
        .expect("sessions dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    files.sort();
    let file = files.into_iter().next().expect("one session file");
    let content = std::fs::read_to_string(&file).expect("read");
    assert!(
        content.contains("seal: blake3:"),
        "file not sealed: {content}"
    );
    assert!(content.contains("## ") && content.contains("one"));
    assert!(content.contains("two"));
    assert!(content.contains("three"));
}

// --- /molecules/{id}/{tackle,tag} ----------------------------------------

#[tokio::test]
async fn tag_molecule_adds_and_removes_tags() {
    let tmp = TempDir::new().expect("tempdir");
    let state_dir = tmp.path().to_path_buf();
    write_molecule(
        &state_dir,
        "default",
        "task-20260423-aaaa",
        "pending",
        "Promote me",
        &["temp:warm"],
        "2026-04-23T10:00:00Z",
    );

    let addr = spawn_server(&state_dir).await;
    let resp = client()
        .post(format!("http://{addr}/molecules/task-20260423-aaaa/tag"))
        .header("Content-Type", "application/json")
        .body(r#"{"add":["temp:hot"],"remove":["temp:warm"]}"#)
        .send()
        .await
        .expect("send");
    if !resp.status().is_success() {
        let code = resp.status();
        let body = resp.text().await.unwrap_or_default();
        panic!("unexpected {code}: {body}");
    }
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["ok"], serde_json::Value::Bool(true));
    assert_eq!(body["id"], "task-20260423-aaaa");

    // Re-read via /inbox to verify state.json was mutated.
    let inbox: serde_json::Value = client()
        .get(format!("http://{addr}/inbox?status=all"))
        .send()
        .await
        .expect("send")
        .error_for_status()
        .expect("200")
        .json()
        .await
        .expect("json");
    let mol = inbox["molecules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == "task-20260423-aaaa")
        .expect("molecule present");
    let tags: Vec<&str> = mol["tags"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(tags.contains(&"temp:hot"));
    assert!(
        !tags.contains(&"temp:warm"),
        "temp:warm should have been removed"
    );
}

#[tokio::test]
async fn tag_molecule_rejects_empty_payload() {
    let tmp = TempDir::new().expect("tempdir");
    let state_dir = tmp.path().to_path_buf();
    write_molecule(
        &state_dir,
        "default",
        "task-tag-empty",
        "pending",
        "none",
        &[],
        "2026-04-23T10:00:00Z",
    );

    let addr = spawn_server(&state_dir).await;
    let resp = client()
        .post(format!("http://{addr}/molecules/task-tag-empty/tag"))
        .header("Content-Type", "application/json")
        .body(r#"{"add":[],"remove":[]}"#)
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// --- T1 instrumentation -------------------------------------------------

#[tokio::test]
async fn instrumentation_emits_one_event_per_call_with_correct_mode() {
    use cosmon_api::instrumentation::{read_ndjson, InvocationMode};

    // Three tempdirs so the ndjson sink sits outside the cosmon state
    // dir (otherwise `cs tag` may walk into the api-instrumentation
    // tree on its way to the worker's worktree), and so the `/ensemble`
    // cluster aggregator scans an isolated (empty) galaxies root rather
    // than the live `$HOME/galaxies` — otherwise on a machine with a
    // large fleet the directory walk exceeds the client's 10s timeout.
    let state_tmp = TempDir::new().expect("tempdir");
    let sink_tmp = TempDir::new().expect("tempdir");
    let galaxies_tmp = TempDir::new().expect("tempdir");
    let state_dir = state_tmp.path().to_path_buf();
    write_molecule(
        &state_dir,
        "default",
        "task-20260503-aaaa",
        "pending",
        "watch me",
        &["temp:warm"],
        "2026-05-03T10:00:00Z",
    );
    let ndjson_path = sink_tmp.path().join("calls.ndjson");

    let path_for_state = ndjson_path.clone();
    let state_dir_for_state = state_dir.clone();
    let galaxies_root_for_state = galaxies_tmp.path().to_path_buf();
    let addr = spawn_server_with(move |s| {
        s.with_state_dir(state_dir_for_state)
            .with_galaxies_root(galaxies_root_for_state)
            .with_instrumentation_path(path_for_state)
    })
    .await;
    let c = client();

    // Hit four routes covering the three modes:
    //   /healthz                   → SubprocessShellOut (--version)
    //   /inbox                     → InProcessStateRead  (<scan-inbox>)
    //   /ensemble                  → InProcessStateRead  (<scan-ensemble>)
    //   /molecules/{id}/tag        → InProcessStateWrite (tag) — T3
    c.get(format!("http://{addr}/healthz"))
        .send()
        .await
        .expect("healthz")
        .error_for_status()
        .expect("200");
    c.get(format!("http://{addr}/inbox"))
        .send()
        .await
        .expect("inbox")
        .error_for_status()
        .expect("200");
    c.get(format!("http://{addr}/ensemble"))
        .send()
        .await
        .expect("ensemble")
        .error_for_status()
        .expect("200");
    let tag_resp = c
        .post(format!("http://{addr}/molecules/task-20260503-aaaa/tag"))
        .header("Content-Type", "application/json")
        .body(r#"{"add":["temp:hot"]}"#)
        .send()
        .await
        .expect("tag");
    if !tag_resp.status().is_success() {
        let code = tag_resp.status();
        let body = tag_resp.text().await.unwrap_or_default();
        panic!("tag {code}: {body}");
    }

    // Give the writer a brief moment to flush — append is sync but the
    // request handler returns to the runtime before we read here.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let events = read_ndjson(&ndjson_path).expect("read ndjson");
    assert!(
        events.len() >= 4,
        "expected ≥4 events, got {}: {events:#?}",
        events.len()
    );

    let healthz = events
        .iter()
        .find(|e| e.caller == "/healthz")
        .expect("healthz event");
    assert_eq!(healthz.mode, InvocationMode::SubprocessShellOut);
    assert_eq!(healthz.verb, "--version");

    let inbox = events
        .iter()
        .find(|e| e.caller == "/inbox")
        .expect("inbox event");
    assert_eq!(inbox.mode, InvocationMode::InProcessStateRead);
    assert_eq!(inbox.verb, "<scan-inbox>");
    assert_eq!(inbox.stdout_bytes, 0, "in-process never has stdout");

    let ensemble = events
        .iter()
        .find(|e| e.caller == "/ensemble")
        .expect("ensemble event");
    assert_eq!(ensemble.mode, InvocationMode::InProcessStateRead);
    assert_eq!(ensemble.verb, "<scan-ensemble>");

    let tag = events
        .iter()
        .find(|e| e.caller == "/molecules/{id}/tag")
        .expect("tag event");
    // T3 — `task-20260503-22ca`: the route is now in-process via
    // `cosmon_state::ops::tag`. The mode flips from `SubprocessShellOut`
    // (legacy shell-out to `cs tag`) to `InProcessStateWrite`.
    assert_eq!(tag.mode, InvocationMode::InProcessStateWrite);
    assert_eq!(tag.verb, "tag");
    // In-process mode: no stdout from the recorded body.
    assert_eq!(tag.stdout_bytes, 0, "in-process never has stdout");
    // Latency is wall-clock; in-process write reads + serialises one
    // small JSON, well under any reasonable upper bound.
    assert!(
        tag.latency_ms < 30_000,
        "tag latency {} ms over 30 s",
        tag.latency_ms
    );
    // Strong post-condition: no SubprocessShellOut event landed for
    // the tag route. If a future refactor re-introduces a shell-out we
    // catch the regression here.
    let any_subprocess_tag = events
        .iter()
        .any(|e| e.caller == "/molecules/{id}/tag" && e.mode == InvocationMode::SubprocessShellOut);
    assert!(
        !any_subprocess_tag,
        "no `SubprocessShellOut` event should land for /molecules/{{id}}/tag after T3"
    );

    // Every event carries an ISO-8601 timestamp suffixed with `Z`.
    for ev in &events {
        assert!(
            ev.timestamp.ends_with('Z'),
            "non-Z timestamp: {}",
            ev.timestamp
        );
    }
}

// --- T2 library-first promotion (observe) -------------------------------
//
// Sister test to `instrumentation_emits_one_event_per_call_with_correct_mode`,
// scoped to the route promoted under T2 (`task-20260503-bbea`). Where the
// pre-extraction handler shelled out to `cs --json observe <id>` and would
// have emitted `mode = SubprocessShellOut` with `verb = "observe"`, the
// post-extraction handler now calls `cosmon_state::ops::observe` directly
// and emits `mode = InProcessStateRead` instead.
//
// This is the "syscall surface delta" the T2 rubric (`rubric-attestation.md`,
// §"Mesure additionnelle requise") asks for: the in-process variant emits
// **zero** subprocess syscalls (`clone` / `fork` / `execve` / `wait4`) on
// the observe route, where the pre-extraction variant emitted one set per
// hit. We assert the proxy here — `mode = InProcessStateRead` ↔ "no
// subprocess spawned" — and document the strace count in `before-after.md`.
#[tokio::test]
async fn observe_molecule_emits_in_process_state_read_event() {
    use cosmon_api::instrumentation::{read_ndjson, InvocationMode};

    let state_tmp = TempDir::new().expect("tempdir");
    let sink_tmp = TempDir::new().expect("tempdir");
    let state_dir = state_tmp.path().to_path_buf();
    write_molecule(
        &state_dir,
        "default",
        "task-20260503-bbea",
        "running",
        "T2 observe",
        &["temp:hot"],
        "2026-05-03T11:00:00Z",
    );
    let ndjson_path = sink_tmp.path().join("calls.ndjson");

    let path_for_state = ndjson_path.clone();
    let state_dir_for_state = state_dir.clone();
    let addr = spawn_server_with(move |s| {
        s.with_state_dir(state_dir_for_state)
            .with_instrumentation_path(path_for_state)
    })
    .await;

    // Hit the observe route.
    let resp = client()
        .get(format!("http://{addr}/molecules/task-20260503-bbea"))
        .send()
        .await
        .expect("get observe");
    assert!(
        resp.status().is_success(),
        "observe returned {}",
        resp.status()
    );
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["id"], "task-20260503-bbea");
    assert_eq!(body["status"], "running");
    assert_eq!(body["formula"], "task-work");

    // Give the writer a brief moment to flush.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let events = read_ndjson(&ndjson_path).expect("read ndjson");

    // Find the observe event by route.
    let observe_event = events
        .iter()
        .find(|e| e.caller == "/molecules/{id}")
        .expect("observe event for /molecules/{id}");

    // Library-first invariant: the route must NOT shell out anymore.
    assert_eq!(
        observe_event.mode,
        InvocationMode::InProcessStateRead,
        "observe route still shells out — extraction did not take effect",
    );
    assert_eq!(observe_event.verb, "observe");
    assert_eq!(observe_event.stdout_bytes, 0, "in-process never has stdout",);

    // Strict regression guard: no `mode = SubprocessShellOut` event was
    // emitted from the observe route during this request. A second call
    // here would catch a regression where the wrapper accidentally
    // delegates back to `cs` for a sub-step.
    let subprocess_on_observe = events
        .iter()
        .filter(|e| e.caller == "/molecules/{id}" && e.mode == InvocationMode::SubprocessShellOut)
        .count();
    assert_eq!(
        subprocess_on_observe, 0,
        "observe route emitted a SubprocessShellOut event — regression",
    );

    // T-RECTIFY (task-20260503-09c8) — observe now consumes the typed
    // `Subject` from T-SUBJECT, and the V0 trivial check emits one
    // `AuthzDecisionEvaluated` per call (T-AUTHZ-INSTR). The cs-api
    // hands `Subject::operator()` since the loopback is mono-tenant
    // until T-RPP-V0; the wire-format label is therefore `"operator"`,
    // the verb is `"observe"`, and the decision is `Allow`.
    use cosmon_state::instrumentation::{
        read_authz_ndjson, AuthzDecision, AUTHZ_NDJSON_RELATIVE_PATH,
    };
    let authz_path = state_dir.join(AUTHZ_NDJSON_RELATIVE_PATH);
    let authz_events = read_authz_ndjson(&authz_path).expect("read authz ndjson");
    let observe_authz = authz_events
        .iter()
        .find(|e| e.verb == "observe")
        .expect("expected an AuthzDecisionEvaluated event for observe");
    assert_eq!(observe_authz.subject_kind, "operator");
    assert_eq!(observe_authz.decision, AuthzDecision::Allow);
    assert!(observe_authz.scope_required.is_none());
}

#[tokio::test]
async fn tag_molecule_rejects_dangerous_ids_and_tags() {
    let tmp = TempDir::new().expect("tempdir");
    let state_dir = tmp.path().to_path_buf();
    std::fs::create_dir_all(&state_dir).unwrap();

    let addr = spawn_server(&state_dir).await;

    // Shell metachar in the id segment — 400.
    let resp = client()
        .post(format!("http://{addr}/molecules/task;rm/tag"))
        .header("Content-Type", "application/json")
        .body(r#"{"add":["temp:hot"]}"#)
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // Tag that looks like a CLI flag — 400.
    let resp = client()
        .post(format!("http://{addr}/molecules/task-safe/tag"))
        .header("Content-Type", "application/json")
        .body(r#"{"add":["--force"]}"#)
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}
