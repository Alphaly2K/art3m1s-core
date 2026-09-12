//! Lightweight project detection without opening or decrypting archives.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use crate::protocol::{
    ART3M1S_KRKR_PROBE_DATA_XP3, ART3M1S_KRKR_PROBE_ROOT_XP3, ART3M1S_KRKR_PROBE_STARTUP_TJS,
    ART3M1S_KRKR_PROBE_SYSTEM_INITIALIZE_TJS,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KrkrEvidenceKind {
    DataXp3,
    RootXp3,
    StartupTjs,
    PatchTjs,
    SystemInitializeTjs,
}

impl KrkrEvidenceKind {
    pub const fn abi_value(self) -> u32 {
        match self {
            Self::DataXp3 => ART3M1S_KRKR_PROBE_DATA_XP3,
            Self::RootXp3 => ART3M1S_KRKR_PROBE_ROOT_XP3,
            Self::StartupTjs => ART3M1S_KRKR_PROBE_STARTUP_TJS,
            Self::SystemInitializeTjs => ART3M1S_KRKR_PROBE_SYSTEM_INITIALIZE_TJS,
            Self::PatchTjs => 0,
        }
    }

    const fn preference(self) -> u8 {
        match self {
            Self::DataXp3 => 0,
            Self::RootXp3 => 1,
            Self::StartupTjs => 2,
            Self::SystemInitializeTjs => 3,
            Self::PatchTjs => 4,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KrkrEvidence {
    pub kind: KrkrEvidenceKind,
    pub path: PathBuf,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KrkrProbe {
    pub evidence: Vec<KrkrEvidence>,
    pub preferred_entry: Option<KrkrEvidence>,
}

impl KrkrProbe {
    pub fn is_krkr(&self) -> bool {
        self.preferred_entry.is_some() || !self.evidence.is_empty()
    }

    pub fn has(&self, kind: KrkrEvidenceKind) -> bool {
        self.evidence.iter().any(|entry| entry.kind == kind)
    }

    pub fn root_xp3_count(&self) -> usize {
        self.evidence
            .iter()
            .filter(|entry| entry.kind == KrkrEvidenceKind::RootXp3)
            .count()
    }
}

#[derive(Debug)]
pub enum KrkrProbeError {
    Metadata {
        path: PathBuf,
        source: std::io::Error,
    },
    ReadDirectory {
        path: PathBuf,
        source: std::io::Error,
    },
    UnsupportedFileType {
        path: PathBuf,
    },
}

impl fmt::Display for KrkrProbeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Metadata { path, source } => {
                write!(f, "failed to inspect {}: {source}", path.display())
            }
            Self::ReadDirectory { path, source } => {
                write!(f, "failed to read {}: {source}", path.display())
            }
            Self::UnsupportedFileType { path } => {
                write!(
                    f,
                    "{} is neither a directory nor a regular file",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for KrkrProbeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Metadata { source, .. } | Self::ReadDirectory { source, .. } => Some(source),
            Self::UnsupportedFileType { .. } => None,
        }
    }
}

pub fn probe_project(path: &Path) -> Result<KrkrProbe, KrkrProbeError> {
    let metadata = fs::metadata(path).map_err(|source| KrkrProbeError::Metadata {
        path: path.to_path_buf(),
        source,
    })?;

    if metadata.is_file() {
        return Ok(probe_xp3_file(path));
    }
    if !metadata.is_dir() {
        return Err(KrkrProbeError::UnsupportedFileType {
            path: path.to_path_buf(),
        });
    }

    probe_directory(path)
}

fn probe_xp3_file(path: &Path) -> KrkrProbe {
    if !has_extension(path, "xp3") {
        return KrkrProbe::default();
    }

    let kind = if file_name_eq(path, "data.xp3") {
        KrkrEvidenceKind::DataXp3
    } else {
        KrkrEvidenceKind::RootXp3
    };
    let evidence = vec![KrkrEvidence {
        kind,
        path: path.to_path_buf(),
    }];
    KrkrProbe {
        preferred_entry: evidence.first().cloned(),
        evidence,
    }
}

fn probe_directory(root: &Path) -> Result<KrkrProbe, KrkrProbeError> {
    let entries = fs::read_dir(root).map_err(|source| KrkrProbeError::ReadDirectory {
        path: root.to_path_buf(),
        source,
    })?;

    let mut evidence = Vec::new();
    let mut system_dir = None;

    for entry in entries {
        let entry = entry.map_err(|source| KrkrProbeError::ReadDirectory {
            path: root.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|source| KrkrProbeError::Metadata {
                path: path.clone(),
                source,
            })?;

        if file_type.is_dir() {
            if file_name_eq(&path, "system") {
                system_dir = Some(path);
            }
            continue;
        }
        if !file_type.is_file() {
            continue;
        }

        if file_name_eq(&path, "data.xp3") {
            evidence.push(KrkrEvidence {
                kind: KrkrEvidenceKind::DataXp3,
                path,
            });
        } else if has_extension(&path, "xp3") {
            evidence.push(KrkrEvidence {
                kind: KrkrEvidenceKind::RootXp3,
                path,
            });
        } else if file_name_eq(&path, "startup.tjs") {
            evidence.push(KrkrEvidence {
                kind: KrkrEvidenceKind::StartupTjs,
                path,
            });
        } else if file_name_eq(&path, "patch.tjs") {
            evidence.push(KrkrEvidence {
                kind: KrkrEvidenceKind::PatchTjs,
                path,
            });
        }
    }

    if let Some(system_dir) = system_dir
        && let Some(path) = find_child_case_insensitive(&system_dir, "Initialize.tjs")?
    {
        evidence.push(KrkrEvidence {
            kind: KrkrEvidenceKind::SystemInitializeTjs,
            path,
        });
    }

    evidence.sort_by(|left, right| {
        left.kind
            .preference()
            .cmp(&right.kind.preference())
            .then_with(|| left.path.cmp(&right.path))
    });

    Ok(KrkrProbe {
        preferred_entry: evidence.first().cloned(),
        evidence,
    })
}

fn find_child_case_insensitive(
    directory: &Path,
    expected: &str,
) -> Result<Option<PathBuf>, KrkrProbeError> {
    let entries = fs::read_dir(directory).map_err(|source| KrkrProbeError::ReadDirectory {
        path: directory.to_path_buf(),
        source,
    })?;

    for entry in entries {
        let entry = entry.map_err(|source| KrkrProbeError::ReadDirectory {
            path: directory.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|source| KrkrProbeError::Metadata {
                path: path.clone(),
                source,
            })?;
        if file_type.is_file() && file_name_eq(&path, expected) {
            return Ok(Some(path));
        }
    }

    Ok(None)
}

fn file_name_eq(path: &Path, expected: &str) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case(expected))
}

fn has_extension(path: &Path, expected: &str) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case(expected))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("art3m1s-krkr-{label}-{}-{id}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn prefers_data_xp3_and_collects_other_evidence() {
        let root = TestDir::new("data-xp3");
        fs::write(root.path().join("data.xp3"), b"xp3").unwrap();
        fs::write(root.path().join("patch.tjs"), b"patch").unwrap();
        fs::create_dir(root.path().join("system")).unwrap();
        fs::write(root.path().join("system/Initialize.tjs"), b"init").unwrap();

        let probe = probe_project(root.path()).unwrap();
        assert!(probe.is_krkr());
        assert!(probe.has(KrkrEvidenceKind::DataXp3));
        assert!(probe.has(KrkrEvidenceKind::PatchTjs));
        assert!(probe.has(KrkrEvidenceKind::SystemInitializeTjs));
        assert_eq!(
            probe.preferred_entry.unwrap().kind,
            KrkrEvidenceKind::DataXp3
        );
    }

    #[test]
    fn detects_names_case_insensitively() {
        let root = TestDir::new("case-insensitive");
        fs::write(root.path().join("ROOT.XP3"), b"xp3").unwrap();
        fs::write(root.path().join("STARTUP.TJS"), b"startup").unwrap();
        let system = root.path().join("SyStEm");
        fs::create_dir(&system).unwrap();
        fs::write(system.join("initialize.TJS"), b"init").unwrap();

        let probe = probe_project(root.path()).unwrap();
        assert_eq!(probe.root_xp3_count(), 1);
        assert!(probe.has(KrkrEvidenceKind::StartupTjs));
        assert!(probe.has(KrkrEvidenceKind::SystemInitializeTjs));
        assert_eq!(
            probe.preferred_entry.unwrap().kind,
            KrkrEvidenceKind::RootXp3
        );
    }

    #[test]
    fn non_krkr_directory_is_not_misidentified() {
        let root = TestDir::new("not-krkr");
        fs::write(root.path().join("system.ini"), b"[WINDOWS]").unwrap();
        fs::write(root.path().join("boot.iet"), b"boot").unwrap();

        let probe = probe_project(root.path()).unwrap();
        assert!(!probe.is_krkr());
        assert!(probe.evidence.is_empty());
    }

    #[test]
    fn detects_xp3_file_input() {
        let root = TestDir::new("xp3-file");
        let archive = root.path().join("game.xp3");
        fs::write(&archive, b"xp3").unwrap();

        let probe = probe_project(&archive).unwrap();
        assert!(probe.is_krkr());
        assert_eq!(
            probe.preferred_entry.unwrap().kind,
            KrkrEvidenceKind::RootXp3
        );
    }
}
