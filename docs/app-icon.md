# アプリのアイコン

元のファイルは`src-tauri/icons/`にある。

| ファイル | 中身 |
|---|---|
| `SMEAP.svg` | アイコンの元の絵。文字はパスにしておく(フォントの有無で描画が変わるため) |
| `android-fg.svg` | Androidのアダプティブアイコンの前景 |
| `icon.json` | `tauri icon`に渡す設定。Androidの背景色もここで決める |

Androidのアダプティブアイコンは、前景の中央の72/108しか表示しない。`icon.json`の`android_fg_scale`は前景の画像には使われない(tauri-cli 2.12時点)ので、`android-fg.svg`で`SMEAP.svg`を72/108に縮めて中央に置いている。

## 作り直す手順

```sh
npx tauri icon src-tauri/icons/icon.json
```

Android用は`src-tauri/gen/android/`に直接書き込まれる。iOSやWindows向けなど使わないファイルもできるが、`.gitignore`で除いている。
