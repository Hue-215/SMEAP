// scripts/licenses.tsのViteプラグインが出す仮想モジュール。
declare module 'virtual:licenses' {
  export interface License {
    name: string
    version: string
    license: string
    text: string
  }

  const licenses: License[]
  export default licenses
}
