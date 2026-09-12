//! Native ABI loading used by integration tests.
//!
//! The `native-bootstrap` feature deliberately exports the ABI with an
//! unsupported implementation. It validates the C/Rust contract and dynamic
//! library boundary before the KRKR runtime is linked.

use crate::abi::{ART3M1S_KRKR_API_ABI_VERSION, Art3M1sKrkrApiV1, KrkrAbiError, validate_api_v1};

unsafe extern "C" {
    fn art3m1s_krkr_get_api_v1(out_size: *mut usize) -> *const Art3M1sKrkrApiV1;
}

pub fn load_api_v1() -> Result<&'static Art3M1sKrkrApiV1, KrkrAbiError> {
    let mut size = 0usize;
    let api = unsafe { art3m1s_krkr_get_api_v1(&mut size) };
    if api.is_null() {
        return Err(KrkrAbiError::Null);
    }
    if size != std::mem::size_of::<Art3M1sKrkrApiV1>() {
        return Err(KrkrAbiError::SizeMismatch {
            expected: std::mem::size_of::<Art3M1sKrkrApiV1>() as u32,
            actual: size.min(u32::MAX as usize) as u32,
        });
    }

    let api = unsafe { &*api };
    validate_api_v1(api)?;
    assert_eq!(api.abi_version, ART3M1S_KRKR_API_ABI_VERSION);
    Ok(api)
}

pub fn probe_null_path() -> i32 {
    let api = load_api_v1().expect("native bootstrap ABI must be valid");
    let mut probe = crate::protocol::Art3m1sKrkrProbeV1::new();
    unsafe { api.probe_project.expect("probe_project is required")(std::ptr::null(), &mut probe) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exports_a_valid_v1_table() {
        let api = load_api_v1().unwrap();
        assert_eq!(api.abi_version, ART3M1S_KRKR_API_ABI_VERSION);
        assert!(api.runtime_create.is_some());
    }

    #[cfg(feature = "native-bootstrap")]
    #[test]
    fn bootstrap_reports_unsupported() {
        assert_eq!(
            probe_null_path(),
            crate::protocol::ART3M1S_KRKR_STATUS_UNSUPPORTED
        );
    }

    #[cfg(feature = "native-upstream-smoke")]
    #[test]
    fn upstream_validates_probe_arguments() {
        assert_eq!(
            probe_null_path(),
            crate::protocol::ART3M1S_KRKR_STATUS_INVALID_ARGUMENT
        );
    }
}
