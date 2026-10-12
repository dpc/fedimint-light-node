use std::fs::File;
use std::io::{self, Write};
use std::path::Path;

use bitcoin::Block;
use bitcoin::consensus::Encodable;
use fedimint_light_node::validated_header_store::ValidatedHeaderStore;

use crate::Result;

/// Print selected state without conflating retained headers and body availability.
pub fn status(store: &ValidatedHeaderStore) -> Result<()> {
    let anchor = store.anchor();
    let (first, tip) = store.bounds()?;
    let mut out = io::stdout().lock();
    writeln!(out, "network={}", anchor.network)?;
    writeln!(out, "height_one_hash={}", anchor.height_one_hash)?;
    writeln!(out, "checkpoint_height={}", anchor.height)?;
    writeln!(out, "checkpoint_hash={}", anchor.hash)?;
    writeln!(out, "selected_height={tip}")?;
    writeln!(
        out,
        "selected_count={}",
        tip.checked_add(1).ok_or("count overflow")?
    )?;
    writeln!(out, "selected_hash={}", store.header(tip)?.block_hash())?;
    writeln!(out, "retained_first={first}")?;
    writeln!(out, "retained_last={tip}")?;
    writeln!(out, "retained_headers={}", tip - first + 1)?;
    writeln!(out, "capacity={}", store.capacity())?;
    writeln!(out, "protected_height={}", store.protected_height()?)?;
    writeln!(out, "freshness=unproven")?;
    writeln!(out, "body_availability=unproven")?;
    writeln!(out, "validation=mechanical-not-full-consensus")?;
    Ok(())
}

/// Fail before network work when any destination entry already exists.
pub fn require_new(path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "output already exists; files, directories and symlinks are never replaced",
        )),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Publish complete canonical block bytes without replacing any existing entry.
///
/// The caller owns the parent directory. An error after publication can leave a
/// complete destination; inspect it rather than deleting or overwriting it.
pub fn block(path: &Path, block: &Block) -> Result<()> {
    require_new(path)?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut staging = tempfile::NamedTempFile::new_in(parent)?;
    block.consensus_encode(staging.as_file_mut())?;
    staging.as_file().sync_all()?;
    staging.persist_noclobber(path)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}
