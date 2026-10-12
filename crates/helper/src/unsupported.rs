//! The stand-in on platforms without Unix sockets (Windows).  There, every
//! client call fails at once, so thumbnails render in-process, and a helper
//! can't start.  Windows thumbnails come from Explorer's thumbnail host,
//! which is a long-lived process and so keeps its GPU loaded by itself.

use crate::types::{ClientError, Exit, ServeError, ServeOptions};
use crate::{Reply, Request};
use std::io;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

/// Fails at once with [`ClientError::Unsupported`], without calling
/// `start`.
pub fn request(req: &Request, start: impl FnOnce() -> io::Result<()>) -> Result<Reply, ClientError> {
    request_at(Path::new(""), req, start)
}

/// Fails at once with [`ClientError::Unsupported`].
pub fn request_at(
    _socket: &Path,
    _req: &Request,
    _start: impl FnOnce() -> io::Result<()>,
) -> Result<Reply, ClientError> {
    Err(ClientError::Unsupported)
}

/// There is never a helper to stop.
pub fn stop(_timeout: Duration) -> Result<bool, ClientError> {
    Ok(false)
}

/// There is never a helper to stop.
pub fn stop_at(_socket: &Path, _timeout: Duration) -> Result<bool, ClientError> {
    Ok(false)
}

/// Helpers aren't started on this platform.
pub fn spawn_detached(_command: &mut Command) -> io::Result<()> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "this platform has no thumbnail helper"))
}

/// Fails with [`ServeError::Unsupported`].
pub fn serve<H>(_handler: H, _opts: ServeOptions) -> Result<Exit, ServeError>
where
    H: Fn(&Request) -> Reply + Sync,
{
    Err(ServeError::Unsupported)
}

/// A helper that can't exist here: [`Server::bind`] always fails.
pub struct Server {
    _never: std::convert::Infallible,
}

impl Server {
    /// Fails with [`ServeError::Unsupported`].
    pub fn bind(_opts: ServeOptions) -> Result<Server, ServeError> {
        Err(ServeError::Unsupported)
    }

    pub fn socket(&self) -> &Path {
        match self._never {}
    }

    pub fn run<H>(self, _handler: H) -> Result<Exit, ServeError>
    where
        H: Fn(&Request) -> Reply + Sync,
    {
        match self._never {}
    }
}
