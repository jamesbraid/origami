use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// The version `ping` reports. A change that breaks an older client bumps it.
pub const PROTOCOL: u32 = 1;

/// A verb's name on the wire, without its arguments.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Verb {
    Ping,
    Status,
    Quit,
    SerialSend,
    SerialRead,
    WaitSerial,
    Screenshot,
    CdromLoad,
    CdromEject,
    Reset,
    PowerButton,
    Nmi,
    Pause,
    Resume,
    Stop,
    Console,
    Lease,
    Release,
    BreakLease,
    Cont,
}

impl Verb {
    pub const ALL: [Verb; 20] = [
        Verb::Ping,
        Verb::Status,
        Verb::Quit,
        Verb::SerialSend,
        Verb::SerialRead,
        Verb::WaitSerial,
        Verb::Screenshot,
        Verb::CdromLoad,
        Verb::CdromEject,
        Verb::Reset,
        Verb::PowerButton,
        Verb::Nmi,
        Verb::Pause,
        Verb::Resume,
        Verb::Stop,
        Verb::Console,
        Verb::Lease,
        Verb::Release,
        Verb::BreakLease,
        Verb::Cont,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Verb::Ping => "ping",
            Verb::Status => "status",
            Verb::Quit => "quit",
            Verb::SerialSend => "serial-send",
            Verb::SerialRead => "serial-read",
            Verb::WaitSerial => "wait-serial",
            Verb::Screenshot => "screenshot",
            Verb::CdromLoad => "cdrom-load",
            Verb::CdromEject => "cdrom-eject",
            Verb::Reset => "reset",
            Verb::PowerButton => "power-button",
            Verb::Nmi => "nmi",
            Verb::Pause => "pause",
            Verb::Resume => "resume",
            Verb::Stop => "stop",
            Verb::Console => "console",
            Verb::Lease => "lease",
            Verb::Release => "release",
            Verb::BreakLease => "break-lease",
            Verb::Cont => "cont",
        }
    }

    pub fn from_name(name: &str) -> Option<Verb> {
        Verb::ALL.into_iter().find(|v| v.name() == name)
    }
}

fn zero() -> u32 {
    0
}

/// A request. Empty struct variants keep `args` an object on the wire, so
/// every verb has the same shape.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", content = "args", rename_all = "kebab-case")]
pub enum Request {
    Ping {},
    Status {},
    Quit {},
    SerialSend {
        data: String,
    },
    /// Output from absolute ring offset `since` (default: the broker's
    /// current tail), at most `max` bytes.
    SerialRead {
        #[serde(default)]
        since: Option<u64>,
        #[serde(default)]
        max: Option<usize>,
    },
    WaitSerial {
        pattern: String,
        timeout_ms: u64,
        #[serde(default)]
        since: Option<u64>,
    },
    Screenshot {
        #[serde(default = "zero")]
        head: u32,
        path: String,
    },
    CdromLoad {
        path: String,
        #[serde(default)]
        drive: Option<String>,
    },
    CdromEject {
        #[serde(default)]
        drive: Option<String>,
    },
    /// The reply carries a [`BootOffsetData`].
    Reset {},
    PowerButton {},
    Nmi {},
    Pause {},
    Resume {},
    /// `timeout` is seconds to wait for a clean shutdown before forcing.
    Stop {
        #[serde(default)]
        clean: bool,
        #[serde(default)]
        timeout: Option<u64>,
    },
    /// `line` names the console (empty: the machine's first). `lease` asks
    /// for the input lease together with the stream. `from` starts the
    /// output at an absolute ring offset instead of the live tail.
    Console {
        #[serde(default)]
        line: String,
        #[serde(default)]
        lease: bool,
        #[serde(default)]
        from: Option<u64>,
    },
    Lease {
        #[serde(default)]
        line: String,
    },
    Release {
        #[serde(default)]
        line: String,
    },
    BreakLease {
        #[serde(default)]
        line: String,
    },
    /// Resume a start made with `--hold`; the reply carries a
    /// [`BootOffsetData`].
    Cont {},
}

impl Request {
    pub fn verb(&self) -> Verb {
        match self {
            Request::Ping {} => Verb::Ping,
            Request::Status {} => Verb::Status,
            Request::Quit {} => Verb::Quit,
            Request::SerialSend { .. } => Verb::SerialSend,
            Request::SerialRead { .. } => Verb::SerialRead,
            Request::WaitSerial { .. } => Verb::WaitSerial,
            Request::Screenshot { .. } => Verb::Screenshot,
            Request::CdromLoad { .. } => Verb::CdromLoad,
            Request::CdromEject { .. } => Verb::CdromEject,
            Request::Reset {} => Verb::Reset,
            Request::PowerButton {} => Verb::PowerButton,
            Request::Nmi {} => Verb::Nmi,
            Request::Pause {} => Verb::Pause,
            Request::Resume {} => Verb::Resume,
            Request::Stop { .. } => Verb::Stop,
            Request::Console { .. } => Verb::Console,
            Request::Lease { .. } => Verb::Lease,
            Request::Release { .. } => Verb::Release,
            Request::BreakLease { .. } => Verb::BreakLease,
            Request::Cont {} => Verb::Cont,
        }
    }

    /// Decode one request line. An unknown verb and a malformed request are
    /// told apart so a server can answer each with its own kind.
    pub fn parse(line: &[u8]) -> Result<Request, Reply> {
        let mut v: Value = serde_json::from_slice(line)
            .map_err(|e| Reply::err(ErrorKind::BadRequest, format!("not JSON: {e}")))?;
        let obj = v
            .as_object_mut()
            .ok_or_else(|| Reply::err(ErrorKind::BadRequest, "request is not an object"))?;
        let cmd = obj
            .get("cmd")
            .and_then(Value::as_str)
            .ok_or_else(|| Reply::err(ErrorKind::BadRequest, "missing \"cmd\""))?
            .to_owned();
        if Verb::from_name(&cmd).is_none() {
            return Err(Reply::err(
                ErrorKind::UnknownVerb,
                format!("unknown verb {cmd:?}"),
            ));
        }
        // iris clients omit `args` for argument-less verbs, or send null.
        if matches!(obj.get("args"), None | Some(Value::Null)) {
            obj.insert("args".into(), json!({}));
        }
        serde_json::from_value(v).map_err(|e| {
            Reply::err(
                ErrorKind::BadRequest,
                format!("bad arguments to {cmd}: {e}"),
            )
        })
    }

    pub fn to_line(&self) -> String {
        serde_json::to_string(self).expect("request serializes")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// Another client holds the engine's single-client channel.
    Busy,
    NotRunning,
    LeaseHeld,
    Timeout,
    BadRequest,
    UnknownVerb,
    TooLarge,
    /// The engine refused or failed the operation.
    Engine,
    Io,
    /// The engine does not model this.
    Unsupported,
    /// A kind from a newer server.
    #[serde(other)]
    Other,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Reply {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ErrorKind>,
}

impl Reply {
    pub fn ok() -> Reply {
        Reply {
            ok: true,
            data: None,
            error: None,
            kind: None,
        }
    }

    pub fn data(data: impl Serialize) -> Reply {
        Reply {
            ok: true,
            data: Some(serde_json::to_value(data).expect("reply data serializes")),
            error: None,
            kind: None,
        }
    }

    pub fn err(kind: ErrorKind, text: impl Into<String>) -> Reply {
        Reply {
            ok: false,
            data: None,
            error: Some(text.into()),
            kind: Some(kind),
        }
    }

    pub fn to_line(&self) -> String {
        serde_json::to_string(self).expect("reply serializes")
    }
}

/// `ping`'s data. A client uses only the verbs offered.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PingData {
    pub protocol: u32,
    pub engine: String,
    pub verbs: Vec<String>,
}

/// Data of `cont` and `reset`: where output after the (re)start begins in
/// the console ring.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BootOffsetData {
    pub boot_offset: u64,
}

/// Data of a `console` reply, sent before the raw bytes begin.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsoleData {
    /// Ring offset of the first byte that follows.
    pub start: u64,
    pub live_from: u64,
    /// This connection holds the input lease.
    pub leased: bool,
}

/// A line pushed by the server between replies.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "kebab-case")]
pub enum Event {
    /// This connection's input was dropped because another holds the lease.
    Notice {
        line: String,
        text: String,
    },
    /// This connection's lease ended; `ctrl_u` is set if Ctrl-U was sent to
    /// clear a half-typed line.
    LeaseLost {
        line: String,
        reason: String,
        ctrl_u: bool,
    },
    StateChanged {
        state: String,
    },
    #[serde(other)]
    Unknown,
}

impl Event {
    pub fn to_line(&self) -> String {
        serde_json::to_string(self).expect("event serializes")
    }
}
