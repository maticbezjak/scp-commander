//! Jump-host (bastion / ProxyJump) tunneling.
//!
//! libssh2's `set_tcp_stream` only accepts a real socket fd, so we can't feed a
//! `direct-tcpip` channel straight into the target session. Instead we open the
//! channel on the bastion and bridge it through a localhost listener: the
//! target session connects to `127.0.0.1:<port>` (a real fd) and its bytes are
//! pumped over the channel to the target. The whole SSH/SFTP handshake runs
//! end-to-end to the target, so its host key is what the target session sees.

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use ssh2::Session;

use crate::sftp::authenticate;
use crate::types::{Error, JumpHost, Result};

/// Keeps a jump tunnel alive. The bastion session and pump thread live inside;
/// dropping it stops the pump and tears the bastion session down.
pub struct Tunnel {
    pub local_port: u16,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Authenticate to `jump`, open a `direct-tcpip` channel to `target_host:port`,
/// and bridge it through a localhost listener. Connect the real session to
/// `127.0.0.1:<tunnel.local_port>`.
pub fn open(jump: &JumpHost, target_host: &str, target_port: u16) -> Result<Tunnel> {
    let tcp = TcpStream::connect((jump.host.as_str(), jump.port))
        .map_err(|e| Error::Connect(format!("jump host {}:{}: {e}", jump.host, jump.port)))?;
    let mut sess = Session::new().map_err(|e| Error::Connect(e.to_string()))?;
    sess.set_tcp_stream(tcp);
    sess.handshake()
        .map_err(|e| Error::Connect(format!("jump host handshake: {e}")))?;

    // Trust-on-first-use for the bastion's own key (recorded in the app store).
    crate::sftp::verify_jump_host_key(&sess, jump)?;

    authenticate(&sess, &jump.username, &jump.auth)?;
    if !sess.authenticated() {
        return Err(Error::Auth("jump host authentication failed".into()));
    }

    let listener =
        TcpListener::bind(("127.0.0.1", 0)).map_err(Error::Io)?;
    listener.set_nonblocking(true).map_err(Error::Io)?;
    let local_port = listener.local_addr().map_err(Error::Io)?.port();

    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    let target_host = target_host.to_string();
    let handle = std::thread::spawn(move || {
        pump(sess, listener, &target_host, target_port, &stop_thread);
    });

    Ok(Tunnel { local_port, stop, handle: Some(handle) })
}

/// Wait for the (single) target-session connection, then shuttle bytes between
/// it and a fresh direct-tcpip channel to the target until either side closes.
fn pump(
    sess: Session,
    listener: TcpListener,
    target_host: &str,
    target_port: u16,
    stop: &AtomicBool,
) {
    // Accept the one connection the target session makes (poll so we can bail).
    let socket = loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        match listener.accept() {
            Ok((s, _)) => break s,
            Err(ref e) if e.kind() == ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => return,
        }
    };
    let mut socket = socket;
    if socket.set_nonblocking(true).is_err() {
        return;
    }

    // The channel must be opened in blocking mode, then we switch to
    // non-blocking for the byte-pump so neither direction starves the other.
    sess.set_blocking(true);
    let mut channel = match sess.channel_direct_tcpip(target_host, target_port, None) {
        Ok(c) => c,
        // The bastion refused the forward (or the target is unreachable). The
        // target session then sees its connection drop as a banner failure.
        Err(_) => return,
    };
    sess.set_blocking(false);

    let mut to_channel: Vec<u8> = Vec::new();
    let mut to_socket: Vec<u8> = Vec::new();
    let mut tc_pos = 0usize;
    let mut ts_pos = 0usize;
    let mut sock_eof = false;
    let mut chan_eof = false;
    let mut buf = [0u8; 32 * 1024];

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let mut progress = false;

        // socket -> channel (only read more once the previous chunk is sent)
        if to_channel.is_empty() && !sock_eof {
            match socket.read(&mut buf) {
                Ok(0) => {
                    sock_eof = true;
                    let _ = channel.send_eof();
                }
                Ok(n) => {
                    to_channel.extend_from_slice(&buf[..n]);
                    tc_pos = 0;
                    progress = true;
                }
                Err(ref e) if e.kind() == ErrorKind::WouldBlock => {}
                Err(_) => break,
            }
        }
        if !to_channel.is_empty() {
            match channel.write(&to_channel[tc_pos..]) {
                Ok(n) => {
                    tc_pos += n;
                    if tc_pos >= to_channel.len() {
                        to_channel.clear();
                    }
                    progress = true;
                }
                Err(ref e) if e.kind() == ErrorKind::WouldBlock => {}
                Err(_) => break,
            }
        }

        // channel -> socket
        if to_socket.is_empty() && !chan_eof {
            match channel.read(&mut buf) {
                Ok(0) => chan_eof = true,
                Ok(n) => {
                    to_socket.extend_from_slice(&buf[..n]);
                    ts_pos = 0;
                    progress = true;
                }
                Err(ref e) if e.kind() == ErrorKind::WouldBlock => {}
                Err(_) => break,
            }
        }
        if !to_socket.is_empty() {
            match socket.write(&to_socket[ts_pos..]) {
                Ok(n) => {
                    ts_pos += n;
                    if ts_pos >= to_socket.len() {
                        to_socket.clear();
                    }
                    progress = true;
                }
                Err(ref e) if e.kind() == ErrorKind::WouldBlock => {}
                Err(_) => break,
            }
        }

        // Done when both directions have closed and their buffers are drained.
        if sock_eof && chan_eof && to_channel.is_empty() && to_socket.is_empty() {
            break;
        }
        if chan_eof && to_socket.is_empty() && (sock_eof || channel.eof()) {
            break;
        }
        if !progress {
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    let _ = channel.close();
}
