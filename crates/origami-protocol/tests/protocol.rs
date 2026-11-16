use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use origami_protocol::*;
use serde_json::{json, Value};

fn samples() -> Vec<Request> {
    vec![
        Request::Ping {},
        Request::Status {},
        Request::Quit {},
        Request::SerialSend {
            data: "ls\r".into(),
        },
        Request::SerialRead {
            since: Some(7),
            max: Some(100),
        },
        Request::WaitSerial {
            pattern: "login:".into(),
            timeout_ms: 5000,
            since: None,
        },
        Request::Screenshot {
            head: 1,
            path: "/tmp/a.png".into(),
        },
        Request::CdromLoad {
            path: "/x.iso".into(),
            drive: Some("cd0".into()),
        },
        Request::CdromEject { drive: None },
        Request::Reset {},
        Request::PowerButton {},
        Request::Nmi {},
        Request::Pause {},
        Request::Resume {},
        Request::Stop {
            clean: true,
            timeout: Some(30),
        },
        Request::Console {
            line: "a".into(),
            lease: true,
            from: Some(9),
        },
        Request::Lease { line: "a".into() },
        Request::Release { line: "a".into() },
        Request::BreakLease { line: "a".into() },
        Request::Cont {},
    ]
}

#[test]
fn every_verb_round_trips() {
    let all = samples();
    assert_eq!(all.len(), Verb::ALL.len());
    for req in all {
        let line = req.to_line();
        assert!(!line.contains('\n'));
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["cmd"], req.verb().name());
        assert!(v["args"].is_object(), "{line}");
        assert_eq!(Request::parse(line.as_bytes()).unwrap(), req, "{line}");
        assert_eq!(Verb::from_name(req.verb().name()), Some(req.verb()));
    }
}

#[test]
fn argumentless_verbs_accept_missing_or_null_args() {
    for line in [
        r#"{"cmd":"status"}"#,
        r#"{"cmd":"status","args":null}"#,
        r#"{"cmd":"status","args":{}}"#,
    ] {
        assert_eq!(Request::parse(line.as_bytes()).unwrap(), Request::Status {});
    }
    // Defaults fill in omitted optional arguments.
    assert_eq!(
        Request::parse(br#"{"cmd":"screenshot","args":{"path":"p"}}"#).unwrap(),
        Request::Screenshot {
            head: 0,
            path: "p".into()
        }
    );
}

#[test]
fn unknown_verb_and_malformed_lines() {
    let e = Request::parse(br#"{"cmd":"frobnicate"}"#).unwrap_err();
    assert_eq!(e.kind, Some(ErrorKind::UnknownVerb));
    for bad in [
        &b"not json"[..],
        b"[1]",
        br#"{"args":{}}"#,
        br#"{"cmd":"serial-send","args":{}}"#,
    ] {
        let e = Request::parse(bad).unwrap_err();
        assert!(!e.ok);
        assert_eq!(
            e.kind,
            Some(ErrorKind::BadRequest),
            "{}",
            String::from_utf8_lossy(bad)
        );
    }
}

#[test]
fn reply_and_event_wire_shape() {
    let ok = Reply::data(json!({"a": 1})).to_line();
    assert_eq!(ok, r#"{"ok":true,"data":{"a":1}}"#);
    let err = Reply::err(ErrorKind::LeaseHeld, "held").to_line();
    assert_eq!(err, r#"{"ok":false,"error":"held","kind":"lease_held"}"#);
    let back: Reply = serde_json::from_str(&err).unwrap();
    assert_eq!(back.kind, Some(ErrorKind::LeaseHeld));
    // A kind from a newer server decodes rather than failing the reply.
    let newer: Reply =
        serde_json::from_str(r#"{"ok":false,"error":"x","kind":"brand_new"}"#).unwrap();
    assert_eq!(newer.kind, Some(ErrorKind::Other));

    let ev = Event::LeaseLost {
        line: "a".into(),
        reason: "broken".into(),
        ctrl_u: true,
    };
    let line = ev.to_line();
    assert!(line.starts_with(r#"{"event":"lease-lost""#));
    assert_eq!(serde_json::from_str::<Event>(&line).unwrap(), ev);
    assert_eq!(
        serde_json::from_str::<Event>(r#"{"event":"later"}"#).unwrap(),
        Event::Unknown
    );
}

#[test]
fn line_reader_limits_and_leftovers() {
    let mut r = LineReader::with_max(&b"ab\r\ncd\nrest"[..], 16);
    assert_eq!(r.read_line().unwrap().unwrap(), b"ab");
    assert_eq!(r.read_line().unwrap().unwrap(), b"cd");
    assert_eq!(r.read_line().unwrap().unwrap(), b"rest");
    assert!(r.read_line().unwrap().is_none());

    let mut r = LineReader::with_max(&b"1\nraw bytes"[..], 16);
    r.read_line().unwrap();
    let (_, pending) = r.into_parts();
    assert_eq!(pending, b"raw bytes");

    let long = vec![b'x'; 100];
    let mut r = LineReader::with_max(&long[..], 16);
    assert!(matches!(r.read_line(), Err(FrameError::TooLong)));
    let mut long = long;
    long.push(b'\n');
    let mut r = LineReader::with_max(&long[..], 16);
    assert!(matches!(r.read_line(), Err(FrameError::TooLong)));
}

struct Echo {
    seen: Mutex<Vec<(u64, Verb)>>,
    closed: Mutex<Vec<u64>>,
}

impl Handler for Echo {
    fn engine(&self) -> String {
        "test".into()
    }
    fn verbs(&self) -> Vec<Verb> {
        Verb::ALL
            .iter()
            .copied()
            .filter(|v| *v != Verb::Nmi)
            .collect()
    }
    fn handle(&self, conn: &Conn, req: Request) -> Outcome {
        self.seen.lock().unwrap().push((conn.id, req.verb()));
        match req {
            Request::Status {} => {
                // An event pushed before the reply must not confuse the client.
                conn.events()
                    .send(&Event::StateChanged {
                        state: "running".into(),
                    })
                    .unwrap();
                Outcome::Reply(Reply::data(json!({"state": "running"})))
            }
            Request::Stop { .. } => {
                Outcome::Reply(Reply::err(ErrorKind::NotRunning, "not running"))
            }
            Request::Console { .. } => Outcome::Raw(Reply::data(ConsoleData {
                start: 5,
                live_from: 3,
                leased: false,
            })),
            Request::Cont {} => Outcome::Reply(Reply::data(BootOffsetData { boot_offset: 42 })),
            _ => Outcome::Reply(Reply::ok()),
        }
    }
    fn raw(&self, conn: &Conn, mut s: RawStream) {
        // Bytes sent straight after the reply, then an echo loop.
        s.write_all(b"banner").unwrap();
        let mut buf = [0u8; 64];
        while let Ok(n) = s.read(&mut buf) {
            if n == 0 {
                break;
            }
            s.write_all(&buf[..n]).unwrap();
        }
        let _ = conn;
    }
    fn closed(&self, conn: &Conn) {
        self.closed.lock().unwrap().push(conn.id);
    }
}

fn start() -> (Server, Arc<Echo>) {
    let h = Arc::new(Echo {
        seen: Mutex::new(vec![]),
        closed: Mutex::new(vec![]),
    });
    (Server::bind("127.0.0.1:0", h.clone()).unwrap(), h)
}

const T: Duration = Duration::from_secs(5);

#[test]
fn ping_negotiates_verbs() {
    let (srv, _) = start();
    let mut c = Client::connect(srv.local_addr().port(), T).unwrap();
    let p = c.ping().unwrap();
    assert_eq!(p.protocol, PROTOCOL);
    assert_eq!(p.engine, "test");
    assert!(p.verbs.contains(&"console".to_string()));
    assert!(!p.verbs.contains(&"nmi".to_string()));
    assert!(c.offers(Verb::Console) && !c.offers(Verb::Nmi));
    // Refused locally, never sent.
    assert!(matches!(
        c.call(&Request::Nmi {}),
        Err(ClientError::NotOffered(Verb::Nmi))
    ));
}

#[test]
fn server_refuses_unoffered_unknown_and_bad_lines() {
    let (srv, _) = start();
    let mut s = TcpStream::connect(srv.local_addr()).unwrap();
    s.set_read_timeout(Some(T)).unwrap();
    s.write_all(b"{\"cmd\":\"nmi\"}\nnonsense\n\n{\"cmd\":\"zap\"}\n{\"cmd\":\"status\"}\n")
        .unwrap();
    let mut r = LineReader::new(s);
    let mut next = || serde_json::from_slice::<Value>(&r.read_line().unwrap().unwrap()).unwrap();
    assert_eq!(next()["kind"], "unsupported");
    assert_eq!(next()["kind"], "bad_request");
    assert_eq!(next()["kind"], "unknown_verb");
    assert_eq!(next()["event"], "state-changed");
    assert_eq!(next()["data"]["state"], "running");
}

#[test]
fn oversized_line_is_refused_and_connection_closed() {
    let (srv, _) = start();
    let mut s = TcpStream::connect(srv.local_addr()).unwrap();
    s.set_read_timeout(Some(T)).unwrap();
    let junk = vec![b'x'; MAX_LINE + 10];
    // The server may close while we are still writing.
    let _ = s.write_all(&junk);
    let mut r = LineReader::new(s);
    let v: Value = serde_json::from_slice(&r.read_line().unwrap().unwrap()).unwrap();
    assert_eq!(v["kind"], "too_large");
    assert!(r.read_line().map(|l| l.is_none()).unwrap_or(true));
}

#[test]
fn client_server_calls_and_events() {
    let (srv, h) = start();
    let mut c = Client::connect(srv.local_addr().port(), T).unwrap();
    c.ping().unwrap();
    let d = c.call_data(&Request::Status {}).unwrap();
    assert_eq!(d["state"], "running");
    assert_eq!(
        c.take_events(),
        vec![Event::StateChanged {
            state: "running".into()
        }]
    );
    match c.call_data(&Request::Stop {
        clean: true,
        timeout: None,
    }) {
        Err(ClientError::Remote {
            kind: ErrorKind::NotRunning,
            ..
        }) => {}
        other => panic!("{other:?}"),
    }
    let d: BootOffsetData =
        serde_json::from_value(c.call_data(&Request::Cont {}).unwrap()).unwrap();
    assert_eq!(d.boot_offset, 42);
    assert_eq!(h.seen.lock().unwrap().len(), 3);
}

#[test]
fn console_switches_to_raw_bytes() {
    let (srv, h) = start();
    let c = Client::connect(srv.local_addr().port(), T).unwrap();
    let (data, mut raw) = c.console("", false, None).unwrap();
    assert_eq!(data["start"], 5);
    // "banner" was written right behind the reply and may already be buffered.
    let mut got = [0u8; 6];
    raw.read_exact(&mut got).unwrap();
    assert_eq!(&got, b"banner");
    raw.write_all(b"hello").unwrap();
    let mut echo = [0u8; 5];
    raw.read_exact(&mut echo).unwrap();
    assert_eq!(&echo, b"hello");
    // Raw mode stops event delivery on that connection.
    drop(raw);
    for _ in 0..200 {
        if !h.closed.lock().unwrap().is_empty() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("closed() never ran");
}

#[test]
fn server_shutdown_closes_connections() {
    let (mut srv, _) = start();
    let mut c = Client::connect(srv.local_addr().port(), T).unwrap();
    c.ping().unwrap();
    srv.shutdown();
    assert!(c.call(&Request::Status {}).is_err());
}

#[test]
fn timeout_poisons_the_client() {
    // A server that accepts and never answers.
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let _keep = std::thread::spawn(move || {
        let _s = l.accept();
        std::thread::sleep(Duration::from_secs(2));
    });
    let mut c = Client::connect(port, Duration::from_millis(100)).unwrap();
    assert!(matches!(
        c.call(&Request::Status {}),
        Err(ClientError::Timeout)
    ));
    assert!(matches!(
        c.call(&Request::Status {}),
        Err(ClientError::Closed)
    ));
}
