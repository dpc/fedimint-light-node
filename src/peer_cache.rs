//! Disposable versioned mainnet availability hints, never part of chain state.

use std::io::{Read, Write};
use std::net::SocketAddr;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::peer_book::PeerBook;

const PREFIX: &str = "fedimint-light-node peers 1 mainnet\n";

/// Ignore corrupt, oversized or inaccessible caches; never touch the header store.
pub(crate) fn load(path: &Path, book: &mut PeerBook) {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return;
    };
    if !meta.is_file() || meta.len() > 8192 {
        return;
    }
    let Ok(file) = std::fs::File::open(path) else {
        return;
    };
    let mut text = String::new();
    if file.take(8193).read_to_string(&mut text).is_err() || text.len() > 8192 {
        return;
    }
    let Some(body) = text.strip_prefix(PREFIX) else {
        return;
    };
    let lines: Vec<_> = body.lines().collect();
    if lines.len() > 64 {
        return;
    }
    let mut addresses = Vec::new();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    for line in lines {
        let Some((time, address)) = line.split_once(' ') else {
            return;
        };
        let Ok(time) = time.parse::<u64>() else {
            return;
        };
        let Ok(addr) = address.parse::<SocketAddr>() else {
            return;
        };
        if !crate::peer_address::discoverable(addr) {
            return;
        }
        if time <= now && now - time < 7 * 86400 {
            addresses.push((addr, now - time));
        }
    }
    for (addr, age) in addresses {
        book.admit(addr, crate::peer_address::group(addr));
        // Cache records are availability history only, never authentication.
        book.completed(addr, true);
        if let Some(entry) = book.entries.iter_mut().find(|e| e.addr == addr)
            && entry.useful.is_some()
        {
            entry.useful = tokio::time::Instant::now().checked_sub(Duration::from_secs(age));
        }
    }
}

/// Atomically replace only a caller-configured disposable cache; errors are nonfatal.
pub(crate) fn save(path: &Path, book: &PeerBook) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => {
            if !meta.is_file() {
                return Err(std::io::Error::other("cache is not a regular file"));
            }
            let mut prefix = vec![0; PREFIX.len()];
            std::fs::File::open(path)?.read_exact(&mut prefix)?;
            if prefix != PREFIX.as_bytes() {
                return Err(std::io::Error::other(
                    "refusing to replace a non-cache file",
                ));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(PREFIX.as_bytes())?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    for entry in book
        .entries
        .iter()
        .filter(|e| e.useful.is_some() && crate::peer_address::discoverable(e.addr))
        .take(64)
    {
        let last_success = now.saturating_sub(entry.useful.expect("filtered").elapsed().as_secs());
        writeln!(file, "{last_success} {}", entry.addr)?;
    }
    file.flush()?;
    file.persist(path).map_err(|e| e.error)?;
    Ok(())
}
