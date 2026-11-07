import { describe, expect, it } from 'vitest'
import { customSchemeOrigin } from './platform'

describe('customSchemeOrigin', () => {
  it.each([
    [
      'Android',
      'http://article-image.localhost',
      'Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Mobile Safari/537.36',
    ],
    [
      'Windows',
      'http://article-image.localhost',
      'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36 Edg/130.0.0.0',
    ],
    [
      'Linux',
      'article-image://localhost',
      'Mozilla/5.0 (X11; Ubuntu; Linux x86_64) AppleWebKit/605.1.15 (KHTML, like Gecko)',
    ],
    [
      'macOS',
      'article-image://localhost',
      'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko)',
    ],
  ])('%sでは%sにする', (_os, origin, userAgent) => {
    expect(customSchemeOrigin('article-image', userAgent)).toBe(origin)
  })
})
