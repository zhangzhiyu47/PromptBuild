//! Lazy daemon connection. First `session()` connects, spawning the daemon
//! if absent.

use crate::beach::Session;
use std::path::{Path, PathBuf};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_millis(100);

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
