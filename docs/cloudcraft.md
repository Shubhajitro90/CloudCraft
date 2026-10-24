# CloudCraft implementation status

This document separates working integrations from future architecture. It should be updated when code or test evidence changes.

## Implemented in the current branch

| Area | Current behavior | Source of truth |
| --- | --- | --- |
| Home | CloudCraft-branded welcome panel, selected-instance launch area, recent-instance and content rows | Existing launcher/profile and content helpers |
| Navigation | CloudCraft route and sidebar entry; Cloud connections section in the CloudCraft hub | Vue router, app shell, and hub section state |
| Cloud connections UI | Shows Kaggle compute and TeraBox storage cards, provider documentation/account links, and explicit disconnected status. It does not claim authentication or account quota is verified | CloudCraft hub; informational UI only |
| Instances | Loads profiles through the existing profile API; installed profiles can Play; non-installed profiles can Install/repair with the existing profile installation API; Details opens the profile route. Errors and completion are surfaced in the UI | Existing Rust/Tauri launcher commands |
| Mods and modpacks | Links to the existing content-browser routes | Existing Modrinth-backed browser and install flow |
| Performance | Reads and persists global min/max memory settings, validates integer bounds, and re-reads settings before saving to avoid overwriting unrelated updates | Existing settings backend |
| Temporary local servers | Validates and persists a local server configuration; runs a user-selected JAR through Java without a shell; captures bounded logs; checks actual process and TCP status; supports graceful stop, confirmed force-stop, and launcher-exit cleanup. Requires explicit EULA acceptance and binds to loopback with online mode enabled | New Tauri local-server commands; full real-JAR integration verification is still required |
| Server shortcuts | Stores a limited list of addresses in local browser storage; supports selecting, copying, and removing saved addresses | Browser local storage and clipboard |
| Local world backups | Copies a selected world directory to a chosen backup folder with a timestamped name; restores into a new directory without overwriting existing content. Copies are uncompressed and remain on this device | Tauri commands and local filesystem copy |
| Save reminders | Stores user-entered labels/notes locally; exports reminder metadata as JSON | Browser local storage and Blob download |
| Cloud session boundary | Typed lifecycle contract for unavailable/starting/ready/stopping/stopped/error states; local launch remains separate | Unconfigured provider adapter; no remote session is created |
| Cloud storage boundary | Typed provider contract for listing backups, upload/download progress, deletion, and cancellation | Unconfigured provider adapter; no account, network transfer, or credentials |
| Update safety | Update checks and installer lookup are scoped to the CloudCraft GitHub repository, not the upstream AstralRinth release feed | CloudCraft release API; no release is published by this change |

## Not implemented

- **Cloud gaming:** no remote VM/session allocation, game streaming, input transport, provider credentials, billing/quota, or session lifecycle exists in this implementation. The Cloud tab is informational and does not log in to Kaggle.
- **Hosted servers:** no remote provider API, provisioning, or remote server status is connected. The separate local server workflow is implemented but still needs end-to-end Java/server-JAR verification; integrated server-world snapshots (with progress and cancellation) and confirmed resource deletion are implemented and CI-tested, but have not been exercised in a running desktop app.
- **Cloud saves:** the typed `cloudStorageProvider` interface defines status, backup listing, upload/download progress, deletion, and cancellation. The Cloud tab links to TeraBox's official integration documentation, but no remote account login, app credentials, encrypted transfer, conflict resolution, synchronization protocol, or recovery workflow is connected; all provider operations remain unavailable until a real integration is reviewed and tested.
- **Skins/capes:** no new system is connected.
- **Self-updating releases:** CloudCraft's update check no longer points at AstralRinth installers. Until a CloudCraft release with signed, platform-appropriate installers is published, automatic update installation is not available. Do not publish an installer until signing, update metadata, and recovery have been tested.

Do not label these features as live or show invented connection state. Kaggle documents OAuth 2.0 for registered third-party apps and its public API, but a registered CloudCraft OAuth client and a supported interactive game runtime are not present. TeraBox's official Open Platform also requires a registered application and authorization. Implementing these features requires provider app registration, secure desktop OAuth callbacks/token storage, backend API clients, reviewing provider terms, explicit consent, and testing failure/recovery paths. The Cloud tab intentionally shows both providers as disconnected until real authentication and capability checks succeed.

## Security and privacy notes

- Do not send Microsoft/Minecraft credentials, launcher tokens, local paths, or world data to a third-party service without a reviewed, explicit integration and user consent.
- Keep secrets out of Vue state persisted to browser storage, logs, telemetry, URLs, and exported manifests.
- The local backup commands require existing directories, reject source/destination overlap, reject symbolic links, avoid overwriting existing restore targets, and attempt to remove partial output after a failed copy. Close Minecraft before copying a live world. This is not a substitute for testing recovery on important worlds.
- Treat server addresses and reminder notes as user-provided data; they are not verified or transmitted by CloudCraft.
- Local servers require a user-selected non-empty JAR directly inside a dedicated directory. CloudCraft does not download server executables, accept arbitrary Java arguments, disable online mode, or bind to a public interface. The UI displays a connection address only after a loopback TCP connection succeeds. Graceful stop is attempted before any separately confirmed force-stop.
- Do not weaken the upstream authentication or launcher security model to simulate a cloud feature.
- The updater now checks only CloudCraft's own GitHub releases. The existing launcher must not install a binary from the AstralRinth upstream release feed.

## Development and validation

The frontend package is `@modrinth/app-frontend`. From the repository root, the configured checks are:

```sh
pnpm install --frozen-lockfile
pnpm --filter @modrinth/app-frontend run tsc:check
pnpm --filter @modrinth/app-frontend run lint
pnpm --filter @modrinth/app-frontend run build
pnpm test
```

The root package also exposes `pnpm app:dev` and `pnpm app:build`. The Tauri desktop crate is `apps/app` (`theseus_gui`); the CloudCraft validation workflow runs `cargo fmt --check --package theseus_gui` and `cargo test --package theseus_gui --no-default-features` after installing Linux build dependencies. The desktop build workflow can produce platform artifacts on supported runners. Native build dependencies differ by operating system.

**Validation record:** CloudCraft validation run [39809890925](https://github.com/Shubhajitro90/CloudCraft/actions/runs/39809890925) completed successfully on implementation commit `26248e8e1ad04cd43d3020ba2bb76dd0fc509469`: Prettier check, ESLint, Vue/TypeScript typecheck, production frontend build, `cargo fmt --check --package theseus_gui`, and `cargo test --package theseus_gui --no-default-features` all passed. The validation workflow was then made read-only and given explicit path filters; follow-up run [39810467704](https://github.com/Shubhajitro90/CloudCraft/actions/runs/39810467704) also completed successfully with the same checks. The Linux desktop artifact build succeeded in [desktop build run 39809890902](https://github.com/Shubhajitro90/CloudCraft/actions/runs/39809890902); Windows and macOS jobs were still in progress/queued when this note was updated. No GUI smoke test or installed-launcher test is claimed.

## Licensing

This repository is based on an existing launcher and uses third-party packages and services. The desktop package declares `GPL-3.0-only` in `apps/app/Cargo.toml`. Preserve the repository's applicable upstream license and notices; check the actual license files and dependency licenses before redistributing binaries. Follow the relevant Modrinth API/brand rules and Minecraft terms. CloudCraft branding does not imply endorsement by Modrinth, Mojang, or Microsoft.
