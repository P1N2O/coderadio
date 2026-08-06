//! A continuous, reconnecting byte stream.
//!
//! `Stream` is the shared engine: one background thread owns the HTTP
//! connection and keeps a bounded ring buffer filled. It is exposed to
//! decoders through `Stream::reader()`, which returns a local `Reader` type
//! implementing `Read` + `Seek` (blocking when the buffer is empty). On any
//! connection error the producer clears the buffer and reconnects with
//! exponential backoff; audio merely stalls ("buffering") while it retries.

use parking_lot::{Condvar, Mutex};
use std::io::{Read, Result, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

const CAPACITY: usize = 512 * 1024; // 512 KiB of buffered audio
const READ_CHUNK: usize = 32 * 1024;

struct Inner {
    buf: Vec<u8>,
    head: usize, // next byte to read
    len: usize,  // bytes currently buffered
}

pub struct Stream {
    inner: Mutex<Inner>,
    cvar: Condvar,
    shutdown: AtomicBool,
    client: reqwest::blocking::Client,
    url: String,
    backoff: AtomicU64,
}

pub struct Reader(Arc<Stream>);

impl Stream {
    pub fn new(url: impl Into<String>) -> Arc<Stream> {
        let client = reqwest::blocking::Client::builder()
            .user_agent(concat!("coderadio/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("http client");
        Arc::new(Stream {
            inner: Mutex::new(Inner { buf: vec![0; CAPACITY], head: 0, len: 0 }),
            cvar: Condvar::new(),
            shutdown: AtomicBool::new(false),
            client,
            url: url.into(),
            backoff: AtomicU64::new(1),
        })
    }

    /// Ask the producer thread to stop and unblock any waiting reader.
    pub fn stop(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
        self.cvar.notify_all();
    }

    /// A `Read + Seek` handle over this stream's buffer.
    pub fn reader(self: &Arc<Self>) -> Reader {
        Reader(self.clone())
    }

    /// Producer loop; runs forever on its own thread until `stop()` is called.
    pub fn run(&self) {
        while !self.shutdown.load(Ordering::Relaxed) {
            let resp = self
                .client
                .get(&self.url)
                .send()
                .and_then(|r| r.error_for_status());
            match resp {
                Ok(mut resp) => {
                    self.backoff.store(1, Ordering::Relaxed);
                    self.pump(&mut resp);
                }
                Err(_) => self.schedule_retry(),
            }
        }
    }

    /// Copy the HTTP body into the ring until EOF or a read error, then return.
    fn pump(&self, resp: &mut reqwest::blocking::Response) {
        let mut chunk = vec![0u8; READ_CHUNK];
        loop {
            match resp.read(&mut chunk) {
                Ok(0) => return,  // clean EOF -> reconnect
                Ok(n) => self.write(&chunk[..n]),
                Err(_) => return, // connection dropped -> reconnect
            }
        }
    }

    fn write(&self, data: &[u8]) {
        let mut off = 0;
        while off < data.len() {
            let mut g = self.inner.lock();
            while g.len == CAPACITY {
                self.cvar.wait(&mut g);
            }
            let n = (CAPACITY - g.len).min(data.len() - off);
            let end = (g.head + g.len) % CAPACITY;
            let first = n.min(CAPACITY - end);
            g.buf[end..end + first].copy_from_slice(&data[off..off + first]);
            if first < n {
                g.buf[..n - first].copy_from_slice(&data[off + first..off + n]);
            }
            g.len += n;
            off += n;
            self.cvar.notify_all();
        }
    }

    fn schedule_retry(&self) {
        thread::sleep(Duration::from_secs(self.backoff.load(Ordering::Relaxed)));
        self.backoff
            .store((self.backoff.load(Ordering::Relaxed) * 2).min(30), Ordering::Relaxed);
        // Drop stale buffered bytes so we don't replay pre-reconnect audio.
        let mut g = self.inner.lock();
        g.head = 0;
        g.len = 0;
        self.cvar.notify_all();
    }

    fn read(&self, out: &mut [u8]) -> Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        let mut g = self.inner.lock();
        // Block until at least one byte is available. Never return a spurious
        // Ok(0) (EOF) while running: the producer reconnects forever.
        while g.len == 0 {
            if self.shutdown.load(Ordering::Relaxed) {
                return Ok(0);
            }
            self.cvar.wait(&mut g);
        }
        let n = g.len.min(out.len());
        let first = n.min(CAPACITY - g.head);
        out[..first].copy_from_slice(&g.buf[g.head..g.head + first]);
        if first < n {
            out[first..n].copy_from_slice(&g.buf[..n - first]);
        }
        g.head = (g.head + n) % CAPACITY;
        g.len -= n;
        self.cvar.notify_all();
        Ok(n)
    }
}

impl Read for Reader {
    fn read(&mut self, out: &mut [u8]) -> Result<usize> {
        self.0.read(out)
    }
}

impl Seek for Reader {
    /// Live streams aren't seekable; a no-op satisfies decoders that
    /// require `Read + Seek`.
    fn seek(&mut self, _pos: SeekFrom) -> Result<u64> {
        Ok(0)
    }
}