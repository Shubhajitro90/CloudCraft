import { ref } from 'vue'
import { getVersion } from '@tauri-apps/api/app'
import { getArtifact, getOS } from '@/helpers/utils.js'

export const allowState = ref(false)
export const installState = ref(false)
export const updateState = ref(false)
export const latestBetaCommitTruncatedSha = ref('')
export const latestBetaCommitLink = ref('')

const os = ref('')

// Keep update checks and download links inside the CloudCraft fork. Pointing this
// launcher at AstralRinth's releases could install an unrelated application.
export const launcherUrl = 'https://github.com/Shubhajitro90/CloudCraft/releases'
const releaseLink = 'https://api.github.com/repos/Shubhajitro90/CloudCraft/releases/latest'
const branchesLink = 'https://api.github.com/repos/Shubhajitro90/CloudCraft/branches'
const failedFetch = ['Failed to fetch remote releases:', 'Failed to fetch remote commits:']
const betaBranch = 'beta'
const osNames = ['macos', 'windows', 'linux']
const macExtension = '.dmg'
const windowsExtension = '.msi'
const linuxExtension = '.AppImage'
const allowedReleaseAssetPrefix = 'https://github.com/Shubhajitro90/CloudCraft/releases/download/'
const blacklistedBuilds = [
  'dev',
  'nightly',
  'dirty',
  'dirty-dev',
  'dirty-nightly',
  'dirty_dev',
  'dirty_nightly',
]

/**
 * Fetch the latest commit metadata for the beta branch.
 */
export async function getBranches() {
  try {
    const response = await fetch(branchesLink)
    if (!response.ok) throw new Error(response.status)
    const branches = await response.json()
    const beta = branches.find((branch) => branch.name.toLowerCase() === betaBranch)
    if (!beta?.commit?.url) throw new Error('CloudCraft beta branch was not found')

    const commitResponse = await fetch(beta.commit.url)
    if (!commitResponse.ok) throw new Error(commitResponse.status)
    const commit = await commitResponse.json()
    latestBetaCommitTruncatedSha.value = commit.sha.slice(0, 7)
    latestBetaCommitLink.value = commit.html_url
  } catch (error) {
    latestBetaCommitTruncatedSha.value = String(error?.message || error)
    latestBetaCommitLink.value = undefined
    console.error(failedFetch[1], error)
  }
}

/**
 * Check CloudCraft's own releases and optionally start the existing installer flow.
 * No release is treated as available when GitHub has no CloudCraft release yet.
 */
export async function getRemote(elementIdBool, downloadArtifactBool) {
  try {
    const response = await fetch(releaseLink)
    if (!response.ok) throw new Error(response.status)
    const data = await response.json()
    os.value = await getOS()

    const remoteVersion = normalizeVersion(data.tag_name || data.name)
    const localVersion = normalizeVersion(await getVersion())
    const releaseLabel = String(data.name || data.tag_name || '')
    if (!remoteVersion) throw new Error('The latest CloudCraft release has no version tag.')

    if (!elementIdBool) {
      const releaseData = document.getElementById('releaseData')
      if (releaseData == null) {
        console.error('Release data element not found.')
        return false
      }
      releaseData.textContent = releaseLabel
    }

    if (!osNames.includes(os.value.toLowerCase())) {
      updateState.value = false
      allowState.value = false
    } else {
      updateState.value = remoteVersion !== localVersion
      allowState.value = updateState.value
    }

    if (downloadArtifactBool) {
      installState.value = true
      try {
        const installer = getInstaller(getExtension(), data.assets || [])
        if (!installer) {
          throw new Error(
            'No trusted CloudCraft installer for this operating system is attached to the latest release.',
          )
        }
        await getArtifact(installer.url, installer.name, os.value, true)
      } finally {
        installState.value = false
      }
    }
    return true
  } catch (error) {
    console.error(failedFetch[0], error)
    if (!elementIdBool) {
      const errorData = document.getElementById('releaseData')
      if (errorData) errorData.textContent = String(error?.message || error)
    }
    updateState.value = false
    allowState.value = false
    installState.value = false
    return false
  }
}

function normalizeVersion(version) {
  return String(version || '')
    .trim()
    .replace(/^v/i, '')
}

/**
 * Find a non-development installer published in the CloudCraft GitHub release.
 */
function getInstaller(extension, assets) {
  if (!extension) return null

  for (const asset of assets) {
    const name = String(asset.name || '')
    const url = String(asset.browser_download_url || '')
    const isBlacklisted = blacklistedBuilds.some((prefix) => name.toLowerCase().startsWith(prefix))

    if (name.endsWith(extension) && !isBlacklisted && url.startsWith(allowedReleaseAssetPrefix)) {
      return { name, url }
    }
  }
  return null
}

function getExtension() {
  const platform = os.value.toLowerCase()
  if (platform === osNames[0]) return macExtension
  if (platform === osNames[1]) return windowsExtension
  if (platform === osNames[2]) return linuxExtension
  return null
}
