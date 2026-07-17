// SPDX-License-Identifier: AGPL-3.0-only

//! T1 mini-loadtest — runs cs-api against the live `/srv/cosmon/cosmon`
//! state and captures `EngineCallEntered` events for the
//! `observations.md` mini-rapport.
//!
//! This test is gated behind `#[ignore]` because it depends on live
//! state and is intended to be run manually:
//!
//!     cargo test -p cosmon-api --test loadtest -- --ignored \
//!         --nocapture mini_loadtest_against_live_state
//!
//! The output NDJSON is left on disk at `$NDJSON_OUT` (default
//! `/tmp/cosmon-api-instr-loadtest.ndjson`). The test prints a tabular
//! summary; copy it into `observations.md`.
#![cfg(test)]

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use cosmon_api::instrumentation::{read_ndjson, EngineCallEntered, InvocationMode};
use cosmon_api::{router, AppState};

fn cs_bin() -> PathBuf {
    let target_dir = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
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
        assert!(status.success(), "failed to build cs binary");
    }
    candidate
}

async fn spawn_server(state: AppState) -> SocketAddr {
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
    tokio::time::sleep(Duration::from_millis(50)).await;
    addr
}

#[tokio::test]
#[ignore]
async fn mini_loadtest_against_live_state() {
    let home = std::env::var("HOME").expect("HOME");
    let live_state = PathBuf::from(&home).join("galaxies/cosmon/.cosmon/state");
    let live_galaxies = PathBuf::from(&home).join("galaxies");
    assert!(
        live_state.exists(),
        "live cosmon state missing at {}",
        live_state.display()
    );

    let ndjson_path = std::env::var("NDJSON_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp/cosmon-api-instr-loadtest.ndjson"));
    // Reset the sink so we measure this run only.
    let _ = std::fs::remove_file(&ndjson_path);

    let state = AppState::new(cs_bin())
        .with_state_dir(live_state.clone())
        .with_galaxies_root(live_galaxies)
        .with_instrumentation_path(ndjson_path.clone());
    let addr = spawn_server(state).await;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("client");

    // 250 events: 5 routes × 50 hits each.
    //
    // - `/healthz`  → SubprocessShellOut (`cs --version`)
    // - `/inbox`    → InProcessStateRead  (`<scan-inbox>`)
    // - `/ensemble` → InProcessStateRead  (`<scan-ensemble>`)
    // - `/galaxies` → InProcessStateRead  (`<scan-galaxies>`)
    // - `/whispers` → InProcessStateRead  (`<scan-whispers-inbox>`)
    //
    // Plus 20 in-process `tag` writes (T3 — `task-20260503-22ca`) on a
    // live molecule, alternating add and remove so the molecule's tag
    // set is restored. Pre-T3 these were `cs tag` subprocesses; the
    // route now lives at `InvocationMode::InProcessStateWrite`.
    let n = 50;
    for _ in 0..n {
        let _ = client
            .get(format!("http://{addr}/healthz"))
            .send()
            .await
            .expect("healthz");
        let _ = client
            .get(format!(
                "http://{addr}/inbox?status=pending,running&limit=20"
            ))
            .send()
            .await
            .expect("inbox");
        let _ = client
            .get(format!("http://{addr}/ensemble"))
            .send()
            .await
            .expect("ensemble");
        let _ = client
            .get(format!("http://{addr}/galaxies"))
            .send()
            .await
            .expect("galaxies");
        let _ = client
            .get(format!("http://{addr}/whispers"))
            .send()
            .await
            .expect("whispers");
    }
    if let Ok(mol_id) = std::env::var("LOADTEST_MOLECULE_ID") {
        for i in 0..20u32 {
            let body = if i % 2 == 0 {
                r#"{"add":["temp:hot"]}"#
            } else {
                r#"{"remove":["temp:hot"]}"#
            };
            let _ = client
                .post(format!("http://{addr}/molecules/{mol_id}/tag"))
                .header("Content-Type", "application/json")
                .body(body)
                .send()
                .await
                .expect("tag");
        }
    }

    // Give the appender a moment.
    tokio::time::sleep(Duration::from_millis(100)).await;

    let events = read_ndjson(&ndjson_path).expect("read ndjson");
    assert!(!events.is_empty(), "no events captured");
    print_report(&ndjson_path, &events);
}

fn print_report(path: &Path, events: &[EngineCallEntered]) {
    println!();
    println!("=== T1 mini-loadtest report ===");
    println!("ndjson sink         : {}", path.display());
    println!("events captured     : {}", events.len());
    println!();

    // 1. Mode breakdown
    let mut by_mode: BTreeMap<String, usize> = BTreeMap::new();
    for e in events {
        *by_mode.entry(format!("{:?}", e.mode)).or_default() += 1;
    }
    println!("MODE BREAKDOWN");
    println!("{:<24}  {:>6}", "mode", "count");
    for (mode, n) in &by_mode {
        println!("{mode:<24}  {n:>6}");
    }
    println!();

    // 2. Latency stats per (mode, caller)
    let mut by_route: HashMap<(InvocationMode, String), Vec<u64>> = HashMap::new();
    for e in events {
        by_route
            .entry((e.mode, e.caller.clone()))
            .or_default()
            .push(e.latency_ms);
    }
    println!("LATENCY (ms) BY MODE × CALLER");
    println!(
        "{:<22}  {:<28}  {:>6}  {:>6}  {:>6}",
        "mode", "caller", "n", "p50", "p95"
    );
    let mut rows: Vec<_> = by_route.into_iter().collect();
    rows.sort_by(|a, b| {
        let ka = format!("{:?}", a.0 .0);
        let kb = format!("{:?}", b.0 .0);
        ka.cmp(&kb).then(a.0 .1.cmp(&b.0 .1))
    });
    for ((mode, caller), mut latencies) in rows {
        latencies.sort_unstable();
        let n = latencies.len();
        let p50 = latencies[n / 2];
        let p95 = latencies[(n * 95) / 100];
        println!(
            "{:<22}  {:<28}  {:>6}  {:>6}  {:>6}",
            format!("{:?}", mode),
            caller,
            n,
            p50,
            p95
        );
    }
    println!();

    // 3. Promotion candidates: (verb, args_hash) seen from ≥2 callers
    let mut callers_by_key: HashMap<(String, u64), std::collections::BTreeSet<String>> =
        HashMap::new();
    for e in events {
        callers_by_key
            .entry((e.verb.clone(), e.args_hash))
            .or_default()
            .insert(e.caller.clone());
    }
    println!("PROMOTION CANDIDATES (verb × args_hash invoked from ≥ 2 callers)");
    println!("{:<28}  {:>20}  callers", "verb", "args_hash");
    let mut promotion: Vec<_> = callers_by_key
        .into_iter()
        .filter(|(_, callers)| callers.len() >= 2)
        .collect();
    promotion.sort_by_key(|x| std::cmp::Reverse(x.1.len()));
    if promotion.is_empty() {
        println!("(none — every (verb, args) used by exactly one caller)");
    }
    for ((verb, hash), callers) in promotion {
        let names: Vec<&str> = callers.iter().map(String::as_str).collect();
        println!("{verb:<28}  {hash:>20}  {}", names.join(", "));
    }
    println!();
}
