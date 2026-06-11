use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::mpsc;

use crate::worker::{self, Cmd, Event};
use scp_core::types::Credentials;

const POOL_SIZE: usize = 3;

/// Round-robin pool of N persistent transfer worker threads.
/// Each worker has its own TCP connection; files are dispatched to whichever
/// worker is next in rotation so they run in parallel.
pub struct TransferPool {
    txs: Vec<mpsc::Sender<Cmd>>,
    next: Arc<AtomicUsize>,
}

impl TransferPool {
    pub fn new(events: async_channel::Sender<Event>) -> Self {
        let txs = worker::spawn_pool(POOL_SIZE, events);
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
