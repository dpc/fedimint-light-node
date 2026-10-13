use bitcoin::BlockHash;

/// Independently recorded guardian state, never inferred from a Core RPC read.
#[derive(Debug, Clone, Copy)]
pub struct GuardianPreflight {
    /// Operator-generated, non-secret 32-byte identity for one guardian database.
    pub guardian_id: [u8; 32],
    /// Last irreversibly consumed block height, NOT a block count.
    pub committed_height: u64,
    /// Hash recorded by that guardian for its last consumed block.
    pub committed_hash: BlockHash,
    /// Oldest block the guardian still needs, including any rescan requirement.
    pub oldest_needed: u64,
}
