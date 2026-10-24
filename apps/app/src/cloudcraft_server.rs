use crate::cloudcraft_server_backup::{
    self as backup, BackupSummary, VerifyReport,
};
use crate::cloudcraft_server_delete::{
    self as deletion, DeleteTarget, DeletionPlan, DeletionResult,
    ServerObservation,
};
use crate::cloudcraft_server_ops::{
    self as ops, OpError, OperationKind, OperationSnapshot, Tracker,
};
use crate::cloudcraft_server_software::{
    self as software, DownloadedSoftware, ResolvedSoftware, SoftwareKind,
};
use crate::cloudcraft_server_world::{
    self as world, WorldExportResult, WorldImportResult,
};
use chrono::{DateTime, Utc};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
#[cfg(test)]
use uuid::Uuid;

const CONFIG_FILE: &str = "cloudcraft-server.json";
const MAX_LOG_LINES: usize = 500;
const STOP_TIMEOUT: Duration = Duration::from_secs(15);

static SERVER_STATE: Lazy<Arc<Mutex<ServerState>>> =
    Lazy::new(|| Arc::new(Mutex::new(ServerState::default())));

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfig {
    pub server_dir: String,
    pub server_jar: String,
    pub java_executable: String,
    pub name: String,
    pub game_version: String,
    pub memory_mb: u32,
    pub max_players: u32,
    pub port: u16,
    pub accept_eula: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum ServerLifecycle {
    Stopped,
    Starting,
    Running,
    Stopping,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerStatus {
    state: ServerLifecycle,
    message: String,
    config: Option<ServerConfig>,
    server_dir: Option<String>,
    name: Option<String>,
    game_version: Option<String>,
    port: Option<u16>,
    pid: Option<u32>,
    reachable: bool,
    connection_address: Option<String>,
    can_start: bool,
    can_stop: bool,
    can_force_stop: bool,
    started_at: Option<DateTime<Utc>>,
    last_error: Option<String>,
}

#[derive(Debug)]
struct ServerState {
    lifecycle: ServerLifecycle,
    config: Option<ServerConfig>,
    child: Option<Child>,
    logs: VecDeque<String>,
    started_at: Option<DateTime<Utc>>,
    last_error: Option<String>,
}

impl Default for ServerState {
    fn default() -> Self {
        Self {
            lifecycle: ServerLifecycle::Stopped,
            config: None,
            child: None,
            logs: VecDeque::new(),
            started_at: None,
            last_error: None,
        }
    }
}

fn lock_state() -> Result<MutexGuard<'static, ServerState>, String> {
    SERVER_STATE.lock().map_err(|_| {
        "The local server state is unavailable after an internal error.".into()
    })
}

fn push_log(state: &mut ServerState, line: String) {
    // Keep diagnostics bounded and never include command lines or environment variables.
    if state.logs.len() == MAX_LOG_LINES {
        state.logs.pop_front();
    }
    state.logs.push_back(line);
}

fn refresh_process(state: &mut ServerState) -> Result<(), String> {
    let exited = match state.child.as_mut() {
        Some(child) => child.try_wait().map_err(|error| {
            format!("Could not check the server process: {error}")
        })?,
        None => None,
    };

    if let Some(exit) = exited {
        state.child = None;
        state.started_at = None;
        state.lifecycle = if exit.success() {
            ServerLifecycle::Stopped
        } else {
            ServerLifecycle::Failed
        };
        state.last_error = if exit.success() {
            None
        } else {
            Some(format!("The server process exited with status {exit}."))
        };
        push_log(
            state,
            format!("[CloudCraft] Server process exited with status {exit}."),
        );
    }
    Ok(())
}

fn is_active(lifecycle: ServerLifecycle) -> bool {
    matches!(
        lifecycle,
        ServerLifecycle::Starting
            | ServerLifecycle::Running
            | ServerLifecycle::Stopping
    )
}

fn status_from(state: &mut ServerState) -> ServerStatus {
    let reachable = if matches!(
        state.lifecycle,
        ServerLifecycle::Starting | ServerLifecycle::Running
    ) && state.child.is_some()
    {
        state.config.as_ref().is_some_and(|config| {
            let address = SocketAddr::from(([127, 0, 0, 1], config.port));
            TcpStream::connect_timeout(&address, Duration::from_millis(150))
                .is_ok()
        })
    } else {
        false
    };

    if reachable && state.lifecycle == ServerLifecycle::Starting {
        state.lifecycle = ServerLifecycle::Running;
    }

    let (message, can_start, can_stop, can_force_stop) = match state.lifecycle {
        ServerLifecycle::Stopped => (
            "Local server is stopped.".to_string(),
            state.config.is_some(),
            false,
            false,
        ),
        ServerLifecycle::Starting => (
            "Server process is running; waiting for the Minecraft port to accept connections."
                .to_string(),
            false,
            true,
            false,
        ),
        ServerLifecycle::Running => (
            "Local server is reachable on this device.".to_string(),
            false,
            true,
            false,
        ),
        ServerLifecycle::Stopping => (
            "Graceful stop was requested but the server has not exited yet.".to_string(),
            false,
            true,
            true,
        ),
        ServerLifecycle::Failed => (
            state
                .last_error
                .clone()
                .unwrap_or_else(|| "The local server stopped unexpectedly.".to_string()),
            state.config.is_some(),
            false,
            false,
        ),
    };

    let config = state.config.as_ref();
    ServerStatus {
        state: state.lifecycle,
        message,
        config: config.cloned(),
        server_dir: config.map(|item| item.server_dir.clone()),
        name: config.map(|item| item.name.clone()),
        game_version: config.map(|item| item.game_version.clone()),
        port: config.map(|item| item.port),
        pid: state.child.as_ref().and_then(|child| {
            if is_active(state.lifecycle) {
                Some(child.id())
            } else {
                None
            }
        }),
        reachable,
        connection_address: if reachable {
            config.map(|item| format!("127.0.0.1:{}", item.port))
        } else {
            None
        },
        can_start,
        can_stop,
        can_force_stop,
        started_at: state.started_at,
        last_error: state.last_error.clone(),
    }
}

fn canonical_directory(value: &str, label: &str) -> Result<PathBuf, String> {
    let path = Path::new(value);
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

fn validate_config(mut config: ServerConfig) -> Result<ServerConfig, String> {
    config.name = config.name.trim().to_string();
    config.game_version = config.game_version.trim().to_string();
    config.java_executable = config.java_executable.trim().to_string();

    if config.name.is_empty()
        || config.name.len() > 64
        || config.name.chars().any(|character| character.is_control())
    {
        return Err(
            "Server name must contain 1–64 printable characters.".into()
        );
    }
    if config.game_version.len() > 64
        || config
            .game_version
            .chars()
            .any(|character| character.is_control())
    {
        return Err(
            "Version label must contain no more than 64 printable characters."
                .into(),
        );
    }
    if !(512..=1_048_576).contains(&config.memory_mb) {
        return Err(
            "Server memory must be between 512 MB and 1,048,576 MB.".into()
        );
    }
    if !(1..=1000).contains(&config.max_players) {
        return Err("Allowed players must be between 1 and 1000.".into());
    }
    if config.port == 0 {
        return Err("Server port must be between 1 and 65535.".into());
    }
    if !config.accept_eula {
        return Err(
            "You must explicitly accept the Minecraft EULA before starting a local server.".into(),
        );
    }

    let server_dir =
        canonical_directory(&config.server_dir, "server directory")?;
    let jar_original = Path::new(&config.server_jar);
    let jar_metadata = fs::symlink_metadata(jar_original)
        .map_err(|error| format!("Could not access the server JAR: {error}"))?;
    if jar_metadata.file_type().is_symlink()
        || !jar_metadata.is_file()
        || jar_metadata.len() == 0
    {
        return Err(
            "Server JAR must be a non-empty regular file, not a symbolic link."
                .into(),
        );
    }
    let server_jar = jar_original.canonicalize().map_err(|error| {
        format!("Could not resolve the server JAR: {error}")
    })?;
    if server_jar.parent() != Some(server_dir.as_path())
        || server_jar
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| !value.eq_ignore_ascii_case("jar"))
            .unwrap_or(true)
    {
        return Err(
            "Place the server JAR directly inside the selected server directory and choose a .jar file."
                .into(),
        );
    }

    if config.java_executable != "java" && config.java_executable != "java.exe"
    {
        let java_original = Path::new(&config.java_executable);
        let java_metadata =
            fs::symlink_metadata(java_original).map_err(|error| {
                format!("Could not access the Java executable: {error}")
            })?;
        if java_metadata.file_type().is_symlink() || !java_metadata.is_file() {
            return Err(
                "Java executable must be a regular file, not a symbolic link."
                    .into(),
            );
        }
        let java = java_original.canonicalize().map_err(|error| {
            format!("Could not resolve the Java executable: {error}")
        })?;
        let valid_name = java
            .file_name()
            .and_then(|value| value.to_str())
            .map(|value| value == "java" || value == "java.exe")
            .unwrap_or(false);
        if !valid_name {
            return Err(
                "Choose a Java executable named java or java.exe.".into()
            );
        }
        config.java_executable = java.to_string_lossy().into_owned();
    }

    config.server_dir = server_dir.to_string_lossy().into_owned();
    config.server_jar = server_jar.to_string_lossy().into_owned();
    Ok(config)
}

fn update_properties(contents: &str, config: &ServerConfig) -> String {
    let managed = [
        "server-port",
        "max-players",
        "online-mode",
        "server-ip",
        "motd",
    ];
    let mut output = contents
        .lines()
        .filter(|line| {
            line.split_once('=')
                .map(|(key, _)| !managed.contains(&key.trim()))
                .unwrap_or(true)
                && !managed.iter().any(|key| {
                    line.trim_start()
                        .strip_prefix(key)
                        .is_some_and(|rest| rest.starts_with('='))
                })
        })
        .map(str::to_string)
        .collect::<Vec<_>>();
    if !output.is_empty() {
        output.push(String::new());
    }
    output
        .push("# Managed by CloudCraft; other settings are preserved.".into());
    output.push(format!("server-port={}", config.port));
    output.push(format!("max-players={}", config.max_players));
    output.push("online-mode=true".into());
    output.push("server-ip=127.0.0.1".into());
    output.push(format!("motd={}", config.name));
    output.push(String::new());
    output.join("\n")
}

fn write_configuration_files(config: &ServerConfig) -> Result<(), String> {
    let server_dir = Path::new(&config.server_dir);
    let serialized = serde_json::to_string_pretty(config).map_err(|error| {
        format!("Could not serialize the server configuration: {error}")
    })?;
    fs::write(server_dir.join(CONFIG_FILE), format!("{serialized}\n"))
        .map_err(|error| {
            format!("Could not save the server configuration: {error}")
        })?;

    fs::write(
        server_dir.join("eula.txt"),
        "# Accepted explicitly in CloudCraft\neula=true\n",
    )
    .map_err(|error| {
        format!("Could not record Minecraft EULA acceptance: {error}")
    })?;

    let properties_path = server_dir.join("server.properties");
    let existing = match fs::read_to_string(&properties_path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            String::new()
        }
        Err(error) => {
            return Err(format!("Could not read server.properties: {error}"))
        }
    };
    fs::write(&properties_path, update_properties(&existing, config)).map_err(
        |error| format!("Could not update server.properties: {error}"),
    )?;
    Ok(())
}

fn read_config(server_dir: &str) -> Result<ServerConfig, String> {
    let directory = canonical_directory(server_dir, "server directory")?;
    let config_path = directory.join(CONFIG_FILE);
    let contents = fs::read_to_string(&config_path).map_err(|error| {
        format!(
            "Could not read {}: {error}. Save a valid local server configuration first.",
            config_path.display()
        )
    })?;
    let config: ServerConfig =
        serde_json::from_str(&contents).map_err(|error| {
            format!("The saved server configuration is invalid: {error}")
        })?;
    validate_config(config)
}

fn configure_server(config: ServerConfig) -> Result<ServerStatus, String> {
    let config = validate_config(config)?;
    let mut state = lock_state()?;
    ensure_no_file_operation()?;
    refresh_process(&mut state)?;
    if is_active(state.lifecycle) {
        return Err(
            "Stop the active local server before changing its configuration."
                .into(),
        );
    }
    write_configuration_files(&config)?;
    state.config = Some(config);
    state.lifecycle = ServerLifecycle::Stopped;
    state.last_error = None;
    Ok(status_from(&mut state))
}

fn load_server(server_dir: String) -> Result<ServerStatus, String> {
    let config = read_config(&server_dir)?;
    let mut state = lock_state()?;
    refresh_process(&mut state)?;
    if is_active(state.lifecycle)
        && state.config.as_ref().map(|item| &item.server_dir)
            != Some(&config.server_dir)
    {
        return Err("Another local server is currently active.".into());
    }
    if !is_active(state.lifecycle) {
        state.config = Some(config);
        state.lifecycle = ServerLifecycle::Stopped;
    }
    Ok(status_from(&mut state))
}

fn forward_output<R>(reader: R, label: &'static str)
where
    R: std::io::Read + Send + 'static,
{
    std::thread::spawn(move || {
        for line in BufReader::new(reader).lines() {
            match line {
                Ok(line) => {
                    if let Ok(mut state) = SERVER_STATE.lock() {
                        push_log(&mut state, format!("[{label}] {line}"));
                    }
                }
                Err(error) => {
                    if let Ok(mut state) = SERVER_STATE.lock() {
                        push_log(
                            &mut state,
                            format!("[CloudCraft] Log read failed: {error}"),
                        );
                    }
                    break;
                }
            }
        }
    });
}

/// Reachability is judged by whether anything answers on the port, so a
/// server must never be started on a port another program already holds;
/// otherwise a crashed start would look like a healthy one.
fn ensure_port_available(port: u16) -> Result<(), String> {
    match std::net::TcpListener::bind(("127.0.0.1", port)) {
        Ok(listener) => {
            drop(listener);
            Ok(())
        }
        Err(error) => Err(format!(
            "Port {port} is not available ({error}). Another program or an earlier server may be using it; choose a different port or stop that program."
        )),
    }
}

fn start_server(server_dir: String) -> Result<ServerStatus, String> {
    let config = read_config(&server_dir)?;
    let mut state = lock_state()?;
    ensure_no_file_operation()?;
    refresh_process(&mut state)?;
    if is_active(state.lifecycle) {
        if state.config.as_ref().map(|item| &item.server_dir)
            == Some(&config.server_dir)
        {
            return Ok(status_from(&mut state));
        }
        return Err("Another local server is already active.".into());
    }

    ensure_port_available(config.port)?;
    let jar = Path::new(&config.server_jar).file_name().ok_or_else(|| {
        "The configured server JAR has no valid file name.".to_string()
    })?;
    let mut child = Command::new(&config.java_executable)
        .current_dir(&config.server_dir)
        .arg(format!("-Xmx{}M", config.memory_mb))
        .arg("-jar")
        .arg(jar)
        .arg("nogui")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            format!("Could not start Java. Check the executable and server JAR: {error}")
        })?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Could not capture server output.".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "Could not capture server errors.".to_string())?;
    forward_output(stdout, "server");
    forward_output(stderr, "server error");

    state.logs.clear();
    state.child = Some(child);
    state.config = Some(config);
    state.lifecycle = ServerLifecycle::Starting;
    state.started_at = Some(Utc::now());
    state.last_error = None;
    push_log(
        &mut state,
        "[CloudCraft] Started the local server process.".into(),
    );
    Ok(status_from(&mut state))
}

fn stop_server() -> Result<ServerStatus, String> {
    stop_server_within(STOP_TIMEOUT)
}

fn stop_server_within(timeout: Duration) -> Result<ServerStatus, String> {
    let mut state = lock_state()?;
    refresh_process(&mut state)?;
    if state.child.is_none() {
        state.lifecycle = ServerLifecycle::Stopped;
        state.last_error = None;
        return Ok(status_from(&mut state));
    }

    {
        let child = state.child.as_mut().expect("checked above");
        if let Some(stdin) = child.stdin.as_mut() {
            stdin
                .write_all(b"stop\n")
                .and_then(|_| stdin.flush())
                .map_err(|error| {
                    format!("Could not send the graceful stop command: {error}")
                })?;
        }
    }
    state.lifecycle = ServerLifecycle::Stopping;
    push_log(
        &mut state,
        "[CloudCraft] Sent the graceful stop command.".into(),
    );

    let deadline = Instant::now() + timeout;
    loop {
        let exited = state
            .child
            .as_mut()
            .expect("checked above")
            .try_wait()
            .map_err(|error| {
                format!("Could not check the stopping server: {error}")
            })?;
        if exited.is_some() {
            break;
        }
        if Instant::now() >= deadline {
            return Ok(status_from(&mut state));
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    refresh_process(&mut state)?;
    if state.lifecycle == ServerLifecycle::Failed {
        state.lifecycle = ServerLifecycle::Stopped;
        state.last_error = None;
    }
    Ok(status_from(&mut state))
}

/// Gracefully stops the server and starts it again from its saved
/// configuration. The configuration is validated before anything is touched,
/// and a server that does not exit in time is never force-killed: the restart
/// is abandoned so the user can decide.
fn restart_server(server_dir: String) -> Result<ServerStatus, String> {
    restart_server_within(server_dir, STOP_TIMEOUT)
}

fn restart_server_within(
    server_dir: String,
    timeout: Duration,
) -> Result<ServerStatus, String> {
    let config = read_config(&server_dir)?;
    let was_active = {
        let mut state = lock_state()?;
        ensure_no_file_operation()?;
        refresh_process(&mut state)?;
        let active = is_active(state.lifecycle);
        if active {
            if state.config.as_ref().map(|item| &item.server_dir)
                != Some(&config.server_dir)
            {
                return Err("Another local server is currently active.".into());
            }
            push_log(&mut state, "[CloudCraft] Restart requested.".into());
        }
        active
    };

    let stopped = stop_server_within(timeout)?;
    if stopped.state == ServerLifecycle::Stopping {
        return Err(format!(
            "The server did not shut down within {} seconds, so it was not restarted and was not force-stopped. Wait for it to finish, or use Force stop.",
            timeout.as_secs().max(1)
        ));
    }
    let status = start_server(server_dir)?;
    if let Ok(mut state) = lock_state() {
        push_log(
            &mut state,
            if was_active {
                "[CloudCraft] Server restarted.".into()
            } else {
                "[CloudCraft] Server started.".into()
            },
        );
    }
    Ok(status)
}

fn force_stop_server() -> Result<ServerStatus, String> {
    let mut state = lock_state()?;
    refresh_process(&mut state)?;
    if let Some(child) = state.child.as_mut() {
        child.kill().map_err(|error| {
            format!("Could not force-stop the server process: {error}")
        })?;
        child.wait().map_err(|error| {
            format!("Could not reap the server process: {error}")
        })?;
    }
    state.child = None;
    state.lifecycle = ServerLifecycle::Stopped;
    state.started_at = None;
    state.last_error = None;
    push_log(
        &mut state,
        "[CloudCraft] Server was force-stopped; unsaved world data may be lost.".into(),
    );
    Ok(status_from(&mut state))
}

fn server_status() -> Result<ServerStatus, String> {
    let mut state = lock_state()?;
    refresh_process(&mut state)?;
    Ok(status_from(&mut state))
}

fn server_logs() -> Result<Vec<String>, String> {
    let state = lock_state()?;
    Ok(state.logs.iter().cloned().collect())
}

#[tauri::command]
pub async fn cloudcraft_server_configure(
    config: ServerConfig,
) -> Result<ServerStatus, String> {
    tokio::task::spawn_blocking(move || configure_server(config))
        .await
        .map_err(|error| {
            format!("Server configuration worker failed: {error}")
        })?
}

#[tauri::command]
pub async fn cloudcraft_server_load(
    server_dir: String,
) -> Result<ServerStatus, String> {
    tokio::task::spawn_blocking(move || load_server(server_dir))
        .await
        .map_err(|error| format!("Server load worker failed: {error}"))?
}

#[tauri::command]
pub async fn cloudcraft_server_start(
    server_dir: String,
) -> Result<ServerStatus, String> {
    tokio::task::spawn_blocking(move || start_server(server_dir))
        .await
        .map_err(|error| format!("Server start worker failed: {error}"))?
}

#[tauri::command]
pub async fn cloudcraft_server_status() -> Result<ServerStatus, String> {
    tokio::task::spawn_blocking(server_status)
        .await
        .map_err(|error| format!("Server status worker failed: {error}"))?
}

#[tauri::command]
pub async fn cloudcraft_server_stop() -> Result<ServerStatus, String> {
    tokio::task::spawn_blocking(stop_server)
        .await
        .map_err(|error| format!("Server stop worker failed: {error}"))?
}

#[tauri::command]
pub async fn cloudcraft_server_restart(
    server_dir: String,
) -> Result<ServerStatus, String> {
    tokio::task::spawn_blocking(move || restart_server(server_dir))
        .await
        .map_err(|error| format!("Server restart worker failed: {error}"))?
}

#[tauri::command]
pub async fn cloudcraft_server_force_stop() -> Result<ServerStatus, String> {
    tokio::task::spawn_blocking(force_stop_server)
        .await
        .map_err(|error| format!("Server force-stop worker failed: {error}"))?
}

#[tauri::command]
pub async fn cloudcraft_server_logs() -> Result<Vec<String>, String> {
    tokio::task::spawn_blocking(server_logs)
        .await
        .map_err(|error| format!("Server log worker failed: {error}"))?
}

/// Starting or reconfiguring a server is refused while a backup, restore or
/// deletion owns the server's files.
fn ensure_no_file_operation() -> Result<(), String> {
    match ops::global().busy() {
        Some(kind) => Err(format!(
            "A {} is in progress. Wait for it to finish before starting or reconfiguring the server.",
            kind.label()
        )),
        None => Ok(()),
    }
}

fn ensure_server_inactive() -> Result<(), String> {
    let mut state = lock_state()?;
    refresh_process(&mut state)?;
    if is_active(state.lifecycle) {
        return Err(
            "Stop the local server before backing up or restoring its world."
                .into(),
        );
    }
    Ok(())
}

fn audit(line: String) {
    tracing::info!(target: "cloudcraft_audit", "{line}");
    if let Ok(mut state) = lock_state() {
        push_log(&mut state, format!("[CloudCraft audit] {line}"));
    }
}

fn configured_port(server_dir: &Path) -> u16 {
    if let Ok(contents) = fs::read_to_string(server_dir.join(CONFIG_FILE)) {
        if let Ok(config) = serde_json::from_str::<ServerConfig>(&contents) {
            return config.port;
        }
    }
    fs::read_to_string(server_dir.join("server.properties"))
        .ok()
        .and_then(|contents| {
            contents
                .lines()
                .filter_map(|line| line.trim().strip_prefix("server-port="))
                .next_back()
                .and_then(|value| value.trim().parse().ok())
        })
        .unwrap_or(25565)
}

/// Decides whether the server for `server_dir` is provably stopped. Anything
/// CloudCraft cannot prove is reported as uncertain, which blocks deletion.
fn observe_server(server_dir: &str) -> ServerObservation {
    let mut state = match lock_state() {
        Ok(state) => state,
        Err(error) => return ServerObservation::Uncertain(error),
    };
    if let Err(error) = refresh_process(&mut state) {
        return ServerObservation::Uncertain(error);
    }
    if is_active(state.lifecycle) || state.child.is_some() {
        return ServerObservation::Active;
    }
    let directory = match canonical_directory(server_dir, "server directory") {
        Ok(directory) => directory,
        Err(error) => return ServerObservation::Uncertain(error),
    };
    let address =
        SocketAddr::from(([127, 0, 0, 1], configured_port(&directory)));
    if TcpStream::connect_timeout(&address, Duration::from_millis(250)).is_ok()
    {
        return ServerObservation::Uncertain(
            "something is accepting connections on the server's port, so a server CloudCraft did not start may be running"
                .into(),
        );
    }
    ServerObservation::Stopped
}

fn run_operation<T, F>(
    kind: OperationKind,
    require_inactive: bool,
    success_message: &'static str,
    task: F,
) -> Result<T, String>
where
    F: FnOnce(&Tracker) -> Result<T, OpError>,
{
    let tracker = ops::global().begin(kind)?;
    let result = if require_inactive {
        ensure_server_inactive().map_err(OpError::Failed)
    } else {
        Ok(())
    }
    .and_then(|()| task(&tracker));
    tracker.finish(&result, success_message);
    result.map_err(String::from)
}

async fn run_blocking<T, F>(work: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| format!("File operation worker failed: {error}"))?
}

#[tauri::command]
pub async fn cloudcraft_server_backup_create(
    server_dir: String,
) -> Result<BackupSummary, String> {
    run_blocking(move || {
        run_operation(OperationKind::Backup, true, "Backup completed.", |t| {
            backup::create_backup(&server_dir, t)
        })
    })
    .await
}

#[tauri::command]
pub async fn cloudcraft_server_backup_list(
    server_dir: String,
) -> Result<Vec<BackupSummary>, String> {
    run_blocking(move || backup::list_backups(&server_dir)).await
}

#[tauri::command]
pub async fn cloudcraft_server_backup_verify(
    server_dir: String,
    backup_id: String,
) -> Result<VerifyReport, String> {
    run_blocking(move || {
        run_operation(
            OperationKind::Verify,
            false,
            "Verification completed.",
            |t| backup::verify_backup(&server_dir, &backup_id, t),
        )
    })
    .await
}

/// Restores into a new folder; the current world is never modified.
#[tauri::command]
pub async fn cloudcraft_server_backup_restore(
    server_dir: String,
    backup_id: String,
) -> Result<String, String> {
    run_blocking(move || {
        run_operation(OperationKind::Restore, true, "Restore completed.", |t| {
            backup::restore_backup(&server_dir, &backup_id, t)
        })
    })
    .await
}

#[tauri::command]
pub async fn cloudcraft_server_software_versions(
    kind: SoftwareKind,
) -> Result<Vec<String>, String> {
    let client = software::build_client()?;
    software::list_versions(&client, kind).await
}

#[tauri::command]
pub async fn cloudcraft_server_software_resolve(
    kind: SoftwareKind,
    version: String,
) -> Result<ResolvedSoftware, String> {
    let client = software::build_client()?;
    software::resolve(&client, kind, &version, None)
        .await
        .map_err(String::from)
}

/// Downloads never overwrite an existing file, so they may run while a server
/// is running; the server only ever sees the finished, verified file.
#[tauri::command]
pub async fn cloudcraft_server_software_download(
    server_dir: String,
    kind: SoftwareKind,
    version: String,
) -> Result<DownloadedSoftware, String> {
    let handle = tokio::runtime::Handle::current();
    run_blocking(move || {
        run_operation(
            OperationKind::Download,
            false,
            "Download verified and installed.",
            |t| software::download(&handle, &server_dir, kind, &version, t),
        )
    })
    .await
}

#[tauri::command]
pub async fn cloudcraft_server_world_export(
    server_dir: String,
    destination: String,
) -> Result<WorldExportResult, String> {
    run_blocking(move || {
        run_operation(OperationKind::Export, true, "Export completed.", |t| {
            let directory = backup::real_directory(
                Path::new(&server_dir),
                "server directory",
            )?;
            let level = backup::level_name(&directory)?;
            world::export_world(&server_dir, &destination, &level, t)
        })
    })
    .await
}

#[tauri::command]
pub async fn cloudcraft_server_world_import(
    server_dir: String,
    source: String,
    name: String,
) -> Result<WorldImportResult, String> {
    run_blocking(move || {
        run_operation(OperationKind::Import, true, "Import completed.", |t| {
            world::import_world(&server_dir, &source, &name, t)
        })
    })
    .await
}

/// Switches `level-name` to an existing world folder. Returns the previous
/// level name so the change can be reversed.
#[tauri::command]
pub async fn cloudcraft_server_world_activate(
    server_dir: String,
    folder: String,
) -> Result<String, String> {
    run_blocking(move || {
        ensure_no_file_operation()?;
        ensure_server_inactive()?;
        world::set_active_world(&server_dir, &folder)
    })
    .await
}

#[tauri::command]
pub async fn cloudcraft_operation_status(
) -> Result<Option<OperationSnapshot>, String> {
    Ok(ops::global().snapshot())
}

#[tauri::command]
pub async fn cloudcraft_operation_cancel() -> Result<OperationSnapshot, String>
{
    ops::global().request_cancel()
}

#[tauri::command]
pub async fn cloudcraft_server_delete_plan(
    server_dir: String,
) -> Result<DeletionPlan, String> {
    run_blocking(move || {
        let observation = observe_server(&server_dir);
        deletion::plan(&server_dir, &observation)
    })
    .await
}

/// Deletes one explicitly confirmed resource. Stopping a server never deletes
/// files, and each target removes only what it names.
#[tauri::command]
pub async fn cloudcraft_server_delete(
    server_dir: String,
    target: DeleteTarget,
    confirmation: String,
) -> Result<DeletionResult, String> {
    run_blocking(move || {
        let tracker = ops::global().begin(OperationKind::Delete)?;
        let observation = observe_server(&server_dir);
        let result = deletion::delete_resource(
            &server_dir,
            &target,
            &confirmation,
            &observation,
            &tracker,
            &mut audit,
        );
        if matches!(target, DeleteTarget::Configuration)
            && result.as_ref().is_ok_and(|outcome| outcome.complete)
        {
            forget_loaded_configuration(&server_dir);
        }
        let message = result
            .as_ref()
            .map(|outcome| outcome.message.clone())
            .unwrap_or_default();
        tracker.finish(&result, &message);
        result.map_err(String::from)
    })
    .await
}

/// After the configuration file is gone the loaded state must not keep
/// offering to start a server that no longer has a configuration.
fn forget_loaded_configuration(server_dir: &str) {
    let Ok(directory) = Path::new(server_dir).canonicalize() else {
        return;
    };
    if let Ok(mut state) = lock_state() {
        let same = state
            .config
            .as_ref()
            .is_some_and(|config| Path::new(&config.server_dir) == directory);
        if same && !is_active(state.lifecycle) {
            state.config = None;
            state.lifecycle = ServerLifecycle::Stopped;
            state.last_error = None;
        }
    }
}

/// Called by the desktop event loop so a launcher exit never abandons a child server.
pub fn shutdown() {
    if let Ok(mut state) = SERVER_STATE.lock() {
        if let Some(child) = state.child.as_mut() {
            if let Some(stdin) = child.stdin.as_mut() {
                let _ = stdin.write_all(b"stop\n");
                let _ = stdin.flush();
            }
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut exited = false;
            while Instant::now() < deadline {
                match child.try_wait() {
                    Ok(Some(_)) => {
                        exited = true;
                        break;
                    }
                    Ok(None) => std::thread::sleep(Duration::from_millis(100)),
                    Err(_) => break,
                }
            }
            if !exited {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        state.child = None;
        state.lifecycle = ServerLifecycle::Stopped;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_root() -> PathBuf {
        std::env::temp_dir()
            .join(format!("cloudcraft-server-test-{}", Uuid::new_v4()))
    }

    fn config(root: &Path) -> ServerConfig {
        let server_dir = root.join("server");
        fs::create_dir_all(&server_dir).unwrap();
        let jar = server_dir.join("server.jar");
        fs::write(&jar, b"test server jar").unwrap();
        ServerConfig {
            server_dir: server_dir.to_string_lossy().into_owned(),
            server_jar: jar.to_string_lossy().into_owned(),
            java_executable: "java".into(),
            name: "Test world".into(),
            game_version: "user supplied label".into(),
            memory_mb: 2048,
            max_players: 10,
            port: 25565,
            accept_eula: true,
        }
    }

    #[test]
    fn configuration_preserves_unmanaged_properties_and_secures_defaults() {
        let root = test_root();
        let server_dir = root.join("server");
        fs::create_dir_all(&server_dir).unwrap();
        fs::write(
            server_dir.join("server.properties"),
            "view-distance=12\nonline-mode=false\nserver-ip=0.0.0.0\n",
        )
        .unwrap();
        let validated = validate_config(config(&root)).unwrap();
        write_configuration_files(&validated).unwrap();

        let properties =
            fs::read_to_string(server_dir.join("server.properties")).unwrap();
        assert!(properties.contains("view-distance=12"));
        assert!(properties.contains("online-mode=true"));
        assert!(properties.contains("server-ip=127.0.0.1"));
        assert!(!properties.contains("online-mode=false"));
        assert!(!properties.contains("server-ip=0.0.0.0"));
        assert_eq!(
            fs::read_to_string(server_dir.join("eula.txt")).unwrap(),
            "# Accepted explicitly in CloudCraft\neula=true\n"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn configuration_requires_explicit_eula_acceptance() {
        let root = test_root();
        let mut config = config(&root);
        config.accept_eula = false;
        assert!(validate_config(config).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn configuration_rejects_jar_outside_server_directory() {
        let root = test_root();
        let mut config = config(&root);
        let outside = root.join("outside.jar");
        fs::write(&outside, b"outside").unwrap();
        config.server_jar = outside.to_string_lossy().into_owned();
        assert!(validate_config(config).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn configuration_rejects_invalid_limits_and_java_arguments() {
        let root = test_root();
        let mut invalid = config(&root);
        invalid.memory_mb = 128;
        assert!(validate_config(invalid).is_err());
        let mut invalid = config(&root);
        invalid.max_players = 0;
        assert!(validate_config(invalid).is_err());
        let mut invalid = config(&root);
        invalid.port = 0;
        assert!(validate_config(invalid).is_err());
        let mut invalid = config(&root);
        invalid.java_executable = "java -Xmalicious".into();
        assert!(validate_config(invalid).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn saved_configuration_round_trips() {
        let root = test_root();
        let validated = validate_config(config(&root)).unwrap();
        write_configuration_files(&validated).unwrap();
        let loaded = read_config(&validated.server_dir).unwrap();
        assert_eq!(loaded.name, "Test world");
        assert_eq!(loaded.memory_mb, 2048);
        assert_eq!(loaded.max_players, 10);
        assert_eq!(loaded.port, 25565);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn inactive_state_has_safe_idempotent_defaults() {
        let mut state = ServerState::default();
        let status = status_from(&mut state);
        assert_eq!(status.state, ServerLifecycle::Stopped);
        assert!(!status.can_start);
        assert!(!status.can_stop);
        assert!(!status.reachable);
        assert!(status.connection_address.is_none());
        assert!(state.logs.is_empty());
    }

    #[test]
    fn log_buffer_is_bounded() {
        let mut state = ServerState::default();
        for index in 0..(MAX_LOG_LINES + 20) {
            push_log(&mut state, format!("line {index}"));
        }
        assert_eq!(state.logs.len(), MAX_LOG_LINES);
        assert_eq!(state.logs.front().unwrap(), "line 20");
    }

    /// Real child processes: a stand-in `java` that behaves like a server
    /// (exits when it reads `stop`, or ignores it entirely).
    #[cfg(unix)]
    mod lifecycle {
        use super::*;
        use std::os::unix::fs::PermissionsExt;

        static SERIAL: Mutex<()> = Mutex::new(());

        const POLITE: &str = "#!/bin/sh\necho fake server up\nwhile IFS= read -r line; do\n  if [ \"$line\" = \"stop\" ]; then echo fake server stopping; exit 0; fi\ndone\n";
        const STUBBORN: &str = "#!/bin/sh\nwhile true; do sleep 0.2; done\n";

        struct Fixture {
            root: PathBuf,
            server_dir: String,
        }

        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = force_stop_server();
                if let Ok(mut state) = lock_state() {
                    state.config = None;
                }
                let _ = fs::remove_dir_all(&self.root);
            }
        }

        fn fixture(script: &str) -> Fixture {
            let root = test_root();
            let mut config = config(&root);
            let bin = root.join("bin");
            fs::create_dir_all(&bin).unwrap();
            let java = bin.join("java");
            fs::write(&java, script).unwrap();
            fs::set_permissions(&java, fs::Permissions::from_mode(0o755))
                .unwrap();
            config.java_executable = java.to_string_lossy().into_owned();
            config.port = 25999;
            let validated = validate_config(config).unwrap();
            write_configuration_files(&validated).unwrap();
            Fixture {
                root,
                server_dir: validated.server_dir,
            }
        }

        fn guard() -> std::sync::MutexGuard<'static, ()> {
            let guard = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
            let _ = force_stop_server();
            guard
        }

        fn logs() -> Vec<String> {
            server_logs().unwrap()
        }

        /// Never leaves a real server process behind, even when an assertion fails.
        struct Cleanup;

        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = force_stop_server();
                if let Ok(mut state) = lock_state() {
                    state.config = None;
                }
            }
        }

        fn varint(mut value: u32) -> Vec<u8> {
            let mut bytes = Vec::new();
            loop {
                let byte = (value & 0x7f) as u8;
                value >>= 7;
                if value == 0 {
                    bytes.push(byte);
                    return bytes;
                }
                bytes.push(byte | 0x80);
            }
        }

        /// Minecraft server-list ping; returns the JSON status reply.
        fn minecraft_status_once(port: u16) -> Result<String, String> {
            let address = SocketAddr::from(([127, 0, 0, 1], port));
            let mut stream =
                TcpStream::connect_timeout(&address, Duration::from_secs(5))
                    .map_err(|error| error.to_string())?;
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .map_err(|error| error.to_string())?;
            let mut handshake = vec![0x00];
            handshake.extend(varint(767));
            handshake.extend(varint(9));
            handshake.extend(b"127.0.0.1");
            handshake.extend(port.to_be_bytes());
            handshake.push(1);
            let mut packet = varint(handshake.len() as u32);
            packet.extend(handshake);
            packet.extend([1, 0]);
            stream
                .write_all(&packet)
                .map_err(|error| error.to_string())?;
            let mut reply = Vec::new();
            let mut buffer = [0u8; 4096];
            while !String::from_utf8_lossy(&reply).contains("\"version\"") {
                let read = std::io::Read::read(&mut stream, &mut buffer)
                    .map_err(|error| error.to_string())?;
                if read == 0 {
                    break;
                }
                reply.extend_from_slice(&buffer[..read]);
            }
            Ok(String::from_utf8_lossy(&reply).into_owned())
        }

        fn minecraft_status(port: u16) -> Result<String, String> {
            let deadline = Instant::now() + Duration::from_secs(90);
            loop {
                match minecraft_status_once(port) {
                    Ok(reply) => return Ok(reply),
                    Err(error) if Instant::now() >= deadline => {
                        return Err(error)
                    }
                    Err(_) => std::thread::sleep(Duration::from_secs(2)),
                }
            }
        }

        fn wait_until_running(what: &str) -> ServerStatus {
            let deadline = Instant::now() + Duration::from_secs(240);
            loop {
                let status = server_status().unwrap();
                if status.state == ServerLifecycle::Running {
                    return status;
                }
                assert!(
                    matches!(status.state, ServerLifecycle::Starting),
                    "{what}: server left Starting: {:?}\n{}",
                    status.state,
                    logs().join("\n")
                );
                assert!(
                    Instant::now() < deadline,
                    "{what}: server never became reachable\n{}",
                    logs().join("\n")
                );
                std::thread::sleep(Duration::from_secs(1));
            }
        }

        /// Runs a genuine Paper server. Accepting the Minecraft EULA is part of
        /// the configuration, so this is opt-in:
        /// `CLOUDCRAFT_REAL_JAVA=/path/to/java cargo test real_server -- --ignored`
        #[test]
        #[ignore = "downloads and runs a real Minecraft server"]
        fn real_server_end_to_end() {
            let _guard = guard();
            let _cleanup = Cleanup;
            let java = std::env::var("CLOUDCRAFT_REAL_JAVA")
                .expect("set CLOUDCRAFT_REAL_JAVA to a java executable");
            let root = test_root();
            let server_dir = root.join("server");
            fs::create_dir_all(&server_dir).unwrap();
            let runtime = tokio::runtime::Runtime::new().unwrap();
            let registry = ops::Registry::new();
            let tracker = registry.begin(OperationKind::Download).unwrap();
            let downloaded = software::download(
                runtime.handle(),
                server_dir.to_str().unwrap(),
                SoftwareKind::Paper,
                "1.21.8",
                &tracker,
            )
            .unwrap();
            println!("downloaded {downloaded:?}");

            let config = ServerConfig {
                server_dir: server_dir.to_string_lossy().into_owned(),
                server_jar: downloaded.path.clone(),
                java_executable: java,
                name: "CloudCraft e2e".into(),
                game_version: "1.21.8".into(),
                memory_mb: 1536,
                max_players: 2,
                port: 25977,
                accept_eula: true,
            };
            configure_server(config).unwrap();
            let dir = server_dir.to_string_lossy().into_owned();

            let first = start_server(dir.clone()).unwrap();
            let first_pid = first.pid.unwrap();
            let running = wait_until_running("first start");
            println!("first start reachable: {:?}", running.connection_address);
            let status = minecraft_status(25977).unwrap();
            assert!(status.contains("1.21.8"), "{status}");

            let restarted =
                restart_server_within(dir.clone(), Duration::from_secs(60))
                    .unwrap();
            assert_ne!(restarted.pid.unwrap(), first_pid);
            wait_until_running("after restart");
            let status = minecraft_status(25977).unwrap();
            assert!(status.contains("1.21.8"), "{status}");
            println!("restart ok, new pid {:?}", restarted.pid);

            let stopped = stop_server_within(Duration::from_secs(60)).unwrap();
            assert_eq!(stopped.state, ServerLifecycle::Stopped);
            assert!(server_dir.join("world/level.dat").is_file());

            let zip = root.join("world-export.zip");
            let level = backup::level_name(&server_dir).unwrap();
            let registry = ops::Registry::new();
            let exported = world::export_world(
                &dir,
                zip.to_str().unwrap(),
                &level,
                &registry.begin(OperationKind::Export).unwrap(),
            )
            .unwrap();
            println!("exported {exported:?}");
            let registry = ops::Registry::new();
            let imported = world::import_world(
                &dir,
                zip.to_str().unwrap(),
                "Imported",
                &registry.begin(OperationKind::Import).unwrap(),
            )
            .unwrap();
            assert_eq!(imported.files, exported.files);
            assert_eq!(
                world::set_active_world(&dir, "Imported").unwrap(),
                "world"
            );

            start_server(dir.clone()).unwrap();
            wait_until_running("imported world");
            assert!(minecraft_status(25977).unwrap().contains("1.21.8"));
            assert!(
                logs().iter().any(|line| line.contains("Done (")),
                "{}",
                logs().join("\n")
            );
            let stopped = stop_server_within(Duration::from_secs(60)).unwrap();
            assert_eq!(stopped.state, ServerLifecycle::Stopped);
            assert!(server_dir.join("Imported/level.dat").is_file());
            println!("imported world loaded and server stopped cleanly");
        }

        #[test]
        fn restart_replaces_the_process_after_a_graceful_stop() {
            let _guard = guard();
            let fixture = fixture(POLITE);
            let first = start_server(fixture.server_dir.clone()).unwrap();
            let first_pid = first.pid.expect("server should be running");
            std::thread::sleep(Duration::from_millis(300));

            let second = restart_server_within(
                fixture.server_dir.clone(),
                Duration::from_secs(10),
            )
            .unwrap();
            let second_pid = second.pid.expect("restarted server has a pid");
            assert_ne!(first_pid, second_pid);
            assert!(second.can_stop);
            let lines = logs();
            assert!(lines.iter().any(|l| l.contains("Server restarted")));

            std::thread::sleep(Duration::from_millis(300));
            let stopped = stop_server_within(Duration::from_secs(10)).unwrap();
            assert_eq!(stopped.state, ServerLifecycle::Stopped);
            assert!(stopped.pid.is_none());
        }

        #[test]
        fn restart_never_force_kills_a_server_that_will_not_stop() {
            let _guard = guard();
            let fixture = fixture(STUBBORN);
            let started = start_server(fixture.server_dir.clone()).unwrap();
            let pid = started.pid.unwrap();
            std::thread::sleep(Duration::from_millis(300));

            let error = restart_server_within(
                fixture.server_dir.clone(),
                Duration::from_millis(500),
            )
            .unwrap_err();
            assert!(error.contains("did not shut down"), "{error}");
            let status = server_status().unwrap();
            assert_eq!(status.state, ServerLifecycle::Stopping);
            assert_eq!(status.pid, Some(pid));
            assert!(status.can_force_stop);

            let forced = force_stop_server().unwrap();
            assert_eq!(forced.state, ServerLifecycle::Stopped);
        }

        #[test]
        fn start_refuses_a_port_another_program_holds() {
            let _guard = guard();
            let fixture = fixture(POLITE);
            let occupied = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = occupied.local_addr().unwrap().port();
            let mut config = read_config(&fixture.server_dir).unwrap();
            config.port = port;
            configure_server(config).unwrap();

            let error = start_server(fixture.server_dir.clone()).unwrap_err();
            assert!(error.contains("not available"), "{error}");
            let status = server_status().unwrap();
            assert_eq!(status.state, ServerLifecycle::Stopped);
            assert!(status.pid.is_none());
            let error = restart_server_within(
                fixture.server_dir.clone(),
                Duration::from_secs(5),
            )
            .unwrap_err();
            assert!(error.contains("not available"), "{error}");
        }

        #[test]
        fn restart_of_a_stopped_server_simply_starts_it() {
            let _guard = guard();
            let fixture = fixture(POLITE);
            load_server(fixture.server_dir.clone()).unwrap();
            assert_eq!(
                server_status().unwrap().state,
                ServerLifecycle::Stopped
            );
            let status = restart_server_within(
                fixture.server_dir.clone(),
                Duration::from_secs(10),
            )
            .unwrap();
            assert!(status.pid.is_some());
            std::thread::sleep(Duration::from_millis(300));
            stop_server_within(Duration::from_secs(10)).unwrap();
        }

        #[test]
        fn restart_validates_configuration_before_touching_the_process() {
            let _guard = guard();
            let fixture = fixture(POLITE);
            let started = start_server(fixture.server_dir.clone()).unwrap();
            let pid = started.pid.unwrap();
            fs::remove_file(Path::new(&fixture.server_dir).join(CONFIG_FILE))
                .unwrap();

            assert!(restart_server_within(
                fixture.server_dir.clone(),
                Duration::from_secs(5)
            )
            .is_err());
            assert_eq!(server_status().unwrap().pid, Some(pid));
        }
    }
}
