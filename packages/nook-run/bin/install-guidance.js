const RELEASE_BASE_URL =
  "https://github.com/NeoTamia/NTNook/releases/latest/download";

export function installationGuidance(platform) {
  if (platform === "win32") {
    return `Install Nook on Windows from PowerShell with:
  irm ${RELEASE_BASE_URL}/nook-installer.ps1 | iex`;
  }

  return `Install Nook on Linux with:
  curl --proto '=https' --tlsv1.2 -LsSf ${RELEASE_BASE_URL}/nook-installer.sh | sh`;
}
