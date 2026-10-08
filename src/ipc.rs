//! Control channel, so `dayz-map toggle` (or a desktop shortcut) can drive the running overlay:
//! a Unix socket on Linux, and a loopback TCP port on Windows (its number kept in a file).

use anyhow::{Context, Result};
use std::io::{BufRead, BufReader, Read, Write};
use std::time::Duration;

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

    /// The socket, and what to put before each command (the socket's location already proves
    /// the client is this user).
    pub fn connect() -> io::Result<(UnixStream, String)> {
        Ok((UnixStream::connect(path())?, String::new()))
    }

    /// The listener, and the prefix commands must carry.
    pub fn bind() -> io::Result<(UnixListener, String)> {
        let _ = std::fs::remove_file(path());
        Ok((UnixListener::bind(path())?, String::new()))
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

    /// Holds "<port> <secret>". It's in this user's profile, so only they can read the secret,
    /// which keeps other users (and anything else on the port) out of the loopback channel.
    fn port_file() -> PathBuf {
        crate::config::data_dir().join("control-port")
    }

    pub fn connect() -> io::Result<(TcpStream, String)> {
        let text = std::fs::read_to_string(port_file())?;
        let (port, secret) = text
            .trim()
            .split_once(' ')
            .ok_or_else(|| io::Error::other("bad port file"))?;
        let port: u16 = port.parse().map_err(io::Error::other)?;
        let stream =
            TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_secs(1))?;
        Ok((stream, format!("{secret} ")))
    }

    pub fn bind() -> io::Result<(TcpListener, String)> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let secret = secret();
        std::fs::create_dir_all(crate::config::data_dir())?;
        std::fs::write(
            port_file(),
            format!("{} {secret}", listener.local_addr()?.port()),
        )?;
        Ok((listener, format!("{secret} ")))
    }

    /// 128 random bits, from the per-process random keys the standard library's hash maps use.
    fn secret() -> String {
        use std::hash::{BuildHasher, RandomState};
        let half = || RandomState::new().hash_one(std::time::SystemTime::now());
        format!("{:016x}{:016x}", half(), half())
    }

    pub fn cleanup() {
        let _ = std::fs::remove_file(port_file());
    }
}

const REPLY_TIMEOUT: Duration = Duration::from_secs(2);

/// Sends a line to the running overlay and returns its reply.
fn request(line: &str) -> Result<String> {
    let (stream, prefix) = endpoint::connect().context("the overlay is not running")?;
    stream.set_read_timeout(Some(REPLY_TIMEOUT))?;
    (&stream).write_all(format!("{prefix}{line}\n").as_bytes())?;
    let mut reply = String::new();
    BufReader::new(&stream).read_line(&mut reply)?;
    Ok(reply.trim().to_owned())
}

/// Sends a command to the running overlay.
pub fn send(command: Command) -> Result<()> {
    match request(command.as_str())?.as_str() {
        "ok" => Ok(()),
        // Versions before 0.2 closed the connection without replying.
        "" => Ok(()),
        reply => anyhow::bail!("unexpected reply from the control channel: {reply:?}"),
    }
}

/// Keeps the control channel open; dropping it removes the socket or port file. It also holds
/// the instance lock, which the system releases however the process ends.
pub struct Listening(#[allow(dead_code)] std::fs::File);

impl Drop for Listening {
    fn drop(&mut self) {
        endpoint::cleanup();
    }
}

/// Starts listening for commands, failing if another overlay is already running.
pub fn listen(on_command: impl Fn(Command) + Send + 'static) -> Result<Listening> {
    // Two copies starting at once (autostart and a double-click) would both find nothing
    // listening below; a lock decides which one runs.
    let dir = crate::config::data_dir();
    std::fs::create_dir_all(&dir)?;
    // (Not truncated: Windows refuses to truncate a file another process has locked.)
    let lock = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join("instance.lock"))
        .context("creating the instance lock")?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => anyhow::bail!("the overlay is already running"),
        Err(std::fs::TryLockError::Error(e)) => return Err(e).context("locking the instance lock"),
    }
    // A port file may be left from a crash and its port reused by something else, so on Windows
    // only a reply proves an overlay is there. A Unix socket only connects while its listener
    // runs (versions before 0.2 close it without replying).
    if request("ping").is_ok_and(|reply| reply == "ok" || (cfg!(unix) && reply.is_empty())) {
        anyhow::bail!("the overlay is already running");
    }
    let (listener, prefix) = endpoint::bind().context("opening the control channel")?;
    let listening = Listening(lock);
    std::thread::Builder::new()
        .name("ipc".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                // Don't let a client that never sends anything hold up the others.
                let _ = stream.set_read_timeout(Some(REPLY_TIMEOUT));
                // Commands are short; don't buffer an endless line.
                let mut line = String::new();
                if BufReader::new((&stream).take(256))
                    .read_line(&mut line)
                    .is_err()
                {
                    continue;
                }
                let Some(line) = line.strip_prefix(prefix.as_str()) else {
                    log::warn!("ignored a control command without this user's secret");
                    continue;
                };
                if line.trim() != "ping" {
                    match Command::parse(line) {
                        Some(command) => on_command(command),
                        None => {
                            log::warn!("unknown command {line:?}");
                            continue;
                        }
                    }
                }
                let _ = (&stream).write_all(b"ok\n");
            }
        })?;
    crate::fresh_log();
    Ok(listening)
}
