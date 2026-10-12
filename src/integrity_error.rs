/// Mechanical rejection of a supplied body or declared proof of work.
///
/// Body errors do not establish that the requested header is invalid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum IntegrityError {
    /// The supplied header hash differs from the requested hash.
    #[error("block does not match the requested hash")]
    RequestedHash,
    /// Serialization exceeds the protocol or caller byte budget.
    #[error("block exceeds serialized byte limit")]
    SerializedSize,
    /// Stripped size or weight exceeds the protocol limit.
    #[error("block exceeds stripped size or weight limit")]
    Weight,
    /// There is no unique coinbase in the first position.
    #[error("block must start with its only coinbase")]
    Coinbase,
    /// A transaction has no inputs or no outputs.
    #[error("transaction inputs and outputs must be nonempty")]
    EmptyTransaction,
    /// Coinbase scriptSig is outside the inclusive 2..=100 byte range.
    #[error("invalid coinbase script length")]
    CoinbaseScript,
    /// An ordinary transaction uses a null previous output.
    #[error("null previous output outside coinbase")]
    NullInput,
    /// A transaction repeats a previous output.
    #[error("duplicate input within a transaction")]
    DuplicateInput,
    /// An individual output or per-transaction sum exceeds MAX_MONEY.
    #[error("transaction output money range exceeded")]
    OutputValue,
    /// Two transactions have the same txid, even if not adjacent.
    #[error("duplicate transaction id")]
    DuplicateTransaction,
    /// Real siblings at a Merkle tree level are identical.
    #[error("mutated transaction Merkle tree")]
    MutatedMerkleTree,
    /// Transactions do not match the header's Merkle root.
    #[error("transaction Merkle root mismatch")]
    MerkleRoot,
    /// A commitment requires exactly one 32-byte coinbase witness element.
    #[error("invalid witness reserved value")]
    WitnessReservedValue,
    /// Witness data does not match the selected coinbase commitment.
    #[error("witness commitment mismatch")]
    WitnessCommitment,
    /// Witnesses are present without an active, matching commitment.
    #[error("unexpected witness data")]
    UnexpectedWitness,
    /// Declared target is zero, negative, overflowing, or above the caller limit.
    #[error("invalid declared proof-of-work target")]
    DeclaredTarget,
    /// Header hash does not meet its declared target.
    #[error("header does not satisfy declared proof of work")]
    DeclaredProofOfWork,
}
