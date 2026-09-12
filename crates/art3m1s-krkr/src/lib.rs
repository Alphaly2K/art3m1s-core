//! KrKr/Kirikiri adaptation boundary for Art3m1s.
//!
//! This crate owns the versioned host ABI, project probing, and protocol
//! types used to drive a native KRKR host shim. It intentionally does not
//! depend on `art3m1s-core`, the Flutter host, or `art3m1s-rfvp`.

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod abi;
#[cfg(any(feature = "native-bootstrap", feature = "native-upstream-smoke"))]
pub mod native;
pub mod probe;
pub mod protocol;

pub use abi::{
    ART3M1S_KRKR_API_ABI_MAGIC, ART3M1S_KRKR_API_ABI_VERSION, Art3M1sKrkrApiV1, KrkrAbiError,
    validate_api_v1,
};
pub use probe::{KrkrEvidence, KrkrEvidenceKind, KrkrProbe, KrkrProbeError, probe_project};
pub use protocol::*;
