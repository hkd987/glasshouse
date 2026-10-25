//! Where this computer's OS releases come from (ADR 0061), besides a
//! folder: Glasshouse's update server over HTTPS, or the org's console,
//! which serves each computer its ring's channel and tells it, as it
//! syncs, when that channel has a newer release. Either way glassd checks
//! every signature, and every image against its signed digest, as it does
//! a folder's: nothing is trusted for where it came from.

use std::io::Write;
use std::sync::{Arc, Mutex, PoisonError};

use glass_gateway_providers::{ConsoleClient, UpdateClient};
use glass_managed::Ring;
use glass_managed::osupdate::{self, FileRequest, MAX_PART};
use glass_runtime::osupdate::Source;

/// Glasshouse's update server, or a mirror of it, over HTTPS.
#[derive(Debug)]
pub struct Https(UpdateClient);

impl Https {
    pub fn new(url: &str, allow_loopback_http: bool) -> Result<Self, String> {
        UpdateClient::new(url, allow_loopback_http).map(Self)
    }
}

impl Source for Https {
    fn describe(&self) -> String {
        self.0.url().to_owned()
    }

    fn keys(&self, max: u64) -> Result<Vec<u8>, String> {
        let mut bytes = Vec::new();
        self.0.get("keys.json", 0, &mut bytes, max)?;
        Ok(bytes)
    }

    fn file(
        &self,
        channel: &str,
        name: &str,
        offset: u64,
        to: &mut dyn Write,
        max: u64,
    ) -> Result<u64, String> {
        if !osupdate::valid_file(channel) || !osupdate::valid_file(name) {
            return Err(format!("{channel}/{name} isn't a release's file"));
        }
        self.0.get(&format!("{channel}/{name}"), offset, to, max)
    }
}

/// The org's console: once managed sync has the computer's key, and the
/// ring the console last said it's in.
#[derive(Debug, Default)]
pub struct ConsoleUpdates {
    link: Mutex<Option<(Arc<ConsoleClient>, String)>>,
    ring: Mutex<Option<Ring>>,
}

impl ConsoleUpdates {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// The console, with the computer's key, as `device`.
    pub fn connect(&self, client: Arc<ConsoleClient>, device: &str) {
        *lock(&self.link) = Some((client, device.to_owned()));
    }

    /// The ring the console says this computer is in: its channel.
    pub fn set_ring(&self, ring: Ring) {
        *lock(&self.ring) = Some(ring);
    }

    /// `name`, from the console part by part, from `offset` on.
    fn read(&self, name: &str, offset: u64, to: &mut dyn Write, max: u64) -> Result<u64, String> {
        let (client, device) = lock(&self.link)
            .clone()
            .ok_or("the console hasn't been reached yet")?;
        let mut capped = Capped { to, left: max };
        let mut total = 0u64;
        loop {
            let n = client.os_part(
                &FileRequest {
                    device: device.clone(),
                    name: name.to_owned(),
                    offset: offset + total,
                },
                &mut capped,
            )?;
            total += n;
            if n < MAX_PART {
                return Ok(total);
            }
        }
    }
}

impl Source for ConsoleUpdates {
    fn describe(&self) -> String {
        "the org's console".into()
    }

    fn channel(&self) -> Option<String> {
        lock(&self.ring).map(|r| r.as_str().to_owned())
    }

    fn keys(&self, max: u64) -> Result<Vec<u8>, String> {
        let mut bytes = Vec::new();
        self.read("keys.json", 0, &mut bytes, max)?;
        Ok(bytes)
    }

    fn file(
        &self,
        channel: &str,
        name: &str,
        offset: u64,
        to: &mut dyn Write,
        max: u64,
    ) -> Result<u64, String> {
        // The console serves the computer's ring; a release read for
        // another channel would be refused anyway.
        match self.channel() {
            None => return Err("the console hasn't said this computer's ring yet".into()),
            Some(ring) if ring != channel => {
                return Err(format!(
                    "the console serves this computer's ring, {ring}, not the {channel} channel"
                ));
            }
            Some(_) => {}
        }
        if !osupdate::valid_file(name) {
            return Err(format!("{name} isn't a release's file"));
        }
        self.read(name, offset, to, max)
    }
}

/// A writer that takes at most `left` bytes.
struct Capped<'a> {
    to: &'a mut dyn Write,
    left: u64,
}

impl Write for Capped<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if buf.len() as u64 > self.left {
            return Err(std::io::Error::other(
                "the file is longer than its signed size",
            ));
        }
        let n = self.to.write(buf)?;
        self.left -= n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.to.flush()
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}
