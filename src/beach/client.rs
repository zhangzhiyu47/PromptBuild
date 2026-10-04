//! Blocking client session. One socket per prompt invocation.
//!
//! Lazy daemon connection. First `session()` connects,
//! spawning the daemon if absent.

use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_millis(100);

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
                resp.get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown"),
            ))
        }
    }
}

pub struct DaemonConn {
    socket_path: PathBuf,
    spawner: Box<dyn Fn(&Path) -> std::io::Result<()> + Send + Sync>,
    session: Option<Session>,
}

impl DaemonConn {
    pub fn new(
        socket_path: PathBuf,
        spawner: impl Fn(&Path) -> std::io::Result<()> + Send + Sync + 'static,
    ) -> Self {
        Self {
            socket_path,
            spawner: Box::new(spawner),
            session: None,
        }
    }

    pub fn session(&mut self) -> Option<&mut Session> {
        if self.session.is_none() {
            self.session = self.connect_or_spawn();
        }
        self.session.as_mut()
    }

    fn connect_or_spawn(&self) -> Option<Session> {
        if let Ok(s) = Session::connect(&self.socket_path, TIMEOUT) {
            return Some(s);
        }
        (self.spawner)(&self.socket_path).ok()?;

        let mut delay = Duration::from_millis(10);
        for _ in 0..8 {
            std::thread::sleep(delay);
            if let Ok(s) = Session::connect(&self.socket_path, TIMEOUT) {
                return Some(s);
            }
            delay = (delay * 2).min(Duration::from_millis(500));
        }
        None
    }
}
