use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::mpsc;

use crate::worker::{self, Cmd, Event};
use scp_core::types::Credentials;

pub const DEFAULT_POOL_SIZE: usize = 3;

/// Round-robin pool of N persistent transfer worker threads.
/// Each worker has its own TCP connection; files are dispatched to whichever
/// worker is next in rotation so they run in parallel.
pub struct TransferPool {
    txs: Vec<mpsc::Sender<Cmd>>,
    next: Arc<AtomicUsize>,
}

impl TransferPool {
    /// `size` is the number of parallel connections (clamped 1…8).
    pub fn new(events: async_channel::Sender<Event>, size: usize) -> Self {
        let txs = worker::spawn_pool(size.clamp(1, 8), events);
        Self { txs, next: Arc::new(AtomicUsize::new(0)) }
    }

    /// Broadcast a silent Connect to every worker in the pool.
    pub fn connect(&self, creds: Credentials) {
        for tx in &self.txs {
            let _ = tx.send(Cmd::Connect { creds: creds.clone(), path: String::new(), silent: true });
        }
    }

    /// Dispatch a transfer command to the next worker in round-robin order.
    pub fn send(&self, cmd: Cmd) {
        let i = self.next.fetch_add(1, Ordering::Relaxed) % self.txs.len();
        let _ = self.txs[i].send(cmd);
    }
}
