// virtual:licensesはscripts/licenses.tsのViteプラグインが出す仮想モジュール。設定画面が動的
// importで読む。
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
