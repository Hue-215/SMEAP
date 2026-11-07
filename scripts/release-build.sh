#!/usr/bin/env bash
# 配布用のビルド(Linux・Windows)。束ねずに実行ファイルだけを作る(tauri.conf.jsonの`bundle.active`)。
# WebKitGTK・WebView2は利用者の環境のものを使う。
#
# WindowsではGit for Windowsのbashで実行する。RustはMSVCのツールチェーンを使い、Visual StudioのC++の
# ビルドツールが要る。
#
# ビルドした人の絶対パスを、バイナリに焼き込まれるパス(依存クレートのパニックの位置等)から外す
# (release-common.sh)。`strip = true`ではシンボルとデバッグ情報しか消えない。Cargoの`trim-paths`が
# 安定版に入ったら、`[profile.release]`に置く形に替える。
#
# 引数はそのまま`tauri build`に渡す。`--target`と`CARGO_TARGET_DIR`には対応しない(検査するバイナリの
# 場所が変わるため)。
set -euo pipefail

if [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
  echo "CARGO_TARGET_DIR を外してください" >&2
  exit 2
fi
for arg in "$@"; do
  case "$arg" in
    -t | -t* | --target | --target=*)
      echo "$arg には対応していません" >&2
      exit 2
      ;;
  esac
done

# shellcheck source=scripts/release-common.sh
source "$(dirname "$0")/release-common.sh"

binary="$root/src-tauri/target/release/smeap"
if ((windows)); then binary+=.exe; fi
# 前のビルドの実行ファイルを、今回のものと取り違えないよう先に消す。
rm -f "$binary"

export_remap_flags
cd "$root"
npm ci
npx tauri build "$@"

if [[ ! -f "$binary" ]]; then
  echo "検査するバイナリがありません: $binary" >&2
  exit 1
fi
check_no_absolute_paths "$binary"
echo "絶対パスは残っていません: $binary"
