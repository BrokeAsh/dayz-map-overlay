//! Control socket, so `dayz-map toggle` (or a desktop shortcut) can drive the running overlay.

use anyhow::{Context, Result};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

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

fn socket_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    dir.join("dayz-map-overlay.sock")
}

/// Sends a command to the running overlay.
pub fn send(command: Command) -> Result<()> {
    let mut stream = UnixStream::connect(socket_path()).context("the overlay is not running")?;
    writeln!(stream, "{}", command.as_str())?;
    Ok(())
}

/// Binds the control socket, failing if another overlay is already running.
pub fn listen(on_command: impl Fn(Command) + Send + 'static) -> Result<()> {
    let path = socket_path();
    if UnixStream::connect(&path).is_ok() {
        anyhow::bail!("the overlay is already running");
    }
    let _ = std::fs::remove_file(&path);
    let listener =
        UnixListener::bind(&path).with_context(|| format!("binding {}", path.display()))?;
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
    let _ = std::fs::remove_file(socket_path());
}
