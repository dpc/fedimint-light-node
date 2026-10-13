//! Offline operations: logical snapshots, independently retained state and no overwrite.

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use bitcoin::hashes::{Hash, sha256};
use fedimint_light_node::store_config::StoreConfig;
use fedimint_light_node::validated_header_store::ValidatedHeaderStore;
use serde_json::{Value, json};

use crate::{Result, output};

/// Check an explicitly supplied identity, also for stopped administrative commands.
pub fn identity(store: &ValidatedHeaderStore, expected: Option<sha256::Hash>) -> Result<()> {
    if store.guardian_id()? != expected.map(|id| id.to_byte_array()) {
        return Err(
            "guardian binding mismatch; supply the independently recorded --guardian-id".into(),
        );
    }
    Ok(())
}

/// Capture every safety-relevant stored field; cache and runtime fee policy are not persisted.
pub fn state(store: &ValidatedHeaderStore) -> Result<Value> {
    let anchor = store.anchor();
    let (first, tip) = store.bounds()?;
    let (protected, protected_hash) = store.protected_checkpoint()?;
    Ok(json!({
        "format": 1, "store_schema": 2,
        "network": anchor.network.to_string(),
        "height_one_hash": anchor.height_one_hash.to_string(),
        "anchor_height": anchor.height, "anchor_hash": anchor.hash.to_string(),
        "capacity": store.capacity(), "retained_first": first,
        "tip_height": tip, "tip_hash": store.header(tip)?.block_hash().to_string(),
        "protected_height": protected, "protected_hash": protected_hash.to_string(),
        "guardian_id": store.guardian_id()?.map(|id| sha256::Hash::from_byte_array(id).to_string()),
    }))
}

/// Publish a bounded JSON record without replacing existing files or symlinks.
pub fn write_json(path: &Path, value: &Value) -> Result<()> {
    output::require_new(path)?;
    let parent = parent(path);
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(staged.as_file_mut(), value)?;
    staged.write_all(b"\n")?;
    staged.as_file().sync_all()?;
    staged.persist_noclobber(path)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

/// Backup while retaining exclusive database ownership, then publish the manifest last.
pub fn backup(
    store: &ValidatedHeaderStore,
    config: StoreConfig,
    destination: &Path,
    manifest: &Path,
) -> Result<()> {
    output::require_new(destination)?;
    output::require_new(manifest)?;
    if destination == manifest {
        return Err("backup and manifest paths must differ".into());
    }
    let temporary = tempfile::Builder::new()
        .prefix(".backup-")
        .tempdir_in(parent(destination))?;
    let staging = temporary.path().join("chain.redb");
    let expected = state(store)?;
    store.snapshot(&staging)?;
    {
        let reopened = ValidatedHeaderStore::open_pinned(&staging, config)?;
        if state(&reopened)? != expected {
            return Err("snapshot verification mismatch".into());
        }
    }
    File::open(&staging)?.sync_all()?;
    let manifest_value = json!({
        "format": 1,
        "created_unix": SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
        "sha256": digest(&staging)?,
        "state": expected,
    });
    publish(&staging, destination)?;
    // Failure here leaves a complete but uncommitted backup: do not silently reuse it.
    write_json(manifest, &manifest_value)?;
    Ok(())
}

/// Verify immutable backup bytes and CURRENT independent state before any publication.
pub fn restore(
    backup: &Path,
    manifest: &Path,
    expected_state: &Path,
    destination: &Path,
    config: StoreConfig,
    guardian: Option<sha256::Hash>,
) -> Result<()> {
    output::require_new(destination)?;
    let manifest = read_json(manifest)?;
    let expected = read_json(expected_state)?;
    if manifest["format"] != 1 || manifest["state"] != expected {
        return Err(
            "backup does not match independent current safety state; no rollback recovery".into(),
        );
    }
    require_regular(backup)?;
    let temporary = tempfile::Builder::new()
        .prefix(".restore-")
        .tempdir_in(parent(destination))?;
    let staging = temporary.path().join("chain.redb");
    // Only immutable snapshot bytes are copied, never the live source database.
    std::fs::copy(backup, &staging)?;
    if manifest["sha256"].as_str() != Some(digest(&staging)?.as_str()) {
        return Err("backup digest mismatch".into());
    }
    {
        let reopened = ValidatedHeaderStore::open_pinned(&staging, config)?;
        identity(&reopened, guardian)?;
        if state(&reopened)? != expected {
            return Err("backup schema/config/identity/anchor/floor/binding mismatch".into());
        }
    }
    File::open(&staging)?.sync_all()?;
    publish(&staging, destination)?;
    Ok(())
}

fn read_json(path: &Path) -> Result<Value> {
    require_regular(path)?;
    let mut bytes = Vec::new();
    File::open(path)?.take(8193).read_to_end(&mut bytes)?;
    if bytes.len() > 8192 {
        return Err("operational record exceeds 8192 bytes".into());
    }
    Ok(serde_json::from_slice(&bytes)?)
}

fn require_regular(path: &Path) -> Result<()> {
    if !std::fs::symlink_metadata(path)?.file_type().is_file() {
        return Err("operational input must be a regular file in a trusted directory".into());
    }
    Ok(())
}

fn digest(path: &Path) -> Result<String> {
    let mut input = File::open(path)?;
    let mut engine = sha256::Hash::engine();
    std::io::copy(&mut input, &mut engine)?;
    Ok(sha256::Hash::from_engine(engine).to_string())
}

fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

fn publish(staging: &Path, destination: &Path) -> Result<()> {
    std::fs::hard_link(staging, destination)?;
    File::open(parent(destination))?.sync_all()?;
    Ok(())
}
