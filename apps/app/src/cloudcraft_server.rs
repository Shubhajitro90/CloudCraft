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

fn start_server(server_dir: String) -> Result<ServerStatus, String> {
    let config = read_config(&server_dir)?;
    let mut state = lock_state()?;
    refresh_process(&mut state)?;
    if is_active(state.lifecycle) {
        if state.config.as_ref().map(|item| &item.server_dir)
            == Some(&config.server_dir)
        {
            return Ok(status_from(&mut state));
        }
        return Err("Another local server is already active.".into());
    }

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

    let deadline = Instant::now() + STOP_TIMEOUT;
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
        let mut config = config(&root);
        config.memory_mb = 128;
        assert!(validate_config(config.clone()).is_err());
        config = config(&root);
        config.max_players = 0;
        assert!(validate_config(config.clone()).is_err());
        config = config(&root);
        config.port = 0;
        assert!(validate_config(config.clone()).is_err());
        config = config(&root);
        config.java_executable = "java -Xmalicious".into();
        assert!(validate_config(config).is_err());
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
}
