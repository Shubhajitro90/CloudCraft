use crate::event::emit::emit_process;
use crate::event::ProcessPayloadType;
use crate::profile;
use crate::util::io::IOError;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::Deserialize;
use serde::Serialize;
use std::process::ExitStatus;
use tokio::process::{Child, Command};
use uuid::Uuid;

pub struct ProcessManager {
    processes: DashMap<Uuid, Process>,
    /// Last exit per profile path, kept for the lifetime of the launcher.
    last_exits: DashMap<String, ProcessExit>,
    stop_requested: DashMap<Uuid, ()>,
}

/// How a game process ended. `exit_code` is `None` when a signal ended it.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ProcessExit {
    pub uuid: Uuid,
    pub profile_path: String,
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    pub exit_code: Option<i32>,
    pub success: bool,
    pub stopped_by_user: bool,
}

impl Default for ProcessManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessManager {
    pub fn new() -> Self {
        Self {
            processes: DashMap::new(),
            last_exits: DashMap::new(),
            stop_requested: DashMap::new(),
        }
    }

    pub fn last_exit(&self, profile_path: &str) -> Option<ProcessExit> {
        self.last_exits.get(profile_path).map(|x| x.clone())
    }

    pub async fn insert_new_process(
        &self,
        profile_path: &str,
        mut mc_command: Command,
        post_exit_command: Option<String>,
    ) -> crate::Result<ProcessMetadata> {
        let mc_proc = mc_command.spawn().map_err(IOError::from)?;

        let process = Process {
            metadata: ProcessMetadata {
                uuid: Uuid::new_v4(),
                start_time: Utc::now(),
                profile_path: profile_path.to_string(),
            },
            child: mc_proc,
        };

        let metadata = process.metadata.clone();

        tokio::spawn(Process::sequential_process_manager(
            profile_path.to_string(),
            post_exit_command,
            metadata.uuid,
        ));

        self.processes.insert(process.metadata.uuid, process);

        emit_process(
            profile_path,
            metadata.uuid,
            ProcessPayloadType::Launched,
            "Launched Minecraft",
            None,
            false,
        )
        .await?;

        Ok(metadata)
    }

    pub fn get(&self, id: Uuid) -> Option<ProcessMetadata> {
        self.processes.get(&id).map(|x| x.metadata.clone())
    }

    pub fn get_all(&self) -> Vec<ProcessMetadata> {
        self.processes
            .iter()
            .map(|x| x.value().metadata.clone())
            .collect()
    }

    pub fn try_wait(
        &self,
        id: Uuid,
    ) -> crate::Result<Option<Option<ExitStatus>>> {
        if let Some(mut process) = self.processes.get_mut(&id) {
            Ok(Some(process.child.try_wait()?))
        } else {
            Ok(None)
        }
    }

    pub async fn wait_for(&self, id: Uuid) -> crate::Result<()> {
        if let Some(mut process) = self.processes.get_mut(&id) {
            process.child.wait().await?;
        }
        Ok(())
    }

    pub async fn kill(&self, id: Uuid) -> crate::Result<()> {
        self.stop_requested.insert(id, ());
        if let Some(mut process) = self.processes.get_mut(&id) {
            process.child.kill().await?;
        }

        Ok(())
    }

    /// Removes the process and records how it ended. Returns whether Stop caused it.
    fn remove(&self, id: Uuid, status: ExitStatus) -> bool {
        let stopped_by_user = self.stop_requested.remove(&id).is_some();
        if let Some((_, process)) = self.processes.remove(&id) {
            let exit = ProcessExit {
                uuid: id,
                profile_path: process.metadata.profile_path.clone(),
                start_time: process.metadata.start_time,
                end_time: Utc::now(),
                exit_code: status.code(),
                success: status.success(),
                stopped_by_user,
            };
            self.last_exits.insert(exit.profile_path.clone(), exit);
        }
        stopped_by_user
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ProcessMetadata {
    pub uuid: Uuid,
    pub profile_path: String,
    pub start_time: DateTime<Utc>,
}

#[derive(Debug)]
struct Process {
    metadata: ProcessMetadata,
    child: Child,
}

impl Process {
    // Spawns a new child process and inserts it into the hashmap
    // Also, as the process ends, it spawns the follow-up process if it exists
    // By convention, ExitStatus is last command's exit status, and we exit on the first non-zero exit status
    async fn sequential_process_manager(
        profile_path: String,
        post_exit_command: Option<String>,
        uuid: Uuid,
    ) -> crate::Result<()> {
        async fn update_playtime(
            last_updated_playtime: &mut DateTime<Utc>,
            profile_path: &str,
            force_update: bool,
        ) {
            let diff = Utc::now()
                .signed_duration_since(*last_updated_playtime)
                .num_seconds();
            if diff >= 60 || force_update {
                if let Err(e) = profile::edit(profile_path, |prof| {
                    prof.recent_time_played += diff as u64;
                    async { Ok(()) }
                })
                .await
                {
                    tracing::warn!(
                        "Failed to update playtime for profile {}: {}",
                        &profile_path,
                        e
                    );
                }
                *last_updated_playtime = Utc::now();
            }
        }

        // Wait on current Minecraft Child
        let mc_exit_status;
        let mut last_updated_playtime = Utc::now();

        let state = crate::State::get().await?;
        loop {
            if let Some(process) = state.process_manager.try_wait(uuid)? {
                if let Some(t) = process {
                    mc_exit_status = t;
                    break;
                }
            } else {
                mc_exit_status = ExitStatus::default();
                break;
            }

            // sleep for 10ms
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

            // Auto-update playtime every minute
            update_playtime(&mut last_updated_playtime, &profile_path, false)
                .await;
        }

        let stopped_by_user =
            state.process_manager.remove(uuid, mc_exit_status);
        let message = match mc_exit_status.code() {
            Some(code) => format!("Exited process with code {code}"),
            None => "Process was terminated".to_string(),
        };
        emit_process(
            &profile_path,
            uuid,
            ProcessPayloadType::Finished,
            &message,
            mc_exit_status.code(),
            stopped_by_user,
        )
        .await?;

        // Now fully complete- update playtime one last time
        update_playtime(&mut last_updated_playtime, &profile_path, true).await;

        // Publish play time update
        // Allow failure, it will be stored locally and sent next time
        // Sent in another thread as first call may take a couple seconds and hold up process ending
        let profile = profile_path.clone();
        tokio::spawn(async move {
            if let Err(e) = profile::try_update_playtime(&profile).await {
                tracing::warn!(
                    "Failed to update playtime for profile {}: {}",
                    profile,
                    e
                );
            }
        });

        let _ = state.discord_rpc.clear_to_default(true).await;

        let _ = state.friends_socket.update_status(None).await;

        // If in tauri, window should show itself again after process exists if it was hidden
        #[cfg(feature = "tauri")]
        {
            let window = crate::EventState::get_main_window().await?;
            if let Some(window) = window {
                window.unminimize()?;
                window.set_focus()?;
            }
        }

        if mc_exit_status.success() {
            // We do not wait on the post exist command to finish running! We let it spawn + run on its own.
            // This behaviour may be changed in the future
            if let Some(hook) = post_exit_command {
                let mut cmd = hook.split(' ');
                if let Some(command) = cmd.next() {
                    let mut command = Command::new(command);
                    command.args(cmd.collect::<Vec<&str>>()).current_dir(
                        profile::get_full_path(&profile_path).await?,
                    );
                    command.spawn().map_err(IOError::from)?;
                }
            }
        }

        Ok(())
    }
}
