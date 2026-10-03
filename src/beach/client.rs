//! Blocking client session. One socket per prompt invocation.

use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

pub struct Session {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
}

impl Session {
    pub fn connect(socket: &Path, timeout: Duration) -> std::io::Result<Self> {
        let stream = UnixStream::connect(socket)?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        let reader = BufReader::new(stream.try_clone()?);
        Ok(Self { stream, reader })
    }

    /// Query a `<provider>.<source>` key. `Ok(None)` = miss.
    pub fn get(&mut self, key: &str, path: Option<&str>) -> std::io::Result<Option<Value>> {
        let req = serde_json::json!({ "op": "get", "key": key, "path": path });
        let mut line = serde_json::to_string(&req).unwrap();
        line.push('\n');
        self.stream.write_all(line.as_bytes())?;

        let mut resp_line = String::new();
        self.reader.read_line(&mut resp_line)?;
        let resp: Value = serde_json::from_str(resp_line.trim())
            .map_err(|e| std::io::Error::other(e.to_string()))?;

        if resp.get("ok").and_then(|v| v.as_bool()) == Some(true) {
            Ok(resp.get("data").cloned().filter(|d| !d.is_null()))
        } else {
            Err(std::io::Error::other(
                resp.get("error").and_then(|v| v.as_str()).unwrap_or("unknown"),
            ))
        }
    }
}
