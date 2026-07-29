//! agent_hub/snapshot — SnapshotEnvelope v1 与 canonical JSON 子集
//!
//! Business Logic（为什么需要这个模块）:
//!     LAN push 与 Git device lane 共用同一可验证 SnapshotEnvelope v1；
//!     在 builder/archive/importer 之前先固定 envelope 形状、canonical hash 与硬上限。
//!
//! Code Logic（这个模块做什么）:
//!     导出 `canonical_json`（RFC8785 兼容子集）与 `envelope`（typed envelope + validate/hash）。

pub mod canonical_json;
pub mod envelope;

pub use canonical_json::{
    canonicalize_value, parse_json_value_strict, CanonicalJsonError, MAX_SAFE_INTEGER,
};
pub use envelope::{
    canonicalize_snapshot_without_hash, compute_snapshot_hash, default_snapshot_limits,
    validate_snapshot, SnapshotAlias, SnapshotAsset, SnapshotConflict, SnapshotEnvelopeV1,
    SnapshotError, SnapshotLimits, SnapshotLineage, SnapshotObjectDescriptor, SnapshotRevision,
    SnapshotSelection, SnapshotVariant, CANONICALIZATION_NAME, FORMAT_NAME, FORMAT_VERSION,
};
