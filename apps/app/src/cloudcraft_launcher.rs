//! CloudCraft launcher diagnostics: how the last game process ended and the
//! crash report Minecraft or the JVM wrote for it.

use chrono::{DateTime, Utc};
use serde::Serialize;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

const MAX_REPORT_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CrashReport {
    pub file_name: String,
    pub kind: &'static str,
    pub modified: DateTime<Utc>,
    pub excerpt: String,
    pub truncated: bool,
    pub size_bytes: u64,
}

/// Resolves a profile id inside the profiles directory, refusing escapes.
fn contained_profile_dir(
    profiles_dir: &Path,
    profile_path: &str,
) -> Result<PathBuf, String> {
    if profile_path.is_empty()
        || profile_path.contains("..")
        || Path::new(profile_path).is_absolute()
    {
        return Err("Invalid instance path.".into());
    }
    let root = fs::canonicalize(profiles_dir).map_err(|e| e.to_string())?;
    let dir = fs::canonicalize(root.join(profile_path))
        .map_err(|_| "The instance folder was not found.".to_string())?;
    if !dir.starts_with(&root) {
        return Err("Invalid instance path.".into());
    }
    Ok(dir)
}

fn read_bounded(path: &Path) -> Result<(String, bool, u64), String> {
    let size = fs::metadata(path).map_err(|e| e.to_string())?.len();
    let mut buffer = Vec::new();
    fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(MAX_REPORT_BYTES)
        .read_to_end(&mut buffer)
        .map_err(|e| e.to_string())?;
    Ok((
        String::from_utf8_lossy(&buffer).into_owned(),
        size > MAX_REPORT_BYTES,
        size,
    ))
}

/// Newest crash report (Minecraft `crash-reports/*.txt` or JVM `hs_err_pid*.log`)
/// modified at or after `since`. Symlinks and non-files are ignored.
pub fn latest_crash_report(
    instance_dir: &Path,
    since: Option<DateTime<Utc>>,
) -> Result<Option<CrashReport>, String> {
    let mut candidates: Vec<(PathBuf, &'static str)> = Vec::new();
    if let Ok(entries) = fs::read_dir(instance_dir.join("crash-reports")) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "txt") {
                candidates.push((path, "minecraft"));
            }
        }
    }
    if let Ok(entries) = fs::read_dir(instance_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with("hs_err_pid") && name.ends_with(".log") {
                candidates.push((entry.path(), "jvm"));
            }
        }
    }
    let newest = candidates
        .into_iter()
        .filter_map(|(path, kind)| {
            let meta = fs::symlink_metadata(&path).ok()?;
            if !meta.file_type().is_file() {
                return None;
            }
            let modified: DateTime<Utc> = meta.modified().ok()?.into();
            Some((path, kind, modified))
        })
        .filter(|(_, _, modified)| since.is_none_or(|since| *modified >= since))
        .max_by_key(|(_, _, modified)| *modified);
    let Some((path, kind, modified)) = newest else {
        return Ok(None);
    };
    let (excerpt, truncated, size_bytes) = read_bounded(&path)?;
    Ok(Some(CrashReport {
        file_name: path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        kind,
        modified,
        excerpt,
        truncated,
        size_bytes,
    }))
}

#[tauri::command]
pub async fn cloudcraft_instance_last_exit(
    profile_path: String,
) -> Result<Option<theseus::data::ProcessExit>, String> {
    let state = theseus::State::get().await.map_err(|e| e.to_string())?;
    Ok(state.process_manager.last_exit(&profile_path))
}

#[tauri::command]
pub async fn cloudcraft_instance_crash_report(
    profile_path: String,
    since: Option<DateTime<Utc>>,
) -> Result<Option<CrashReport>, String> {
    let state = theseus::State::get().await.map_err(|e| e.to_string())?;
    let dir = contained_profile_dir(
        &state.directories.profiles_dir(),
        &profile_path,
    )?;
    tokio::task::spawn_blocking(move || latest_crash_report(&dir, since))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn cloudcraft_instance_open_folder(
    profile_path: String,
    child: Option<String>,
) -> Result<(), String> {
    let state = theseus::State::get().await.map_err(|e| e.to_string())?;
    let dir = contained_profile_dir(
        &state.directories.profiles_dir(),
        &profile_path,
    )?;
    let target = match child.as_deref() {
        None => dir,
        Some(name @ ("crash-reports" | "logs")) => dir.join(name),
        Some(_) => return Err("Unsupported folder.".into()),
    };
    if !target.is_dir() {
        return Err("That folder does not exist yet.".into());
    }
    opener::open(&target).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("cloudcraft-launcher-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(dir.join("profiles/Survival/crash-reports"))
            .unwrap();
        dir
    }

    #[test]
    fn profile_path_cannot_escape_profiles_dir() {
        let root = temp();
        let profiles = root.join("profiles");
        assert!(contained_profile_dir(&profiles, "Survival").is_ok());
        assert!(contained_profile_dir(&profiles, "../").is_err());
        assert!(contained_profile_dir(&profiles, "Survival/../../etc").is_err());
        assert!(contained_profile_dir(&profiles, "/etc").is_err());
        assert!(contained_profile_dir(&profiles, "").is_err());
        assert!(contained_profile_dir(&profiles, "Missing").is_err());
    }

    #[test]
    fn newest_report_after_since_is_returned_and_bounded() {
        let root = temp();
        let instance = root.join("profiles/Survival");
        assert_eq!(latest_crash_report(&instance, None).unwrap(), None);
        fs::write(instance.join("crash-reports/crash-old-client.txt"), "old")
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let since = Utc::now();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let big = "x".repeat((MAX_REPORT_BYTES + 10) as usize);
        fs::write(instance.join("hs_err_pid42.log"), &big).unwrap();
        let report = latest_crash_report(&instance, Some(since))
            .unwrap()
            .unwrap();
        assert_eq!(report.kind, "jvm");
        assert!(report.truncated);
        assert_eq!(report.excerpt.len() as u64, MAX_REPORT_BYTES);
        let future = Utc::now() + chrono::Duration::hours(1);
        assert_eq!(latest_crash_report(&instance, Some(future)).unwrap(), None);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_reports_are_ignored() {
        let root = temp();
        let instance = root.join("profiles/Survival");
        fs::write(root.join("secret.txt"), "secret").unwrap();
        std::os::unix::fs::symlink(
            root.join("secret.txt"),
            instance.join("crash-reports/link.txt"),
        )
        .unwrap();
        assert_eq!(latest_crash_report(&instance, None).unwrap(), None);
    }
}
