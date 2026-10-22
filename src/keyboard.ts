// フォーカスを受け取れる要素。ダイアログのフォーカストラップと、引き出し(useDrawer.ts)を開いた
// ときのフォーカスの移し先を探すのに使う。
export const FOCUSABLE_SELECTOR =
  'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])'
