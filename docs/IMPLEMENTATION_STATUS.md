# CloudCraft implementation checklist

Branch: `cloudcraft-implementation`. Status entries distinguish code presence from end-to-end verification. Update this file whenever evidence changes.

## Status definitions

- **Implemented and verified:** the real workflow was exercised with the required runtime and evidence is recorded.
- **Implemented but not yet verified:** the workflow is connected in code, but its required desktop runtime, account, provider, or installer test has not completed.
- **Partially implemented:** useful real functionality exists, but listed gaps remain.
- **Not implemented:** no working product workflow exists.
- **Blocked by an external dependency:** completion needs identified credentials, provider capabilities, signing material, or hardware.

## Foundation and local launcher

| Requirement | Status | Evidence or remaining gap |
| --- | --- | --- |
| Preserve Vue 3, TypeScript, Rust, and Tauri architecture | Implemented and verified | Existing AstralRinth/Theseus engine remains the launcher backend; no replacement application was introduced. |
| Home, instances, discovery, and settings UI | Implemented but not yet verified | CloudCraft hub uses real profile, install, launch, Modrinth-route, and settings APIs. No current GUI smoke test is claimed. |
| Instance creation, editing, duplication, deletion, Java, and launch arguments | Implemented but not yet verified | Existing upstream instance wizard and settings remain available. CloudCraft's own hub currently lists, installs/repairs, launches, and opens details; it does not duplicate every upstream control. |
| Persistent global memory settings | Implemented but not yet verified | Reads and writes the existing backend settings object with bounds checks; desktop restart verification remains. |
| Microsoft/Minecraft authentication | Implemented but not yet verified | Existing upstream authentication is preserved. No new password flow or ownership bypass exists; account sign-in was not exercised in this run. |
| Backend initialization | Implemented but not yet verified | Removed a duplicate `State::init()` call. Full desktop startup still requires the Tauri test environment. |
| Content discovery and installation | Implemented but not yet verified | Existing Modrinth browser and compatibility/install workflows are retained. No live content download was performed in this run. |

## Backups and storage

| Requirement | Status | Evidence or remaining gap |
| --- | --- | --- |
| Local world folder backup and restore | Implemented but not yet verified | Existing commands copy to new destinations, reject symlinks/overlap/overwrite, and clean partial output. Unit tests exist; important-world recovery still needs a desktop integration test. |
| Backup progress, cancellation, and cryptographic integrity manifest | Not implemented | Current copies are asynchronous from the UI but do not report byte progress, support cancellation, or write checksums. |
| Provider-independent cloud storage boundary | Partially implemented | Typed status, transfer, deletion, and cancellation contracts exist with an honest unconfigured adapter. |
| TeraBox remote backup integration | Blocked by an external dependency | Requires a registered/authorized Open Platform app, secure credential handling, verified API capabilities, and real transfer tests. No undocumented API is used. |

## Temporary multiplayer servers

| Requirement | Status | Evidence or remaining gap |
| --- | --- | --- |
| Local server configuration | Implemented but not yet verified | Validated name, optional version label, Java executable, local JAR, memory, players, port, and explicit EULA acceptance persist in `cloudcraft-server.json`. Managed properties preserve unrelated entries, require online mode, and bind to loopback. |
| Real local process lifecycle | Implemented but not yet verified | Tauri commands start `java -Xmx… -jar … nogui` without a shell, capture bounded logs, poll actual exit state, wait for TCP reachability, send graceful `stop`, and offer separately confirmed force-stop. Duplicate starts are prevented and launcher exit attempts cleanup. |
| Server lifecycle unit tests | Implemented but not yet verified | Tests cover EULA, path/argument and limit validation, property preservation/security, configuration round-trip, safe defaults, and bounded logs. Isolated stable-Rust compilation passed; full Tauri test execution is pending. |
| Public/remote temporary hosting | Blocked by an external dependency | No authorized compute/network provider is configured. Local mode is never labeled cloud hosting and no public address is shown. |
| Integrated server-world backup/restore | Implemented, CI-verified | `cloudcraft_server_backup.rs` snapshots the `level-name` world into `cloudcraft-backups/<id>/` with a SHA-256 manifest, rejects symlinks, special files, traversal and the reserved `cloudcraft-backups` level name, removes partial backups on failure, flags missing, modified, extra, duplicate and unsafe manifest entries, and restores only into a NEW folder. The commands refuse to run while the server is active. The `rust` CI job (format plus `cargo test --package theseus_gui`) and frontend typecheck, lint and build passed on commit 7022e1f0. The Tauri command wiring and UI have not been exercised in a running desktop app. |
| Server resource deletion | Not implemented | No workflow deletes server directories or backups; this is intentional until explicit confirmation UX exists. |

## Cloud gaming

| Requirement | Status | Evidence or remaining gap |
| --- | --- | --- |
| Provider and session boundary | Partially implemented | Typed unavailable/starting/ready/stopping/stopped/error contract is separate from local launch and multiplayer servers. |
| Remote graphical Minecraft client, streaming, input, audio, and cleanup | Blocked by an external dependency | No verified provider supplies the required interactive graphical runtime and streaming path. No remote session, latency, FPS, quota, or success state is fabricated. |
| Free/no-card provider such as Kaggle | Blocked by an external dependency | Notebook quota is not treated as proof of supported interactive gaming. Provider terms, app registration, networking, process supervision, and end-to-end testing remain unresolved. |

## Release quality

| Requirement | Status | Evidence or remaining gap |
| --- | --- | --- |
| Frontend formatting, lint, typecheck, and build | Implemented and verified | Passed locally after this change set. Existing upstream Vite/CSS/chunk-size warnings remain non-fatal. |
| Rust formatting and full desktop tests | Implemented but not yet verified | New module was formatted and isolated compilation passed; the repository's full Tauri workflow must provide final results. |
| Windows, macOS, and Linux installers | Partially implemented | Existing desktop release workflow and CloudCraft product naming are retained. Current artifacts, installation, startup, signing, and update metadata require release-run verification. |
| Automatic updates | Blocked by an external dependency | Checks are scoped to CloudCraft releases, but signed installers and tested release/update metadata are not published by this change. |
| Secret and account safety | Implemented but not yet verified | No provider secrets were added. Local server commands do not accept shell arguments or expose environment/credential data. Security review and desktop tests remain. |

## Exact external dependencies still required

1. A legitimate Microsoft/Minecraft account, Java runtime, desktop OS, and game assets for authenticated local-launch integration testing.
2. Java and a legitimate Minecraft server JAR for an end-to-end local server start, join, graceful stop, and world-persistence test.
3. A reviewed provider that explicitly supports the needed interactive graphical runtime and streaming for cloud gaming; notebook access alone is insufficient.
4. An authorized storage developer application and credentials for any remote storage adapter, kept outside frontend code and source control.
5. Platform signing identities, update signing keys, published release metadata, and physical/virtual installation environments for signed installer and updater acceptance.
