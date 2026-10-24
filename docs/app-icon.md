# アプリのアイコン

元のファイルは`src-tauri/icons/`にある。

| ファイル | 中身 |
|---|---|
| `SMEAP.svg` | アイコンの元の絵。文字はパスにしておく(フォントの有無で描画が変わるため) |
| `android-fg.svg` | Androidのアダプティブアイコンの前景。`SMEAP.svg`を72/108に縮めて中央に置く |
| `icon.json` | `tauri icon`に渡す設定。Androidの背景色もここで決める |

Androidのアダプティブアイコンは、前景の中央の72/108しか表示しない。`icon.json`の`android_fg_scale`はSVGには効かなかったので、`android-fg.svg`で縮めている。

## 作り直す手順

`tauri icon`はiOSやWindows向けなど使わないファイルも出すので、別の場所に出してから必要なものだけを写す。

```sh
out="$(mktemp -d)"
npx tauri icon src-tauri/icons/icon.json -o "$out"
cp "$out"/{32x32,128x128,128x128@2x,icon}.png src-tauri/icons/
res=src-tauri/gen/android/app/src/main/res
for d in mdpi hdpi xhdpi xxhdpi xxxhdpi; do cp "$out"/android/mipmap-$d/*.png "$res"/mipmap-$d/; done
cp "$out"/android/values/ic_launcher_background.xml "$res"/values/
```
