import { afterEach, describe, expect, it, vi } from 'vitest'
import { heroImageUrl, isValidSlug, isWindowsReservedSlug } from './api'

// convertFileSrcはTauriが画面に入れる関数を呼ぶ。OSによってURLの前半の形が違う
const stubConvertFileSrc = (origin: (protocol: string) => string) =>
  vi.stubGlobal('window', {
    __TAURI_INTERNALS__: {
      convertFileSrc: (path: string, protocol: string) =>
        `${origin(protocol)}/${encodeURIComponent(path)}`,
    },
  })

describe('heroImageUrl', () => {
  afterEach(() => {
    vi.unstubAllGlobals()
  })

  it.each([
    ['LinuxとmacOS', (protocol: string) => `${protocol}://localhost`],
    ['WindowsとAndroid', (protocol: string) => `http://${protocol}.localhost`],
  ])('%sで、共有の画像なら../../assets/{名前}を指す', (_os, origin) => {
    stubConvertFileSrc(origin)

    const shared = new URL(heroImageUrl('post', { name: 'a.png', shared: true }, 0))
    expect(shared.pathname).toBe('/post/..%2F..%2Fassets%2Fa.png')

    const article = new URL(heroImageUrl('post', { name: 'a.png', shared: false }, 0))
    expect(article.pathname).toBe('/post/..%2F..%2Fassets%2Fpost%2Fa.png')
  })
})

describe('slugの検査', () => {
  it('Windowsの予約名を見分ける', () => {
    for (const slug of ['con', 'nul', 'com0', 'lpt9']) {
      expect(isWindowsReservedSlug(slug), slug).toBe(true)
      expect(isValidSlug(slug), slug).toBe(false)
    }
    // Conは文字の決まりで断る
    for (const slug of ['com10', 'console', 'con-1', 'Con']) {
      expect(isWindowsReservedSlug(slug), slug).toBe(false)
    }
    expect(isValidSlug('Con')).toBe(false)
    expect(isValidSlug('console')).toBe(true)
  })
})
