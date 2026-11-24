// SPDX-License-Identifier: GPL-3.0-or-later
//! `xdctl enroll ls`, `xdctl approve HOST`, `xdctl enroll deny ID` against a
//! fake xdebbied on loopback: the requests they make and what they print.

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    process::Command,
    sync::{Arc, Mutex},
};

/// One request the fake saw: method, path, Authorization header.
type Seen = Arc<Mutex<Vec<(String, String, Option<String>)>>>;

/// Serve `routes` (method, path) -> (status, JSON body); anything else 404.
/// `/v1/leader` is 404 too, which xdctl takes for an old daemon and uses as-is.
fn fake(routes: Vec<(&'static str, &'static str, u16, String)>) -> (String, Seen) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    let seen: Seen = Arc::default();
    let s = seen.clone();
    std::thread::spawn(move || {
        for conn in l.incoming() {
            let Ok(mut conn) = conn else { return };
            let mut r = BufReader::new(conn.try_clone().unwrap());
            let mut line = String::new();
            if r.read_line(&mut line).is_err() {
                continue;
            }
            let mut parts = line.split_whitespace();
            let (method, path) = (
                parts.next().unwrap_or("").to_string(),
                parts.next().unwrap_or("").to_string(),
            );
            let (mut auth, mut len) = (None, 0usize);
            loop {
                let mut h = String::new();
                if r.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" {
                    break;
                }
                let (k, v) = h.split_once(':').unwrap_or((&h, ""));
                match k.to_ascii_lowercase().as_str() {
                    "authorization" => auth = Some(v.trim().to_string()),
                    "content-length" => len = v.trim().parse().unwrap_or(0),
                    _ => {}
                }
            }
            let mut body = vec![0; len];
            let _ = r.read_exact(&mut body);
            s.lock().unwrap().push((method.clone(), path.clone(), auth));
            let (code, json) = routes
                .iter()
                .find(|(m, p, _, _)| *m == method && *p == path)
                .map(|(_, _, c, j)| (*c, j.clone()))
                .unwrap_or((404, r#"{"error":"not found"}"#.into()));
            let _ = write!(
                conn,
                "HTTP/1.1 {code} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
                json.len()
            );
        }
    });
    (url, seen)
}

fn xdctl(url: &str, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_xdctl"))
        .args(args)
        .env("XDEBBIE_URL", url)
        .env("XDEBBIE_TOKEN", "tok")
        .env("XDEBBIE_CA", "/nonexistent")
        .env_remove("XDEBBIE_INSECURE")
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn enrolment(id: &str, host: &str, b: u8, status: &str) -> String {
    format!(
        r#"{{"id":"{id}","hostname":"{host}","box":{b},"api_url":"https://{host}:23443",
            "fingerprint":"sha256:0123456789abcdef00000000000000000000000000000000fedcba9876543210",
            "status":"{status}","requested_at":"2026-11-24T10:00:00Z"}}"#
    )
}

#[test]
fn enroll_ls_shows_host_box_fingerprint_and_status() {
    let list = format!("[{}]", enrolment("ab12cd34", "joiner", 3, "pending"));
    let (url, seen) = fake(vec![("GET", "/v1/enroll", 200, list)]);
    let (ok, out, err) = xdctl(&url, &["enroll", "ls"]);
    assert!(ok, "{err}");
    let header = out.lines().next().unwrap();
    for col in ["ID", "HOST", "BOX", "URL", "FINGERPRINT", "STATUS", "AGE"] {
        assert!(header.contains(col), "{header}");
    }
    let row = out.lines().nth(1).unwrap();
    assert!(row.contains("ab12cd34") && row.contains("joiner"), "{row}");
    assert!(row.contains(" 3 "), "box column: {row}");
    assert!(
        row.contains("01234567…76543210"),
        "short fingerprint: {row}"
    );
    assert!(row.contains("pending"), "{row}");
    let seen = seen.lock().unwrap();
    let call = seen.iter().find(|c| c.1.starts_with("/v1/enroll")).unwrap();
    assert_eq!(call.2.as_deref(), Some("Bearer tok"));
}

#[test]
fn enroll_ls_all_asks_for_everything_and_an_empty_queue_says_so() {
    let (url, seen) = fake(vec![
        ("GET", "/v1/enroll?all=true", 200, "[]".into()),
        ("GET", "/v1/enroll", 200, "[]".into()),
    ]);
    let (ok, out, _) = xdctl(&url, &["enroll", "ls", "--all"]);
    assert!(ok);
    assert_eq!(out.trim(), "no enrolments");
    let (ok, out, _) = xdctl(&url, &["enroll", "ls"]);
    assert!(ok);
    assert_eq!(out.trim(), "no pending enrolments");
    assert!(
        seen.lock()
            .unwrap()
            .iter()
            .any(|c| c.1 == "/v1/enroll?all=true")
    );
}

#[test]
fn approve_and_enroll_approve_post_to_the_approve_route() {
    let ok_body = enrolment("ab12cd34", "joiner", 5, "approved");
    let (url, seen) = fake(vec![
        ("POST", "/v1/enroll/joiner/approve", 200, ok_body.clone()),
        ("POST", "/v1/enroll/ab12cd34/approve", 200, ok_body),
    ]);
    let (ok, out, err) = xdctl(&url, &["approve", "joiner"]);
    assert!(ok, "{err}");
    assert!(out.contains("approved joiner (box 5, sha256:0123"), "{out}");
    let (ok, _, err) = xdctl(&url, &["enroll", "approve", "ab12cd34"]);
    assert!(ok, "{err}");
    let seen = seen.lock().unwrap();
    assert!(
        seen.iter()
            .any(|c| c.0 == "POST" && c.1 == "/v1/enroll/joiner/approve")
    );
    assert!(
        seen.iter()
            .any(|c| c.0 == "POST" && c.1 == "/v1/enroll/ab12cd34/approve")
    );
}

#[test]
fn deny_posts_to_the_deny_route() {
    let (url, seen) = fake(vec![(
        "POST",
        "/v1/enroll/ab12cd34/deny",
        200,
        enrolment("ab12cd34", "joiner", 5, "denied"),
    )]);
    let (ok, out, err) = xdctl(&url, &["enroll", "deny", "ab12cd34"]);
    assert!(ok, "{err}");
    assert!(out.starts_with("denied joiner"), "{out}");
    assert!(
        seen.lock()
            .unwrap()
            .iter()
            .any(|c| c.0 == "POST" && c.1 == "/v1/enroll/ab12cd34/deny")
    );
}

#[test]
fn a_refused_approval_fails_with_the_servers_reason() {
    let (url, _) = fake(vec![(
        "POST",
        "/v1/enroll/ghost/approve",
        404,
        r#"{"error":"no enrolment \"ghost\""}"#.into(),
    )]);
    let (ok, _, err) = xdctl(&url, &["approve", "ghost"]);
    assert!(!ok);
    assert!(err.contains("no enrolment"), "{err}");
}
