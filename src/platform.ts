export const isAndroid = /Android/i.test(navigator.userAgent)

// Rust側の独自プロトコルのURLの前半。AndroidとWindowsのTauriは`http://{scheme}.localhost`で受ける
export function customSchemeOrigin(scheme: string, userAgent: string): string {
  return /Android|Windows/i.test(userAgent) ? `http://${scheme}.localhost` : `${scheme}://localhost`
}
