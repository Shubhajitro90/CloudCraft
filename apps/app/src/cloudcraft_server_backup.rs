//! Integrated local server world backups with a SHA-256 integrity manifest.
//!
//! Backups are built in a hidden staging folder and renamed into place only at
//! the commit point. Restores always target a new folder, built the same way,
//! so an existing world is never overwritten or deleted and a partially copied
//! world is never visible under its final name.

use crate::cloudcraft_server_ops::{OpError, Tracker};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

pub const BACKUP_ROOT: &str = "cloudcraft-backups";
pub const WORLD_SUBDIR: &str = "world";
const MANIFEST_FILE: &str = "manifest.json";
const DEFAULT_LEVEL: &str = "world";
const MANIFEST_VERSION: u32 = 1;
const PARTIAL_PREFIX: &str = ".partial-";
const RESTORING_PREFIX: &str = ".restoring-";
const TOTAL_PHASES: u32 = 3;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestFile {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupManifest {
    pub version: u32,
    pub id: String,
    pub level_name: String,
    pub created_at: String,
    pub total_bytes: u64,
    pub files: Vec<ManifestFile>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupSummary {
    pub id: String,
    pub level_name: String,
    pub created_at: String,
    pub total_bytes: u64,
    pub file_count: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyReport {
    pub id: String,
    pub ok: bool,
    pub checked_files: usize,
    pub problems: Vec<String>,
}

/// Measured size of a tree, gathered before any data is copied.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TreeScan {
    pub files: u64,
    pub bytes: u64,
}

pub fn real_directory(path: &Path, label: &str) -> Result<PathBuf, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("Could not access {label}: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!(
            "{label} must be an existing directory, not a file or symbolic link."
        ));
    }
    path.canonicalize()
        .map_err(|error| format!("Could not resolve {label}: {error}"))
}

pub fn valid_component(name: &str, label: &str) -> Result<(), String> {
    let mut components = Path::new(name).components();
    let ok = matches!(components.next(), Some(Component::Normal(_)))
        && components.next().is_none()
        && !name.contains(['/', '\\']);
    if ok {
        Ok(())
    } else {
        Err(format!("{label} must be a single folder name."))
    }
}

/// Reads `level-name` from server.properties, defaulting to `world`.
pub fn level_name(server_dir: &Path) -> Result<String, String> {
    let level = match fs::read_to_string(server_dir.join("server.properties")) {
        Ok(contents) => contents
            .lines()
            .filter_map(|line| line.trim().strip_prefix("level-name="))
            .next_back()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| DEFAULT_LEVEL.to_string()),
        Err(_) => DEFAULT_LEVEL.to_string(),
    };
    valid_component(&level, "The server level-name")?;
    if level == BACKUP_ROOT
        || level.starts_with(PARTIAL_PREFIX)
        || level.starts_with(RESTORING_PREFIX)
    {
        return Err(format!(
            "The server level-name '{level}' is reserved by CloudCraft."
        ));
    }
    Ok(level)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn hash_stream(
    path: &Path,
    mut sink: Option<&mut fs::File>,
    tracker: &Tracker,
) -> Result<(u64, String), OpError> {
    let mut input = fs::File::open(path).map_err(|error| {
        format!("Could not read {}: {error}", path.display())
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut size = 0u64;
    loop {
        tracker.check_cancel()?;
        let read = input.read(&mut buffer).map_err(|error| {
            format!("Could not read {}: {error}", path.display())
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        if let Some(output) = sink.as_deref_mut() {
            output.write_all(&buffer[..read]).map_err(|error| {
                format!("Could not write backup data: {error}")
            })?;
        }
        size += read as u64;
        tracker.add_bytes(read as u64);
    }
    if let Some(output) = sink {
        output
            .sync_all()
            .map_err(|error| format!("Could not flush backup data: {error}"))?;
    }
    Ok((size, hex(&hasher.finalize())))
}

fn copy_file(
    source: &Path,
    destination: &Path,
    tracker: &Tracker,
) -> Result<(u64, String), OpError> {
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|error| {
            format!("Could not create {}: {error}", destination.display())
        })?;
    hash_stream(source, Some(&mut output), tracker)
}

/// Measures a tree and rejects symlinks and special files up front, so an
/// unsafe world fails before any data is written.
pub fn scan_tree(root: &Path, tracker: &Tracker) -> Result<TreeScan, OpError> {
    let mut scan = TreeScan::default();
    let mut stack = vec![root.to_path_buf()];
    while let Some(current) = stack.pop() {
        tracker.check_cancel()?;
        let entries = fs::read_dir(&current).map_err(|error| {
            format!("Could not list {}: {error}", current.display())
        })?;
        for entry in entries {
            let entry = entry.map_err(|error| {
                format!("Could not list {}: {error}", current.display())
            })?;
            let path = entry.path();
            let file_type = entry.file_type().map_err(|error| {
                format!("Could not inspect {}: {error}", path.display())
            })?;
            if file_type.is_symlink() {
                return Err(format!(
                    "Refusing to copy symbolic link {}.",
                    path.display()
                )
                .into());
            } else if file_type.is_dir() {
                stack.push(path);
            } else if file_type.is_file() {
                let length = entry
                    .metadata()
                    .map_err(|error| {
                        format!("Could not inspect {}: {error}", path.display())
                    })?
                    .len();
                scan.files += 1;
                scan.bytes += length;
            } else {
                return Err(format!(
                    "Refusing to copy special file {}.",
                    path.display()
                )
                .into());
            }
        }
    }
    Ok(scan)
}

/// Recursively copies `source` into the new `destination`, rejecting symlinks
/// and special files, and records every file in `files`.
pub fn copy_tree(
    source: &Path,
    destination: &Path,
    relative: &Path,
    files: &mut Vec<ManifestFile>,
    tracker: &Tracker,
) -> Result<(), OpError> {
    fs::create_dir(destination).map_err(|error| {
        format!("Could not create {}: {error}", destination.display())
    })?;
    let mut entries: Vec<_> = fs::read_dir(source)
        .and_then(|iter| iter.collect::<Result<Vec<_>, _>>())
        .map_err(|error| {
            format!("Could not list {}: {error}", source.display())
        })?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        tracker.check_cancel()?;
        let name = entry.file_name();
        let from = entry.path();
        let to = destination.join(&name);
        let rel = relative.join(&name);
        let file_type = entry.file_type().map_err(|error| {
            format!("Could not inspect {}: {error}", from.display())
        })?;
        if file_type.is_symlink() {
            return Err(format!(
                "Refusing to copy symbolic link {}.",
                from.display()
            )
            .into());
        } else if file_type.is_dir() {
            copy_tree(&from, &to, &rel, files, tracker)?;
        } else if file_type.is_file() {
            let path = rel.to_string_lossy().replace('\\', "/");
            tracker.file_started(&path);
            let (size, sha256) = copy_file(&from, &to, tracker)?;
            tracker.file_done();
            files.push(ManifestFile { path, size, sha256 });
        } else {
            return Err(format!(
                "Refusing to copy special file {}.",
                from.display()
            )
            .into());
        }
    }
    Ok(())
}

pub fn backups_root(server_dir: &Path) -> PathBuf {
    server_dir.join(BACKUP_ROOT)
}

fn summary(manifest: &BackupManifest) -> BackupSummary {
    BackupSummary {
        id: manifest.id.clone(),
        level_name: manifest.level_name.clone(),
        created_at: manifest.created_at.clone(),
        total_bytes: manifest.total_bytes,
        file_count: manifest.files.len(),
    }
}

/// Removes an incomplete staging folder. If it cannot be removed the outcome
/// becomes a failure that names the leftover folder, so cleanup problems are
/// never hidden behind a "cancelled" or generic error.
pub fn discard_staging<T>(
    staging: &Path,
    result: Result<T, OpError>,
) -> Result<T, OpError> {
    let Err(original) = result else {
        return result;
    };
    if fs::symlink_metadata(staging).is_err() {
        return Err(original);
    }
    match fs::remove_dir_all(staging) {
        Ok(()) => Err(original),
        Err(error) => {
            let reason = match original {
                OpError::Cancelled => "The operation was cancelled".to_string(),
                OpError::Failed(message) => message,
            };
            Err(OpError::Failed(format!(
                "{reason}, but the incomplete folder '{}' could not be removed: {error}. It is safe to delete manually.",
                staging.file_name().unwrap_or_default().to_string_lossy()
            )))
        }
    }
}

/// Removes leftover staging folders from a crashed run. Only plain folders
/// whose names carry CloudCraft's staging prefix are touched.
pub fn remove_stale_staging(parent: &Path, prefix: &str) {
    let Ok(entries) = fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let is_plain_dir = entry
            .file_type()
            .map(|kind| kind.is_dir() && !kind.is_symlink())
            .unwrap_or(false);
        if is_plain_dir
            && entry.file_name().to_string_lossy().starts_with(prefix)
        {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

/// Creates a verified snapshot of the server world. The caller must ensure the
/// server process is stopped so the world is consistent.
pub fn create_backup(
    server_dir: &str,
    tracker: &Tracker,
) -> Result<BackupSummary, OpError> {
    let server_dir = real_directory(Path::new(server_dir), "server directory")?;
    let level = level_name(&server_dir)?;
    let world = real_directory(
        &server_dir.join(&level),
        &format!("world folder '{level}'"),
    )?;

    tracker.phase("Scanning world", 1, TOTAL_PHASES, 0, 0, true);
    let scan = scan_tree(&world, tracker)?;

    let root = backups_root(&server_dir);
    if fs::symlink_metadata(&root).is_err() {
        fs::create_dir(&root).map_err(|error| {
            format!("Could not create backup folder: {error}")
        })?;
    }
    let root = real_directory(&root, "backup folder")?;
    remove_stale_staging(&root, PARTIAL_PREFIX);

    let id = Utc::now().format("%Y%m%dT%H%M%S%.3fZ").to_string();
    let staging = root.join(format!("{PARTIAL_PREFIX}{id}"));
    let final_dir = root.join(&id);
    if fs::symlink_metadata(&final_dir).is_ok() {
        return Err(format!("Backup {id} already exists.").into());
    }
    fs::create_dir(&staging)
        .map_err(|error| format!("Could not create backup {id}: {error}"))?;

    let result = (|| -> Result<BackupSummary, OpError> {
        tracker.phase(
            "Copying world",
            2,
            TOTAL_PHASES,
            scan.bytes,
            scan.files,
            true,
        );
        let mut files = Vec::new();
        copy_tree(
            &world,
            &staging.join(WORLD_SUBDIR),
            Path::new(""),
            &mut files,
            tracker,
        )?;
        let manifest = BackupManifest {
            version: MANIFEST_VERSION,
            id: id.clone(),
            level_name: level.clone(),
            created_at: Utc::now().to_rfc3339(),
            total_bytes: files.iter().map(|file| file.size).sum(),
            files,
        };
        let json = serde_json::to_string_pretty(&manifest)
            .map_err(|error| format!("Could not write manifest: {error}"))?;
        fs::write(staging.join(MANIFEST_FILE), json)
            .map_err(|error| format!("Could not write manifest: {error}"))?;

        tracker.begin_commit("Finalizing backup")?;
        fs::rename(&staging, &final_dir).map_err(|error| {
            OpError::Failed(format!("Could not finalize backup {id}: {error}"))
        })?;
        Ok(summary(&manifest))
    })();
    discard_staging(&staging, result)
}

pub fn load_manifest(
    server_dir: &Path,
    id: &str,
) -> Result<(PathBuf, BackupManifest), String> {
    valid_component(id, "The backup id")?;
    if id.starts_with('.') {
        return Err("The backup id is not valid.".into());
    }
    let root = real_directory(&backups_root(server_dir), "backup folder")?;
    let dir = real_directory(&root.join(id), "backup")?;
    if !dir.starts_with(&root) {
        return Err("The backup is outside the backup folder.".into());
    }
    let contents = fs::read_to_string(dir.join(MANIFEST_FILE))
        .map_err(|error| format!("Could not read backup manifest: {error}"))?;
    let manifest: BackupManifest = serde_json::from_str(&contents)
        .map_err(|error| format!("The backup manifest is invalid: {error}"))?;
    if manifest.version != MANIFEST_VERSION || manifest.id != id {
        return Err(
            "The backup manifest is not supported or does not match its folder."
                .into(),
        );
    }
    Ok((dir, manifest))
}

pub fn list_backups(server_dir: &str) -> Result<Vec<BackupSummary>, String> {
    let server_dir = real_directory(Path::new(server_dir), "server directory")?;
    let root = backups_root(&server_dir);
    if fs::symlink_metadata(&root).is_err() {
        return Ok(Vec::new());
    }
    let root = real_directory(&root, "backup folder")?;
    let mut backups = Vec::new();
    for entry in fs::read_dir(&root)
        .map_err(|error| format!("Could not list backups: {error}"))?
        .flatten()
    {
        let Ok(id) = entry.file_name().into_string() else {
            continue;
        };
        if let Ok((_, manifest)) = load_manifest(&server_dir, &id) {
            backups.push(summary(&manifest));
        }
    }
    backups.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(backups)
}

fn verify_inner(
    server_dir: &Path,
    id: &str,
    tracker: &Tracker,
    phase_index: u32,
    phase_count: u32,
) -> Result<(VerifyReport, BackupManifest, PathBuf), OpError> {
    let (dir, manifest) = load_manifest(server_dir, id)?;
    let world = dir.join(WORLD_SUBDIR);
    let mut problems = Vec::new();
    let mut expected = std::collections::HashSet::new();

    tracker.phase(
        "Verifying backup files",
        phase_index,
        phase_count,
        manifest.total_bytes,
        manifest.files.len() as u64,
        true,
    );

    for file in &manifest.files {
        tracker.check_cancel()?;
        let relative = Path::new(&file.path);
        if relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        {
            problems.push(format!("Unsafe path in manifest: {}", file.path));
            continue;
        }
        let path = world.join(relative);
        if !expected.insert(path.clone()) {
            problems.push(format!("Duplicate manifest entry: {}", file.path));
            continue;
        }
        tracker.file_started(&file.path);
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_file() => {
                match hash_stream(&path, None, tracker) {
                    Ok((size, sha))
                        if size == file.size && sha == file.sha256 => {}
                    Ok(_) => problems
                        .push(format!("Modified or corrupt: {}", file.path)),
                    Err(OpError::Cancelled) => return Err(OpError::Cancelled),
                    Err(OpError::Failed(error)) => problems.push(error),
                }
            }
            _ => problems.push(format!("Missing: {}", file.path)),
        }
        tracker.file_done();
    }

    let mut stack = vec![world.clone()];
    while let Some(current) = stack.pop() {
        tracker.check_cancel()?;
        let Ok(entries) = fs::read_dir(&current) else {
            problems.push(format!("Unreadable folder: {}", current.display()));
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(kind) if kind.is_symlink() => {
                    problems.push(format!("Symbolic link: {}", path.display()))
                }
                Ok(kind) if kind.is_dir() => stack.push(path),
                Ok(kind) if kind.is_file() && !expected.contains(&path) => {
                    problems.push(format!(
                        "Unexpected file: {}",
                        path.strip_prefix(&world).unwrap_or(&path).display()
                    ));
                }
                _ => {}
            }
        }
    }

    let report = VerifyReport {
        id: id.to_string(),
        ok: problems.is_empty(),
        checked_files: manifest.files.len(),
        problems,
    };
    Ok((report, manifest, dir))
}

/// Verifies every file against its recorded size and hash, and reports files
/// that are missing, unexpected, or symbolic links. A cancelled verification
/// returns `Cancelled`; it is never reported as passed or failed.
pub fn verify_backup(
    server_dir: &str,
    id: &str,
    tracker: &Tracker,
) -> Result<VerifyReport, OpError> {
    let server_dir = real_directory(Path::new(server_dir), "server directory")?;
    verify_inner(&server_dir, id, tracker, 1, 1).map(|(report, _, _)| report)
}

/// Restores a verified backup into a brand-new folder inside the server
/// directory. It builds the world in a hidden staging folder and renames it
/// into place only at the commit point, so a partial world is never exposed.
/// It never overwrites or removes the current world.
pub fn restore_backup(
    server_dir: &str,
    id: &str,
    tracker: &Tracker,
) -> Result<String, OpError> {
    let server_dir = real_directory(Path::new(server_dir), "server directory")?;
    let (report, manifest, dir) =
        verify_inner(&server_dir, id, tracker, 1, TOTAL_PHASES)?;
    if !report.ok {
        return Err(format!(
            "Backup failed integrity verification: {}",
            report.problems.join("; ")
        )
        .into());
    }

    let name = format!("{}-restored-{}", manifest.level_name, manifest.id);
    valid_component(&name, "The restore folder")?;
    let target = server_dir.join(&name);
    if fs::symlink_metadata(&target).is_ok() {
        return Err(format!(
            "A folder named '{name}' already exists; it was not modified."
        )
        .into());
    }
    remove_stale_staging(&server_dir, RESTORING_PREFIX);
    let staging = server_dir.join(format!("{RESTORING_PREFIX}{name}"));

    tracker.phase(
        "Copying world",
        2,
        TOTAL_PHASES,
        manifest.total_bytes,
        manifest.files.len() as u64,
        true,
    );
    let result = (|| -> Result<String, OpError> {
        let mut copied = Vec::new();
        copy_tree(
            &dir.join(WORLD_SUBDIR),
            &staging,
            Path::new(""),
            &mut copied,
            tracker,
        )?;

        let expected: HashMap<&str, &ManifestFile> = manifest
            .files
            .iter()
            .map(|file| (file.path.as_str(), file))
            .collect();
        let matches_manifest = copied.len() == expected.len()
            && copied.iter().all(|file| {
                expected.get(file.path.as_str()).is_some_and(|want| {
                    want.size == file.size && want.sha256 == file.sha256
                })
            });
        if !matches_manifest {
            return Err(OpError::Failed(
                "The copied world does not match the backup manifest; the backup changed during restore.".into(),
            ));
        }

        tracker.begin_commit("Finalizing restore")?;
        if fs::symlink_metadata(&target).is_ok() {
            return Err(OpError::Failed(format!(
                "A folder named '{name}' appeared during restore; it was not modified."
            )));
        }
        fs::rename(&staging, &target).map_err(|error| {
            OpError::Failed(format!("Could not finalize restore: {error}"))
        })?;
        Ok(name.clone())
    })();
    discard_staging(&staging, result)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::cloudcraft_server_ops::{OperationKind, Registry};

    fn t() -> Tracker {
        Registry::new().begin(OperationKind::Backup).unwrap()
    }

    fn create_backup(server: &str) -> Result<BackupSummary, String> {
        super::create_backup(server, &t()).map_err(String::from)
    }

    fn verify_backup(server: &str, id: &str) -> Result<VerifyReport, String> {
        super::verify_backup(server, id, &t()).map_err(String::from)
    }

    fn restore_backup(server: &str, id: &str) -> Result<String, String> {
        super::restore_backup(server, id, &t()).map_err(String::from)
    }

    fn temp_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "cloudcraft-backup-{label}-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&path).unwrap();
        path.canonicalize().unwrap()
    }

    fn server_with_world() -> PathBuf {
        let dir = temp_dir("server");
        fs::create_dir_all(dir.join("world/region")).unwrap();
        fs::write(dir.join("world/level.dat"), b"level").unwrap();
        fs::write(dir.join("world/region/r.0.0.mca"), b"region-data").unwrap();
        dir
    }

    #[test]
    fn backup_verify_and_restore_round_trip() {
        let dir = server_with_world();
        let server = dir.to_str().unwrap();
        let backup = create_backup(server).unwrap();
        assert_eq!(backup.file_count, 2);
        assert_eq!(list_backups(server).unwrap().len(), 1);
        assert!(verify_backup(server, &backup.id).unwrap().ok);

        let restored = restore_backup(server, &backup.id).unwrap();
        assert_eq!(
            fs::read(dir.join(&restored).join("region/r.0.0.mca")).unwrap(),
            b"region-data"
        );
        assert!(restore_backup(server, &backup.id).is_err());
        assert_eq!(fs::read(dir.join("world/level.dat")).unwrap(), b"level");
    }

    #[test]
    fn tampered_backup_fails_verification_and_restore() {
        let dir = server_with_world();
        let server = dir.to_str().unwrap();
        let backup = create_backup(server).unwrap();
        let file = backups_root(&dir).join(&backup.id).join("world/level.dat");
        fs::write(&file, b"tampered").unwrap();
        fs::write(file.with_file_name("extra.dat"), b"x").unwrap();

        let report = verify_backup(server, &backup.id).unwrap();
        assert!(!report.ok);
        assert_eq!(report.problems.len(), 2);
        assert!(restore_backup(server, &backup.id).is_err());
        assert!(!dir.join(format!("world-restored-{}", backup.id)).exists());
    }

    #[test]
    fn rejects_traversal_in_level_name_and_backup_id() {
        let dir = server_with_world();
        fs::write(dir.join("server.properties"), "level-name=../outside\n")
            .unwrap();
        assert!(create_backup(dir.to_str().unwrap()).is_err());
        fs::write(dir.join("server.properties"), "level-name=world\n").unwrap();
        assert!(create_backup(dir.to_str().unwrap()).is_ok());
        assert!(verify_backup(dir.to_str().unwrap(), "../x").is_err());
    }

    #[test]
    fn missing_backup_file_is_reported() {
        let dir = server_with_world();
        let server = dir.to_str().unwrap();
        let backup = create_backup(server).unwrap();
        fs::remove_file(
            backups_root(&dir).join(&backup.id).join("world/level.dat"),
        )
        .unwrap();
        let report = verify_backup(server, &backup.id).unwrap();
        assert!(!report.ok);
        assert!(report.problems.iter().any(|p| p.starts_with("Missing")));
        assert!(restore_backup(server, &backup.id).is_err());
    }

    #[test]
    fn extra_backup_file_is_reported() {
        let dir = server_with_world();
        let server = dir.to_str().unwrap();
        let backup = create_backup(server).unwrap();
        fs::write(
            backups_root(&dir).join(&backup.id).join("world/extra"),
            b"x",
        )
        .unwrap();
        let report = verify_backup(server, &backup.id).unwrap();
        assert!(report.problems.iter().any(|p| p.starts_with("Unexpected")));
    }

    #[test]
    fn malformed_or_mismatched_manifest_is_rejected() {
        let dir = server_with_world();
        let server = dir.to_str().unwrap();
        let backup = create_backup(server).unwrap();
        let manifest = backups_root(&dir).join(&backup.id).join(MANIFEST_FILE);

        fs::write(&manifest, "not json").unwrap();
        assert!(verify_backup(server, &backup.id).is_err());
        assert!(list_backups(server).unwrap().is_empty());

        fs::write(
            &manifest,
            r#"{"version":99,"id":"x","levelName":"world","createdAt":"","totalBytes":0,"files":[]}"#,
        )
        .unwrap();
        assert!(verify_backup(server, &backup.id).is_err());
    }

    #[test]
    fn unsafe_and_duplicate_manifest_paths_are_reported() {
        let dir = server_with_world();
        let server = dir.to_str().unwrap();
        let backup = create_backup(server).unwrap();
        let manifest_path =
            backups_root(&dir).join(&backup.id).join(MANIFEST_FILE);
        let mut manifest: BackupManifest =
            serde_json::from_str(&fs::read_to_string(&manifest_path).unwrap())
                .unwrap();
        let first = manifest.files[0].clone();
        manifest.files.push(first);
        manifest.files.push(ManifestFile {
            path: "../escape".into(),
            size: 0,
            sha256: String::new(),
        });
        fs::write(&manifest_path, serde_json::to_string(&manifest).unwrap())
            .unwrap();

        let report = verify_backup(server, &backup.id).unwrap();
        assert!(report.problems.iter().any(|p| p.starts_with("Duplicate")));
        assert!(report.problems.iter().any(|p| p.starts_with("Unsafe")));
        assert!(restore_backup(server, &backup.id).is_err());
    }

    #[test]
    fn restore_never_overwrites_an_existing_destination() {
        let dir = server_with_world();
        let server = dir.to_str().unwrap();
        let backup = create_backup(server).unwrap();
        let target = dir.join(format!("world-restored-{}", backup.id));
        fs::create_dir(&target).unwrap();
        fs::write(target.join("keep.txt"), b"keep").unwrap();

        assert!(restore_backup(server, &backup.id).is_err());
        assert_eq!(fs::read(target.join("keep.txt")).unwrap(), b"keep");
    }

    #[test]
    fn reserved_level_name_is_rejected() {
        let dir = server_with_world();
        fs::write(
            dir.join("server.properties"),
            format!("level-name={BACKUP_ROOT}\n"),
        )
        .unwrap();
        assert!(create_backup(dir.to_str().unwrap()).is_err());
    }

    #[test]
    fn missing_world_or_server_directory_fails_without_artifacts() {
        let dir = temp_dir("empty");
        assert!(create_backup(dir.to_str().unwrap()).is_err());
        assert!(
            !backups_root(&dir).exists()
                || list_backups(dir.to_str().unwrap()).unwrap().is_empty()
        );
        assert!(create_backup(dir.join("nope").to_str().unwrap()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlinks_before_writing_anything() {
        let dir = server_with_world();
        std::os::unix::fs::symlink("/etc/hostname", dir.join("world/link"))
            .unwrap();
        assert!(create_backup(dir.to_str().unwrap()).is_err());
        assert!(list_backups(dir.to_str().unwrap()).unwrap().is_empty());
        assert!(!backups_root(&dir).exists());
    }

    fn visible_entries(path: &Path) -> Vec<String> {
        fs::read_dir(path)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect()
    }

    fn big_world(megabytes: usize) -> PathBuf {
        let dir = server_with_world();
        let chunk = vec![7u8; 1024 * 1024];
        let mut file = fs::File::create(dir.join("world/big.dat")).unwrap();
        for _ in 0..megabytes {
            file.write_all(&chunk).unwrap();
        }
        dir
    }

    /// Runs `work` and requests cancellation as soon as real bytes were
    /// processed. Returns the result, or `None` if the work finished first.
    fn cancel_mid_flight<T: Send + 'static>(
        kind: OperationKind,
        work: impl FnOnce(&Tracker) -> Result<T, OpError> + Send + 'static,
    ) -> Option<Result<T, OpError>> {
        let registry = Registry::new();
        let tracker = registry.begin(kind).unwrap();
        let watcher = registry.clone();
        let handle = std::thread::spawn(move || {
            let result = work(&tracker);
            tracker.finish(&result, "done");
            result
        });
        let mut cancelled = false;
        while !handle.is_finished() {
            if let Some(snapshot) = watcher.snapshot() {
                if snapshot.done_bytes > 0 && snapshot.cancellable {
                    cancelled = watcher.request_cancel().is_ok();
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_micros(200));
        }
        let result = handle.join().unwrap();
        cancelled.then_some(result)
    }

    #[test]
    fn progress_totals_come_from_the_measured_world() {
        let dir = server_with_world();
        let registry = Registry::new();
        let tracker = registry.begin(OperationKind::Backup).unwrap();
        let summary =
            super::create_backup(dir.to_str().unwrap(), &tracker).unwrap();
        let snapshot = registry.snapshot().unwrap();
        assert_eq!(snapshot.total_bytes, summary.total_bytes);
        assert_eq!(snapshot.total_files, summary.file_count as u64);
        assert_eq!(snapshot.done_bytes, summary.total_bytes);
        assert_eq!(snapshot.done_files, summary.file_count as u64);
        assert_eq!(snapshot.phase_count, 3);
    }

    #[test]
    fn cancelled_backup_leaves_no_artifacts() {
        let dir = big_world(96);
        let path = dir.to_string_lossy().to_string();
        let mut observed = false;
        for _ in 0..5 {
            let server = path.clone();
            let outcome = cancel_mid_flight(OperationKind::Backup, move |t| {
                super::create_backup(&server, t)
            });
            if let Some(result) = outcome {
                if matches!(result, Err(OpError::Cancelled)) {
                    observed = true;
                    assert!(visible_entries(&backups_root(&dir)).is_empty());
                    assert!(list_backups(&path).unwrap().is_empty());
                    break;
                }
                let _ = fs::remove_dir_all(backups_root(&dir));
            }
        }
        assert!(observed, "never managed to cancel a backup in flight");
    }

    #[test]
    fn cancelled_restore_exposes_no_partial_world() {
        let dir = big_world(96);
        let path = dir.to_string_lossy().to_string();
        let backup = create_backup(&path).unwrap();
        let before = visible_entries(&dir);
        let mut observed = false;
        for _ in 0..5 {
            let (server, id) = (path.clone(), backup.id.clone());
            let outcome = cancel_mid_flight(OperationKind::Restore, move |t| {
                super::restore_backup(&server, &id, t)
            });
            if let Some(Err(OpError::Cancelled)) = outcome {
                observed = true;
                let mut after = visible_entries(&dir);
                let mut expected = before.clone();
                after.sort();
                expected.sort();
                assert_eq!(after, expected);
                break;
            }
            let restored = format!("world-restored-{}", backup.id);
            let _ = fs::remove_dir_all(dir.join(restored));
        }
        assert!(observed, "never managed to cancel a restore in flight");
    }

    #[test]
    fn cancelled_verify_is_neither_passed_nor_failed() {
        let dir = big_world(96);
        let path = dir.to_string_lossy().to_string();
        let backup = create_backup(&path).unwrap();
        let mut observed = false;
        for _ in 0..5 {
            let (server, id) = (path.clone(), backup.id.clone());
            let outcome = cancel_mid_flight(OperationKind::Verify, move |t| {
                super::verify_backup(&server, &id, t)
            });
            if let Some(result) = outcome {
                if result.is_err() {
                    assert_eq!(result.unwrap_err(), OpError::Cancelled);
                    observed = true;
                    break;
                }
            }
        }
        assert!(observed, "never managed to cancel a verification");
        assert!(verify_backup(&path, &backup.id).unwrap().ok);
    }

    #[test]
    fn cancel_requested_before_work_starts_is_honored() {
        let dir = server_with_world();
        let registry = Registry::new();
        let tracker = registry.begin(OperationKind::Backup).unwrap();
        tracker.phase("Scanning world", 1, 3, 0, 0, true);
        registry.request_cancel().unwrap();
        let result = super::create_backup(dir.to_str().unwrap(), &tracker);
        assert_eq!(result.unwrap_err(), OpError::Cancelled);
        assert!(list_backups(dir.to_str().unwrap()).unwrap().is_empty());
    }

    #[test]
    fn backups_appear_atomically_and_stale_staging_is_ignored_and_cleaned() {
        let dir = server_with_world();
        let server = dir.to_str().unwrap();
        let root = backups_root(&dir);
        fs::create_dir_all(root.join(".partial-crashed/world")).unwrap();
        fs::write(root.join(".partial-crashed/world/level.dat"), b"x").unwrap();
        assert!(list_backups(server).unwrap().is_empty());
        assert!(verify_backup(server, ".partial-crashed").is_err());
        assert!(restore_backup(server, ".partial-crashed").is_err());

        let backup = create_backup(server).unwrap();
        assert_eq!(visible_entries(&root), vec![backup.id.clone()]);
        assert!(!dir.join(format!("world-restored-{}", backup.id)).exists());
    }

    #[test]
    fn stale_restore_staging_is_cleaned_and_restore_still_succeeds() {
        let dir = server_with_world();
        let server = dir.to_str().unwrap();
        let backup = create_backup(server).unwrap();
        let stale = dir.join(".restoring-world-restored-crashed");
        fs::create_dir_all(&stale).unwrap();
        let name = restore_backup(server, &backup.id).unwrap();
        assert!(!stale.exists());
        assert!(dir.join(&name).join("level.dat").exists());
        assert!(!visible_entries(&dir)
            .iter()
            .any(|entry| entry.starts_with(".restoring-")));
    }

    #[test]
    fn restore_failure_removes_staging_and_keeps_original_world() {
        let dir = server_with_world();
        let server = dir.to_str().unwrap();
        let backup = create_backup(server).unwrap();
        let tampered = backups_root(&dir)
            .join(&backup.id)
            .join("world/region/r.0.0.mca");
        fs::write(tampered, b"corrupted").unwrap();
        assert!(restore_backup(server, &backup.id).is_err());
        assert!(!visible_entries(&dir)
            .iter()
            .any(|entry| entry.contains("restored") || entry.starts_with('.')));
        assert_eq!(fs::read(dir.join("world/level.dat")).unwrap(), b"level");
    }
}
