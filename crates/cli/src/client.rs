//! A blocking connection to the daemon's control socket.

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use weir_protocol::{FullState, Request, RpcRequest, ServerMessage, JSONRPC_VERSION};

/// One connection to the daemon.
pub struct Client {
    writer: UnixStream,
    reader: BufReader<UnixStream>,
    next_id: u64,
}

impl Client {
    /// Connect to the daemon listening on `path`.
    pub fn connect(path: &Path) -> Result<Self> {
        let stream = UnixStream::connect(path).with_context(|| {
            format!("connecting to {} (is weir-daemon running?)", path.display())
        })?;
        let reader = BufReader::new(stream.try_clone()?);
        Ok(Self {
            writer: stream,
            reader,
            next_id: 1,
        })
    }

    /// Send `req` and wait for its result. An error from the daemon comes
    /// back as its message.
    pub fn call(&mut self, req: &Request) -> Result<Value> {
        let v = serde_json::to_value(req)?;
        let method = v["method"].as_str().unwrap_or_default().to_string();
        self.call_raw(&method, v.get("params").cloned())
    }

    /// Send any method, with `params` as they are, and wait for its result.
    pub fn call_raw(&mut self, method: &str, params: Option<Value>) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let envelope = RpcRequest {
            jsonrpc: JSONRPC_VERSION.into(),
            id: Some(Value::from(id)),
            method: method.into(),
            params,
        };
        self.writer
            .write_all(serde_json::to_string(&envelope)?.as_bytes())?;
        self.writer.write_all(b"\n")?;
        // Notifications may arrive before the response; skip them.
        loop {
            if let ServerMessage::Response(r) = ServerMessage::parse(&self.next_line()?)? {
                if r.id == id {
                    return match r.error {
                        Some(e) => Err(anyhow!("{}", e.message)),
                        None => Ok(r.result.unwrap_or(Value::Null)),
                    };
                }
            }
        }
    }

    /// Everything the daemon knows.
    pub fn state(&mut self) -> Result<FullState> {
        Ok(serde_json::from_value(self.call(&Request::GetState)?)?)
    }

    /// The next line from the daemon, without its newline.
    pub fn next_line(&mut self) -> Result<String> {
        let mut line = String::new();
        if self.reader.read_line(&mut line)? == 0 {
            bail!("daemon closed the connection");
        }
        Ok(line.trim().to_string())
    }
}
