# CloudCraft implementation status

This document separates working integrations from future architecture. It should be updated when code or test evidence changes.

## Implemented in the current branch

| Area | Current behavior | Source of truth |
| --- | --- | --- |
| Home | CloudCraft-branded welcome panel; existing recent-instance and content rows remain | Existing launcher/profile and content helpers |
| Navigation | CloudCraft route and sidebar entry | Vue router and app shell |
| Instances | Loads profiles through the existing profile API; Details opens the profile route; Play invokes the existing launch command only for installed profiles | Existing Rust/Tauri launcher commands |
| Mods and modpacks | Links to existing content-browser routes | Existing Modrinth-backed browser and install flow |
| Performance | Reads and persists global min/max memory settings | Existing settings backend |
| Server shortcuts | Stores a limited list of addresses in local browser storage; supports copying an address | Browser local storage and clipboard |
| Save reminders | Stores user-entered labels/notes locally; exports reminder metadata as JSON | Browser local storage and Blob download |

## Not implemented

- **Cloud gaming:** no remote VM/session allocation, game streaming, input transport, provider credentials, billing/quota, or session lifecycle exists in this implementation.
- **Hosted servers:** no provider API, server provisioning, lifecycle management, or remote server status is connected.
- **Cloud saves:** no remote account, encrypted upload/download, conflict resolution, synchronization protocol, or recovery workflow is connected.
- **World backups:** the reminder list does not read or copy world files and cannot restore a world.
- **Skins/capes:** no new system is connected.

Do not label these features as live or show invented connection state. Implementing them requires choosing a real provider, reviewing its API and terms, designing credential storage and consent, adding the required backend commands, and testing failure/recovery paths.

## Security and privacy notes

- Do not send Microsoft/Minecraft credentials, launcher tokens, local paths, or world data to a third-party service without a reviewed, explicit integration and user consent.
- Keep secrets out of Vue state persisted to browser storage, logs, telemetry, URLs, and exported manifests.
- Validate all file paths and archive entries in any future backup/restore feature. Restoration must avoid path traversal, overwrite surprises, symlinks, and partial writes.
- Treat server addresses and reminder notes as user-provided data; they are not verified or transmitted by CloudCraft.
- Do not weaken the upstream authentication or launcher security model to simulate a cloud feature.

## Development and validation

The frontend package is `@modrinth/app-frontend`. From the repository root, the configured checks are:

```sh
pnpm install --frozen-lockfile
pnpm --filter @modrinth/app-frontend run tsc:check
pnpm --filter @modrinth/app-frontend run lint
pnpm --filter @modrinth/app-frontend run build
pnpm test
```

The root package also exposes `pnpm app:dev` and `pnpm app:build`. Run Rust formatting, tests, and compilation from the actual Tauri crate directory once identified in the checkout. Native build dependencies differ by operating system.

**Validation status for this implementation session:** repository files and package scripts were inspected through GitHub. A complete checkout with dependencies and a Rust toolchain was not available to this session, so no frontend build, lint, TypeScript check, automated test, native compile, or GUI smoke test is claimed as passing. CI results must be reviewed before release.

## Licensing

This repository is based on an existing launcher and uses third-party packages and services. Preserve the repository's applicable upstream license and notices; check the actual license files and dependency licenses before redistributing binaries. Follow the relevant Modrinth API/brand rules and Minecraft terms. CloudCraft branding does not imply endorsement by Modrinth, Mojang, or Microsoft.
