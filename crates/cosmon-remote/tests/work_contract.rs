// SPDX-License-Identifier: AGPL-3.0-only

//! Client work requests carry attachment evidence without a caller-selected sender.

use cosmon_core::collaboration::{AttachmentId, AttachmentProof};
use cosmon_core::work_message::{Confidentiality, DeliveryAdapter, Disposition};
use cosmon_remote::work::{AckRequest, PullRequest, SendRequest};
use cosmon_remote::{Client, Profile};
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn work_commands_present_private_attachment_evidence_and_keep_ack_separate_from_reply() {
    let server = MockServer::start().await;
    let mut profile = Profile::from_host(server.uri());
    profile.sub = "pilot".into();
    profile.aud = "cosmon-rpp".into();
    profile.oidc_url = "https://issuer.invalid".into();
    profile.phone_home = false;
    let client = Client::new(&profile, Some("test-bearer".into())).unwrap();
    let attachment = AttachmentId::from_random([1; 8]);
    let proof = AttachmentProof::from_secret_bytes([2; 32]);
    let send = SendRequest {
        scope_revision: "revision".into(),
        recipient: "cto".into(),
        idempotency_key: "request-1".into(),
        sender_time: "2026-10-23T12:00:00Z".into(),
        reply_to: None,
        phase: None,
        confidentiality: Confidentiality::Internal,
        ttl_secs: None,
        payload: "evidence".into(),
    };
    let body = serde_json::to_value(&send).unwrap();
    assert!(body.get("sender").is_none());
    let base = || {
        Mock::given(header("authorization", "Bearer test-bearer"))
            .and(header("cosmon-attachment", attachment.as_str()))
            .and(header("cosmon-attachment-proof", proof.expose_secret()))
    };
    base()
        .and(method("GET"))
        .and(path("/v1/work/owner"))
        .and(query_param("limit", "100"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"scope_revision":"revision","schema_version":1})),
        )
        .expect(1)
        .mount(&server)
        .await;
    base()
        .and(method("POST"))
        .and(path("/v1/work/owner/messages"))
        .and(body_json(body))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"duplicate":false})))
        .expect(1)
        .mount(&server)
        .await;
    base()
        .and(method("GET"))
        .and(path("/v1/work/owner/inbox"))
        .and(query_param("scope_revision", "revision"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"messages":[]})))
        .expect(1)
        .mount(&server)
        .await;
    let pull = PullRequest {
        scope_revision: "revision".into(),
        adapter: DeliveryAdapter::Pull,
        context_observation: None,
        peek: false,
    };
    base()
        .and(method("POST"))
        .and(path("/v1/work/owner/inbox"))
        .and(body_json(serde_json::to_value(&pull).unwrap()))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"messages":[],"receipts":[]})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let ack = AckRequest {
        scope_revision: "revision".into(),
        payload_digest: "digest".into(),
        disposition: Disposition::Considered,
        reply: None,
        note: None,
    };
    base()
        .and(method("POST"))
        .and(path("/v1/work/owner/messages/request-1/ack"))
        .and(body_json(serde_json::to_value(&ack).unwrap()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"acknowledged":true})))
        .expect(1)
        .mount(&server)
        .await;
    client
        .work_list("owner", &attachment, &proof, 0, 100)
        .await
        .unwrap();
    client
        .work_send("owner", &attachment, &proof, &send)
        .await
        .unwrap();
    client
        .work_inbox(
            "owner",
            &attachment,
            &proof,
            &PullRequest {
                peek: true,
                ..pull.clone()
            },
        )
        .await
        .unwrap();
    client
        .work_inbox("owner", &attachment, &proof, &pull)
        .await
        .unwrap();
    client
        .work_ack("owner", "request-1", &attachment, &proof, &ack)
        .await
        .unwrap();
}

#[test]
fn send_dto_refuses_a_sender_in_the_request() {
    let spoofed = json!({"scope_revision":"r","recipient":"cto","idempotency_key":"k","sender_time":"2026-10-23T12:00:00Z","reply_to":null,"phase":null,"confidentiality":"internal","ttl_secs":null,"payload":"hello","sender":"mado"});
    assert!(serde_json::from_value::<SendRequest>(spoofed).is_err());
}
