// SPDX-License-Identifier: AGPL-3.0-only

//! Pilot presence requests carry attachment evidence and never a mission,
//! attachment, role or lease field in the body (issue #212, W7a).

use cosmon_core::collaboration::{AttachmentId, AttachmentProof};
use cosmon_remote::pilot::AttachRequest;
use cosmon_remote::{Client, Profile};
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn pilot_commands_reach_the_four_presence_routes_with_attachment_evidence() {
    let server = MockServer::start().await;
    let mut profile = Profile::from_host(server.uri());
    profile.sub = "pilot".into();
    profile.aud = "cosmon-rpp".into();
    profile.oidc_url = "https://issuer.invalid".into();
    profile.phone_home = false;
    let client = Client::new(&profile, Some("test-bearer".into())).unwrap();
    let attachment = AttachmentId::from_random([1; 8]);
    let proof = AttachmentProof::from_secret_bytes([2; 32]);
    let base = || {
        Mock::given(header("authorization", "Bearer test-bearer"))
            .and(header("cosmon-attachment", attachment.as_str()))
            .and(header("cosmon-attachment-proof", proof.expose_secret()))
    };
    let mission = "task-20261120-b000";
    let own = format!("/v1/pilot/{mission}/attachments/{attachment}");
    base()
        .and(method("POST"))
        .and(path(format!("/v1/pilot/{mission}/attachments")))
        .and(body_json(json!({"idempotency_key":"k1","label":"laptop"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"duplicate":false})))
        .expect(1)
        .mount(&server)
        .await;
    base()
        .and(method("PUT"))
        .and(path(format!("{own}/heartbeat")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(1)
        .mount(&server)
        .await;
    base()
        .and(method("DELETE"))
        .and(path(own.clone()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"detached":true})))
        .expect(1)
        .mount(&server)
        .await;
    base()
        .and(method("GET"))
        .and(path(format!("/v1/pilot/{mission}/attachments")))
        .and(query_param("limit", "1"))
        .and(query_param("after", attachment.as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"attachments":[]})))
        .expect(1)
        .mount(&server)
        .await;
    let body = AttachRequest {
        idempotency_key: "k1".into(),
        label: Some("laptop".into()),
    };
    client
        .pilot_attach(mission, &attachment, &proof, &body)
        .await
        .unwrap();
    client
        .pilot_heartbeat(mission, &attachment, &proof)
        .await
        .unwrap();
    client
        .pilot_detach(mission, &attachment, &proof)
        .await
        .unwrap();
    client
        .pilot_peers(mission, &attachment, &proof, Some(attachment.as_str()), 1)
        .await
        .unwrap();
}

#[test]
fn attach_dto_refuses_authority_fields() {
    for forged in [
        json!({"idempotency_key":"k","role":"primary"}),
        json!({"idempotency_key":"k","lease_epoch":1}),
        json!({"idempotency_key":"k","mission":"task-20261120-b000"}),
    ] {
        assert!(serde_json::from_value::<AttachRequest>(forged).is_err());
    }
}

/// W7b: send, pull and ack reach the three mailbox routes with attachment
/// evidence; the send body names no author or mission.
#[tokio::test]
async fn pilot_commands_reach_the_three_mailbox_routes_with_attachment_evidence() {
    use cosmon_remote::pilot::SendRequest;
    let server = MockServer::start().await;
    let mut profile = Profile::from_host(server.uri());
    profile.sub = "pilot".into();
    profile.aud = "cosmon-rpp".into();
    profile.oidc_url = "https://issuer.invalid".into();
    profile.phone_home = false;
    let client = Client::new(&profile, Some("test-bearer".into())).unwrap();
    let attachment = AttachmentId::from_random([1; 8]);
    let proof = AttachmentProof::from_secret_bytes([2; 32]);
    let base = || {
        Mock::given(header("authorization", "Bearer test-bearer"))
            .and(header("cosmon-attachment", attachment.as_str()))
            .and(header("cosmon-attachment-proof", proof.expose_secret()))
    };
    let mission = "task-20261121-b000";
    let messages = format!("/v1/pilot/{mission}/messages");
    base()
        .and(method("POST"))
        .and(path(messages.clone()))
        .and(body_json(
            json!({"idempotency_key":"k1","to":"att-0202020202020202","message":"hi","expires_in_secs":60}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"duplicate":false})))
        .expect(1)
        .mount(&server)
        .await;
    base()
        .and(method("GET"))
        .and(path(messages.clone()))
        .and(query_param("limit", "5"))
        .and(query_param("after", "7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"messages":[]})))
        .expect(1)
        .mount(&server)
        .await;
    base()
        .and(method("POST"))
        .and(path(format!("{messages}/rmsg-0011/ack")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"duplicate":false})))
        .expect(1)
        .mount(&server)
        .await;
    let body = SendRequest {
        idempotency_key: "k1".into(),
        to: "att-0202020202020202".into(),
        message: "hi".into(),
        expires_in_secs: Some(60),
    };
    client
        .pilot_send(mission, &attachment, &proof, &body)
        .await
        .unwrap();
    client
        .pilot_pull(mission, &attachment, &proof, Some(7), 5)
        .await
        .unwrap();
    client
        .pilot_ack(mission, "rmsg-0011", &attachment, &proof)
        .await
        .unwrap();
}

#[test]
fn send_dto_refuses_author_and_mission_fields() {
    use cosmon_remote::pilot::SendRequest;
    for forged in [
        json!({"idempotency_key":"k","to":"x","message":"m","from":"att-1"}),
        json!({"idempotency_key":"k","to":"x","message":"m","mission":"task-20261121-b000"}),
    ] {
        assert!(serde_json::from_value::<SendRequest>(forged).is_err());
    }
}
