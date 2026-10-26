//! Self-hosted cloud gaming provider: a remote computer the user controls runs
//! Sunshine (open-source GameStream host) and the user's own licensed Minecraft
//! launcher; CloudCraft configures the remote app through Sunshine's documented
//! REST API and runs the Moonlight client locally to receive the encoded video
//! and audio stream and send keyboard, mouse and controller input back.
//!
//! Security model:
//! - The Sunshine API certificate is self-signed, so it is pinned by SHA-256
//!   fingerprint (trust on first use, confirmed by the user). Every API call
//!   verifies the pin and the TLS handshake signature; redirects are refused.
//! - The Sunshine password is held in memory only and never persisted, logged,
//!   or returned to the frontend. Configuration on disk contains no secrets.
//! - Moonlight is started directly (no shell) with fixed argument positions.
//!
//! Readiness and ownership:
//! - A session is reported ready only when Moonlight is still running AND the
//!   host's GameStream `serverinfo` endpoint reports an application session as
//!   running (`SUNSHINE_SERVER_BUSY`). Sunshine starts the app only after a
//!   client completed the authenticated launch handshake, so a process that
//!   merely started, or a pairing that merely succeeded, is never "ready".
//! - Moonlight runs in its own process group (Unix) so Stop terminates it and
//!   any helper it spawned. On Linux it also receives SIGKILL if the launcher
//!   dies. On every platform its PID and executable are recorded; a launcher
//!   that restarts after a crash kills a still-running recorded Moonlight.
//!
//! Pairing Moonlight with the host is a one-time manual step performed in
//! Moonlight and the Sunshine web UI; CloudCraft does not bypass it.

use crate::cloudcraft_cloud_session::{
    redact_secrets, CloudSessionProvider, ProviderError, RemoteSession,
    StartRequest,
};
use futures::future::BoxFuture;
use rustls::client::danger::{
    HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier,
};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

pub const PROVIDER_ID: &str = "sunshine";
pub const REMOTE_APP_NAME: &str = "CloudCraft Minecraft";
pub const DEFAULT_API_PORT: u16 = 47990;
const CONFIG_VERSION: u32 = 1;
const MAX_LOG_LINES: usize = 300;
const API_TIMEOUT: Duration = Duration::from_secs(15);
/// How long Moonlight may take to complete the launch handshake (includes the
/// remote launcher starting, which Sunshine waits for before streaming).
const STREAM_READY_TIMEOUT: Duration = Duration::from_secs(90);
const STREAM_READY_POLL: Duration = Duration::from_secs(1);
/// Grace period between SIGTERM and SIGKILL when stopping Moonlight.
const STREAM_TERM_GRACE: Duration = Duration::from_secs(3);
const PID_FILE_NAME: &str = "cloudcraft-moonlight.pid.json";

/// Host session state reported by the GameStream `serverinfo` endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostStatus {
    pub busy: bool,
    pub current_game: u64,
    pub paired: Option<bool>,
}

fn xml_tag<'a>(xml: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(xml[start..end].trim())
}

/// Parses the XML `serverinfo` response. Requires an explicit `<state>`.
pub fn parse_server_info(xml: &str) -> Result<HostStatus, String> {
    if xml_tag(xml, "root").is_none() && !xml.contains("<root") {
        return Err(
            "The host's serverinfo response is not GameStream XML.".into()
        );
    }
    let state = xml_tag(xml, "state").ok_or(
        "The host's serverinfo response has no session state.".to_string(),
    )?;
    let busy = state.ends_with("_SERVER_BUSY");
    if !busy && !state.ends_with("_SERVER_FREE") {
        return Err(format!("Unknown host session state {state:?}."));
    }
    let current_game = xml_tag(xml, "currentgame")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    let paired = xml_tag(xml, "PairStatus").map(|v| v == "1");
    Ok(HostStatus {
        busy,
        current_game,
        paired,
    })
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct PidRecord {
    pid: u32,
    exe: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SunshineConfig {
    /// Hostname or IP address of the remote computer (no scheme, no port).
    pub host: String,
    pub api_port: u16,
    pub username: String,
    /// Lowercase hex SHA-256 of the Sunshine API certificate (DER).
    pub cert_sha256: String,
    /// Command Sunshine runs on the remote computer to start Minecraft.
    pub launch_command: String,
    pub working_dir: Option<String>,
    /// Absolute path of the local Moonlight executable.
    pub moonlight_path: String,
    /// `WIDTHxHEIGHT`, for example `1920x1080`.
    pub resolution: Option<String>,
    pub fps: Option<u16>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedConfig {
    version: u32,
    config: SunshineConfig,
}

pub fn validate_host(host: &str) -> Result<(), String> {
    let host = host.trim();
    if host.is_empty() || host.len() > 253 {
        return Err(
            "Enter the remote computer's hostname or IP address.".into()
        );
    }
    if host.contains("://") || host.contains('/') {
        return Err("Enter only the hostname or IP address, without https:// or a path.".into());
    }
    let valid = host.chars().all(|c| {
        c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '_')
    });
    if !valid || host.starts_with('-') {
        return Err("The host contains unsupported characters.".into());
    }
    Ok(())
}

fn validate_text(value: &str, label: &str, max: usize) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("{label} is required."));
    }
    if value.len() > max {
        return Err(format!("{label} is too long (maximum {max} characters)."));
    }
    if value.chars().any(|c| c.is_control()) {
        return Err(format!(
            "{label} must be a single line without control characters."
        ));
    }
    Ok(())
}

pub fn parse_fingerprint(value: &str) -> Result<[u8; 32], String> {
    let cleaned: String = value
        .chars()
        .filter(|c| !matches!(c, ':' | ' '))
        .collect::<String>()
        .to_ascii_lowercase();
    if cleaned.len() != 64 || !cleaned.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("The certificate fingerprint must be a SHA-256 value (64 hex digits).".into());
    }
    let mut out = [0u8; 32];
    for (index, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&cleaned[index * 2..index * 2 + 2], 16)
            .map_err(|_| "Invalid fingerprint.".to_string())?;
    }
    Ok(out)
}

pub fn format_fingerprint(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl SunshineConfig {
    pub fn validate(&self) -> Result<(), String> {
        validate_host(&self.host)?;
        if self.api_port == 0 {
            return Err(
                "The Sunshine API port must be between 1 and 65535.".into()
            );
        }
        validate_text(&self.username, "The Sunshine username", 128)?;
        parse_fingerprint(&self.cert_sha256)?;
        validate_text(&self.launch_command, "The remote launch command", 1024)?;
        if let Some(dir) = &self.working_dir {
            if !dir.is_empty() {
                validate_text(dir, "The remote working directory", 1024)?;
            }
        }
        validate_text(&self.moonlight_path, "The Moonlight path", 4096)?;
        if !Path::new(&self.moonlight_path).is_absolute() {
            return Err("The Moonlight path must be an absolute path to the Moonlight program.".into());
        }
        if let Some(resolution) = &self.resolution {
            let valid = resolution.split_once('x').is_some_and(|(w, h)| {
                [w, h].iter().all(|part| {
                    (3..=4).contains(&part.len())
                        && part.chars().all(|c| c.is_ascii_digit())
                })
            });
            if !valid {
                return Err("Resolution must look like 1920x1080.".into());
            }
        }
        if let Some(fps) = self.fps {
            if !(30..=240).contains(&fps) {
                return Err("Frame rate must be between 30 and 240.".into());
            }
        }
        Ok(())
    }

    fn url_host(&self) -> String {
        let host = self.host.trim();
        if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]")
        } else {
            host.to_string()
        }
    }

    pub fn api_base(&self) -> String {
        format!("https://{}:{}", self.url_host(), self.api_port)
    }

    /// Sunshine's GameStream HTTP port is the web UI port minus one
    /// (defaults 47989 and 47990).
    pub fn gamestream_http_port(&self) -> u16 {
        self.api_port.saturating_sub(1).max(1)
    }

    pub fn server_info_url(&self) -> String {
        format!(
            "http://{}:{}/serverinfo?uniqueid=0123456789ABCDEF",
            self.url_host(),
            self.gamestream_http_port()
        )
    }

    /// Host argument for Moonlight; includes the port only when it is not the default.
    fn moonlight_host(&self) -> String {
        if self.api_port == DEFAULT_API_PORT {
            self.host.trim().to_string()
        } else {
            format!("{}:{}", self.url_host(), self.gamestream_http_port())
        }
    }

    /// Arguments for `moonlight stream`. Fixed positions; never passed through a shell.
    pub fn moonlight_stream_args(&self) -> Vec<String> {
        let mut args = vec![
            "stream".to_string(),
            self.moonlight_host(),
            REMOTE_APP_NAME.to_string(),
            "--quit-after".to_string(),
        ];
        if let Some(resolution) = &self.resolution {
            args.push("--resolution".into());
            args.push(resolution.clone());
        }
        if let Some(fps) = self.fps {
            args.push("--fps".into());
            args.push(fps.to_string());
        }
        args
    }

    /// The Sunshine application definition CloudCraft owns on the host.
    pub fn sunshine_app(&self, index: i64) -> serde_json::Value {
        serde_json::json!({
            "name": REMOTE_APP_NAME,
            "output": "",
            "cmd": self.launch_command,
            "working-dir": self.working_dir.clone().unwrap_or_default(),
            "index": index,
            "exclude-global-prep-cmd": false,
            "elevated": false,
            "auto-detach": true,
            "wait-all": true,
            "exit-timeout": 5,
            "prep-cmd": [],
            "detached": [],
            "image-path": "",
        })
    }
}

type SeenFingerprint = Arc<Mutex<Option<[u8; 32]>>>;

/// Verifies the TLS handshake signature with the normal algorithms and accepts
/// the end-entity certificate only when its SHA-256 matches the pin. With no pin
/// (probe mode) it records the fingerprint and never sends credentials.
#[derive(Debug)]
struct PinnedVerifier {
    expected: Option<[u8; 32]>,
    seen: Arc<Mutex<Option<[u8; 32]>>>,
    crypto: Arc<CryptoProvider>,
}

impl ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let digest: [u8; 32] = Sha256::digest(end_entity.as_ref()).into();
        if let Ok(mut seen) = self.seen.lock() {
            *seen = Some(digest);
        }
        match self.expected {
            Some(expected) if expected != digest => Err(rustls::Error::General(
                "Sunshine certificate fingerprint does not match the trusted value".into(),
            )),
            _ => Ok(ServerCertVerified::assertion()),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.crypto.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.crypto.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.crypto
            .signature_verification_algorithms
            .supported_schemes()
    }
}

fn pinned_client(
    expected: Option<[u8; 32]>,
) -> Result<(reqwest::Client, SeenFingerprint), String> {
    let crypto = Arc::new(rustls::crypto::ring::default_provider());
    let seen = Arc::new(Mutex::new(None));
    let verifier = PinnedVerifier {
        expected,
        seen: seen.clone(),
        crypto: crypto.clone(),
    };
    let mut tls = rustls::ClientConfig::builder_with_provider(crypto)
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("TLS setup failed: {e}"))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    tls.alpn_protocols = vec![b"http/1.1".to_vec()];
    let client = reqwest::Client::builder()
        .use_preconfigured_tls(tls)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(API_TIMEOUT)
        .build()
        .map_err(|e| format!("Could not create the HTTPS client: {e}"))?;
    Ok((client, seen))
}

/// Connects without credentials and returns the API certificate fingerprint for
/// the user to compare with the one shown on the host before trusting it.
pub async fn probe_certificate(
    host: &str,
    api_port: u16,
) -> Result<String, String> {
    validate_host(host)?;
    let probe = SunshineConfig {
        host: host.to_string(),
        api_port,
        username: String::new(),
        cert_sha256: String::new(),
        launch_command: String::new(),
        working_dir: None,
        moonlight_path: String::new(),
        resolution: None,
        fps: None,
    };
    let (client, seen) = pinned_client(None)?;
    // configLocale is documented as unauthenticated; no credentials are sent.
    let result = client
        .get(format!("{}/api/configLocale", probe.api_base()))
        .send()
        .await;
    let fingerprint = seen.lock().ok().and_then(|value| *value);
    match (fingerprint, result) {
        (Some(fingerprint), _) => Ok(format_fingerprint(&fingerprint)),
        (None, Err(error)) => Err(format!(
            "Could not reach Sunshine at {}: {}",
            probe.api_base(),
            redact_secrets(&error.to_string())
        )),
        (None, Ok(_)) => {
            Err("The host did not present a TLS certificate.".into())
        }
    }
}

/// Minimal Sunshine REST client (documented endpoints only).
pub struct SunshineApi {
    client: reqwest::Client,
    base: String,
    username: String,
    password: String,
}

impl SunshineApi {
    pub fn new(
        config: &SunshineConfig,
        password: &str,
    ) -> Result<Self, String> {
        let pin = parse_fingerprint(&config.cert_sha256)?;
        let (client, _) = pinned_client(Some(pin))?;
        Ok(Self {
            client,
            base: config.api_base(),
            username: config.username.clone(),
            password: password.to_string(),
        })
    }

    #[cfg(test)]
    fn plain_for_tests(base: String, username: &str, password: &str) -> Self {
        Self {
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(API_TIMEOUT)
                .build()
                .unwrap(),
            base,
            username: username.into(),
            password: password.into(),
        }
    }

    fn describe(&self, error: reqwest::Error) -> String {
        let text = error.to_string();
        if text.contains("fingerprint does not match")
            || format!("{error:?}").contains("fingerprint does not match")
        {
            return "The host's certificate no longer matches the trusted fingerprint. Re-check the host and trust it again only if you changed it yourself.".into();
        }
        redact_secrets(&format!("Could not reach Sunshine: {text}"))
    }

    async fn check(
        response: reqwest::Response,
    ) -> Result<reqwest::Response, String> {
        match response.status().as_u16() {
            200..=299 => Ok(response),
            401 => Err("Sunshine rejected the username or password.".into()),
            status => Err(format!("Sunshine returned HTTP {status}.")),
        }
    }

    pub async fn list_apps(&self) -> Result<Vec<serde_json::Value>, String> {
        let response = self
            .client
            .get(format!("{}/api/apps", self.base))
            .basic_auth(&self.username, Some(&self.password))
            .send()
            .await
            .map_err(|e| self.describe(e))?;
        let body: serde_json::Value =
            Self::check(response).await?.json().await.map_err(|_| {
                "Sunshine returned an unreadable application list.".to_string()
            })?;
        Ok(body
            .get("apps")
            .and_then(|apps| apps.as_array())
            .cloned()
            .unwrap_or_default())
    }

    /// Creates or updates the CloudCraft application and returns its index.
    pub async fn upsert_app(
        &self,
        config: &SunshineConfig,
    ) -> Result<i64, String> {
        let apps = self.list_apps().await?;
        let existing = apps.iter().position(|app| {
            app.get("name").and_then(|n| n.as_str()) == Some(REMOTE_APP_NAME)
        });
        let index = existing.map(|i| i as i64).unwrap_or(-1);
        let response = self
            .client
            .post(format!("{}/api/apps", self.base))
            .basic_auth(&self.username, Some(&self.password))
            .json(&config.sunshine_app(index))
            .send()
            .await
            .map_err(|e| self.describe(e))?;
        let body: serde_json::Value = Self::check(response)
            .await?
            .json()
            .await
            .unwrap_or(serde_json::Value::Null);
        if body.get("status").and_then(|s| s.as_bool()) == Some(false) {
            return Err(format!(
                "Sunshine refused the application definition: {}",
                redact_secrets(
                    &body
                        .get("error")
                        .map(|e| e.to_string())
                        .unwrap_or_default()
                )
            ));
        }
        Ok(if index >= 0 { index } else { apps.len() as i64 })
    }

    pub async fn close_app(&self) -> Result<(), String> {
        let response = self
            .client
            .post(format!("{}/api/apps/close", self.base))
            .basic_auth(&self.username, Some(&self.password))
            .send()
            .await
            .map_err(|e| self.describe(e))?;
        Self::check(response).await.map(|_| ())
    }

    /// Round-trip time of an authenticated request, in milliseconds.
    pub async fn ping(&self) -> Result<u64, String> {
        let started = Instant::now();
        self.list_apps().await?;
        Ok(started.elapsed().as_millis() as u64)
    }
}

/// Reads the host's session state. Plain HTTP is what GameStream exposes before
/// the paired HTTPS channel; no credentials are sent and the answer is used
/// only as a readiness signal (Moonlight itself authenticates the stream).
pub async fn fetch_host_status(url: &str) -> Result<HostStatus, String> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| format!("Could not create the HTTP client: {e}"))?;
    let response = client.get(url).send().await.map_err(|e| {
        redact_secrets(&format!("Host status unavailable: {e}"))
    })?;
    if !response.status().is_success() {
        return Err(format!(
            "Host status request returned HTTP {}.",
            response.status().as_u16()
        ));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("Could not read host status: {e}"))?;
    if bytes.len() > 64 * 1024 {
        return Err("The host status response is unexpectedly large.".into());
    }
    parse_server_info(&String::from_utf8_lossy(&bytes))
}

struct StreamProcess {
    child: Child,
    remote_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderView {
    pub config: Option<SunshineConfig>,
    pub unlocked: bool,
    pub moonlight_found: bool,
    pub streaming: bool,
    pub moonlight_candidates: Vec<String>,
}

pub struct SunshineProvider {
    config_path: PathBuf,
    pid_path: PathBuf,
    config: Mutex<Option<SunshineConfig>>,
    password: Mutex<Option<String>>,
    stream: Mutex<Option<StreamProcess>>,
    logs: Arc<Mutex<VecDeque<String>>>,
    ready_timeout: Duration,
}

fn push_log(logs: &Arc<Mutex<VecDeque<String>>>, line: impl AsRef<str>) {
    if let Ok(mut logs) = logs.lock() {
        if logs.len() >= MAX_LOG_LINES {
            logs.pop_front();
        }
        logs.push_back(redact_secrets(line.as_ref()));
    }
}

pub fn moonlight_candidates() -> Vec<String> {
    let mut paths: Vec<PathBuf> = Vec::new();
    #[cfg(target_os = "windows")]
    {
        for base in ["ProgramFiles", "ProgramFiles(x86)"] {
            if let Ok(dir) = std::env::var(base) {
                paths.push(
                    PathBuf::from(dir)
                        .join("Moonlight Game Streaming")
                        .join("Moonlight.exe"),
                );
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        paths.push(PathBuf::from(
            "/Applications/Moonlight.app/Contents/MacOS/Moonlight",
        ));
    }
    #[cfg(target_os = "linux")]
    {
        paths.push(PathBuf::from("/usr/bin/moonlight"));
        paths.push(PathBuf::from("/usr/bin/moonlight-qt"));
        paths.push(PathBuf::from("/usr/local/bin/moonlight"));
        paths.push(PathBuf::from(
            "/var/lib/flatpak/exports/bin/com.moonlight_stream.Moonlight",
        ));
        if let Some(home) = dirs::home_dir() {
            paths.push(home.join(".local/share/flatpak/exports/bin/com.moonlight_stream.Moonlight"));
        }
    }
    paths
        .into_iter()
        .filter(|p| p.is_file())
        .map(|p| p.to_string_lossy().into_owned())
        .collect()
}

impl SunshineProvider {
    pub fn load(config_path: PathBuf) -> Self {
        let config = fs::read_to_string(&config_path)
            .ok()
            .and_then(|text| {
                serde_json::from_str::<PersistedConfig>(&text).ok()
            })
            .filter(|saved| saved.version == CONFIG_VERSION)
            .map(|saved| saved.config)
            .filter(|config| config.validate().is_ok());
        let pid_path = config_path.with_file_name(PID_FILE_NAME);
        let provider = Self {
            config_path,
            pid_path,
            config: Mutex::new(config),
            password: Mutex::new(None),
            stream: Mutex::new(None),
            logs: Arc::new(Mutex::new(VecDeque::new())),
            ready_timeout: STREAM_READY_TIMEOUT,
        };
        provider.recover_orphaned_stream();
        provider
    }

    /// Kills a Moonlight process recorded by a launcher that exited without
    /// cleaning up. The PID is only trusted when the running process has the
    /// recorded executable, so a reused PID is never killed.
    fn recover_orphaned_stream(&self) {
        let Some(record) = fs::read_to_string(&self.pid_path)
            .ok()
            .and_then(|text| serde_json::from_str::<PidRecord>(&text).ok())
        else {
            let _ = fs::remove_file(&self.pid_path);
            return;
        };
        let pid = sysinfo::Pid::from_u32(record.pid);
        let mut system = sysinfo::System::new();
        let recorded = fs::canonicalize(&record.exe)
            .unwrap_or_else(|_| PathBuf::from(&record.exe));
        let refreshed = system.refresh_process_specifics(
            pid,
            sysinfo::ProcessRefreshKind::new()
                .with_exe(sysinfo::UpdateKind::Always),
        );
        let message = refreshed.then(|| {
            match system.process(pid) {
                Some(process)
                    if process.exe().is_some_and(|exe| {
                        fs::canonicalize(exe).unwrap_or_else(|_| exe.into())
                            == recorded
                    }) =>
                {
                    if process.kill() {
                        format!(
                            "Stopped Moonlight (pid {}) left running by a previous launcher session",
                            record.pid
                        )
                    } else {
                        format!(
                            "Moonlight (pid {}) from a previous session could not be stopped; close it manually",
                            record.pid
                        )
                    }
                }
                _ => format!(
                    "Recorded pid {} is now a different program; it was left alone",
                    record.pid
                ),
            }
        });
        if let Some(message) = message {
            push_log(&self.logs, message);
        }
        let _ = fs::remove_file(&self.pid_path);
    }

    fn write_pid_record(&self, pid: u32, exe: &str) {
        let record = PidRecord {
            pid,
            exe: fs::canonicalize(exe)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| exe.to_string()),
        };
        if let Some(parent) = self.pid_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(text) = serde_json::to_string(&record) {
            let _ = fs::write(&self.pid_path, text);
        }
    }

    fn clear_pid_record(&self) {
        let _ = fs::remove_file(&self.pid_path);
    }

    fn config(&self) -> Option<SunshineConfig> {
        self.config.lock().ok().and_then(|c| c.clone())
    }

    fn password(&self) -> Option<String> {
        self.password.lock().ok().and_then(|p| p.clone())
    }

    pub fn view(&self) -> ProviderView {
        let config = self.config();
        let moonlight_found = config
            .as_ref()
            .is_some_and(|c| Path::new(&c.moonlight_path).is_file());
        ProviderView {
            config,
            unlocked: self.password().is_some(),
            moonlight_found,
            streaming: self.stream_running(),
            moonlight_candidates: moonlight_candidates(),
        }
    }

    pub fn logs(&self) -> Vec<String> {
        self.logs
            .lock()
            .map(|logs| logs.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub fn save_config(&self, config: SunshineConfig) -> Result<(), String> {
        let mut config = config;
        config.host = config.host.trim().to_string();
        config.cert_sha256 =
            format_fingerprint(&parse_fingerprint(&config.cert_sha256)?);
        config.working_dir =
            config.working_dir.filter(|d| !d.trim().is_empty());
        config.resolution = config.resolution.filter(|r| !r.trim().is_empty());
        config.validate()?;
        if let Some(parent) = self.config_path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let serialized = serde_json::to_string_pretty(&PersistedConfig {
            version: CONFIG_VERSION,
            config: config.clone(),
        })
        .map_err(|e| e.to_string())?;
        let temp = self.config_path.with_extension("json.tmp");
        fs::write(&temp, serialized).map_err(|e| e.to_string())?;
        fs::rename(&temp, &self.config_path).map_err(|e| e.to_string())?;
        let changed_identity = self.config().is_none_or(|old| {
            old.host != config.host
                || old.username != config.username
                || old.api_port != config.api_port
                || old.cert_sha256 != config.cert_sha256
        });
        if let Ok(mut current) = self.config.lock() {
            *current = Some(config);
        }
        if changed_identity {
            self.lock_credentials();
        }
        Ok(())
    }

    pub fn clear_config(&self) -> Result<(), String> {
        match fs::remove_file(&self.config_path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
        if let Ok(mut current) = self.config.lock() {
            *current = None;
        }
        self.lock_credentials();
        Ok(())
    }

    /// Verifies the password against the host, then keeps it in memory only.
    pub async fn unlock(&self, password: String) -> Result<(), String> {
        let config = self.config().ok_or("Configure the remote host first.")?;
        if password.is_empty() || password.len() > 256 {
            return Err("Enter the Sunshine password.".into());
        }
        SunshineApi::new(&config, &password)?.list_apps().await?;
        if let Ok(mut current) = self.password.lock() {
            *current = Some(password);
        }
        Ok(())
    }

    pub fn lock_credentials(&self) {
        if let Ok(mut current) = self.password.lock() {
            *current = None;
        }
    }

    fn api(&self) -> Result<(SunshineConfig, SunshineApi), String> {
        let config =
            self.config().ok_or("The remote host is not configured.")?;
        let password = self.password().ok_or(
            "Unlock the remote host with its Sunshine password first.",
        )?;
        let api = SunshineApi::new(&config, &password)?;
        Ok((config, api))
    }

    pub async fn ping(&self) -> Result<u64, String> {
        self.api()?.1.ping().await
    }

    fn stream_running(&self) -> bool {
        let Ok(mut stream) = self.stream.lock() else {
            return false;
        };
        stream
            .as_mut()
            .is_some_and(|s| matches!(s.child.try_wait(), Ok(None)))
    }

    fn spawn_moonlight(
        &self,
        config: &SunshineConfig,
        remote_id: &str,
    ) -> Result<(), String> {
        if !Path::new(&config.moonlight_path).is_file() {
            return Err(format!(
                "Moonlight was not found at {}. Install Moonlight or correct the path.",
                config.moonlight_path
            ));
        }
        let mut command = Command::new(&config.moonlight_path);
        command
            .args(config.moonlight_stream_args())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        #[cfg(target_os = "linux")]
        {
            let parent = std::process::id() as libc::pid_t;
            // SAFETY: only async-signal-safe libc calls run between fork and exec.
            unsafe {
                command.pre_exec(move || {
                    if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    // The launcher may have died before prctl took effect.
                    if libc::getppid() != parent {
                        libc::_exit(1);
                    }
                    Ok(())
                });
            }
        }
        let mut child = command
            .spawn()
            .map_err(|e| format!("Could not start Moonlight: {e}"))?;
        let pid = child.id().unwrap_or(0);
        if pid != 0 {
            self.write_pid_record(pid, &config.moonlight_path);
        }
        push_log(
            &self.logs,
            format!("Started Moonlight (pid {pid}) for {}", config.host),
        );
        for reader in [
            child.stdout.take().map(|s| {
                Box::new(s) as Box<dyn tokio::io::AsyncRead + Unpin + Send>
            }),
            child.stderr.take().map(|s| {
                Box::new(s) as Box<dyn tokio::io::AsyncRead + Unpin + Send>
            }),
        ]
        .into_iter()
        .flatten()
        {
            let logs = self.logs.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(reader).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    push_log(&logs, line);
                }
            });
        }
        let mut stream = self
            .stream
            .lock()
            .map_err(|_| "Stream state is unavailable.")?;
        if let Some(mut previous) = stream.take() {
            let _ = previous.child.start_kill();
        }
        *stream = Some(StreamProcess {
            child,
            remote_id: remote_id.to_string(),
        });
        Ok(())
    }

    /// Returns the exit description if Moonlight already ended.
    fn stream_exit(&self) -> Option<String> {
        let mut stream = self.stream.lock().ok()?;
        let process = stream.as_mut()?;
        match process.child.try_wait() {
            Ok(Some(status)) => Some({
                self.clear_pid_record();
                match status.code() {
                    Some(code) => format!("Moonlight exited with code {code}"),
                    None => "Moonlight was terminated by a signal".into(),
                }
            }),
            Ok(None) => None,
            Err(error) => Some(format!("Moonlight state is unknown: {error}")),
        }
    }

    /// Asks Moonlight's process group to quit, then force-kills it. Returns only
    /// after the direct child has been reaped (or the wait timed out, which is logged).
    async fn kill_stream(&self) {
        let process = self.stream.lock().ok().and_then(|mut s| s.take());
        if let Some(mut process) = process {
            #[cfg(unix)]
            if let Some(pid) = process.child.id() {
                signal_group(pid, libc::SIGTERM);
                if tokio::time::timeout(STREAM_TERM_GRACE, process.child.wait())
                    .await
                    .is_err()
                {
                    signal_group(pid, libc::SIGKILL);
                }
                // Helpers that ignored SIGTERM must not outlive the session.
                signal_group(pid, libc::SIGKILL);
            }
            let _ = process.child.start_kill();
            let reaped = tokio::time::timeout(
                Duration::from_secs(5),
                process.child.wait(),
            )
            .await
            .is_ok();
            self.clear_pid_record();
            push_log(
                &self.logs,
                if reaped {
                    format!("Moonlight stopped for {}", process.remote_id)
                } else {
                    format!(
                        "Moonlight did not exit after being killed for {}",
                        process.remote_id
                    )
                },
            );
        }
    }

    /// Kills the local stream on launcher exit. The remote app is released on
    /// the next start through the recovered session's Stop.
    pub fn shutdown(&self) {
        if let Ok(mut stream) = self.stream.lock() {
            if let Some(process) = stream.as_mut() {
                #[cfg(unix)]
                if let Some(pid) = process.child.id() {
                    signal_group(pid, libc::SIGKILL);
                }
                let _ = process.child.start_kill();
            }
        }
        self.clear_pid_record();
    }

    fn log_tail(&self, lines: usize) -> String {
        let tail = self
            .logs()
            .into_iter()
            .rev()
            .take(lines)
            .collect::<Vec<_>>();
        tail.into_iter().rev().collect::<Vec<_>>().join(" | ")
    }

    /// Waits until Moonlight is alive and the host reports a running app session.
    async fn wait_until_streaming(
        &self,
        config: &SunshineConfig,
    ) -> Result<HostStatus, String> {
        let url = config.server_info_url();
        let deadline = Instant::now() + self.ready_timeout;
        let mut last_problem;
        loop {
            if let Some(exit) = self.stream_exit() {
                return Err(format!(
                    "{exit} before the stream started. Check that Moonlight is paired with this host and the host is reachable. {}",
                    self.log_tail(3)
                ));
            }
            match fetch_host_status(&url).await {
                Ok(status) if status.busy => return Ok(status),
                Ok(_) => {
                    last_problem =
                        "the host reports no running app session".into()
                }
                Err(error) => last_problem = error,
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "Moonlight is running but the stream did not start within {} s ({last_problem}). {}",
                    self.ready_timeout.as_secs(),
                    self.log_tail(3)
                ));
            }
            tokio::time::sleep(STREAM_READY_POLL).await;
        }
    }

    async fn start_with(
        &self,
        config: &SunshineConfig,
        api: &SunshineApi,
    ) -> Result<RemoteSession, ProviderError> {
        let fail = |message: String| ProviderError {
            message,
            remote_id: None,
        };
        if !Path::new(&config.moonlight_path).is_file() {
            return Err(fail(format!(
                "Moonlight was not found at {}.",
                config.moonlight_path
            )));
        }
        if let Ok(status) = fetch_host_status(&config.server_info_url()).await {
            if status.busy {
                return Err(fail(
                    "The host is already streaming an app to another client. Stop that session on the host first.".into(),
                ));
            }
        }
        api.upsert_app(config).await.map_err(fail)?;
        push_log(
            &self.logs,
            format!("Configured \"{REMOTE_APP_NAME}\" on {}", config.host),
        );
        let remote_id =
            format!("{PROVIDER_ID}:{}:{REMOTE_APP_NAME}", config.host);
        self.spawn_moonlight(config, &remote_id).map_err(fail)?;

        match self.wait_until_streaming(config).await {
            Ok(status) => {
                push_log(
                    &self.logs,
                    format!(
                        "Host {} reports the app session running (app id {})",
                        config.host, status.current_game
                    ),
                );
                Ok(RemoteSession { remote_id })
            }
            Err(message) => {
                let released = api.close_app().await.is_ok();
                self.kill_stream().await;
                Err(ProviderError {
                    message,
                    remote_id: if released { None } else { Some(remote_id) },
                })
            }
        }
    }

    async fn start_inner(&self) -> Result<RemoteSession, ProviderError> {
        let (config, api) = self.api().map_err(|message| ProviderError {
            message,
            remote_id: None,
        })?;
        self.start_with(&config, &api).await
    }
}

impl CloudSessionProvider for SunshineProvider {
    fn id(&self) -> &'static str {
        PROVIDER_ID
    }

    fn unavailable_reason(&self) -> Option<String> {
        let Some(config) = self.config() else {
            return Some("Set up a remote host: a computer you control running Sunshine, plus Moonlight on this device.".into());
        };
        if !Path::new(&config.moonlight_path).is_file() {
            return Some(format!(
                "Moonlight was not found at {}. Install Moonlight or correct the path.",
                config.moonlight_path
            ));
        }
        if self.password().is_none() {
            return Some(format!(
                "Unlock {} with its Sunshine password to start a session.",
                config.host
            ));
        }
        None
    }

    fn start<'a>(
        &'a self,
        _request: &'a StartRequest,
    ) -> BoxFuture<'a, Result<RemoteSession, ProviderError>> {
        Box::pin(async move {
            // Dropping this future (cancel/timeout) must not leave a stream running.
            let guard = StreamGuard {
                provider: self,
                armed: true,
            };
            let result = self.start_inner().await;
            let mut guard = guard;
            guard.armed = result.is_err();
            result
        })
    }

    fn stop<'a>(
        &'a self,
        remote_id: &'a str,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let (_, api) = self.api()?;
            api.close_app().await?;
            self.kill_stream().await;
            push_log(
                &self.logs,
                format!("Closed the remote app for {remote_id}"),
            );
            Ok(())
        })
    }

    fn health(&self, _remote_id: &str) -> Option<String> {
        let exit = self.stream_exit()?;
        Some(format!(
            "{exit}. Minecraft may still be running on the remote computer: Reconnect to resume or Stop to close it."
        ))
    }

    fn supports_reconnect(&self) -> bool {
        true
    }

    fn reconnect<'a>(
        &'a self,
        remote_id: &'a str,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let config =
                self.config().ok_or("The remote host is not configured.")?;
            self.spawn_moonlight(&config, remote_id)?;
            match self.wait_until_streaming(&config).await {
                Ok(_) => Ok(()),
                Err(message) => {
                    self.kill_stream().await;
                    Err(format!("Reconnect failed: {message}"))
                }
            }
        })
    }
}

/// Signals the process group led by `pid` (Moonlight is spawned as a group leader).
#[cfg(unix)]
fn signal_group(pid: u32, signal: libc::c_int) {
    if let Ok(pid) = libc::pid_t::try_from(pid) {
        if pid > 1 {
            // SAFETY: plain syscall; a negative pid addresses the group only.
            unsafe {
                libc::kill(-pid, signal);
            }
        }
    }
}

/// Kills a stream that was spawned by a start future that was then dropped.
struct StreamGuard<'a> {
    provider: &'a SunshineProvider,
    armed: bool,
}

impl Drop for StreamGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.provider.shutdown();
            if let Ok(mut stream) = self.provider.stream.lock() {
                stream.take();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn config(moonlight: &str) -> SunshineConfig {
        SunshineConfig {
            host: "gaming-pc.tail1234.ts.net".into(),
            api_port: DEFAULT_API_PORT,
            username: "admin".into(),
            cert_sha256: "ab".repeat(32),
            launch_command: "C:\\Games\\launcher.exe --launch survival".into(),
            working_dir: None,
            moonlight_path: moonlight.into(),
            resolution: Some("1920x1080".into()),
            fps: Some(60),
        }
    }

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("cloudcraft-sunshine-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn config_validation_rejects_unsafe_values() {
        let abs = if cfg!(windows) {
            "C:\\moonlight.exe"
        } else {
            "/usr/bin/moonlight"
        };
        assert!(config(abs).validate().is_ok());
        let mut c = config(abs);
        c.host = "https://evil.example/path".into();
        assert!(c.validate().is_err());
        let mut c = config(abs);
        c.host = "-oProxyCommand=x".into();
        assert!(c.validate().is_err());
        let mut c = config(abs);
        c.launch_command = "run\nsecond".into();
        assert!(c.validate().is_err());
        let mut c = config(abs);
        c.moonlight_path = "moonlight".into();
        assert!(c.validate().is_err());
        let mut c = config(abs);
        c.resolution = Some("1920*1080".into());
        assert!(c.validate().is_err());
        let mut c = config(abs);
        c.fps = Some(1000);
        assert!(c.validate().is_err());
        let mut c = config(abs);
        c.cert_sha256 = "abc".into();
        assert!(c.validate().is_err());
    }

    #[test]
    fn fingerprint_parsing_accepts_colon_format_and_round_trips() {
        let colon = (0..32)
            .map(|i| format!("{i:02X}"))
            .collect::<Vec<_>>()
            .join(":");
        let parsed = parse_fingerprint(&colon).unwrap();
        assert_eq!(parsed[31], 31);
        assert_eq!(
            parse_fingerprint(&format_fingerprint(&parsed)).unwrap(),
            parsed
        );
    }

    #[test]
    fn moonlight_arguments_are_fixed_and_shell_free() {
        let args = config("/usr/bin/moonlight").moonlight_stream_args();
        assert_eq!(
            args,
            vec![
                "stream",
                "gaming-pc.tail1234.ts.net",
                REMOTE_APP_NAME,
                "--quit-after",
                "--resolution",
                "1920x1080",
                "--fps",
                "60"
            ]
        );
    }

    #[test]
    fn ipv6_hosts_are_bracketed_in_api_url() {
        let mut c = config("/usr/bin/moonlight");
        c.host = "fd7a:115c:a1e0::1".into();
        assert_eq!(c.api_base(), "https://[fd7a:115c:a1e0::1]:47990");
    }

    #[test]
    fn saved_config_contains_no_password_and_reloads() {
        let dir = temp_dir();
        let path = dir.join("cloud.json");
        let provider = SunshineProvider::load(path.clone());
        let abs = if cfg!(windows) {
            "C:\\moonlight.exe"
        } else {
            "/usr/bin/moonlight"
        };
        provider.save_config(config(abs)).unwrap();
        *provider.password.lock().unwrap() = Some("hunter2-secret".into());
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("hunter2"));
        let reloaded = SunshineProvider::load(path);
        assert_eq!(reloaded.config(), Some(config(abs)));
        assert!(reloaded.password().is_none());
        assert!(reloaded.unavailable_reason().is_some());
    }

    #[test]
    fn changing_host_locks_credentials() {
        let dir = temp_dir();
        let provider = SunshineProvider::load(dir.join("cloud.json"));
        let abs = if cfg!(windows) {
            "C:\\moonlight.exe"
        } else {
            "/usr/bin/moonlight"
        };
        provider.save_config(config(abs)).unwrap();
        *provider.password.lock().unwrap() = Some("pw".into());
        let mut other = config(abs);
        other.launch_command = "different".into();
        provider.save_config(other.clone()).unwrap();
        assert!(
            provider.password().is_some(),
            "non-identity edit keeps unlock"
        );
        other.host = "other-host".into();
        provider.save_config(other).unwrap();
        assert!(provider.password().is_none(), "new host requires unlock");
    }

    async fn mock_sunshine(
        expected_auth: &'static str,
    ) -> (String, Arc<Mutex<Vec<String>>>) {
        let (base, seen, _) = mock_sunshine_host(expected_auth, None).await;
        (base, seen)
    }

    /// Tiny HTTP/1.1 server emulating the documented Sunshine endpoints plus
    /// GameStream `serverinfo`. `busy_after` = number of `serverinfo` answers
    /// that report FREE before BUSY (None = never busy). Closing the app frees it.
    async fn mock_sunshine_host(
        expected_auth: &'static str,
        busy_after: Option<usize>,
    ) -> (String, Arc<Mutex<Vec<String>>>, u16) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let apps =
            Arc::new(Mutex::new(vec![serde_json::json!({"name": "Desktop"})]));
        let info_calls = Arc::new(Mutex::new((0usize, false)));
        let seen_clone = seen.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let seen = seen_clone.clone();
                let apps = apps.clone();
                let info_calls = info_calls.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 65536];
                    let mut len = 0;
                    loop {
                        let n = socket.read(&mut buf[len..]).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        len += n;
                        let text =
                            String::from_utf8_lossy(&buf[..len]).to_string();
                        if let Some(end) = text.find("\r\n\r\n") {
                            let content_length = text[..end]
                                .lines()
                                .find_map(|l| {
                                    l.to_ascii_lowercase()
                                        .strip_prefix("content-length:")
                                        .map(|v| {
                                            v.trim()
                                                .parse::<usize>()
                                                .unwrap_or(0)
                                        })
                                })
                                .unwrap_or(0);
                            if len >= end + 4 + content_length {
                                let request_line = text
                                    .lines()
                                    .next()
                                    .unwrap_or("")
                                    .to_string();
                                let authorized = text.lines().any(|l| {
                                    l.eq_ignore_ascii_case(&format!(
                                        "authorization: {expected_auth}"
                                    ))
                                });
                                let body =
                                    &text[end + 4..end + 4 + content_length];
                                seen.lock()
                                    .unwrap()
                                    .push(format!("{request_line} {body}"));
                                let (status, payload) = if request_line
                                    .starts_with("GET /serverinfo")
                                {
                                    let mut calls = info_calls.lock().unwrap();
                                    calls.0 += 1;
                                    if busy_after.is_some_and(|n| calls.0 > n) {
                                        calls.1 = true;
                                    }
                                    let (state, game) = if calls.1 {
                                        ("SUNSHINE_SERVER_BUSY", 7)
                                    } else {
                                        ("SUNSHINE_SERVER_FREE", 0)
                                    };
                                    ("200 OK", format!("<?xml version=\"1.0\" encoding=\"utf-8\"?><root status_code=\"200\"><hostname>mock</hostname><PairStatus>0</PairStatus><currentgame>{game}</currentgame><state>{state}</state></root>"))
                                } else if !authorized {
                                    ("401 Unauthorized", "{}".to_string())
                                } else if request_line
                                    .starts_with("GET /api/apps")
                                {
                                    ("200 OK", serde_json::json!({"env": {}, "apps": *apps.lock().unwrap()}).to_string())
                                } else if request_line
                                    .starts_with("POST /api/apps/close")
                                {
                                    let mut calls = info_calls.lock().unwrap();
                                    calls.1 = false;
                                    calls.0 = 0;
                                    drop(calls);
                                    ("200 OK", r#"{"status":true}"#.to_string())
                                } else if request_line
                                    .starts_with("POST /api/apps")
                                {
                                    let app: serde_json::Value =
                                        serde_json::from_str(body).unwrap();
                                    let index = app["index"].as_i64().unwrap();
                                    let mut apps = apps.lock().unwrap();
                                    if index < 0 {
                                        apps.push(app);
                                    } else {
                                        apps[index as usize] = app;
                                    }
                                    ("200 OK", r#"{"status":true}"#.to_string())
                                } else {
                                    ("404 Not Found", "{}".to_string())
                                };
                                let response = format!(
                                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                                    payload.len()
                                );
                                let _ =
                                    socket.write_all(response.as_bytes()).await;
                                return;
                            }
                        }
                    }
                });
            }
        });
        (base, seen, port)
    }

    // "admin:pw" base64
    const AUTH: &str = "Basic YWRtaW46cHc=";

    #[tokio::test]
    async fn api_upserts_app_once_and_closes() {
        let (base, seen) = mock_sunshine(AUTH).await;
        let api = SunshineApi::plain_for_tests(base, "admin", "pw");
        let c = config("/usr/bin/moonlight");
        assert_eq!(api.upsert_app(&c).await.unwrap(), 1);
        assert_eq!(
            api.upsert_app(&c).await.unwrap(),
            1,
            "second upsert updates in place"
        );
        let apps = api.list_apps().await.unwrap();
        assert_eq!(apps.len(), 2);
        assert_eq!(apps[1]["cmd"], c.launch_command);
        api.close_app().await.unwrap();
        assert!(api.ping().await.is_ok());
        let log = seen.lock().unwrap().join("\n");
        assert!(log.contains("POST /api/apps/close"));
    }

    #[tokio::test]
    async fn api_reports_wrong_password_clearly() {
        let (base, _) = mock_sunshine(AUTH).await;
        let api = SunshineApi::plain_for_tests(base, "admin", "wrong");
        let error = api.list_apps().await.unwrap_err();
        assert!(
            error.contains("rejected the username or password"),
            "{error}"
        );
        assert!(!error.contains("wrong"));
    }

    #[tokio::test]
    async fn pinned_client_refuses_mismatched_certificate_host() {
        // Nothing listens on this port; the point is that a pinned client is
        // constructed and a connection failure is reported without secrets.
        let mut c = config("/usr/bin/moonlight");
        c.host = "127.0.0.1".into();
        c.api_port = 9;
        let api = SunshineApi::new(&c, "secret-password").unwrap();
        let error = api.list_apps().await.unwrap_err();
        assert!(!error.contains("secret-password"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn moonlight_process_lifecycle_is_tracked_and_killed() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir();
        let fake = dir.join("moonlight");
        fs::write(&fake, "#!/bin/sh\necho \"stream $2\"\nsleep 30\n").unwrap();
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
        let provider = SunshineProvider::load(dir.join("cloud.json"));
        let c = config(fake.to_str().unwrap());
        provider.spawn_moonlight(&c, "sunshine:test").unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(provider.stream_running());
        assert!(provider.health("sunshine:test").is_none());
        assert!(provider
            .logs()
            .iter()
            .any(|l| l.contains("stream gaming-pc")));
        provider.kill_stream().await;
        assert!(!provider.stream_running());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn exited_stream_is_reported_by_health() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir();
        let fake = dir.join("moonlight");
        fs::write(&fake, "#!/bin/sh\necho 'not paired' 1>&2\nexit 3\n")
            .unwrap();
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
        let provider = SunshineProvider::load(dir.join("cloud.json"));
        provider
            .spawn_moonlight(&config(fake.to_str().unwrap()), "sunshine:test")
            .unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        let health = provider.health("sunshine:test").unwrap();
        assert!(health.contains("code 3"), "{health}");
    }

    #[test]
    fn server_info_parsing_requires_explicit_state() {
        let busy = parse_server_info("<root><PairStatus>1</PairStatus><currentgame>3</currentgame><state>SUNSHINE_SERVER_BUSY</state></root>").unwrap();
        assert_eq!(
            busy,
            HostStatus {
                busy: true,
                current_game: 3,
                paired: Some(true)
            }
        );
        let free = parse_server_info(
            "<root><currentgame>0</currentgame><state>SUNSHINE_SERVER_FREE</state></root>",
        )
        .unwrap();
        assert!(!free.busy);
        assert!(
            parse_server_info("<root><currentgame>3</currentgame></root>")
                .is_err()
        );
        assert!(parse_server_info("<root><state>WEIRD</state></root>").is_err());
        assert!(parse_server_info("<html>login</html>").is_err());
    }

    #[test]
    fn non_default_port_is_passed_to_moonlight_and_serverinfo() {
        let mut c = config("/usr/bin/moonlight");
        c.api_port = 50000;
        assert_eq!(c.gamestream_http_port(), 49999);
        assert_eq!(
            c.moonlight_stream_args()[1],
            "gaming-pc.tail1234.ts.net:49999"
        );
        assert!(c
            .server_info_url()
            .starts_with("http://gaming-pc.tail1234.ts.net:49999/serverinfo"));
        assert_eq!(
            config("/usr/bin/moonlight").moonlight_stream_args()[1],
            "gaming-pc.tail1234.ts.net"
        );
    }

    #[cfg(unix)]
    fn fake_moonlight(dir: &Path, script: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let fake = dir.join("moonlight");
        fs::write(&fake, format!("#!/bin/sh\n{script}\n")).unwrap();
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
        fake.to_string_lossy().into_owned()
    }

    #[cfg(unix)]
    fn local_config(moonlight: &str, port: u16) -> SunshineConfig {
        let mut c = config(moonlight);
        c.host = "127.0.0.1".into();
        c.api_port = port + 1;
        c
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ready_only_after_host_reports_running_session() {
        let dir = temp_dir();
        let moonlight = fake_moonlight(&dir, "sleep 30");
        let (base, seen, port) = mock_sunshine_host(AUTH, Some(3)).await;
        let api = SunshineApi::plain_for_tests(base, "admin", "pw");
        let mut provider = SunshineProvider::load(dir.join("cloud.json"));
        provider.ready_timeout = Duration::from_secs(20);
        let c = local_config(&moonlight, port);
        let session = provider.start_with(&c, &api).await.unwrap();
        assert!(session.remote_id.starts_with("sunshine:127.0.0.1"));
        assert!(provider.stream_running());
        assert!(
            provider.pid_path.exists(),
            "pid recorded for crash recovery"
        );
        let info_requests = seen
            .lock()
            .unwrap()
            .iter()
            .filter(|l| l.starts_with("GET /serverinfo"))
            .count();
        assert!(info_requests >= 4, "polled until busy: {info_requests}");
        assert!(provider
            .logs()
            .iter()
            .any(|l| l.contains("reports the app session running")));

        provider.stop(&session.remote_id).await.unwrap_err(); // locked: no password
        provider.kill_stream().await;
        assert!(!provider.stream_running());
        assert!(!provider.pid_path.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn running_process_without_host_session_times_out_and_cleans_up() {
        let dir = temp_dir();
        let moonlight = fake_moonlight(&dir, "sleep 30");
        let (base, seen, port) = mock_sunshine_host(AUTH, None).await;
        let api = SunshineApi::plain_for_tests(base, "admin", "pw");
        let mut provider = SunshineProvider::load(dir.join("cloud.json"));
        provider.ready_timeout = Duration::from_secs(2);
        let error = provider
            .start_with(&local_config(&moonlight, port), &api)
            .await
            .unwrap_err();
        assert!(
            error.message.contains("did not start within"),
            "{}",
            error.message
        );
        assert!(
            error.message.contains("no running app session"),
            "{}",
            error.message
        );
        assert!(error.remote_id.is_none(), "remote app was released");
        assert!(!provider.stream_running(), "Moonlight was killed");
        assert!(seen
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.starts_with("POST /api/apps/close")));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn moonlight_exit_before_stream_is_reported_with_its_output() {
        let dir = temp_dir();
        let moonlight =
            fake_moonlight(&dir, "echo 'Computer is not paired' 1>&2\nexit 2");
        let (base, _, port) = mock_sunshine_host(AUTH, None).await;
        let api = SunshineApi::plain_for_tests(base, "admin", "pw");
        let provider = SunshineProvider::load(dir.join("cloud.json"));
        let error = provider
            .start_with(&local_config(&moonlight, port), &api)
            .await
            .unwrap_err();
        assert!(
            error.message.contains("exited with code 2"),
            "{}",
            error.message
        );
        assert!(error.message.contains("not paired"), "{}", error.message);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn busy_host_is_refused_before_launching() {
        let dir = temp_dir();
        let moonlight = fake_moonlight(&dir, "sleep 30");
        let (base, seen, port) = mock_sunshine_host(AUTH, Some(0)).await;
        let api = SunshineApi::plain_for_tests(base, "admin", "pw");
        let provider = SunshineProvider::load(dir.join("cloud.json"));
        let error = provider
            .start_with(&local_config(&moonlight, port), &api)
            .await
            .unwrap_err();
        assert!(
            error.message.contains("already streaming"),
            "{}",
            error.message
        );
        assert!(!provider.stream_running());
        assert!(!seen.lock().unwrap().iter().any(|l| l.starts_with("POST")));
    }

    #[cfg(unix)]
    fn process_alive(pid: i32) -> bool {
        // A zombie is dead for our purposes.
        match fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(stat) => !stat
                .rsplit_once(')')
                .is_some_and(|(_, rest)| rest.trim_start().starts_with('Z')),
            Err(_) => unsafe { libc::kill(pid, 0) == 0 },
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stop_kills_helpers_spawned_by_moonlight() {
        let dir = temp_dir();
        let child_pid = dir.join("helper.pid");
        let moonlight = fake_moonlight(
            &dir,
            &format!(
                "trap '' TERM\nsleep 60 &\necho $! > {}\nwait",
                child_pid.display()
            ),
        );
        let provider = SunshineProvider::load(dir.join("cloud.json"));
        provider
            .spawn_moonlight(&config(&moonlight), "sunshine:test")
            .unwrap();
        let mut helper = None;
        for _ in 0..50 {
            if let Some(pid) = fs::read_to_string(&child_pid)
                .ok()
                .and_then(|t| t.trim().parse::<i32>().ok())
            {
                helper = Some(pid);
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let helper = helper.expect("helper started");
        assert!(process_alive(helper));
        provider.kill_stream().await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!process_alive(helper), "helper {helper} outlived Stop");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn orphaned_moonlight_from_crashed_launcher_is_killed_on_load() {
        let dir = temp_dir();
        // A shell that stays the process image (`; true` prevents exec of sleep);
        // multicall coreutils builds make a copied `sleep` re-exec another file.
        let exe = fs::canonicalize("/bin/sh").unwrap();
        let mut orphan = std::process::Command::new(&exe)
            .args(["-c", "sleep 60; true"])
            .spawn()
            .unwrap();
        let pid = orphan.id();
        std::thread::sleep(Duration::from_millis(200));
        let config_path = dir.join("cloud.json");
        fs::write(
            config_path.with_file_name(PID_FILE_NAME),
            serde_json::to_string(&PidRecord {
                pid,
                exe: exe.to_string_lossy().into_owned(),
            })
            .unwrap(),
        )
        .unwrap();
        let provider = SunshineProvider::load(config_path.clone());
        let status = orphan.wait().unwrap();
        assert!(!status.success(), "orphan was killed");
        assert!(provider.logs().iter().any(|l| l.contains("left running")));
        assert!(!config_path.with_file_name(PID_FILE_NAME).exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn reused_pid_with_different_program_is_left_alone() {
        let dir = temp_dir();
        let mut unrelated = std::process::Command::new("/bin/sleep")
            .arg("60")
            .spawn()
            .unwrap();
        let config_path = dir.join("cloud.json");
        fs::write(
            config_path.with_file_name(PID_FILE_NAME),
            serde_json::to_string(&PidRecord {
                pid: unrelated.id(),
                exe: "/opt/not-moonlight".into(),
            })
            .unwrap(),
        )
        .unwrap();
        let provider = SunshineProvider::load(config_path);
        assert!(
            unrelated.try_wait().unwrap().is_none(),
            "unrelated process kept"
        );
        assert!(provider.logs().iter().any(|l| l.contains("left alone")));
        let _ = unrelated.kill();
        let _ = unrelated.wait();
    }

    /// Opt-in test against a REAL Sunshine host with an already-paired local
    /// Moonlight. Never runs in ordinary CI; it panics when not configured so
    /// that an explicit run cannot silently pass.
    ///
    /// ```text
    /// CLOUDCRAFT_SUNSHINE_HOST=192.168.1.20 \
    /// CLOUDCRAFT_SUNSHINE_USER=admin CLOUDCRAFT_SUNSHINE_PASSWORD=... \
    /// CLOUDCRAFT_SUNSHINE_CERT_SHA256=<64 hex> \
    /// CLOUDCRAFT_MOONLIGHT=/usr/bin/moonlight \
    /// [CLOUDCRAFT_SUNSHINE_PORT=47990] [CLOUDCRAFT_SUNSHINE_LAUNCH_COMMAND=...] \
    /// [CLOUDCRAFT_SUNSHINE_HOLD_SECS=10] \
    /// cargo test -p theseus_gui --no-default-features real_sunshine -- --ignored --nocapture
    /// ```
    #[tokio::test]
    #[ignore = "needs a real Sunshine host and a paired Moonlight; see doc comment"]
    async fn real_sunshine_moonlight_stream_lifecycle() {
        let env = |key: &str| {
            std::env::var(key)
                .ok()
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| {
                    panic!("{key} must be set for the real Sunshine test")
                })
        };
        let host = env("CLOUDCRAFT_SUNSHINE_HOST");
        let password = env("CLOUDCRAFT_SUNSHINE_PASSWORD");
        let api_port = std::env::var("CLOUDCRAFT_SUNSHINE_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(DEFAULT_API_PORT);
        let hold = std::env::var("CLOUDCRAFT_SUNSHINE_HOLD_SECS")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(10u64);
        let real = SunshineConfig {
            host: host.clone(),
            api_port,
            username: env("CLOUDCRAFT_SUNSHINE_USER"),
            cert_sha256: env("CLOUDCRAFT_SUNSHINE_CERT_SHA256"),
            launch_command: std::env::var("CLOUDCRAFT_SUNSHINE_LAUNCH_COMMAND")
                .unwrap_or_else(|_| "sleep 600".into()),
            working_dir: None,
            moonlight_path: env("CLOUDCRAFT_MOONLIGHT"),
            resolution: Some("1280x720".into()),
            fps: Some(30),
        };
        let dir = temp_dir();

        // Invalid configuration and missing dependency are refused up front.
        let mut invalid = real.clone();
        invalid.moonlight_path = "moonlight".into();
        assert!(invalid.validate().is_err());
        let missing = SunshineProvider::load(dir.join("missing.json"));
        let mut absent = real.clone();
        absent.moonlight_path =
            dir.join("no-moonlight").to_string_lossy().into();
        missing.save_config(absent).unwrap();
        let reason = missing.unavailable_reason().unwrap();
        assert!(reason.contains("not found"), "{reason}");

        // The pinned certificate is the one the host presents.
        let probed = probe_certificate(&host, api_port).await.unwrap();
        assert_eq!(
            probed,
            format_fingerprint(&parse_fingerprint(&real.cert_sha256).unwrap()),
            "host certificate differs from CLOUDCRAFT_SUNSHINE_CERT_SHA256"
        );

        let provider = SunshineProvider::load(dir.join("cloud.json"));
        provider.save_config(real.clone()).unwrap();
        let wrong = provider.unlock("definitely-wrong-password".into()).await;
        assert!(wrong.unwrap_err().contains("rejected"));
        provider.unlock(password).await.unwrap();
        assert!(provider.unavailable_reason().is_none());
        let before = fetch_host_status(&real.server_info_url()).await.unwrap();
        assert!(!before.busy, "host must be idle before the test");

        let started = Instant::now();
        let session = provider
            .start(&StartRequest { game_version: None })
            .await
            .unwrap_or_else(|e| {
                panic!(
                    "stream did not start: {}\nlogs:\n{}",
                    e.message,
                    provider.logs().join("\n")
                )
            });
        println!("stream ready after {:?}", started.elapsed());
        let during = fetch_host_status(&real.server_info_url()).await.unwrap();
        assert!(during.busy, "host reports the session running");
        let pid = provider
            .stream
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|s| s.child.id())
            .unwrap();
        for _ in 0..hold {
            assert!(
                provider.health(&session.remote_id).is_none(),
                "{:?}",
                provider.logs()
            );
            tokio::time::sleep(Duration::from_secs(1)).await;
        }

        provider.stop(&session.remote_id).await.unwrap();
        assert!(!provider.stream_running());
        #[cfg(unix)]
        assert!(
            !process_alive(pid as i32),
            "Moonlight pid {pid} survived Stop"
        );
        let mut freed = false;
        for _ in 0..15 {
            if !fetch_host_status(&real.server_info_url())
                .await
                .unwrap()
                .busy
            {
                freed = true;
                break;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        assert!(freed, "host still busy 15 s after Stop");
        println!("--- provider log ---\n{}", provider.logs().join("\n"));
    }
}
