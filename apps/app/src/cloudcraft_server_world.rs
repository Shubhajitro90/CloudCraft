//! Standalone world export and import.
//!
//! Export writes the server's world into a single `.zip`; import accepts a
//! `.zip` or a world folder and creates a brand-new world folder inside the
//! server directory. Neither ever overwrites or removes an existing world or
//! file. Everything is built in a hidden staging location and renamed into
//! place only at the commit point, so cancelling or failing leaves no partial
//! result. Archives are inspected before extraction: path traversal, symbolic
//! links, unsafe names, oversized archives and archives without a valid
//! `level.dat` are rejected.

use crate::cloudcraft_server_backup::{
    copy_tree, discard_staging, real_directory, remove_stale_staging,
    valid_component, BACKUP_ROOT,
};
use crate::cloudcraft_server_ops::{OpError, Tracker};
use serde::Serialize;
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

const IMPORT_PREFIX: &str = ".importing-";
const MAX_ENTRIES: usize = 200_000;
const MAX_UNCOMPRESSED_BYTES: u64 = 64 * 1024 * 1024 * 1024;
const LEVEL_FILE: &str = "level.dat";
const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];
const CHUNK: usize = 64 * 1024;
const RESERVED_DEVICE_NAMES: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6",
    "COM7", "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6",
    "LPT7", "LPT8", "LPT9",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorldExportResult {
    pub path: String,
    pub level_name: String,
    pub files: u64,
    pub bytes: u64,
    pub archive_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorldImportResult {
    pub folder: String,
    pub files: u64,
    pub bytes: u64,
    pub skipped_entries: u64,
}

fn zip_error(error: zip::result::ZipError) -> String {
    format!("The zip archive could not be processed: {error}")
}

/// File and folder names that are safe on every supported platform.
fn safe_component(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    if name.is_empty()
        || name.ends_with('.')
        || name.ends_with(' ')
        || name
            .chars()
            .any(|c| c.is_control() || "<>:\"|?*\\/".contains(c))
    {
        return false;
    }
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    !RESERVED_DEVICE_NAMES.contains(&stem.as_str())
}

fn safe_relative_path(path: &Path) -> bool {
    path.components().all(|component| match component {
        Component::Normal(name) => safe_component(name),
        _ => false,
    })
}

fn is_symlink_mode(mode: u32) -> bool {
    mode & 0o170000 == 0o120000
}

/// A new world folder name: a single, visible, portable path component that
/// CloudCraft does not reserve.
pub fn validate_world_name(name: &str) -> Result<(), String> {
    valid_component(name, "The world folder name")?;
    if name.len() > 64
        || name.starts_with('.')
        || name == BACKUP_ROOT
        || !safe_component(OsStr::new(name))
    {
        return Err(
            "Use up to 64 letters, numbers, spaces, dashes or underscores for the world folder name; it cannot start with a dot or use reserved names."
                .into(),
        );
    }
    Ok(())
}

fn require_level_file(world: &Path) -> Result<(), String> {
    let level = world.join(LEVEL_FILE);
    match fs::symlink_metadata(&level) {
        Ok(metadata) if metadata.is_file() && metadata.len() > 0 => Ok(()),
        _ => Err(format!(
            "'{}' does not contain a {LEVEL_FILE} file, so it is not a Minecraft world.",
            world.file_name().unwrap_or_default().to_string_lossy()
        )),
    }
}

fn require_level_file_valid(world: &Path) -> Result<(), String> {
    require_level_file(world)?;
    let mut head = [0u8; 2];
    fs::File::open(world.join(LEVEL_FILE))
        .and_then(|mut file| file.read_exact(&mut head))
        .map_err(|error| format!("Could not read {LEVEL_FILE}: {error}"))?;
    if head != GZIP_MAGIC {
        return Err(format!(
            "{LEVEL_FILE} is not a valid Minecraft level file, so the world was not imported."
        ));
    }
    Ok(())
}

struct FsEntry {
    relative: PathBuf,
    is_dir: bool,
    size: u64,
}

/// Lists a world's folders and files in a stable order, rejecting symbolic
/// links and special files before anything is written.
fn collect_entries(
    root: &Path,
    tracker: &Tracker,
) -> Result<Vec<FsEntry>, OpError> {
    let mut entries = Vec::new();
    let mut stack = vec![PathBuf::new()];
    while let Some(relative) = stack.pop() {
        tracker.check_cancel()?;
        let directory = root.join(&relative);
        let mut children = fs::read_dir(&directory)
            .and_then(|iter| iter.collect::<Result<Vec<_>, _>>())
            .map_err(|error| {
                format!("Could not list {}: {error}", directory.display())
            })?;
        children.sort_by_key(|entry| entry.file_name());
        for child in children {
            let path = child.path();
            let child_relative = relative.join(child.file_name());
            let kind = child.file_type().map_err(|error| {
                format!("Could not inspect {}: {error}", path.display())
            })?;
            if kind.is_symlink() {
                return Err(format!(
                    "Refusing to include symbolic link {}.",
                    path.display()
                )
                .into());
            } else if kind.is_dir() {
                entries.push(FsEntry {
                    relative: child_relative.clone(),
                    is_dir: true,
                    size: 0,
                });
                stack.push(child_relative);
            } else if kind.is_file() {
                let size = child
                    .metadata()
                    .map_err(|error| {
                        format!("Could not inspect {}: {error}", path.display())
                    })?
                    .len();
                entries.push(FsEntry {
                    relative: child_relative,
                    is_dir: false,
                    size,
                });
            } else {
                return Err(format!(
                    "Refusing to include special file {}.",
                    path.display()
                )
                .into());
            }
        }
    }
    entries.sort_by(|a, b| a.relative.cmp(&b.relative));
    Ok(entries)
}

fn zip_name(level: &str, relative: &Path, is_dir: bool) -> String {
    let mut name = level.to_string();
    for component in relative.components() {
        name.push('/');
        name.push_str(&component.as_os_str().to_string_lossy());
    }
    if is_dir {
        name.push('/');
    }
    name
}

fn export_destination(
    destination: &str,
    world: &Path,
) -> Result<(PathBuf, PathBuf), String> {
    let path = Path::new(destination);
    let file_name = path
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or("Choose a file name for the exported world.")?;
    valid_component(file_name, "The export file name")?;
    if !file_name.to_ascii_lowercase().ends_with(".zip") {
        return Err("The export file must end in .zip.".into());
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or("Choose a full path for the exported world.")?;
    let parent = real_directory(parent, "export folder")?;
    if parent.starts_with(world) {
        return Err(
            "Save the export outside the world folder being exported.".into()
        );
    }
    let final_path = parent.join(file_name);
    if fs::symlink_metadata(&final_path).is_ok() {
        return Err(format!(
            "'{file_name}' already exists. Choose a new name; nothing was overwritten."
        ));
    }
    Ok((parent, final_path))
}

fn drain(
    reader: &mut impl Read,
    tracker: &Tracker,
    mut on_chunk: impl FnMut(&[u8]) -> Result<(), OpError>,
    context: &str,
) -> Result<u64, OpError> {
    let mut buffer = vec![0u8; CHUNK];
    let mut total = 0u64;
    loop {
        tracker.check_cancel()?;
        let read = reader
            .read(&mut buffer)
            .map_err(|error| format!("Could not read {context}: {error}"))?;
        if read == 0 {
            return Ok(total);
        }
        on_chunk(&buffer[..read])?;
        total += read as u64;
    }
}

/// Writes the world to `destination` (a new `.zip`) and verifies the archive
/// before it appears under its final name. The caller must ensure the server
/// is stopped so the world is consistent.
pub fn export_world(
    server_dir: &str,
    destination: &str,
    level: &str,
    tracker: &Tracker,
) -> Result<WorldExportResult, OpError> {
    const PHASES: u32 = 3;
    let server_dir = real_directory(Path::new(server_dir), "server directory")?;
    let world = real_directory(
        &server_dir.join(level),
        &format!("world folder '{level}'"),
    )?;
    require_level_file(&world)?;
    let (parent, final_path) = export_destination(destination, &world)?;

    tracker.phase("Scanning world", 1, PHASES, 0, 0, true);
    let entries = collect_entries(&world, tracker)?;
    let files = entries.iter().filter(|entry| !entry.is_dir).count() as u64;
    let bytes: u64 = entries.iter().map(|entry| entry.size).sum();

    let part = parent.join(format!(
        ".{}.cloudcraft-part",
        final_path.file_name().unwrap_or_default().to_string_lossy()
    ));
    if fs::symlink_metadata(&part).is_ok_and(|meta| meta.is_file()) {
        let _ = fs::remove_file(&part);
    }
    let output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&part)
        .map_err(|error| {
            format!("Could not create the export file: {error}")
        })?;

    let result = (|| -> Result<WorldExportResult, OpError> {
        tracker.phase("Writing archive", 2, PHASES, bytes, files, true);
        let mut writer = zip::ZipWriter::new(output);
        let options = zip::write::FileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for entry in &entries {
            tracker.check_cancel()?;
            let name = zip_name(level, &entry.relative, entry.is_dir);
            if entry.is_dir {
                writer.add_directory(name, options).map_err(zip_error)?;
                continue;
            }
            tracker.file_started(&name);
            writer
                .start_file(
                    name,
                    options.large_file(entry.size >= u32::MAX as u64),
                )
                .map_err(zip_error)?;
            let source = world.join(&entry.relative);
            let mut input = fs::File::open(&source).map_err(|error| {
                format!("Could not read {}: {error}", source.display())
            })?;
            drain(
                &mut input,
                tracker,
                |chunk| {
                    writer.write_all(chunk).map_err(|error| {
                        OpError::Failed(format!(
                            "Could not write the archive: {error}"
                        ))
                    })?;
                    tracker.add_bytes(chunk.len() as u64);
                    Ok(())
                },
                &source.display().to_string(),
            )?;
            tracker.file_done();
        }
        let finished = writer.finish().map_err(zip_error)?;
        finished.sync_all().map_err(|error| {
            format!("Could not flush the export file: {error}")
        })?;
        drop(finished);

        tracker.phase("Verifying archive", 3, PHASES, bytes, files, true);
        let mut archive =
            zip::ZipArchive::new(fs::File::open(&part).map_err(|error| {
                format!("Could not reopen the export file: {error}")
            })?)
            .map_err(zip_error)?;
        if archive.len() != entries.len() {
            return Err(OpError::Failed(
                "The finished archive does not contain every world entry."
                    .into(),
            ));
        }
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index).map_err(zip_error)?;
            if entry.is_dir() {
                continue;
            }
            let name = entry.name().to_string();
            drain(
                &mut entry,
                tracker,
                |chunk| {
                    tracker.add_bytes(chunk.len() as u64);
                    Ok(())
                },
                &name,
            )
            .map_err(|error| match error {
                OpError::Failed(message) => OpError::Failed(format!(
                    "The finished archive failed verification: {message}"
                )),
                other => other,
            })?;
        }
        drop(archive);

        let archive_bytes = fs::metadata(&part)
            .map_err(|error| {
                format!("Could not inspect the export file: {error}")
            })?
            .len();
        tracker.begin_commit("Finalizing export")?;
        if fs::symlink_metadata(&final_path).is_ok() {
            return Err(OpError::Failed(
                "The export file appeared during export; it was not modified."
                    .into(),
            ));
        }
        fs::rename(&part, &final_path).map_err(|error| {
            OpError::Failed(format!("Could not finalize the export: {error}"))
        })?;
        Ok(WorldExportResult {
            path: final_path.to_string_lossy().into_owned(),
            level_name: level.to_string(),
            files,
            bytes,
            archive_bytes,
        })
    })();

    match result {
        Ok(done) => Ok(done),
        Err(error) => Err(remove_partial_file(&part, error)),
    }
}

fn remove_partial_file(part: &Path, original: OpError) -> OpError {
    match fs::remove_file(part) {
        Ok(()) => original,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => original,
        Err(error) => {
            let reason = match original {
                OpError::Cancelled => "The operation was cancelled".to_string(),
                OpError::Failed(message) => message,
            };
            OpError::Failed(format!(
                "{reason}, but the incomplete file '{}' could not be removed: {error}. It is safe to delete manually.",
                part.file_name().unwrap_or_default().to_string_lossy()
            ))
        }
    }
}

struct ZipPlan {
    entries: Vec<ZipItem>,
    files: u64,
    bytes: u64,
    skipped: u64,
}

struct ZipItem {
    index: usize,
    relative: PathBuf,
    is_dir: bool,
    size: u64,
}

fn inspect_archive(
    archive: &mut zip::ZipArchive<fs::File>,
    tracker: &Tracker,
) -> Result<ZipPlan, OpError> {
    if archive.is_empty() {
        return Err("The archive is empty.".to_string().into());
    }
    if archive.len() > MAX_ENTRIES {
        return Err(format!(
            "The archive has more than {MAX_ENTRIES} entries and was not imported."
        )
        .into());
    }

    let mut all = Vec::with_capacity(archive.len());
    let mut declared = 0u64;
    for index in 0..archive.len() {
        tracker.check_cancel()?;
        let entry = archive.by_index(index).map_err(zip_error)?;
        let name = entry.name().to_string();
        let Some(path) = entry.enclosed_name().map(Path::to_path_buf) else {
            return Err(format!(
                "The archive entry '{name}' would escape the world folder; the archive was rejected."
            )
            .into());
        };
        if entry.unix_mode().is_some_and(is_symlink_mode) {
            return Err(format!(
                "The archive contains the symbolic link '{name}'; the archive was rejected."
            )
            .into());
        }
        if !safe_relative_path(&path) {
            return Err(format!(
                "The archive entry '{name}' has a name that is not safe on every platform; the archive was rejected."
            )
            .into());
        }
        let is_dir = entry.is_dir();
        let size = if is_dir { 0 } else { entry.size() };
        declared = declared
            .checked_add(size)
            .ok_or_else(|| "The archive size is invalid.".to_string())?;
        if declared > MAX_UNCOMPRESSED_BYTES {
            return Err("The archive would expand to more than the 64 GiB limit and was not imported."
                .to_string()
                .into());
        }
        all.push(ZipItem {
            index,
            relative: path,
            is_dir,
            size,
        });
    }

    let level_paths: Vec<&PathBuf> = all
        .iter()
        .filter(|item| {
            !item.is_dir
                && item.relative.file_name() == Some(OsStr::new(LEVEL_FILE))
                && item.relative.components().count() <= 2
        })
        .map(|item| &item.relative)
        .collect();
    let prefix: Option<String> = if level_paths
        .iter()
        .any(|path| path.components().count() == 1)
    {
        None
    } else {
        let folders: BTreeSet<String> = level_paths
            .iter()
            .filter_map(|path| path.components().next())
            .map(|component| {
                component.as_os_str().to_string_lossy().into_owned()
            })
            .collect();
        match folders.len() {
            0 => {
                return Err(format!(
                    "The archive has no {LEVEL_FILE}, so it is not a Minecraft world."
                )
                .into())
            }
            1 => folders.into_iter().next(),
            _ => {
                return Err("The archive contains more than one world. Export each world separately."
                    .to_string()
                    .into())
            }
        }
    };

    let mut entries = Vec::new();
    let mut skipped = 0u64;
    let mut files = 0u64;
    let mut bytes = 0u64;
    for item in all {
        let relative = match &prefix {
            None => Some(item.relative.clone()),
            Some(prefix) => item
                .relative
                .strip_prefix(prefix)
                .ok()
                .map(Path::to_path_buf),
        };
        match relative {
            Some(relative) if relative.as_os_str().is_empty() => {}
            Some(relative) => {
                if !item.is_dir {
                    files += 1;
                    bytes += item.size;
                }
                entries.push(ZipItem { relative, ..item });
            }
            None => skipped += 1,
        }
    }
    Ok(ZipPlan {
        entries,
        files,
        bytes,
        skipped,
    })
}

fn extract_archive(
    archive: &mut zip::ZipArchive<fs::File>,
    plan: &ZipPlan,
    staging: &Path,
    tracker: &Tracker,
) -> Result<(), OpError> {
    fs::create_dir(staging).map_err(|error| {
        format!("Could not create the import folder: {error}")
    })?;
    let mut written_total = 0u64;
    for item in &plan.entries {
        tracker.check_cancel()?;
        let target = staging.join(&item.relative);
        if item.is_dir {
            fs::create_dir_all(&target).map_err(|error| {
                format!("Could not create {}: {error}", target.display())
            })?;
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                format!("Could not create {}: {error}", parent.display())
            })?;
        }
        let label = item.relative.to_string_lossy().replace('\\', "/");
        tracker.file_started(&label);
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .map_err(|error| {
                format!(
                    "Could not create {label} (is it listed twice in the archive?): {error}"
                )
            })?;
        let mut entry = archive.by_index(item.index).map_err(zip_error)?;
        let mut written = 0u64;
        drain(
            &mut entry,
            tracker,
            |chunk| {
                written += chunk.len() as u64;
                written_total += chunk.len() as u64;
                if written > item.size || written_total > MAX_UNCOMPRESSED_BYTES
                {
                    return Err(OpError::Failed(format!(
                        "'{label}' is larger than the archive declared; the archive was rejected."
                    )));
                }
                output.write_all(chunk).map_err(|error| {
                    OpError::Failed(format!(
                        "Could not write {label}: {error}"
                    ))
                })?;
                tracker.add_bytes(chunk.len() as u64);
                Ok(())
            },
            &label,
        )
        .map_err(|error| match error {
            OpError::Failed(message) if message.starts_with("Could not read") => {
                OpError::Failed(format!(
                    "'{label}' is corrupt in the archive ({message})."
                ))
            }
            other => other,
        })?;
        if written != item.size {
            return Err(OpError::Failed(format!(
                "'{label}' is shorter than the archive declared; it is corrupt."
            )));
        }
        tracker.file_done();
    }
    Ok(())
}

/// Imports a `.zip` or a world folder as a new folder named `name`.
pub fn import_world(
    server_dir: &str,
    source: &str,
    name: &str,
    tracker: &Tracker,
) -> Result<WorldImportResult, OpError> {
    const PHASES: u32 = 3;
    validate_world_name(name)?;
    let server_dir = real_directory(Path::new(server_dir), "server directory")?;
    let target = server_dir.join(name);
    if fs::symlink_metadata(&target).is_ok() {
        return Err(format!(
            "A folder named '{name}' already exists; it was not modified."
        )
        .into());
    }
    let source_path = Path::new(source);
    let metadata = fs::symlink_metadata(source_path).map_err(|error| {
        format!("Could not access the import source: {error}")
    })?;
    if metadata.file_type().is_symlink() {
        return Err("The import source must not be a symbolic link."
            .to_string()
            .into());
    }
    remove_stale_staging(&server_dir, IMPORT_PREFIX);
    let staging = server_dir.join(format!("{IMPORT_PREFIX}{name}"));

    let result = if metadata.is_dir() {
        import_folder(source_path, &server_dir, &staging, tracker)
    } else if metadata.is_file() {
        let is_zip = source_path
            .extension()
            .and_then(OsStr::to_str)
            .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"));
        if !is_zip {
            return Err("Choose a .zip archive or a world folder."
                .to_string()
                .into());
        }
        import_zip(source_path, &staging, tracker)
    } else {
        return Err("The import source is not a file or folder."
            .to_string()
            .into());
    };

    let result = result.and_then(|(files, bytes, skipped_entries)| {
        tracker.phase("Validating world", PHASES, PHASES, 0, 0, true);
        require_level_file_valid(&staging)?;
        tracker.begin_commit("Finalizing import")?;
        if fs::symlink_metadata(&target).is_ok() {
            return Err(OpError::Failed(format!(
                "A folder named '{name}' appeared during import; it was not modified."
            )));
        }
        fs::rename(&staging, &target).map_err(|error| {
            OpError::Failed(format!("Could not finalize the import: {error}"))
        })?;
        Ok(WorldImportResult {
            folder: name.to_string(),
            files,
            bytes,
            skipped_entries,
        })
    });
    discard_staging(&staging, result)
}

fn import_zip(
    source: &Path,
    staging: &Path,
    tracker: &Tracker,
) -> Result<(u64, u64, u64), OpError> {
    tracker.phase("Inspecting archive", 1, 3, 0, 0, true);
    let file = fs::File::open(source)
        .map_err(|error| format!("Could not open the archive: {error}"))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|error| {
        format!("This is not a readable zip archive: {error}")
    })?;
    let plan = inspect_archive(&mut archive, tracker)?;
    tracker.phase("Extracting world", 2, 3, plan.bytes, plan.files, true);
    extract_archive(&mut archive, &plan, staging, tracker)?;
    Ok((plan.files, plan.bytes, plan.skipped))
}

fn import_folder(
    source: &Path,
    server_dir: &Path,
    staging: &Path,
    tracker: &Tracker,
) -> Result<(u64, u64, u64), OpError> {
    tracker.phase("Scanning folder", 1, 3, 0, 0, true);
    let source = real_directory(source, "world folder to import")?;
    if server_dir.starts_with(&source) {
        return Err("Choose the world folder itself, not a folder that contains this server."
            .to_string()
            .into());
    }
    require_level_file(&source)?;
    let entries = collect_entries(&source, tracker)?;
    let files = entries.iter().filter(|entry| !entry.is_dir).count() as u64;
    let bytes: u64 = entries.iter().map(|entry| entry.size).sum();
    if entries
        .iter()
        .any(|entry| !safe_relative_path(&entry.relative))
    {
        return Err("The folder contains names that are not safe on every platform; it was not imported."
            .to_string()
            .into());
    }
    tracker.phase("Copying world", 2, 3, bytes, files, true);
    let mut copied = Vec::new();
    copy_tree(&source, staging, Path::new(""), &mut copied, tracker)?;
    Ok((files, bytes, 0))
}

/// Points the server at an existing world folder by updating `level-name`.
/// Returns the previous level name. The caller must ensure the server is
/// stopped.
pub fn set_active_world(
    server_dir: &str,
    folder: &str,
) -> Result<String, String> {
    valid_component(folder, "The world folder name")?;
    if folder.starts_with('.') || folder == BACKUP_ROOT {
        return Err("That folder is reserved and cannot be a world.".into());
    }
    let server_dir = real_directory(Path::new(server_dir), "server directory")?;
    let world = real_directory(
        &server_dir.join(folder),
        &format!("world folder '{folder}'"),
    )?;
    require_level_file(&world)?;

    let properties = server_dir.join("server.properties");
    let existing = match fs::read_to_string(&properties) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            String::new()
        }
        Err(error) => {
            return Err(format!("Could not read server.properties: {error}"))
        }
    };
    let mut previous = None;
    let mut lines: Vec<String> = Vec::new();
    for line in existing.lines() {
        if let Some(value) = line.trim_start().strip_prefix("level-name=") {
            previous = Some(value.trim().to_string());
        } else {
            lines.push(line.to_string());
        }
    }
    lines.push(format!("level-name={folder}"));
    let temporary = server_dir.join("server.properties.cloudcraft-tmp");
    fs::write(&temporary, format!("{}\n", lines.join("\n"))).map_err(
        |error| format!("Could not update server.properties: {error}"),
    )?;
    if let Err(error) = fs::rename(&temporary, &properties) {
        let _ = fs::remove_file(&temporary);
        return Err(format!("Could not update server.properties: {error}"));
    }
    Ok(previous
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "world".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloudcraft_server_ops::{OperationKind, Registry};
    use std::sync::Arc;
    use std::time::Duration;

    fn tracker(kind: OperationKind) -> Tracker {
        Registry::new().begin(kind).unwrap()
    }

    fn temp_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "cloudcraft-world-{label}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&path).unwrap();
        path.canonicalize().unwrap()
    }

    fn level_bytes() -> Vec<u8> {
        let mut bytes = vec![0x1f, 0x8b, 8, 0];
        bytes.extend_from_slice(b"level-data");
        bytes
    }

    fn server_with_world(level: &str) -> PathBuf {
        let dir = temp_dir("server");
        let world = dir.join(level);
        fs::create_dir_all(world.join("region")).unwrap();
        fs::create_dir_all(world.join("empty-dir")).unwrap();
        fs::write(world.join("level.dat"), level_bytes()).unwrap();
        fs::write(world.join("region/r.0.0.mca"), vec![7u8; 200_000]).unwrap();
        dir
    }

    fn export(server: &Path, zip: &Path) -> Result<WorldExportResult, String> {
        export_world(
            server.to_str().unwrap(),
            zip.to_str().unwrap(),
            "world",
            &tracker(OperationKind::Export),
        )
        .map_err(String::from)
    }

    fn import(
        server: &Path,
        source: &Path,
        name: &str,
    ) -> Result<WorldImportResult, String> {
        import_world(
            server.to_str().unwrap(),
            source.to_str().unwrap(),
            name,
            &tracker(OperationKind::Import),
        )
        .map_err(String::from)
    }

    fn build_zip(path: &Path, entries: &[(&str, &[u8])], stored: bool) {
        let mut writer = zip::ZipWriter::new(fs::File::create(path).unwrap());
        let options =
            zip::write::FileOptions::default().compression_method(if stored {
                zip::CompressionMethod::Stored
            } else {
                zip::CompressionMethod::Deflated
            });
        for (name, data) in entries {
            if name.ends_with('/') {
                writer.add_directory(*name, options).unwrap();
            } else {
                writer.start_file(*name, options).unwrap();
                writer.write_all(data).unwrap();
            }
        }
        writer.finish().unwrap();
    }

    fn no_leftovers(dir: &Path) {
        let names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            !names.iter().any(|name| name.starts_with(".importing-")
                || name.ends_with(".cloudcraft-part")),
            "leftover staging data: {names:?}"
        );
    }

    #[test]
    fn export_then_import_round_trips_every_file() {
        let server = server_with_world("world");
        let out = temp_dir("out");
        let zip = out.join("backup.zip");
        let exported = export(&server, &zip).unwrap();
        assert_eq!(exported.files, 2);
        assert!(exported.archive_bytes > 0);
        assert!(zip.is_file());
        no_leftovers(&out);

        let other = temp_dir("other-server");
        let imported = import(&other, &zip, "Imported World").unwrap();
        assert_eq!(imported.files, 2);
        assert_eq!(imported.skipped_entries, 0);
        let folder = other.join("Imported World");
        assert_eq!(fs::read(folder.join("level.dat")).unwrap(), level_bytes());
        assert_eq!(
            fs::read(folder.join("region/r.0.0.mca")).unwrap(),
            vec![7u8; 200_000]
        );
        assert!(folder.join("empty-dir").is_dir());
        no_leftovers(&other);
        assert!(import(&other, &zip, "Imported World").is_err());
    }

    #[test]
    fn export_never_overwrites_or_writes_inside_the_world() {
        let server = server_with_world("world");
        let out = temp_dir("out");
        let existing = out.join("taken.zip");
        fs::write(&existing, b"mine").unwrap();
        assert!(export(&server, &existing)
            .unwrap_err()
            .contains("already exists"));
        assert_eq!(fs::read(&existing).unwrap(), b"mine");

        let inside = server.join("world/self.zip");
        assert!(export(&server, &inside).is_err());
        assert!(export(&server, &out.join("notazip.txt")).is_err());
        assert!(!server.join("world/self.zip").exists());
    }

    #[test]
    fn export_requires_a_real_world() {
        let server = temp_dir("empty-server");
        fs::create_dir(server.join("world")).unwrap();
        let out = temp_dir("out");
        assert!(export(&server, &out.join("a.zip"))
            .unwrap_err()
            .contains("level.dat"));
        no_leftovers(&out);
        assert!(!out.join("a.zip").exists());
    }

    #[test]
    fn zip_slip_entries_are_rejected_without_touching_disk() {
        let server = temp_dir("server");
        let out = temp_dir("out");
        let zip = out.join("evil.zip");
        let level = level_bytes();
        build_zip(
            &zip,
            &[("world/level.dat", &level), ("world/../../evil.txt", b"x")],
            false,
        );
        let error = import(&server, &zip, "w").unwrap_err();
        assert!(error.contains("escape"), "{error}");
        assert!(!server.parent().unwrap().join("evil.txt").exists());
        assert!(!server.join("w").exists());
        no_leftovers(&server);
    }

    #[test]
    fn archives_without_a_world_or_with_several_are_rejected() {
        let server = temp_dir("server");
        let out = temp_dir("out");
        let level = level_bytes();

        let none = out.join("none.zip");
        build_zip(&none, &[("readme.txt", b"hi")], false);
        assert!(import(&server, &none, "a")
            .unwrap_err()
            .contains("level.dat"));

        let two = out.join("two.zip");
        build_zip(
            &two,
            &[("one/level.dat", &level), ("two/level.dat", &level)],
            false,
        );
        assert!(import(&server, &two, "b")
            .unwrap_err()
            .contains("more than one"));

        let bad_level = out.join("bad.zip");
        build_zip(&bad_level, &[("level.dat", b"not gzip")], false);
        assert!(import(&server, &bad_level, "c")
            .unwrap_err()
            .contains("not a valid Minecraft level file"));
        no_leftovers(&server);
        assert!(!server.join("a").exists());
        assert!(!server.join("b").exists());
        assert!(!server.join("c").exists());
    }

    #[test]
    fn top_level_folder_is_unwrapped_and_junk_is_counted() {
        let server = temp_dir("server");
        let out = temp_dir("out");
        let level = level_bytes();
        let zip = out.join("wrapped.zip");
        build_zip(
            &zip,
            &[
                ("My World/", b""),
                ("My World/level.dat", &level),
                ("My World/data/x.dat", b"x"),
                ("__MACOSX/._level.dat", b"junk"),
            ],
            false,
        );
        let done = import(&server, &zip, "Wrapped").unwrap();
        assert_eq!(done.files, 2);
        assert_eq!(done.skipped_entries, 1);
        assert!(server.join("Wrapped/level.dat").is_file());
        assert!(server.join("Wrapped/data/x.dat").is_file());
        assert!(!server.join("Wrapped/My World").exists());
    }

    #[test]
    fn corrupt_archive_data_fails_and_leaves_nothing() {
        let server = temp_dir("server");
        let out = temp_dir("out");
        let level = level_bytes();
        let zip = out.join("corrupt.zip");
        let marker = b"UNIQUE-MARKER-PAYLOAD-0123456789";
        build_zip(&zip, &[("level.dat", &level), ("data.bin", marker)], true);
        let mut bytes = fs::read(&zip).unwrap();
        let position = bytes
            .windows(marker.len())
            .position(|window| window == marker)
            .unwrap();
        bytes[position + 3] ^= 0xff;
        fs::write(&zip, bytes).unwrap();

        let error = import(&server, &zip, "corrupt").unwrap_err();
        assert!(error.contains("corrupt"), "{error}");
        assert!(!server.join("corrupt").exists());
        no_leftovers(&server);
        assert!(import(&server, &out.join("missing.zip"), "m").is_err());
        let text = out.join("a.txt");
        fs::write(&text, b"x").unwrap();
        assert!(import(&server, &text, "t").unwrap_err().contains(".zip"));
    }

    #[test]
    fn import_refuses_unsafe_or_existing_names_without_changes() {
        let server = server_with_world("world");
        let out = temp_dir("out");
        let zip = out.join("w.zip");
        export(&server, &zip).unwrap();
        for bad in ["", ".hidden", "a/b", "..", "con", "x:y", BACKUP_ROOT] {
            assert!(import(&server, &zip, bad).is_err(), "accepted {bad:?}");
        }
        assert!(import(&server, &zip, "world")
            .unwrap_err()
            .contains("already exists"));
        assert_eq!(
            fs::read(server.join("world/level.dat")).unwrap(),
            level_bytes()
        );
    }

    #[test]
    fn world_folder_import_copies_and_validates() {
        let source_server = server_with_world("world");
        let server = temp_dir("server");
        let done =
            import(&server, &source_server.join("world"), "Copy").unwrap();
        assert_eq!(done.files, 2);
        assert_eq!(
            fs::read(server.join("Copy/level.dat")).unwrap(),
            level_bytes()
        );

        let not_world = temp_dir("not-world");
        assert!(import(&server, &not_world, "Nope")
            .unwrap_err()
            .contains("level.dat"));
        assert!(import(&source_server, &source_server, "Self").is_err());
        no_leftovers(&server);
    }

    #[cfg(unix)]
    #[test]
    fn symbolic_links_in_folders_are_rejected() {
        let source_server = server_with_world("world");
        std::os::unix::fs::symlink(
            "/etc/passwd",
            source_server.join("world/link"),
        )
        .unwrap();
        let server = temp_dir("server");
        let error =
            import(&server, &source_server.join("world"), "L").unwrap_err();
        assert!(error.contains("symbolic link"), "{error}");
        assert!(!server.join("L").exists());
        no_leftovers(&server);
        let out = temp_dir("out");
        assert!(export(&source_server, &out.join("l.zip")).is_err());
        no_leftovers(&out);
    }

    #[test]
    fn symlink_modes_and_unsafe_names_are_detected() {
        assert!(is_symlink_mode(0o120777));
        assert!(!is_symlink_mode(0o100644));
        assert!(!safe_relative_path(Path::new("a/../b")));
        assert!(!safe_relative_path(Path::new("/abs")));
        assert!(!safe_relative_path(Path::new("nul.txt")));
        assert!(!safe_relative_path(Path::new("bad:name")));
        assert!(safe_relative_path(Path::new("region/r.0.0.mca")));
    }

    #[test]
    fn cancelling_an_import_leaves_no_partial_world() {
        let server = temp_dir("server");
        let out = temp_dir("out");
        let level = level_bytes();
        let big = vec![1u8; 4 * 1024 * 1024];
        let zip = out.join("big.zip");
        let mut entries: Vec<(String, &[u8])> =
            vec![("level.dat".into(), level.as_slice())];
        for index in 0..40 {
            entries.push((format!("data/{index}.bin"), big.as_slice()));
        }
        let borrowed: Vec<(&str, &[u8])> = entries
            .iter()
            .map(|(name, data)| (name.as_str(), *data))
            .collect();
        build_zip(&zip, &borrowed, true);

        let registry: Arc<Registry> = Registry::new();
        let tracker = registry.begin(OperationKind::Import).unwrap();
        let canceller = std::thread::spawn({
            let registry = registry.clone();
            move || {
                for _ in 0..2000 {
                    std::thread::sleep(Duration::from_millis(2));
                    if registry.request_cancel().is_ok() {
                        return true;
                    }
                }
                false
            }
        });
        let result = import_world(
            server.to_str().unwrap(),
            zip.to_str().unwrap(),
            "Cancelled",
            &tracker,
        );
        assert!(canceller.join().unwrap());
        assert_eq!(result.unwrap_err(), OpError::Cancelled);
        assert!(!server.join("Cancelled").exists());
        no_leftovers(&server);
    }

    #[test]
    fn active_world_update_preserves_other_properties() {
        let server = server_with_world("world");
        fs::create_dir_all(server.join("Other")).unwrap();
        fs::write(server.join("Other/level.dat"), level_bytes()).unwrap();
        fs::write(
            server.join("server.properties"),
            "view-distance=12\nlevel-name=world\nmotd=Hi\n",
        )
        .unwrap();
        let previous =
            set_active_world(server.to_str().unwrap(), "Other").unwrap();
        assert_eq!(previous, "world");
        let text =
            fs::read_to_string(server.join("server.properties")).unwrap();
        assert!(text.contains("view-distance=12"));
        assert!(text.contains("motd=Hi"));
        assert!(text.contains("level-name=Other"));
        assert_eq!(text.matches("level-name=").count(), 1);
        assert!(set_active_world(server.to_str().unwrap(), "missing").is_err());
        assert!(set_active_world(server.to_str().unwrap(), "../x").is_err());
        assert!(!server.join("server.properties.cloudcraft-tmp").exists());
    }
}
