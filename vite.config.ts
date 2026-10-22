import react from '@vitejs/plugin-react'
import { defineConfig } from 'vite'

// devUrl(http://localhost:1420)はsrc-tauri/tauri.conf.jsonと合わせる。
// https://v2.tauri.app/start/frontend/vite/
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    // Androidの実機・エミュレーターからは、tauri-cliが渡すアドレスで開発サーバーへつなぐ
    host: process.env.TAURI_DEV_HOST || false,
    hmr: process.env.TAURI_DEV_HOST ? { protocol: 'ws', host: process.env.TAURI_DEV_HOST, port: 1421 } : undefined,
    watch: { ignored: ['**/src-tauri/**'] },
  },
})
