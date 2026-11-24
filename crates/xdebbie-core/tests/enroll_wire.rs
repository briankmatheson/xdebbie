// SPDX-License-Identifier: GPL-3.0-or-later
//! The enrolment wire format (`POST /v1/enroll`, `GET /v1/enroll/{id}`)
//! that joiners, `xdctl enroll ls` and the aviary page all parse.

use serde_json::json;
use xdebbie_core::{EnrollKind, EnrollRequest, EnrollStatus, Enrollment};

#[test]
fn a_box_request_says_box_and_leaves_kind_out() {
    let r = EnrollRequest {
        kind: EnrollKind::Box,
        hostname: "joiner".into(),
        box_id: Some(4),
        api_url: "https://joiner:23443".into(),
        csr: "PEM".into(),
        wg_pubkey: None,
    };
    let v = serde_json::to_value(&r).unwrap();
    assert_eq!(v["box"], 4);
    assert!(v.get("box_id").is_none() && v.get("kind").is_none());
    assert!(v.get("wg_pubkey").is_none());
    assert_eq!(v["api_url"], "https://joiner:23443");
    let back: EnrollRequest = serde_json::from_value(v).unwrap();
    assert_eq!(back, r);
    // no wish: the key is absent, and an older joiner that never sends it parses
    let none = EnrollRequest { box_id: None, ..r };
    let v = serde_json::to_value(&none).unwrap();
    assert!(v.get("box").is_none());
    let old: EnrollRequest = serde_json::from_value(
        json!({"hostname": "j", "api_url": "https://j:23443", "csr": "PEM"}),
    )
    .unwrap();
    assert_eq!((old.kind, old.box_id), (EnrollKind::Box, None));
}

#[test]
fn a_box_id_outside_u8_is_rejected_on_the_wire() {
    let bad = serde_json::from_value::<EnrollRequest>(
        json!({"hostname": "j", "box": 256, "api_url": "", "csr": "PEM"}),
    );
    assert!(bad.is_err());
    let neg = serde_json::from_value::<EnrollRequest>(
        json!({"hostname": "j", "box": -1, "api_url": "", "csr": "PEM"}),
    );
    assert!(neg.is_err());
}

#[test]
fn an_enrolment_round_trips_with_status_lowercase() {
    let v = json!({
        "id": "ab12cd34",
        "hostname": "joiner",
        "box": 3,
        "api_url": "https://joiner:23443",
        "fingerprint": "sha256:00",
        "status": "pending",
        "requested_at": "2026-11-24T10:00:00Z"
    });
    let e: Enrollment = serde_json::from_value(v.clone()).unwrap();
    assert_eq!(
        (e.box_id, e.status, e.kind),
        (3, EnrollStatus::Pending, EnrollKind::Box)
    );
    assert!(e.cert.is_none() && e.ca.is_none() && e.decided_at.is_none());
    assert_eq!(
        serde_json::to_value(&e).unwrap(),
        v,
        "nothing added on the way back"
    );
    for (s, want) in [
        ("pending", EnrollStatus::Pending),
        ("approved", EnrollStatus::Approved),
        ("denied", EnrollStatus::Denied),
    ] {
        let mut v = v.clone();
        v["status"] = json!(s);
        let e: Enrollment = serde_json::from_value(v).unwrap();
        assert_eq!(e.status, want);
        assert_eq!(e.status.to_string(), s, "what xdctl enroll ls prints");
    }
    // a client says so
    let mut c = e.clone();
    c.kind = EnrollKind::Client;
    assert_eq!(serde_json::to_value(&c).unwrap()["kind"], "client");
}
