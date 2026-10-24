# CloudCraft

CloudCraft is a desktop Minecraft launcher built on the existing AstralRinth / Theseus-based codebase. It keeps the existing launcher engine and adds an original dark, violet-accented workspace for instances, content discovery, performance settings, and local multiplayer/save utilities.

> **Status:** The CloudCraft interface and launcher integrations are implemented on the `cloudcraft-implementation` branch. This is not a claim that cloud gaming or cloud storage is live. See [the architecture and provider notes](docs/cloudcraft.md) and [the implementation checklist](docs/IMPLEMENTATION_STATUS.md) for exact scope and validation status.

## What works today

- **Existing launcher:** the upstream profile, installation, authentication, Java configuration, and launch systems remain the source of truth.
- **CloudCraft hub:** a dedicated dashboard with Overview, Cloud, Instances, Mods & modpacks, Performance, Servers, and Saves sections.
- **Cloud connection setup:** an informational Cloud tab for Kaggle compute and TeraBox storage with official documentation links and honest disconnected status. Provider login, remote game streaming, and cloud-save sync are not yet connected.
- **Instances:** reads profiles from the existing launcher backend, opens a profile's existing details page, and invokes the normal launch command for installed profiles.
- **Content discovery:** links into the existing Modrinth-backed browser for mods, modpacks, resource packs, and shaders.
- **Performance:** reads and saves the launcher's actual global minimum/maximum memory allocation.
- **Temporary local servers:** configures and runs a user-selected Minecraft server JAR with Java, explicit EULA acceptance, bounded logs, actual process/reachability status, graceful stop, confirmed force-stop, and launcher-exit cleanup. It binds to loopback and is labeled local mode, not cloud hosting.
- **Server shortcuts:** stores up to ten server addresses in local browser storage for convenience.
- **Local server validation:** configuration tests cover path and argument rejection, EULA acceptance, safe properties, persistence, and bounded logs. Full desktop and real-JAR lifecycle verification remains required.
- **Local world backups:** copies a selected world folder into a backup directory you choose and restores a backup into a new folder. Existing folders are never overwritten; symbolic links are rejected.
- **Save reminders:** stores labels and notes locally and exports those reminders as JSON. This metadata export is not cloud synchronization.

## What is not connected

- No remote Minecraft client / cloud gaming provider.
- No remote/hosted multiplayer server provider. The temporary server workflow currently supports local mode only.
- No cloud storage account, world uploads, cross-device save synchronization, or remote restore service.
- No new skins/capes service.

These features require real integrations, explicit user consent, secure credential handling, and end-to-end testing before they can be advertised as operational.

## Development

The repository is a pnpm monorepo using Vue 3, TypeScript, Vite, and a Rust/Tauri desktop backend. Use the package manager version specified in the root `package.json` and the checked-in lockfile.

From the repository root:

```sh
pnpm install --frozen-lockfile
pnpm --filter @modrinth/app-frontend run dev
pnpm --filter @modrinth/app-frontend run tsc:check
pnpm --filter @modrinth/app-frontend run lint
pnpm --filter @modrinth/app-frontend run build
```

The root project also defines `pnpm app:dev`, `pnpm app:build`, `pnpm lint`, and `pnpm test`. Rust checks should be run from the actual Tauri crate directory discovered in the checkout, for example `cargo fmt --check`, `cargo test`, and `cargo check` where supported by the project's configured targets and system dependencies.

Validation record: [CloudCraft validation run #108](https://github.com/Shubhajitro90/CloudCraft/actions/runs/39809890925) passed the frontend formatting check, ESLint, Vue/TypeScript typecheck, production frontend build, Rust formatting, and desktop-crate tests. [Desktop build run](https://github.com/Shubhajitro90/CloudCraft/actions/runs/39809890902) produced the Linux artifact successfully; Windows and macOS jobs were still running when this note was written. No GUI smoke test or installed-launcher test is claimed. Check the latest CI run before shipping.

## Licensing and attribution

CloudCraft builds on an existing open-source launcher codebase and its dependencies. Preserve the upstream license and notices, review each dependency's license and applicable Modrinth branding/API rules, and comply with Minecraft's terms. Do not redistribute upstream assets or imply endorsement by Mojang/Microsoft or Modrinth. Consult the actual license files and dependency notices in the full repository before publishing binaries; no new license is implied by the CloudCraft rebrand.

## Project status

See [docs/cloudcraft.md](docs/cloudcraft.md) for architecture and [docs/IMPLEMENTATION_STATUS.md](docs/IMPLEMENTATION_STATUS.md) for requirement-level evidence and remaining dependencies.
