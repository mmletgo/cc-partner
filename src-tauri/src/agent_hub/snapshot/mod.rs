//! agent_hub/snapshot — SnapshotEnvelope v1、builder 与可读 archive
//!
//! Business Logic（为什么需要这个模块）:
//!     LAN push 与 Git device lane 共用同一可验证 SnapshotEnvelope v1；
//!     builder 从 Hub DAG/CAS 导出确定性 envelope；archive 展开/重打包保持字节稳定。
//!
//! Code Logic（这个模块做什么）:
//!     导出 `canonical_json`、`envelope`、`builder`、`archive` 公共 API。

pub mod archive;
pub mod builder;
pub mod canonical_json;
pub mod envelope;

pub use archive::{expand_readable_archive, repack_readable_archive, ExpandedSnapshot};
pub use builder::{build_snapshot, BuiltSnapshot, SnapshotSelectionMode, SnapshotSelectionRequest};
pub use canonical_json::{
    canonicalize_value, parse_json_value_strict, CanonicalJsonError, MAX_SAFE_INTEGER,
};
pub use envelope::{
    canonicalize_snapshot_without_hash, compute_snapshot_hash, default_snapshot_limits,
    validate_snapshot, SnapshotAlias, SnapshotAsset, SnapshotConflict, SnapshotEnvelopeV1,
    SnapshotError, SnapshotLimits, SnapshotLineage, SnapshotObjectDescriptor, SnapshotRevision,
    SnapshotSelection, SnapshotVariant, CANONICALIZATION_NAME, FORMAT_NAME, FORMAT_VERSION,
};
