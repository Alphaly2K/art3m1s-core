//! Host-owned logical resources. Implementations decide where bytes are stored.
//!
//! This is a synchronous Rust interface. Implementations must not call back into
//! the same runtime.

/// A resource view supplied by the embedding Host, without physical paths.
///
/// Keys use `/` and Artemis's ASCII case-insensitive lookup convention. The
/// runtime resolves magic paths before calling this provider. Save keys retain
/// the script's logical savepath prefix; the Host supplies the physical sandbox.
/// Calls may also originate from a legacy media worker, so providers must be
/// thread-safe. A successful write acknowledges the Host's committed bytes.
pub trait ResourceProvider: Send + Sync {
    /// `Some(0)` is an empty file; `None` is absent, and errors stay errors.
    fn query_size(&self, key: &str) -> Result<Option<u64>, String>;

    /// Reads at most `output.len()` bytes, allowing short reads and EOF (zero).
    fn read_range(&self, key: &str, offset: u64, output: &mut [u8]) -> Result<usize, String>;

    /// Atomically replaces the logical save file, or reports a failure.
    fn write(&self, _key: &str, _data: &[u8]) -> Result<(), String> {
        Err("host resource provider is read-only".into())
    }

    fn delete(&self, _key: &str) -> Result<(), String> {
        Err("host resource provider is read-only".into())
    }

    /// Legacy script calendar fields: year, month, day, hour, minute, second.
    fn file_mtime(&self, _key: &str) -> Option<[i64; 6]> {
        None
    }
}
