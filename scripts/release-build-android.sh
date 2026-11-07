#!/usr/bin/env bash
# Androidの配布用のビルド(署名したAPK)。
#
# ビルドした人の絶対パスをネイティブのライブラリ(.so)から外してビルドし(release-common.sh)、出来たAPKの
# 中身(入っているABI・残ったパス)を確かめてから、署名して`dist-release/`に置く。
#
#   scripts/release-build-android.sh [--target aarch64|x86_64]
#
# 配布するのは実機向けの`aarch64`(既定)だけ。`x86_64`は、エミュレーターでリリースのAPKを確かめるためのもの。
# 署名の鍵は環境変数で渡す。パスワードは、署名する`apksigner`にだけ渡す(npm・cargo・Gradleと、
# ビルドの後も残るGradleのデーモンには渡さない)。
#
#   SMEAP_ANDROID_KEYSTORE           キーストア(PKCS12)のファイルの絶対パス
#   SMEAP_ANDROID_KEYSTORE_PASSWORD  キーストアのパスワード
#   SMEAP_ANDROID_KEY_ALIAS          鍵の別名
set -euo pipefail

case "$#:${1:-}:${2:-}" in
  0:: | 2:--target:aarch64) target=aarch64 cpu=arm64 abi=arm64-v8a ;;
  2:--target:x86_64) target=x86_64 cpu=x86_64 abi=x86_64 ;;
  *)
    echo "usage: scripts/release-build-android.sh [--target aarch64|x86_64]" >&2
    exit 2
    ;;
esac

for name in SMEAP_ANDROID_KEYSTORE SMEAP_ANDROID_KEYSTORE_PASSWORD SMEAP_ANDROID_KEY_ALIAS ANDROID_HOME NDK_HOME; do
  if [[ -z "${!name:-}" ]]; then
    echo "環境変数 $name を設定してください" >&2
    exit 1
  fi
done
# 途中で作業ディレクトリを移るので、取り違えないよう絶対パスだけを受け付ける。
if [[ "$SMEAP_ANDROID_KEYSTORE" != /* || ! -f "$SMEAP_ANDROID_KEYSTORE" ]]; then
  echo "SMEAP_ANDROID_KEYSTORE は、キーストアのファイルの絶対パスにしてください: $SMEAP_ANDROID_KEYSTORE" >&2
  exit 1
fi
# パスワードは、ここから先で起動するプロセスの環境変数に載せない。
keystore_password="$SMEAP_ANDROID_KEYSTORE_PASSWORD"
unset SMEAP_ANDROID_KEYSTORE_PASSWORD
build_tools="$(find "$ANDROID_HOME/build-tools" -mindepth 1 -maxdepth 1 -type d 2>/dev/null | sort -V | tail -1 || true)"
apksigner="$build_tools/apksigner"
zipalign="$build_tools/zipalign"
aapt2="$build_tools/aapt2"
if [[ -z "$build_tools" || ! -x "$apksigner" || ! -x "$zipalign" || ! -x "$aapt2" ]]; then
  echo "apksigner・zipalign・aapt2 がありません(SDK Managerで「Android SDK Build-Tools」を入れてください)" >&2
  exit 1
fi

# shellcheck source=scripts/release-common.sh
source "$(dirname "$0")/release-common.sh"

version="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$root/src-tauri/Cargo.toml" | head -1)"
if [[ -z "$version" ]]; then
  echo "src-tauri/Cargo.toml から版を読めません" >&2
  exit 1
fi
dist="$root/dist-release/smeap-$version-android-$cpu.apk"
built="$root/src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release-unsigned.apk"
# 前の配布物と前のビルドのAPKを、今回のものと取り違えないよう先に消す。
rm -f "$dist" "$built"

export_remap_flags
cd "$root"
npm ci
# 置き換えの指定は、tauri-cli → Gradle → tauri-cli → cargoと、環境変数のまま届く。
npx tauri android build --apk --target "$target"

if [[ ! -f "$built" ]]; then
  echo "ビルドしたAPKがありません: $built" >&2
  exit 1
fi

# APKの中には圧縮されたファイルもあるので、展開してから中身を確かめる。
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
unzip -q "$built" -d "$work/unpacked"

# tauri.conf.jsonにversionが無いと、Cargo.tomlの版は使われず1.0になる。
apk_version="$("$aapt2" dump badging "$built" | sed -n "s/^package: .* versionName='\([^']*\)'.*/\1/p")"
if [[ "$apk_version" != "$version" ]]; then
  echo "APKの版が src-tauri/Cargo.toml と違います: $apk_version(想定: $version)" >&2
  exit 1
fi

abis="$(find "$work/unpacked/lib" -mindepth 1 -maxdepth 1 -printf '%f\n' | sort | tr '\n' ' ')"
if [[ "$abis" != "$abi " ]]; then
  echo "APKに入っているABIが想定と違います: $abis(想定: $abi)" >&2
  exit 1
fi
check_no_absolute_paths "$work/unpacked"
echo "絶対パスは残っていません"
check_licenses_include 'Material Components for Android'

# 署名はAPKの中のファイルを変えないので、上の検査は署名したものにも当てはまる。
# 署名の前に、中のファイルの位置が揃っていること(署名の後には直せない)を確かめる。
# 検査していない・署名の無いものが配布物の置き場所に残らないよう、署名と確認を終えてから移す。
"$zipalign" -c 4 "$built"
SMEAP_ANDROID_KEYSTORE_PASSWORD="$keystore_password" "$apksigner" sign \
  --ks "$SMEAP_ANDROID_KEYSTORE" --ks-type PKCS12 --ks-key-alias "$SMEAP_ANDROID_KEY_ALIAS" \
  --ks-pass env:SMEAP_ANDROID_KEYSTORE_PASSWORD --key-pass env:SMEAP_ANDROID_KEYSTORE_PASSWORD \
  --v4-signing-enabled false --out "$work/signed.apk" "$built"
"$apksigner" verify "$work/signed.apk"
mkdir -p "$(dirname "$dist")"
mv "$work/signed.apk" "$dist"
echo "配布物: $dist"
