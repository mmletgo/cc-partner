//! agent_hub/snapshot — SnapshotEnvelope v1、builder、archive 与 importer
//!
//! Business Logic（为什么需要这个模块）:
//!     LAN push 与 Git device lane 共用同一可验证 SnapshotEnvelope v1；
//!     builder 从 Hub DAG/CAS 导出确定性 envelope；archive 展开/重打包保持字节稳定；
//!     importer 两阶段导入 lineage/alias/head，禁止 LWW。
//!
//! Code Logic（这个模块做什么）:
//!     导出 `canonical_json`、`envelope`、`builder`、`archive`、`importer` 公共 API；
//!     builder 经单读事务冻结身份；build/repack 共用 selection_state_hash 公式。

pub mod archive;
pub mod builder;
pub mod canonical_json;
pub mod envelope;
pub mod importer;
pub mod portable_builder;

pub use archive::{expand_readable_archive, repack_readable_archive, ExpandedSnapshot};
pub use builder::{
    build_snapshot, hash_selection, hash_selection_state, BuiltSnapshot, SnapshotSelectionMode,
    SnapshotSelectionRequest,
};
pub use canonical_json::{
    canonicalize_value, parse_json_value_strict, CanonicalJsonError, MAX_SAFE_INTEGER,
};
pub use envelope::{
    canonicalize_snapshot_without_hash, compute_snapshot_hash, default_snapshot_limits,
    validate_snapshot, SnapshotAlias, SnapshotAsset, SnapshotConflict, SnapshotEnvelopeV1,
    SnapshotError, SnapshotLimits, SnapshotLineage, SnapshotObjectDescriptor, SnapshotRevision,
    SnapshotSelection, SnapshotVariant, CANONICALIZATION_NAME, FORMAT_NAME, FORMAT_VERSION,
};
pub use importer::{
    ConfirmedImportSelection, ConfirmedProjectMapping, ProjectMappingCandidate,
    ResolvedProjectMapping, SnapshotImportOutcome, SnapshotImportPreview, SnapshotImporter,
    ValidatedSnapshot,
};
pub use portable_builder::{
    build_portable_selection_envelope, bytes_are_legacy_lossy, BuiltPortableSelection,
    PortableSelectionItem, LEGACY_LOSSY_PLACEHOLDER,
};
