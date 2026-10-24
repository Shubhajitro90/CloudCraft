//! Safe deletion of CloudCraft-managed local server resources.
//!
//! Stopping a server process and deleting server data are separate actions.
//! Three independent targets exist, and each removes only what it names:
//!
//! * `Configuration` removes CloudCraft's `cloudcraft-server.json` registration.
//!   The world, backups, server JAR, `server.properties` and `eula.txt` stay.
//! * `World` removes the single world folder named by `level-name`, and only in
//!   a directory CloudCraft manages, only when it is a real Minecraft world.
//! * `Backup` removes one backup folder from `cloudcraft-backups`.
//!
//! The server folder itself, other folders, and unrelated instances are never
//! removed. Every request needs the exact resource name typed back, is refused
//! while the server is active or its state is uncertain, and refuses symbolic
//! links and special files before removing anything.

use crate::cloudcraft_server_backup::{
    backups_root, level_name, list_backups, load_manifest, real_directory,
    scan_tree, valid_component, BackupSummary, BACKUP_ROOT,
};
use crate::cloudcraft_server_ops::{OpError, OperationKind, Registry, Tracker};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

pub const CONFIG_FILE: &str = "cloudcraft-server.json";
const MAX_REPORTED_FAILURES: usize = 20;

/// What CloudCraft could establish about the local server process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerObservation {
    Stopped,
    Active,
    Uncertain(String),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum DeleteTarget {
    Configuration,
    World,
    Backup { id: String },
}

impl DeleteTarget {
    fn label(&self) -> &'static str {
        match self {
            Self::Configuration => "configuration",
            Self::World => "world",
            Self::Backup { .. } => "backup",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeletionPlan {
    pub server_name: String,
    pub level_name: Option<String>,
    pub level_name_error: Option<String>,
    pub world_exists: bool,
    pub world_files: u64,
    pub world_bytes: u64,
    pub world_blocker: Option<String>,
    pub backups: Vec<BackupSummary>,
    pub backup_bytes: u64,
    pub configuration_removes: Vec<String>,
    pub configuration_retains: Vec<String>,
    pub blocked_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeletionResult {
    pub target: String,
    pub name: String,
    pub complete: bool,
    pub deleted_files: u64,
    pub deleted_bytes: u64,
    pub failures: Vec<String>,
    pub retained: Vec<String>,
    pub message: String,
}

#[derive(Deserialize)]
struct ManagedMarker {
    name: String,
}

fn check_observation(observation: &ServerObservation) -> Result<(), String> {
    match observation {
        ServerObservation::Stopped => Ok(()),
        ServerObservation::Active => Err(
            "The local server is running or stopping. Stop it first. Stopping a server only ends its process; deleting removes files, and nothing was deleted."
                .into(),
        ),
        ServerObservation::Uncertain(reason) => Err(format!(
            "CloudCraft cannot confirm the server is stopped ({reason}). Nothing was deleted."
        )),
    }
}

/// Proves the folder is a CloudCraft-managed server and returns its name.
fn managed_server(server_dir: &Path) -> Result<String, String> {
    let marker = server_dir.join(CONFIG_FILE);
    let metadata = fs::symlink_metadata(&marker).map_err(|_| {
        "This folder has no CloudCraft server configuration, so CloudCraft will not delete anything in it."
            .to_string()
    })?;
    if !metadata.is_file() {
        return Err(
            "The CloudCraft server configuration is not a regular file.".into(),
        );
    }
    let contents = fs::read_to_string(&marker).map_err(|error| {
        format!("Could not read the CloudCraft server configuration: {error}")
    })?;
    let parsed: ManagedMarker =
        serde_json::from_str(&contents).map_err(|error| {
            format!("The CloudCraft server configuration is invalid: {error}")
        })?;
    Ok(parsed.name)
}

fn detached_tracker() -> Result<Tracker, String> {
    Registry::new().begin(OperationKind::Delete)
}

struct WorldCheck {
    name: String,
    files: u64,
    bytes: u64,
}

/// Validates that the world folder is safe to delete and measures it.
fn check_world(
    server_dir: &Path,
    tracker: &Tracker,
) -> Result<WorldCheck, String> {
    let level = level_name(server_dir)?;
    let path = server_dir.join(&level);
    let world = real_directory(&path, &format!("world folder '{level}'"))?;
    if world.parent() != Some(server_dir) {
        return Err(format!(
            "The world folder '{level}' is not directly inside the server folder."
        ));
    }
    let level_dat = fs::symlink_metadata(world.join("level.dat"));
    if !level_dat.is_ok_and(|meta| meta.is_file()) {
        return Err(format!(
            "The folder '{level}' does not contain level.dat, so CloudCraft does not treat it as a Minecraft world and will not delete it."
        ));
    }
    let scan = scan_tree(&world, tracker).map_err(String::from)?;
    Ok(WorldCheck {
        name: level,
        files: scan.files,
        bytes: scan.bytes,
    })
}

/// Read-only summary of what each deletion would remove and keep.
pub fn plan(
    server_dir: &str,
    observation: &ServerObservation,
) -> Result<DeletionPlan, String> {
    let server_dir = real_directory(Path::new(server_dir), "server directory")?;
    let server_name = managed_server(&server_dir)?;
    let tracker = detached_tracker()?;

    let (level, level_error) = match level_name(&server_dir) {
        Ok(level) => (Some(level), None),
        Err(error) => (None, Some(error)),
    };
    let mut world_exists = false;
    let (mut world_files, mut world_bytes) = (0, 0);
    let mut world_blocker = level_error.clone();
    if level_error.is_none() {
        match check_world(&server_dir, &tracker) {
            Ok(check) => {
                world_exists = true;
                world_files = check.files;
                world_bytes = check.bytes;
            }
            Err(error) => world_blocker = Some(error),
        }
    }

    let backups = list_backups(&server_dir.to_string_lossy())?;
    let backup_bytes = backups.iter().map(|backup| backup.total_bytes).sum();
    let level_label = level.clone().unwrap_or_else(|| "world".into());

    Ok(DeletionPlan {
        server_name,
        level_name: level,
        level_name_error: level_error,
        world_exists,
        world_files,
        world_bytes,
        world_blocker,
        backups,
        backup_bytes,
        configuration_removes: vec![CONFIG_FILE.into()],
        configuration_retains: vec![
            format!("World folder '{level_label}'"),
            format!("Backups in '{BACKUP_ROOT}'"),
            "Server JAR, server.properties, eula.txt and logs".into(),
        ],
        blocked_reason: check_observation(observation).err(),
    })
}

fn remove_file_with_retry(path: &Path) -> std::io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            if let Ok(metadata) = fs::metadata(path) {
                let mut permissions = metadata.permissions();
                #[allow(clippy::permissions_set_readonly_false)]
                permissions.set_readonly(false);
                let _ = fs::set_permissions(path, permissions);
            }
            fs::remove_file(path)
        }
        other => other,
    }
}

struct Progress<'a> {
    base: &'a Path,
    tracker: &'a Tracker,
    files: u64,
    bytes: u64,
    failures: Vec<String>,
}

impl Progress<'_> {
    fn fail(&mut self, path: &Path, reason: &str) {
        if self.failures.len() < MAX_REPORTED_FAILURES {
            let relative = path.strip_prefix(self.base).unwrap_or(path);
            self.failures.push(format!(
                "{}: {reason}",
                relative.display().to_string().replace('\\', "/")
            ));
        }
    }
}

/// Removes a tree without ever following a symbolic link. Anything that is not
/// a plain file or folder is skipped and reported instead of touched.
fn remove_tree(dir: &Path, progress: &mut Progress<'_>) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            progress.fail(dir, &format!("could not list folder ({error})"));
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(kind) if kind.is_symlink() => {
                progress.fail(&path, "skipped: symbolic link");
            }
            Ok(kind) if kind.is_dir() => remove_tree(&path, progress),
            Ok(kind) if kind.is_file() => {
                let length =
                    entry.metadata().map(|meta| meta.len()).unwrap_or(0);
                match remove_file_with_retry(&path) {
                    Ok(()) => {
                        progress.files += 1;
                        progress.bytes += length;
                        progress.tracker.add_bytes(length);
                        progress.tracker.file_done();
                    }
                    Err(error) => progress.fail(&path, &error.to_string()),
                }
            }
            Ok(_) => progress.fail(&path, "skipped: not a regular file"),
            Err(error) => progress.fail(&path, &error.to_string()),
        }
    }
    if let Err(error) = fs::remove_dir(dir) {
        if progress.failures.is_empty() {
            progress.fail(dir, &format!("could not remove folder ({error})"));
        }
    }
}

fn delete_tree(
    root: &Path,
    name: &str,
    tracker: &Tracker,
) -> (u64, u64, Vec<String>) {
    let mut progress = Progress {
        base: root,
        tracker,
        files: 0,
        bytes: 0,
        failures: Vec::new(),
    };
    remove_tree(root, &mut progress);
    if fs::symlink_metadata(root).is_ok() && progress.failures.is_empty() {
        progress
            .failures
            .push(format!("{name}: folder still exists"));
    }
    (progress.files, progress.bytes, progress.failures)
}

fn require_confirmation(expected: &str, typed: &str) -> Result<(), String> {
    if typed == expected {
        Ok(())
    } else {
        Err(format!(
            "Confirmation did not match. Type exactly '{expected}' to continue. Nothing was deleted."
        ))
    }
}

fn finish_tree_deletion(
    target: &str,
    name: &str,
    root: &Path,
    retained: Vec<String>,
    tracker: &Tracker,
) -> DeletionResult {
    let (deleted_files, deleted_bytes, failures) =
        delete_tree(root, name, tracker);
    let complete = failures.is_empty();
    let message = if complete {
        format!("Deleted {target} '{name}' ({deleted_files} files).")
    } else {
        format!(
            "Deleting {target} '{name}' was only partly completed: {deleted_files} files were removed but some items remain."
        )
    };
    DeletionResult {
        target: target.into(),
        name: name.into(),
        complete,
        deleted_files,
        deleted_bytes,
        failures,
        retained,
        message,
    }
}

fn perform(
    server_dir: &Path,
    target: &DeleteTarget,
    confirmation: &str,
    tracker: &Tracker,
    audit: &mut dyn FnMut(String),
) -> Result<DeletionResult, String> {
    match target {
        DeleteTarget::Configuration => {
            let server_name = managed_server(server_dir)?;
            require_confirmation(&server_name, confirmation)?;
            let level = level_name(server_dir)
                .unwrap_or_else(|_| "the world folder".into());
            audit(format!(
                "delete configuration requested for server '{server_name}'"
            ));
            tracker.phase("Removing configuration", 1, 1, 0, 1, false);
            match fs::remove_file(server_dir.join(CONFIG_FILE)) {
                Ok(()) => {
                    tracker.file_done();
                    Ok(DeletionResult {
                        target: "configuration".into(),
                        name: server_name.clone(),
                        complete: true,
                        deleted_files: 1,
                        deleted_bytes: 0,
                        failures: Vec::new(),
                        retained: vec![
                            format!("World '{level}'"),
                            format!("Backups in '{BACKUP_ROOT}'"),
                            "Server JAR, server.properties, eula.txt".into(),
                        ],
                        message: format!(
                            "Removed the CloudCraft configuration for '{server_name}'. Your world and backups were kept."
                        ),
                    })
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    Err("The configuration was already removed.".into())
                }
                Err(error) => Err(format!(
                    "Could not remove the configuration: {error}. Nothing else was changed."
                )),
            }
        }
        DeleteTarget::World => {
            let server_name = managed_server(server_dir)?;
            let world = check_world(server_dir, tracker)?;
            require_confirmation(&world.name, confirmation)?;
            audit(format!(
                "delete world requested for server '{server_name}': world '{}' ({} files, {} bytes)",
                world.name, world.files, world.bytes
            ));
            tracker.phase(
                "Deleting world",
                1,
                1,
                world.bytes,
                world.files,
                false,
            );
            Ok(finish_tree_deletion(
                "world",
                &world.name,
                &server_dir.join(&world.name),
                vec![
                    format!("Backups in '{BACKUP_ROOT}'"),
                    "CloudCraft configuration, server JAR, server.properties"
                        .into(),
                ],
                tracker,
            ))
        }
        DeleteTarget::Backup { id } => {
            valid_component(id, "The backup id")?;
            require_confirmation(id, confirmation)?;
            let (dir, manifest) = load_manifest(server_dir, id)?;
            if dir.parent() != Some(backups_root(server_dir).as_path()) {
                return Err("The backup is outside the backup folder.".into());
            }
            let scan = scan_tree(&dir, tracker).map_err(String::from)?;
            audit(format!(
                "delete backup requested: '{id}' of world '{}' ({} files)",
                manifest.level_name, scan.files
            ));
            tracker.phase(
                "Deleting backup",
                1,
                1,
                scan.bytes,
                scan.files,
                false,
            );
            Ok(finish_tree_deletion(
                "backup",
                id,
                &dir,
                vec!["The current world".into(), "All other backups".into()],
                tracker,
            ))
        }
    }
}

/// Deletes one resource. The caller must already hold the operation slot via
/// `tracker` and have determined `observation` under the server state lock.
pub fn delete_resource(
    server_dir: &str,
    target: &DeleteTarget,
    confirmation: &str,
    observation: &ServerObservation,
    tracker: &Tracker,
    audit: &mut dyn FnMut(String),
) -> Result<DeletionResult, OpError> {
    let label = target.label();
    let outcome = (|| -> Result<DeletionResult, String> {
        check_observation(observation)?;
        let server_dir =
            real_directory(Path::new(server_dir), "server directory")?;
        perform(&server_dir, target, confirmation, tracker, audit)
    })();
    match &outcome {
        Ok(result) if result.complete => audit(format!(
            "delete {label} completed: '{}' ({} files, {} bytes)",
            result.name, result.deleted_files, result.deleted_bytes
        )),
        Ok(result) => audit(format!(
            "delete {label} PARTIAL: '{}' removed {} files, {} problems",
            result.name,
            result.deleted_files,
            result.failures.len()
        )),
        Err(reason) => {
            let mut scrubbed = reason.replace(server_dir, "<server folder>");
            if let Ok(canonical) = Path::new(server_dir).canonicalize() {
                scrubbed = scrubbed.replace(
                    canonical.to_string_lossy().as_ref(),
                    "<server folder>",
                );
            }
            audit(format!("delete {label} rejected or failed: {scrubbed}"));
        }
    }
    outcome.map_err(OpError::Failed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloudcraft_server_backup::create_backup;
    use chrono::Utc;
    use std::path::PathBuf;

    fn temp_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "cloudcraft-delete-{label}-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&path).unwrap();
        path.canonicalize().unwrap()
    }

    fn managed_server() -> PathBuf {
        let dir = temp_dir("server");
        fs::write(dir.join(CONFIG_FILE), r#"{"name":"My Server"}"#).unwrap();
        fs::write(dir.join("server.jar"), b"jar").unwrap();
        fs::write(dir.join("server.properties"), "level-name=world\n").unwrap();
        fs::create_dir_all(dir.join("world/region")).unwrap();
        fs::write(dir.join("world/level.dat"), b"level").unwrap();
        fs::write(dir.join("world/region/r.0.0.mca"), b"region").unwrap();
        dir
    }

    fn run(
        dir: &Path,
        target: DeleteTarget,
        confirmation: &str,
        observation: ServerObservation,
    ) -> (Result<DeletionResult, OpError>, Vec<String>) {
        let tracker = Registry::new().begin(OperationKind::Delete).unwrap();
        let mut lines = Vec::new();
        let result = delete_resource(
            dir.to_str().unwrap(),
            &target,
            confirmation,
            &observation,
            &tracker,
            &mut |line| lines.push(line),
        );
        (result, lines)
    }

    fn failure(result: Result<DeletionResult, OpError>) -> String {
        match result {
            Err(OpError::Failed(message)) => message,
            other => panic!("expected failure, got {other:?}"),
        }
    }

    #[test]
    fn configuration_delete_keeps_world_backups_and_files() {
        let dir = managed_server();
        let tracker = Registry::new().begin(OperationKind::Backup).unwrap();
        create_backup(dir.to_str().unwrap(), &tracker).unwrap();
        let (result, _) = run(
            &dir,
            DeleteTarget::Configuration,
            "My Server",
            ServerObservation::Stopped,
        );
        let result = result.unwrap();
        assert!(result.complete);
        assert!(!dir.join(CONFIG_FILE).exists());
        assert!(dir.join("world/level.dat").exists());
        assert!(dir.join("server.jar").exists());
        assert!(dir.join("server.properties").exists());
        assert_eq!(list_backups(dir.to_str().unwrap()).unwrap().len(), 1);
    }

    #[test]
    fn world_delete_removes_only_the_world() {
        let dir = managed_server();
        let tracker = Registry::new().begin(OperationKind::Backup).unwrap();
        create_backup(dir.to_str().unwrap(), &tracker).unwrap();
        let (result, lines) = run(
            &dir,
            DeleteTarget::World,
            "world",
            ServerObservation::Stopped,
        );
        let result = result.unwrap();
        assert!(result.complete);
        assert_eq!(result.deleted_files, 2);
        assert!(!dir.join("world").exists());
        assert!(dir.join(CONFIG_FILE).exists());
        assert!(dir.join("server.jar").exists());
        assert_eq!(list_backups(dir.to_str().unwrap()).unwrap().len(), 1);
        assert!(lines.iter().any(|line| line.contains("completed")));
    }

    #[test]
    fn backup_delete_removes_only_that_backup() {
        let dir = managed_server();
        let tracker = Registry::new().begin(OperationKind::Backup).unwrap();
        let backup = create_backup(dir.to_str().unwrap(), &tracker).unwrap();
        let (result, _) = run(
            &dir,
            DeleteTarget::Backup {
                id: backup.id.clone(),
            },
            &backup.id,
            ServerObservation::Stopped,
        );
        assert!(result.unwrap().complete);
        assert!(list_backups(dir.to_str().unwrap()).unwrap().is_empty());
        assert!(dir.join("world/level.dat").exists());
    }

    #[test]
    fn wrong_or_missing_confirmation_deletes_nothing() {
        let dir = managed_server();
        for typed in ["", "World", "my server", "world "] {
            let (result, _) = run(
                &dir,
                DeleteTarget::World,
                typed,
                ServerObservation::Stopped,
            );
            assert!(failure(result).contains("Confirmation did not match"));
        }
        let (result, _) = run(
            &dir,
            DeleteTarget::Configuration,
            "world",
            ServerObservation::Stopped,
        );
        assert!(failure(result).contains("Confirmation did not match"));
        assert!(dir.join("world/level.dat").exists());
        assert!(dir.join(CONFIG_FILE).exists());
    }

    #[test]
    fn running_server_blocks_every_target() {
        let dir = managed_server();
        for target in [
            DeleteTarget::Configuration,
            DeleteTarget::World,
            DeleteTarget::Backup { id: "x".into() },
        ] {
            let (result, lines) =
                run(&dir, target, "My Server", ServerObservation::Active);
            assert!(failure(result).contains("Stop it first"));
            assert!(lines.iter().any(|line| line.contains("rejected")));
        }
        assert!(dir.join("world/level.dat").exists());
        assert!(dir.join(CONFIG_FILE).exists());
    }

    #[test]
    fn uncertain_status_blocks_deletion() {
        let dir = managed_server();
        let (result, _) = run(
            &dir,
            DeleteTarget::World,
            "world",
            ServerObservation::Uncertain("the port answers".into()),
        );
        let message = failure(result);
        assert!(message.contains("cannot confirm"));
        assert!(message.contains("the port answers"));
        assert!(dir.join("world/level.dat").exists());
    }

    #[test]
    fn traversal_in_level_name_or_backup_id_is_rejected() {
        let dir = managed_server();
        let outside = temp_dir("outside");
        fs::write(outside.join("level.dat"), b"precious").unwrap();
        let escape =
            format!("../{}", outside.file_name().unwrap().to_string_lossy());
        fs::write(
            dir.join("server.properties"),
            format!("level-name={escape}\n"),
        )
        .unwrap();
        let (result, _) = run(
            &dir,
            DeleteTarget::World,
            &escape,
            ServerObservation::Stopped,
        );
        assert!(failure(result).contains("single folder name"));
        assert!(outside.join("level.dat").exists());

        for id in ["..", "../world", "a/b", "a\\b", ".partial-x"] {
            let (result, _) = run(
                &dir,
                DeleteTarget::Backup { id: id.into() },
                id,
                ServerObservation::Stopped,
            );
            assert!(result.is_err(), "id {id:?} must be rejected");
        }
        assert!(dir.join("world/level.dat").exists());
    }

    #[test]
    fn unmanaged_folders_are_never_touched() {
        let dir = temp_dir("unmanaged");
        fs::create_dir_all(dir.join("world")).unwrap();
        fs::write(dir.join("world/level.dat"), b"x").unwrap();
        for target in [DeleteTarget::World, DeleteTarget::Configuration] {
            let (result, _) =
                run(&dir, target, "world", ServerObservation::Stopped);
            assert!(
                failure(result).contains("no CloudCraft server configuration")
            );
        }
        assert!(dir.join("world/level.dat").exists());
    }

    #[test]
    fn a_folder_that_is_not_a_world_is_refused() {
        let dir = managed_server();
        fs::remove_file(dir.join("world/level.dat")).unwrap();
        fs::write(dir.join("world/notes.txt"), b"mine").unwrap();
        let (result, _) = run(
            &dir,
            DeleteTarget::World,
            "world",
            ServerObservation::Stopped,
        );
        assert!(failure(result).contains("level.dat"));
        assert!(dir.join("world/notes.txt").exists());
    }

    #[test]
    fn reserved_level_names_are_refused() {
        let dir = managed_server();
        fs::write(
            dir.join("server.properties"),
            format!("level-name={BACKUP_ROOT}\n"),
        )
        .unwrap();
        let tracker = Registry::new().begin(OperationKind::Backup).unwrap();
        let _ = create_backup(dir.to_str().unwrap(), &tracker);
        let (result, _) = run(
            &dir,
            DeleteTarget::World,
            BACKUP_ROOT,
            ServerObservation::Stopped,
        );
        assert!(failure(result).contains("reserved"));
    }

    #[test]
    fn missing_resources_are_reported_without_side_effects() {
        let dir = managed_server();
        fs::remove_dir_all(dir.join("world")).unwrap();
        let (result, _) = run(
            &dir,
            DeleteTarget::World,
            "world",
            ServerObservation::Stopped,
        );
        assert!(failure(result).contains("Could not access"));

        let (result, _) = run(
            &dir,
            DeleteTarget::Backup {
                id: "20240101T000000.000Z".into(),
            },
            "20240101T000000.000Z",
            ServerObservation::Stopped,
        );
        assert!(result.is_err());

        fs::remove_file(dir.join(CONFIG_FILE)).unwrap();
        let (result, _) = run(
            &dir,
            DeleteTarget::Configuration,
            "My Server",
            ServerObservation::Stopped,
        );
        assert!(failure(result).contains("no CloudCraft server configuration"));

        let (result, _) = run(
            &dir.join("nope"),
            DeleteTarget::World,
            "world",
            ServerObservation::Stopped,
        );
        assert!(result.is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_refused_and_never_followed() {
        use std::os::unix::fs::symlink;

        let dir = managed_server();
        let outside = temp_dir("outside-link");
        fs::write(outside.join("keep.txt"), b"keep").unwrap();

        symlink(&outside, dir.join("world/link")).unwrap();
        let (result, _) = run(
            &dir,
            DeleteTarget::World,
            "world",
            ServerObservation::Stopped,
        );
        assert!(failure(result).contains("symbolic link"));
        assert!(dir.join("world/level.dat").exists());
        assert!(outside.join("keep.txt").exists());

        fs::remove_file(dir.join("world/link")).unwrap();
        fs::remove_dir_all(dir.join("world")).unwrap();
        fs::write(outside.join("level.dat"), b"x").unwrap();
        symlink(&outside, dir.join("world")).unwrap();
        let (result, _) = run(
            &dir,
            DeleteTarget::World,
            "world",
            ServerObservation::Stopped,
        );
        assert!(failure(result).contains("not a file or symbolic link"));
        assert!(outside.join("keep.txt").exists());
        assert!(outside.join("level.dat").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_server_folder_is_refused() {
        use std::os::unix::fs::symlink;

        let real = managed_server();
        let link = temp_dir("holder").join("link");
        symlink(&real, &link).unwrap();
        let (result, _) = run(
            &link,
            DeleteTarget::World,
            "world",
            ServerObservation::Stopped,
        );
        assert!(failure(result).contains("not a file or symbolic link"));
        assert!(real.join("world/level.dat").exists());
    }

    #[cfg(unix)]
    #[test]
    fn partial_failures_are_reported_and_remaining_data_is_kept() {
        use std::os::unix::fs::PermissionsExt;

        let dir = managed_server();
        let locked = dir.join("world/region");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o555))
            .unwrap();
        let can_write = fs::write(locked.join("probe"), b"x").is_ok();
        if can_write {
            fs::set_permissions(&locked, fs::Permissions::from_mode(0o755))
                .unwrap();
            return;
        }

        let (result, lines) = run(
            &dir,
            DeleteTarget::World,
            "world",
            ServerObservation::Stopped,
        );
        let result = result.unwrap();
        assert!(!result.complete);
        assert!(!result.failures.is_empty());
        assert!(result.message.contains("only partly completed"));
        assert!(lines.iter().any(|line| line.contains("PARTIAL")));
        assert!(locked.join("r.0.0.mca").exists());

        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755))
            .unwrap();
        assert!(dir.join("server.jar").exists());
        assert!(dir.join(CONFIG_FILE).exists());
    }

    #[test]
    fn audit_lines_never_contain_absolute_paths() {
        let dir = managed_server();
        let dir_text = dir.to_string_lossy().to_string();
        let (_, lines) = run(
            &dir,
            DeleteTarget::World,
            "world",
            ServerObservation::Stopped,
        );
        assert!(!lines.is_empty());
        assert!(lines.iter().all(|line| !line.contains(&dir_text)));
        let (_, lines) = run(
            &dir,
            DeleteTarget::World,
            "world",
            ServerObservation::Active,
        );
        assert!(lines.iter().all(|line| !line.contains(&dir_text)));
    }

    #[cfg(unix)]
    #[test]
    fn audit_scrubs_paths_from_low_level_errors() {
        use std::os::unix::fs::symlink;

        let dir = managed_server();
        symlink("/etc/hostname", dir.join("world/link")).unwrap();
        let dir_text = dir.to_string_lossy().to_string();
        let (result, lines) = run(
            &dir,
            DeleteTarget::World,
            "world",
            ServerObservation::Stopped,
        );
        assert!(result.is_err());
        assert!(lines.iter().all(|line| !line.contains(&dir_text)));
        assert!(lines.iter().any(|line| line.contains("<server folder>")));
    }

    #[test]
    fn plan_describes_what_is_kept_and_deleted_without_deleting() {
        let dir = managed_server();
        let tracker = Registry::new().begin(OperationKind::Backup).unwrap();
        create_backup(dir.to_str().unwrap(), &tracker).unwrap();
        let plan_for =
            |observation| plan(dir.to_str().unwrap(), &observation).unwrap();

        let stopped = plan_for(ServerObservation::Stopped);
        assert_eq!(stopped.server_name, "My Server");
        assert!(stopped.world_exists);
        assert_eq!(stopped.world_files, 2);
        assert_eq!(stopped.backups.len(), 1);
        assert!(stopped.blocked_reason.is_none());
        assert!(stopped
            .configuration_retains
            .iter()
            .any(|item| item.contains("World folder")));

        assert!(plan_for(ServerObservation::Active).blocked_reason.is_some());
        assert!(dir.join("world/level.dat").exists());
    }

    #[test]
    fn deletion_holds_the_single_operation_slot() {
        let registry = Registry::new();
        let _backup = registry.begin(OperationKind::Backup).unwrap();
        assert!(registry.begin(OperationKind::Delete).is_err());
    }
}
