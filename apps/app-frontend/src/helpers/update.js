import { ref } from 'vue'
import { getVersion } from '@tauri-apps/api/app'
import { getArtifact, getOS } from '@/helpers/utils.js'

export const allowState = ref(false)
export const installState = ref(false)
export const updateState = ref(false)
export const latestBetaCommitTruncatedSha = ref('')
export const latestBetaCommitLink = ref('')

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
    latestBetaCommitTruncatedSha.value = error.message
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
    const latestRelease = String(data.name || '')
    const localVersion = await getVersion()
    let remoteVersion = latestRelease

    if (!elementIdBool) {
      const releaseData = document.getElementById('releaseData')
      if (releaseData == null) {
        console.error('Release data element not found.')
        return false
      }
      releaseData.textContent = latestRelease
      remoteVersion = releaseData.textContent
    }

    if (osNames.includes(os.value.toLowerCase()) && remoteVersion.startsWith('v' + localVersion)) {
      updateState.value = false
      allowState.value = false
    } else if (osNames.includes(os.value.toLowerCase()) && latestRelease) {
      updateState.value = true
      allowState.value = true
    } else {
      updateState.value = false
      allowState.value = false
    }

    if (downloadArtifactBool) {
      installState.value = true
      try {
        const installer = getInstaller(getExtension(), data.assets || [])
        if (installer) {
          await getArtifact(installer[1], installer[0], os.value, true)
        } else {
          throw new Error('No compatible CloudCraft installer is attached to the latest release.')
        }
      } finally {
        installState.value = false
      }
    }
    return true
  } catch (error) {
    console.error(failedFetch[0], error)
    if (!elementIdBool) {
      const errorData = document.getElementById('releaseData')
      if (errorData) errorData.textContent = String(error.message || error)
    }
    updateState.value = false
    allowState.value = false
    installState.value = false
    return false
  }
}

/**
 * Find a non-development installer for the current platform.
 */
function getInstaller(osExtension, builds) {
  if (!osExtension) return null
  for (const build of builds) {
    const isBlacklisted = blacklistedBuilds.some((prefix) =>
      build.name.toLowerCase().startsWith(prefix),
    )
    if (build.name.endsWith(osExtension) && !isBlacklisted) {
      return [build.name, build.browser_download_url]
    }
  }
  return null
}

function getExtension() {
  if (os.value.toLowerCase() === osNames[0]) return macExtension
  if (os.value.toLowerCase() === osNames[1]) return windowsExtension
  return null
}
