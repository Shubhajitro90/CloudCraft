use crate::state::CachedEntry;
use crate::State;
pub use daedalus::minecraft::VersionManifest;
pub use daedalus::modded::Manifest;

#[tracing::instrument]
pub async fn get_minecraft_versions() -> crate::Result<VersionManifest> {
    let state = State::get().await?;
    let minecraft_versions = CachedEntry::get_minecraft_manifest(
        None,
        &state.pool,
        &state.api_semaphore,
    )
    .await?
    .ok_or_else(|| {
        crate::ErrorKind::NoValueFor("minecraft versions".to_string())
    })?;

    Ok(minecraft_versions)
}

// #[tracing::instrument]
pub async fn get_loader_versions(loader: &str) -> crate::Result<Manifest> {
    let state = State::get().await?;
    let cached = CachedEntry::get_loader_manifest(
        loader,
        None,
        &state.pool,
        &state.api_semaphore,
    )
    .await;

    match cached {
        Ok(Some(loaders)) => Ok(loaders.manifest),
        // Modrinth's metadata mirror no longer publishes Quilt; QuiltMC's own API is
        // the authoritative source for the same data.
        _ if loader == "quilt" => {
            tracing::warn!(
                "Quilt manifest unavailable from the launcher metadata mirror; using meta.quiltmc.org"
            );
            quilt::manifest_from_upstream(&state).await
        }
        Ok(None) => Err(crate::ErrorKind::NoValueFor(format!(
            "{} loader versions",
            loader
        ))
        .into()),
        Err(error) => Err(error),
    }
}

mod quilt {
    use daedalus::modded::{
        LoaderVersion, Manifest, Version, DUMMY_REPLACE_STRING,
    };
    use reqwest::Method;
    use serde::Deserialize;

    use crate::util::fetch::fetch_json;
    use crate::State;

    const QUILT_META_URL: &str = "https://meta.quiltmc.org/v3/versions";

    #[derive(Deserialize)]
    pub(super) struct QuiltLoader {
        pub version: String,
    }

    #[derive(Deserialize)]
    pub(super) struct QuiltGame {
        pub version: String,
        pub stable: bool,
    }

    pub(super) async fn manifest_from_upstream(
        state: &State,
    ) -> crate::Result<Manifest> {
        let loaders: Vec<QuiltLoader> = fetch_json(
            Method::GET,
            &format!("{QUILT_META_URL}/loader"),
            None,
            None,
            &state.api_semaphore,
            &state.pool,
        )
        .await?;
        let games: Vec<QuiltGame> = fetch_json(
            Method::GET,
            &format!("{QUILT_META_URL}/game"),
            None,
            None,
            &state.api_semaphore,
            &state.pool,
        )
        .await?;
        Ok(build_manifest(loaders, games))
    }

    /// Version strings are interpolated into URL paths, so anything outside the
    /// characters Quilt actually uses is dropped rather than escaped.
    fn is_safe_version(version: &str) -> bool {
        !version.is_empty()
            && version.len() <= 64
            && version
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+' | '_'))
    }

    /// Produces the Modrinth metadata layout theseus and the frontend expect: one
    /// placeholder game version listing every loader, then each supported game version.
    pub(super) fn build_manifest(
        loaders: Vec<QuiltLoader>,
        games: Vec<QuiltGame>,
    ) -> Manifest {
        let loaders = loaders
            .into_iter()
            .filter(|loader| is_safe_version(&loader.version))
            .map(|loader| LoaderVersion {
                url: format!(
                    "{QUILT_META_URL}/loader/{DUMMY_REPLACE_STRING}/{}/profile/json",
                    loader.version
                ),
                stable: !loader.version.contains('-'),
                id: loader.version,
            })
            .collect();

        let mut game_versions = vec![Version {
            id: DUMMY_REPLACE_STRING.to_string(),
            stable: true,
            loaders,
        }];
        game_versions.extend(
            games
                .into_iter()
                .filter(|game| is_safe_version(&game.version))
                .map(|game| Version {
                    id: game.version,
                    stable: game.stable,
                    loaders: Vec::new(),
                }),
        );
        Manifest { game_versions }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn builds_modrinth_layout_and_rejects_unsafe_versions() {
            let manifest = build_manifest(
                vec![
                    QuiltLoader { version: "0.29.2".into() },
                    QuiltLoader { version: "0.31.0-beta.4".into() },
                    QuiltLoader { version: "../../evil".into() },
                ],
                vec![
                    QuiltGame { version: "1.21.8".into(), stable: true },
                    QuiltGame { version: "x/y".into(), stable: true },
                ],
            );
            let placeholder = &manifest.game_versions[0];
            assert_eq!(placeholder.id, DUMMY_REPLACE_STRING);
            assert_eq!(placeholder.loaders.len(), 2);
            assert!(placeholder.loaders[0].stable);
            assert!(!placeholder.loaders[1].stable);
            assert_eq!(
                placeholder.loaders[0]
                    .url
                    .replace(DUMMY_REPLACE_STRING, "1.21.8"),
                "https://meta.quiltmc.org/v3/versions/loader/1.21.8/0.29.2/profile/json"
            );
            assert_eq!(manifest.game_versions.len(), 2);
            assert_eq!(manifest.game_versions[1].id, "1.21.8");
        }
    }
}
