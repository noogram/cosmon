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
