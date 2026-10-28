import { defineConfig } from 'vitest/config'

// vite.config.tsのプラグイン(ライセンス一覧)と開発サーバーの設定を持ち込まないよう分ける
export default defineConfig({
  test: {
    environment: 'node',
  },
})
