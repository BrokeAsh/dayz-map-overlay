//! Control channel, so `dayz-map toggle` (or a desktop shortcut) can drive the running overlay:
//! a Unix socket on Linux, and a loopback TCP port on Windows (its number kept in a file).

use anyhow::{Context, Result};
use std::io::{BufRead, BufReader, Write};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Show,
    Hide,
    Toggle,
    Quit,
}

impl Command {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim() {
            "show" => Self::Show,
            "hide" => Self::Hide,
            "toggle" => Self::Toggle,
            "quit" => Self::Quit,
            _ => return None,
        })
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Show => "show",
            Self::Hide => "hide",
            Self::Toggle => "toggle",
            Self::Quit => "quit",
        }
    }
}

#[cfg(unix)]
mod endpoint {
    use std::io;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;

    fn path() -> PathBuf {
        let dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        dir.join("dayz-map-overlay.sock")
    }

    pub fn connect() -> io::Result<UnixStream> {
        UnixStream::connect(path())
    }

    pub fn bind() -> io::Result<UnixListener> {
        let _ = std::fs::remove_file(path());
        UnixListener::bind(path())
    }

    pub fn cleanup() {
        let _ = std::fs::remove_file(path());
    }
}

#[cfg(windows)]
mod endpoint {
    use std::io;
    use std::net::{TcpListener, TcpStream};
    use std::path::PathBuf;
    use std::time::Duration;

    fn port_file() -> PathBuf {
        crate::config::data_dir().join("control-port")
    }

    pub fn connect() -> io::Result<TcpStream> {
        let port: u16 = std::fs::read_to_string(port_file())?
            .trim()
            .parse()
            .map_err(io::Error::other)?;
        TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_secs(1))
    }

    pub fn bind() -> io::Result<TcpListener> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        std::fs::create_dir_all(crate::config::data_dir())?;
        std::fs::write(port_file(), listener.local_addr()?.port().to_string())?;
        Ok(listener)
    }

    pub fn cleanup() {
        let _ = std::fs::remove_file(port_file());
    }
}

/// Sends a command to the running overlay.
pub fn send(command: Command) -> Result<()> {
    let mut stream = endpoint::connect().context("the overlay is not running")?;
    writeln!(stream, "{}", command.as_str())?;
    Ok(())
}

/// Starts listening for commands, failing if another overlay is already running.
pub fn listen(on_command: impl Fn(Command) + Send + 'static) -> Result<()> {
    if endpoint::connect().is_ok() {
        anyhow::bail!("the overlay is already running");
    }
    let listener = endpoint::bind().context("opening the control channel")?;
    std::thread::Builder::new()
        .name("ipc".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut line = String::new();
                if BufReader::new(stream).read_line(&mut line).is_ok() {
                    match Command::parse(&line) {
                        Some(command) => on_command(command),
                        None => log::warn!("unknown command {line:?}"),
                    }
                }
            }
        })?;
    Ok(())
}

pub fn cleanup() {
    endpoint::cleanup();
}
