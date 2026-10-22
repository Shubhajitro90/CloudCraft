#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

use native_dialog::{MessageDialog, MessageType};
use tauri::{Listener, Manager};
use theseus::prelude::*;

mod api;
mod error;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "macos")]
#[macro_use]
extern crate cocoa;

#[cfg(target_os = "macos")]
#[macro_use]
extern crate objc;

// Should be called in launcher initialization
#[tracing::instrument(skip_all)]
#[tauri::command]
async fn initialize_state(app: tauri::AppHandle) -> api::Result<()> {
    tracing::info!("Initializing app event state...");
    theseus::EventState::init(app.clone()).await?;

    // #[cfg(feature = "updater")]
    // {
    //     use tauri_plugin_updater::UpdaterExt;

    //     let updater = app.updater_builder().build()?;

    //     let update_fut = updater.check();

        // tracing::info!("Initializing app state...");
        State::init().await?;

    //     let check_bar = theseus::init_loading(
    //         theseus::LoadingBarType::CheckingForUpdates,
    //         1.0,
    //         "Checking for updates...",
    //     )
    //     .await?;

        // tracing::info!("Checking for updates...");
        // let update = update_fut.await;

    //     drop(check_bar);

    //     if let Some(update) = update.ok().flatten() {
    //         tracing::info!("Update found: {:?}", update.download_url);
    //         let loader_bar_id = theseus::init_loading(
    //             theseus::LoadingBarType::LauncherUpdate {
    //                 version: update.version.clone(),
    //                 current_version: update.current_version.clone(),
    //             },
    //             1.0,
    //             "Updating Modrinth App...",
    //         )
    //         .await?;

    //         // 100 MiB
    //         const DEFAULT_CONTENT_LENGTH: u64 = 1024 * 1024 * 100;

    //         update
    //             .download_and_install(
    //                 |chunk_length, content_length| {
    //                     let _ = theseus::emit_loading(
    //                         &loader_bar_id,
    //                         (chunk_length as f64)
    //                             / (content_length
    //                                 .unwrap_or(DEFAULT_CONTENT_LENGTH)
    //                                 as f64),
    //                         None,
    //                     );
    //                 },
    //                 || {},
    //             )
    //             .await?;

    //         app.restart();
    //     }
    // }

    // #[cfg(not(feature = "updater"))]
    // {
        // }
        
    State::init().await?;
    tracing::info!("AstralRinth state successfully initialized.");
    let state = State::get().await?;
    app.asset_protocol_scope()
        .allow_directory(state.directories.caches_dir(), true)?;
    app.asset_protocol_scope()
        .allow_directory(state.directories.caches_dir().join("icons"), true)?;

    Ok(())
}

// Should be call once Vue has mounted the app
#[tracing::instrument(skip_all)]
#[tauri::command]
fn show_window(app: tauri::AppHandle) {
    let win = app.get_window("main").unwrap();
    if let Err(e) = win.show() {
        MessageDialog::new()
            .set_type(MessageType::Error)
            .set_title("Initialization error")
            .set_text(&format!(
                "Cannot display application window due to an error:\n{}",
                e
            ))
            .show_alert()
            .unwrap();
        panic!("cannot display application window")
    } else {
        let _ = win.set_focus();
    }
}

#[tauri::command]
fn is_dev() -> bool {
    cfg!(debug_assertions)
}

// Toggles decorations
#[tauri::command]
async fn toggle_decorations(b: bool, window: tauri::Window) -> api::Result<()> {
    window.set_decorations(b).map_err(|e| {
        theseus::Error::from(theseus::ErrorKind::OtherError(format!(
            "Failed to toggle decorations: {}",
            e
        )))
    })?;
    Ok(())
}

#[tauri::command]
fn restart_app(app: tauri::AppHandle) {
    app.restart();
}

fn cloudcraft_validate_directory(path: &str, label: &str) -> std::result::Result<std::path::PathBuf, String> {
    use std::fs;
    use std::path::Path;

    let requested = Path::new(path);
    let metadata = fs::symlink_metadata(requested)
        .map_err(|error| format!("Could not access {label}: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!("{label} must be an existing directory, not a symlink or file."));
    }

    requested
        .canonicalize()
        .map_err(|error| format!("Could not resolve {label}: {error}"))
}

fn cloudcraft_copy_directory(
    source: &std::path::Path,
    destination: &std::path::Path,
) -> std::io::Result<()> {
    use std::fs;
    use std::io::{Error, ErrorKind};

    fs::create_dir(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());

        if file_type.is_symlink() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!("Backup stopped because '{}' is a symbolic link.", source_path.display()),
            ));
        }

        if file_type.is_dir() {
            cloudcraft_copy_directory(&source_path, &destination_path)?;
        } else if file_type.is_file() {
            fs::copy(&source_path, &destination_path)?;
        } else {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                format!("Backup stopped because '{}' is not a regular file or directory.", source_path.display()),
            ));
        }
    }
    Ok(())
}

fn cloudcraft_backup_world_blocking(
    source_path: &str,
    backup_root: &str,
) -> std::result::Result<String, String> {
    use std::fs;
    use std::path::Path;

    let source_original = Path::new(source_path);
    let source_metadata = fs::symlink_metadata(source_original)
        .map_err(|error| format!("Could not access the world folder: {error}"))?;
    if source_metadata.file_type().is_symlink() || !source_metadata.is_dir() {
        return Err("Choose an existing world folder, not a file or symbolic link.".into());
    }
    let source = cloudcraft_validate_directory(source_path, "world folder")?;
    let backup_root = cloudcraft_validate_directory(backup_root, "backup destination")?;

    if source == backup_root || source.starts_with(&backup_root) || backup_root.starts_with(&source) {
        return Err("The world folder and backup destination must be separate directories.".into());
    }

    let world_name = source
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "The selected world folder has no valid name.".to_string())?;
    let timestamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let destination = backup_root.join(format!("{world_name}-cloudcraft-backup-{timestamp}"));
    if destination.exists() {
        return Err("A backup with this timestamp already exists. Try again in a moment.".into());
    }

    if let Err(error) = cloudcraft_copy_directory(&source, &destination) {
        let _ = fs::remove_dir_all(&destination);
        return Err(format!("World backup failed; any partial copy was removed where possible: {error}"));
    }

    Ok(destination.to_string_lossy().into_owned())
}

fn cloudcraft_restore_world_blocking(
    backup_path: &str,
    destination_parent: &str,
) -> Result<String, String> {
    use std::fs;
    use std::path::Path;

    let backup_original = Path::new(backup_path);
    let backup_metadata = fs::symlink_metadata(backup_original)
        .map_err(|error| format!("Could not access the backup folder: {error}"))?;
    if backup_metadata.file_type().is_symlink() || !backup_metadata.is_dir() {
        return Err("Choose an existing backup folder, not a file or symbolic link.".into());
    }
    let backup = cloudcraft_validate_directory(backup_path, "backup folder")?;
    let destination_parent =
        cloudcraft_validate_directory(destination_parent, "restore destination")?;

    if backup == destination_parent
        || backup.starts_with(&destination_parent)
        || destination_parent.starts_with(&backup)
    {
        return Err("The backup and restore destination must be separate directories.".into());
    }

    let backup_name = backup
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "The selected backup folder has no valid name.".to_string())?;
    let restored_name = match backup_name.rsplit_once("-cloudcraft-backup-") {
        Some((original_name, timestamp))
            if timestamp.len() == 15
                && timestamp
                    .chars()
                    .all(|character| character.is_ascii_digit() || character == '-') =>
        {
            original_name
        }
        _ => backup_name,
    };
    let destination = destination_parent.join(restored_name);
    if destination.exists() {
        return Err(format!(
            "Restore cancelled because '{}' already exists. Choose another destination or move the existing folder first.",
            destination.display()
        ));
    }

    if let Err(error) = cloudcraft_copy_directory(&backup, &destination) {
        let _ = fs::remove_dir_all(&destination);
        return Err(format!("World restore failed; any partial copy was removed where possible: {error}"));
    }

    Ok(destination.to_string_lossy().into_owned())
}

#[tauri::command]
async fn cloudcraft_backup_world(source_path: String, backup_root: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || {
        cloudcraft_backup_world_blocking(&source_path, &backup_root)
    })
    .await
    .map_err(|error| format!("Backup worker failed: {error}"))?
}

#[tauri::command]
async fn cloudcraft_restore_world(backup_path: String, destination_parent: String) -> Result<String, String> {
    tokio::task::spawn_blocking(move || {
        cloudcraft_restore_world_blocking(&backup_path, &destination_parent)
    })
    .await
    .map_err(|error| format!("Restore worker failed: {error}"))?
}

// if Tauri app is called with arguments, then those arguments will be treated as commands
// ie: deep links or filepaths for .mrpacks
fn main() {
    /*
        tracing is set basd on the environment variable RUST_LOG=xxx, depending on the amount of logs to show
            ERROR > WARN > INFO > DEBUG > TRACE
        eg. RUST_LOG=info will show info, warn, and error logs
            RUST_LOG="theseus=trace" will show *all* messages but from theseus only (and not dependencies using similar crates)
            RUST_LOG="theseus=trace" will show *all* messages but from theseus only (and not dependencies using similar crates)

        Error messages returned to Tauri will display as traced error logs if they return an error.
        This will also include an attached span trace if the error is from a tracing error, and the level is set to info, debug, or trace

        on unix:
            RUST_LOG="theseus=trace" {run command}

    */
    let _log_guard = theseus::start_logger();

    tracing::info!("Initialized tracing subscriber. Loading Modrinth App!");

    let mut builder = tauri::Builder::default();

    #[cfg(feature = "updater")]
    {
        builder = builder.plugin(tauri_plugin_updater::Builder::new().build());
    }

    builder = builder
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            if let Some(payload) = args.get(1) {
                tracing::info!("Handling deep link from arg {payload}");
                let payload = payload.clone();
                tauri::async_runtime::spawn(api::utils::handle_command(
                    payload,
                ));
            }

            if let Some(win) = app.get_window("main") {
                let _ = win.set_focus();
            }
        }))
        .plugin(tauri_plugin_os::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_filename(
                    if std::env::current_dir()
                        .ok()
                        .map(|dir| dir.join("portable.txt").exists())
                        .unwrap_or(false)
                    {
                        std::env::current_dir()
                            .ok()
                            .map(|dir| dir.join("UserData/app-window-state.json").to_string_lossy().into_owned())
                            .unwrap()
                    } else {
                        "app-window-state.json".to_string()
                    },
                )
                .build(),
        )
        .setup(|app| {
            #[cfg(target_os = "macos")]
            {
                let payload = macos::deep_link::get_or_init_payload(app);

                let mtx_copy = payload.payload.clone();
                app.listen("deep-link://new-url", move |url| {
                    let mtx_copy_copy = mtx_copy.clone();
                    let request = url.payload().to_owned();

                    let actual_request =
                        serde_json::from_str::<Vec<String>>(&request)
                            .ok()
                            .map(|mut x| x.remove(0))
                            .unwrap_or(request);

                    tauri::async_runtime::spawn(async move {
                        tracing::info!("Handling deep link {actual_request}");

                        let mut payload = mtx_copy_copy.lock().await;
                        if payload.is_none() {
                            *payload = Some(actual_request.clone());
                        }

                        let _ =
                            api::utils::handle_command(actual_request).await;
                    });
                });
            };

            #[cfg(not(target_os = "macos"))]
            app.listen("deep-link://new-url", |url| {
                let payload = url.payload().to_owned();
                tracing::info!("Handling deep link {payload}");
                tauri::async_runtime::spawn(api::utils::handle_command(
                    payload,
                ));
                dbg!(url);
            });

            #[cfg(not(target_os = "linux"))]
            {
                if let Some(window) = app.get_window("main") {
                    window.set_shadow(true).unwrap();
                }
            }

            Ok(())
        });

    builder = builder
        .plugin(api::auth::init())
        .plugin(api::mr_auth::init())
        .plugin(api::import::init())
        .plugin(api::logs::init())
        .plugin(api::jre::init())
        .plugin(api::metadata::init())
        .plugin(api::pack::init())
        .plugin(api::process::init())
        .plugin(api::profile::init())
        .plugin(api::profile_create::init())
        .plugin(api::settings::init())
        .plugin(api::tags::init())
        .plugin(api::utils::init())
        .plugin(api::cache::init())
        .plugin(api::friends::init())
        .invoke_handler(tauri::generate_handler![
            initialize_state,
            is_dev,
            toggle_decorations,
            show_window,
            restart_app,
            cloudcraft_backup_world,
            cloudcraft_restore_world,
        ]);

    #[cfg(target_os = "macos")]
    {
        builder = builder.plugin(macos::window_ext::init());
    }

    tracing::info!("Initializing app...");
    let app = builder.build(tauri::generate_context!());

    match app {
        Ok(app) => {
            #[allow(unused_variables)]
            app.run(|app, event| {
                #[cfg(target_os = "macos")]
                if let tauri::RunEvent::Opened { urls } = event {
                    tracing::info!("Handling webview open {urls:?}");

                    let file = urls
                        .into_iter()
                        .filter_map(|url| url.to_file_path().ok())
                        .next();

                    if let Some(file) = file {
                        let payload =
                            macos::deep_link::get_or_init_payload(app);

                        let mtx_copy = payload.payload.clone();
                        let request = file.to_string_lossy().to_string();
                        tauri::async_runtime::spawn(async move {
                            let mut payload = mtx_copy.lock().await;
                            if payload.is_none() {
                                *payload = Some(request.clone());
                            }

                            let _ = api::utils::handle_command(request).await;
                        });
                    }
                }
            });
        }
        Err(e) => {
            #[cfg(target_os = "windows")]
            {
                // tauri doesn't expose runtime errors, so matching a string representation seems like the only solution
                if format!("{:?}", e).contains(
                    "Runtime(CreateWebview(WebView2Error(WindowsError",
                ) {
                    MessageDialog::new()
                        .set_type(MessageType::Error)
                        .set_title("Initialization error")
                        .set_text("Your Microsoft Edge WebView2 installation is corrupt.\n\nMicrosoft Edge WebView2 is required to run Modrinth App.\n\nLearn how to repair it at https://support.modrinth.com/en/articles/8797765-corrupted-microsoft-edge-webview2-installation")
                        .show_alert()
                        .unwrap();

                    panic!("webview2 initialization failed")
                }
            }

            MessageDialog::new()
                .set_type(MessageType::Error)
                .set_title("Initialization error")
                .set_text(&format!(
                    "Cannot initialize application due to an error:\n{:?}",
                    e
                ))
                .show_alert()
                .unwrap();

            tracing::error!("Error while running tauri application: {:?}", e);
            panic!("{1}: {:?}", e, "error while running tauri application")
        }
    }
}
