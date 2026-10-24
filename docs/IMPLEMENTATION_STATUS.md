# CloudCraft implementation checklist

Branch: `cloudcraft-implementation`. Status entries distinguish code presence from end-to-end verification. Update this file whenever evidence changes.

## Status definitions

- **Implemented and verified:** the real workflow was exercised with the required runtime and evidence is recorded.
- **Implemented, CI-verified:** compiled and covered by automated tests in GitHub Actions (unit tests with real temporary files, typecheck, lint, build). The workflow has NOT been exercised in a running desktop app unless a row says so.
- **Implemented but not yet verified:** the workflow is connected in code, but its required desktop runtime, account, provider, or installer test has not completed.
- **Partially implemented:** useful real functionality exists, but listed gaps remain.
- **Not implemented:** no working product workflow exists.
- **Blocked by an external dependency:** completion needs identified credentials, provider capabilities, signing material, or hardware.

## Foundation and local launcher

| Requirement | Status | Evidence or remaining gap |
| --- | --- | --- |
| Preserve Vue 3, TypeScript, Rust, and Tauri architecture | Implemented and verified | Existing AstralRinth/Theseus engine remains the launcher backend; no replacement application was introduced. |
| Home, instances, discovery, and settings UI | Implemented but not yet verified | CloudCraft hub uses real profile, install, launch, Modrinth-route, and settings APIs. No current GUI smoke test is claimed. |
| Instance creation, editing, duplication, deletion, Java, and launch arguments | Implemented but not yet verified | The CloudCraft hub now creates (name validation, release versions filtered by loader support, Vanilla/Fabric/Forge/Quilt/NeoForge with loader versions), installs, repairs, launches, stops, and deletes instances, and installs the Java version an instance requires. A failed install keeps the created instance and offers Repair; delete refuses a running instance and needs a second confirming click. Launch runs a preflight (installed, no install in progress, signed-in account, compatible Java, memory bounds, not already running) and reports every problem at once; process events and a post-reload reconciliation track game exit. Logic is covered by 26 unit tests with injected dependencies (`cloudcraftLauncher.test.mjs`, `cloudcraftInstances.test.mjs`), run in CI. Not exercised against the real backend: no display, account, or network game download was available. Not implemented: crash-report display and exit codes (the inherited process event carries none), per-download progress bars in the CloudCraft hub (the upstream downloads UI still shows them), and duplication/editing (use the upstream instance page via Details). |
| Persistent global memory settings | Implemented but not yet verified | Reads and writes the existing backend settings object with bounds checks; desktop restart verification remains. |
| Microsoft/Minecraft authentication | Implemented but not yet verified | The CloudCraft hub has an account panel that signs in through the existing upstream Microsoft flow (`login`), switches the active account, and signs out. The upstream "offline account" function is deliberately not exposed in CloudCraft, and no password flow or ownership bypass exists. Sign-in was not exercised: it needs a real Microsoft account and a display. |
| Backend initialization | Implemented but not yet verified | Removed a duplicate `State::init()` call. Full desktop startup still requires the Tauri test environment. |
| Content discovery and installation | Implemented but not yet verified | Existing Modrinth browser and compatibility/install workflows are retained. No live content download was performed in this run. |

## Backups and storage

| Requirement | Status | Evidence or remaining gap |
| --- | --- | --- |
| Local world folder backup and restore | Implemented but not yet verified | Existing commands copy to new destinations, reject symlinks/overlap/overwrite, and clean partial output. Unit tests exist; important-world recovery still needs a desktop integration test. |
| Backup progress, cancellation, and cryptographic integrity manifest (server worlds) | Implemented, CI-verified | SHA-256 manifest as before. Progress counters come from a pre-scan of the real world (bytes and files) and from work actually done; the UI shows an indeterminate bar when nothing is measured. See "Server-world operations" below. Not exercised in a running desktop app. |
| Generic local world copy (`cloudcraft_backup_world`) | Implemented but not yet verified | Unchanged by this milestone: no progress, cancellation, or checksums. The server-world workflow above is the one with these features. |
| Provider-independent cloud storage boundary | Partially implemented | Typed status, transfer, deletion, and cancellation contracts exist with an honest unconfigured adapter. |
| TeraBox remote backup integration | Blocked by an external dependency | Requires a registered/authorized Open Platform app, secure credential handling, verified API capabilities, and real transfer tests. No undocumented API is used. |

## Temporary multiplayer servers

| Requirement | Status | Evidence or remaining gap |
| --- | --- | --- |
| Local server configuration | Implemented but not yet verified | Validated name, optional version label, Java executable, local JAR, memory, players, port, and explicit EULA acceptance persist in `cloudcraft-server.json`. Managed properties preserve unrelated entries, require online mode, and bind to loopback. |
| Real local process lifecycle | Implemented but not yet verified | Tauri commands start `java -Xmx… -jar … nogui` without a shell, capture bounded logs, poll actual exit state, wait for TCP reachability, send graceful `stop`, and offer separately confirmed force-stop. Duplicate starts are prevented and launcher exit attempts cleanup. |
| Server lifecycle unit tests | Implemented, CI-verified | Tests cover EULA, path/argument and limit validation, property preservation/security, configuration round-trip, safe defaults, and bounded logs. `cargo test --package theseus_gui --no-default-features` passed in CI run 40101710990 (commit `ff476a7`, Rust job, 3m33s). These are unit tests only; no real server process was started. |
| Server hosting: remaining gaps (Priority 3) | Not implemented | (1) Restart as one action (stop, then start) is not provided; users stop then start. (2) World import/export as a standalone action is not provided; backup/restore covers the integrated world only. (3) Server software and version management is limited to a user-supplied local JAR; CloudCraft does not download server software. (4) No end-to-end run against a real server JAR. |
| Public/remote temporary hosting | Blocked by an external dependency | No authorized compute/network provider is configured. Local mode is never labeled cloud hosting and no public address is shown. |
| Integrated server-world backup/restore | Implemented, CI-verified | Snapshots the `level-name` world into `cloudcraft-backups/<id>/` with a SHA-256 manifest. A pre-scan rejects symlinks, special files, traversal and reserved level names before anything is written. Backups are built in a hidden `.partial-<id>` folder and renamed into place only at the commit point; restores build in `.restoring-<name>` and rename only after the copied hashes match the manifest, so a partial world is never visible under its final name and the current world is never overwritten. Commands refuse to run while the server is active. Verified by `cargo test --package theseus_gui` in CI (see evidence). Not exercised in a running desktop app. |
| Backup/restore/verify progress and cancellation | Implemented, CI-verified | One operation at a time (backup, verify, restore, delete) through `cloudcraft_server_ops.rs`. Cancel is accepted only while the phase is cancellable; the commit point and cancel are serialized by one mutex, so cancel cannot race the commit or cleanup. Cancelled backup/restore remove their staging folder before the slot is released; cancelled verification returns neither pass nor fail. Tests cancel real in-flight copies and hashes. |
| Server resource deletion | Implemented, CI-verified | Three separate confirmed targets: remove CloudCraft configuration (`cloudcraft-server.json` only), delete the world folder, delete one backup. Each needs the exact name typed back, refuses while the server is running, stopping, or uncertain (including an unexplained listener on the server port), requires a CloudCraft-managed folder, and refuses symlinks and special files before deleting. The server folder, JAR, other worlds, and unrelated instances are never removed. The UI shows what is deleted and kept and reports success only from a complete backend result; partial failures are listed. Audit lines are secret-free and scrub absolute paths. Not exercised in a running desktop app. |

## Cloud gaming

| Requirement | Status | Evidence or remaining gap |
| --- | --- | --- |
| Provider-independent session manager (`apps/app/src/cloudcraft_cloud_session.rs`) | Implemented and unit-tested | Rust state machine (unavailable, idle, starting, ready, stopping, stopped, error) with a validated transition table, single-flight start, start cancellation, idempotent stop, start/stop timeouts, credential-free metadata persistence, crash recovery (an interrupted session with a recorded remote id is restored as an uncertain error that must be stopped), retained cleanup after failed stop, and secret redaction. 19 unit tests with a fake provider cover transitions, concurrency, cancellation, timeouts, failure, cleanup, and recovery. Exposed through Tauri commands; the Cloud Gaming panel is driven by backend snapshots. Not runtime-verified in the desktop app. The only provider is `UnconfiguredProvider`, so state is always "unavailable" and no remote resource is ever created. |
| Remote graphical Minecraft client, streaming, input, audio, and cleanup | Blocked by an external dependency | No verified provider supplies the required interactive graphical runtime and streaming path. No remote session, latency, FPS, quota, or success state is fabricated. |
| Free/no-card provider such as Kaggle | Blocked by an external dependency | Notebook quota is not treated as proof of supported interactive gaming. Provider terms, app registration, networking, process supervision, and end-to-end testing remain unresolved. |

## Release quality

| Requirement | Status | Evidence or remaining gap |
| --- | --- | --- |
| Frontend formatting, lint, typecheck, build, and logic tests | Implemented, CI-verified | Prettier, ESLint, `vue-tsc`, production build, and Node unit tests for progress/confirmation/outcome logic passed on `a8cc74a9`. There are no component-level or end-to-end UI tests. Existing upstream Vite/CSS/chunk-size warnings remain non-fatal. |
| Rust formatting and full desktop tests | Implemented, CI-verified | `cargo fmt --check --package theseus_gui` and `cargo test --package theseus_gui --no-default-features` passed on `a8cc74a9` (55 tests). |
| Windows, macOS, and Linux installers | Implemented, CI-built; install and launch not verified | The `CloudCraft desktop build` workflow built installers on all three platforms for commit `a8cc74a9` (see evidence). File types were checked after download, and the Linux binary contains the new commands. No installer has been installed or launched, none are signed, and no update metadata is published. |
| Automatic updates | Blocked by an external dependency | Checks are scoped to CloudCraft releases, but signed installers and tested release/update metadata are not published by this change. |
| Secret and account safety | Implemented but not yet verified | No provider secrets were added. Local server commands do not accept shell arguments or expose environment/credential data. Security review and desktop tests remain. |

## Server-world operations

- **Stopping is not deleting.** Stop ends the process. Deleting is separate, typed-confirmation, and limited to the items named above.
- **Cancellation by phase.** Backup: cancellable while scanning, copying and writing the manifest; not cancellable during the final rename. Verify: always cancellable, read-only. Restore: cancellable while verifying and copying; not cancellable during the final rename. Delete: not cancellable, because stopping halfway would leave a half-deleted world.
- **Known limits.** Progress is per phase (bytes and files), not one blended percentage. A world modified by an external process during a copy can make done exceed total; the UI clamps to 100%. Another process holding the world open cannot be detected beyond the port probe, so keep Minecraft closed. Deletion is permanent and does not use the recycle bin. Windows file locking and read-only attributes are handled only by a single retry; failures are reported as partial deletion.

## Verification evidence

| Item | Result |
| --- | --- |
| Commit | `a8cc74a9` on `cloudcraft-implementation`, pushed to GitHub (previous head `d3c5686f`) |
| `CloudCraft validation` (push run 40097470253) | Success: format, 55 Rust tests including 18 deletion, 6 operation-registry and the backup cancel/progress tests, frontend typecheck/lint/build, Node logic tests |
| `CloudCraft desktop build` (run 40097470336) | Success on Windows, macOS and Linux |
| Artifacts | `CloudCraft_0.9.204_x64-setup.exe` (NSIS) and `_x64_en-US.msi`; `CloudCraft_0.9.204_universal.dmg`; `CloudCraft_0.9.204_amd64.AppImage`, `.deb`, `-1.x86_64.rpm`. GitHub Actions artifacts expire per repository retention settings |
| Desktop run | NOT done. The sandbox cannot run the AppImage (glibc older than the build, no display or GTK stack). The backup panel and deletion workflow have not been exercised in a running app |

| Re-check at `cd51f66f` (v0 sandbox, 2026-10-24) | Node logic tests 9/9 passed and `vue-tsc --noEmit` reported no errors. After installing Rust 1.99.1 and the Tauri Linux libraries: frontend production build (`vue-tsc --noEmit && vite build`) succeeded; `cargo test -p theseus_gui cloudcraft` passed 55/55 (backup, restore, verification, cancellation, deletion safeguards, server configuration); `cargo clippy -p theseus_gui --no-deps` reported nothing in CloudCraft files; `cargo fmt --check` is clean for CloudCraft files but reports diffs in inherited upstream files (`download.rs`, `minecraft_auth.rs`, `api/mod.rs`, `profile/mod.rs`, `launcher/mod.rs`, `util/utils.rs`), which were not reformatted. No display or Minecraft account exists in the sandbox, so no desktop launch, game launch, installer run, or provider test was possible. Feature classifications above are unchanged: unit-tested, not runtime-verified. |
| CI at `ff476a7` (workflow `CloudCraft validation`, run 40101710990, branch `v0/status-recheck`) | Frontend job passed: prettier, eslint, `vue-tsc`, production build, and 35 Node logic tests (operations 9, launcher 14, instances 12). Rust job passed: `cargo fmt --check` and `cargo test --package theseus_gui --no-default-features`. The workflow previously ran only for pushes to `cloudcraft-implementation` and PRs to `beta`, so this branch had never been tested by CI; it now also runs on pushes to `v0/status-recheck`. |

## Provider research (unconfirmed leads)

These come from web-search summaries on 2026-10-24, not from reading the providers' own documents. Confirm each against the first-party page before relying on it. No provider code was written.

- **TeraBox Open Platform:** apps appear to need an approved `client_id`, `client_secret` and `private_secret`, with an app name, logo and URL schemes. Upload looks like a three-step flow (`precreate`, sharded `superfile2` upload to a per-token upload domain, then `create`) with an MD5 block list. This fits the existing provider contract, but secrets must stay out of the frontend, and approval, quotas, terms and real transfer tests are all still outstanding.
- **Kaggle for cloud gaming:** the Terms of Use and Acceptable Use Policy reportedly prohibit non-machine-learning use such as game streaming and unauthorized remote-desktop or tunnel access. If that is confirmed, Kaggle must not be used for the cloud-gaming or hosting features, and the row stays "Blocked by an external dependency".

## Exact external dependencies still required

1. A legitimate Microsoft/Minecraft account, Java runtime, desktop OS, and game assets for authenticated local-launch integration testing.
2. Java and a legitimate Minecraft server JAR for an end-to-end local server start, join, graceful stop, and world-persistence test.
3. A reviewed provider that explicitly supports the needed interactive graphical runtime and streaming for cloud gaming; notebook access alone is insufficient.
4. An authorized storage developer application and credentials for any remote storage adapter, kept outside frontend code and source control.
5. Platform signing identities, update signing keys, published release metadata, and physical/virtual installation environments for signed installer and updater acceptance.
