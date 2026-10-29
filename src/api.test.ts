import { describe, expect, it } from 'vitest'
import { heroImageUrl } from './api'

describe('heroImageUrl', () => {
  it('heroImageUrlは共有の画像なら../../assets/{名前}を指す', () => {
    const shared = new URL(heroImageUrl('post', { name: 'a.png', shared: true }, 0))
    expect(shared.pathname).toBe('/post/..%2F..%2Fassets%2Fa.png')

    const article = new URL(heroImageUrl('post', { name: 'a.png', shared: false }, 0))
    expect(article.pathname).toBe('/post/..%2F..%2Fassets%2Fpost%2Fa.png')
  })
})
