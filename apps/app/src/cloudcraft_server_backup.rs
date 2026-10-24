//! Integrated local server world backups with a SHA-256 integrity manifest.
//!
//! Backups are always written to a new directory and restores always target a
//! new directory, so an existing world is never overwritten or deleted.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

const BACKUP_ROOT: &str = "cloudcraft-backups";
const MANIFEST_FILE: &str = "manifest.json";
const WORLD_SUBDIR: &str = "world";
const DEFAULT_LEVEL: &str = "world";
const MANIFEST_VERSION: u32 = 1;

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

fn real_directory(path: &Path, label: &str) -> Result<PathBuf, String> {
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

fn valid_component(name: &str, label: &str) -> Result<(), String> {
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
    Ok(level)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn hash_stream(
    path: &Path,
    mut sink: Option<&mut fs::File>,
) -> Result<(u64, String), String> {
    let mut input = fs::File::open(path).map_err(|error| {
        format!("Could not read {}: {error}", path.display())
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut size = 0u64;
    loop {
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
) -> Result<(u64, String), String> {
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|error| {
            format!("Could not create {}: {error}", destination.display())
        })?;
    hash_stream(source, Some(&mut output))
}

/// Recursively copies `source` into the new `destination`, rejecting symlinks
/// and special files, and records every file in `files`.
fn copy_tree(
    source: &Path,
    destination: &Path,
    relative: &Path,
    files: &mut Vec<ManifestFile>,
) -> Result<(), String> {
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
            ));
        } else if file_type.is_dir() {
            copy_tree(&from, &to, &rel, files)?;
        } else if file_type.is_file() {
            let (size, sha256) = copy_file(&from, &to)?;
            files.push(ManifestFile {
                path: rel.to_string_lossy().replace('\\', "/"),
                size,
                sha256,
            });
        } else {
            return Err(format!(
                "Refusing to copy special file {}.",
                from.display()
            ));
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

/// Creates a verified snapshot of the server world. The caller must ensure the
/// server process is stopped so the world is consistent.
pub fn create_backup(server_dir: &str) -> Result<BackupSummary, String> {
    let server_dir = real_directory(Path::new(server_dir), "server directory")?;
    let level = level_name(&server_dir)?;
    let world = real_directory(
        &server_dir.join(&level),
        &format!("world folder '{level}'"),
    )?;

    let root = backups_root(&server_dir);
    if !root.exists() {
        fs::create_dir(&root).map_err(|error| {
            format!("Could not create backup folder: {error}")
        })?;
    }
    let root = real_directory(&root, "backup folder")?;

    let id = Utc::now().format("%Y%m%dT%H%M%S%.3fZ").to_string();
    let backup_dir = root.join(&id);
    fs::create_dir(&backup_dir)
        .map_err(|error| format!("Could not create backup {id}: {error}"))?;

    let result = (|| {
        let mut files = Vec::new();
        copy_tree(
            &world,
            &backup_dir.join(WORLD_SUBDIR),
            Path::new(""),
            &mut files,
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
        fs::write(backup_dir.join(MANIFEST_FILE), json)
            .map_err(|error| format!("Could not write manifest: {error}"))?;
        Ok(summary(&manifest))
    })();

    if result.is_err() {
        let _ = fs::remove_dir_all(&backup_dir);
    }
    result
}

fn load_manifest(
    server_dir: &Path,
    id: &str,
) -> Result<(PathBuf, BackupManifest), String> {
    valid_component(id, "The backup id")?;
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
    if !root.exists() {
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

/// Verifies every file against its recorded size and hash, and reports files
/// that are missing, unexpected, or symbolic links.
pub fn verify_backup(
    server_dir: &str,
    id: &str,
) -> Result<VerifyReport, String> {
    let server_dir = real_directory(Path::new(server_dir), "server directory")?;
    let (dir, manifest) = load_manifest(&server_dir, id)?;
    let world = dir.join(WORLD_SUBDIR);
    let mut problems = Vec::new();
    let mut expected = std::collections::HashSet::new();

    for file in &manifest.files {
        let relative = Path::new(&file.path);
        if relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        {
            problems.push(format!("Unsafe path in manifest: {}", file.path));
            continue;
        }
        let path = world.join(relative);
        expected.insert(path.clone());
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_file() => match hash_stream(&path, None) {
                Ok((size, sha)) if size == file.size && sha == file.sha256 => {}
                Ok(_) => {
                    problems.push(format!("Modified or corrupt: {}", file.path))
                }
                Err(error) => problems.push(error),
            },
            _ => problems.push(format!("Missing: {}", file.path)),
        }
    }

    let mut stack = vec![world.clone()];
    while let Some(current) = stack.pop() {
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

    Ok(VerifyReport {
        id: id.to_string(),
        ok: problems.is_empty(),
        checked_files: manifest.files.len(),
        problems,
    })
}

/// Restores a verified backup into a brand-new folder inside the server
/// directory. It never overwrites or removes the current world.
pub fn restore_backup(server_dir: &str, id: &str) -> Result<String, String> {
    let report = verify_backup(server_dir, id)?;
    if !report.ok {
        return Err(format!(
            "Backup failed integrity verification: {}",
            report.problems.join("; ")
        ));
    }
    let server_dir = real_directory(Path::new(server_dir), "server directory")?;
    let (dir, manifest) = load_manifest(&server_dir, id)?;

    let name = format!("{}-restored-{}", manifest.level_name, manifest.id);
    valid_component(&name, "The restore folder")?;
    let target = server_dir.join(&name);
    if fs::symlink_metadata(&target).is_ok() {
        return Err(format!(
            "A folder named '{name}' already exists; it was not modified."
        ));
    }

    let mut copied = Vec::new();
    if let Err(error) =
        copy_tree(&dir.join(WORLD_SUBDIR), &target, Path::new(""), &mut copied)
    {
        let _ = fs::remove_dir_all(&target);
        return Err(error);
    }
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[cfg(unix)]
    #[test]
    fn refuses_symlinks_and_cleans_partial_backup() {
        let dir = server_with_world();
        std::os::unix::fs::symlink("/etc/hostname", dir.join("world/link"))
            .unwrap();
        assert!(create_backup(dir.to_str().unwrap()).is_err());
        assert!(list_backups(dir.to_str().unwrap()).unwrap().is_empty());
        assert_eq!(fs::read_dir(backups_root(&dir)).unwrap().count(), 0);
    }
}
