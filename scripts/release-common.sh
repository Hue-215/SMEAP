# 配布用のビルドのスクリプト(release-build.sh・release-build-android.sh)が読み込む共通の部分。
# ビルドした人の絶対パス(ホームディレクトリ・CARGO_HOME・このリポジトリの場所)を、バイナリに
# 焼き込まれるパス(依存クレートのパニックの位置等)から外す指定と、外れたことの検査を持つ。
# 画面のライセンスの一覧を作るcargo-aboutが入っているかも、ここで確かめる。

if [[ -n "${RUSTFLAGS:-}" || -n "${CARGO_ENCODED_RUSTFLAGS:-}" ]]; then
  echo "RUSTFLAGS / CARGO_ENCODED_RUSTFLAGS を外してから実行してください(指定がこのスクリプトの置き換えの指定で黙って捨てられるため)" >&2
  exit 1
fi

if ! command -v cargo-about >/dev/null; then
  echo "cargo-about を入れてください(画面のライセンスの一覧に使う): cargo install --locked cargo-about" >&2
  exit 1
fi

# WindowsではGit Bashで動かす。rustcに渡るパスは/c/...ではなくC:\...の形になる
case "$(uname -s)" in
  MINGW* | MSYS*) windows=1 ;;
  *) windows=0 ;;
esac

native_path() {
  if ((windows)); then cygpath -w "$1"; else printf '%s\n' "$1"; fi
}

# 置き換える場所は、書かれたままのパス(cargoはCARGO_HOMEのリンクを解決せずに使う)と、
# リンクを解決したパスの両方を持つ。
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
root_physical="$(cd "$root" && pwd -P)"
home="$HOME"
home_physical="$(cd "$home" && pwd -P)"
cargo_home="${CARGO_HOME:-$HOME/.cargo}"
cargo_home_physical="$(cd "$cargo_home" && pwd -P)"

if [[ "$home" != /?* || "$home_physical" == / ]]; then
  echo "HOME がルートか相対パスです。置き換えがすべてのパスに当たるので止めます: $home" >&2
  exit 1
fi

# 置き換えの指定を、これ以降のcargoに渡す。
export_remap_flags() {
  # rustcは後に書いたものから当てはまるかを見るので、広いもの(ホーム)を先に置く。
  local prefixes=() pair from to flag
  for pair in \
    "$home=~" "$home_physical=~" \
    "$cargo_home=cargo-home" "$cargo_home_physical=cargo-home" \
    "$root=." "$root_physical=."; do
    from="${pair%=*}" to="${pair##*=}"
    flag="--remap-path-prefix=$(native_path "$from")=$to"
    [[ " ${prefixes[*]-} " == *" $flag "* ]] || prefixes+=("$flag")
  done
  # 空白を含むパスでも割れないよう、区切りが0x1fのCARGO_ENCODED_RUSTFLAGSで渡す。
  CARGO_ENCODED_RUSTFLAGS="$(IFS=$'\x1f'; echo "${prefixes[*]}")"
  export CARGO_ENCODED_RUSTFLAGS
}

# 渡されたファイル(フォルダなら中のすべてのファイル)に、置き換えたはずのパスとホームの親が残っていないかを確かめる。
# 残っていれば、その場所を書いて1を返す。
check_no_absolute_paths() {
  local dir="$1" found=0 found_list path pattern status file
  local paths=() patterns=() grep_options=()
  # ホームの親(/home等)は、上のどれでもない形(別のリンク越しの場所等)で焼き込まれた分を拾うため。
  for path in "$home" "$home_physical" "$cargo_home" "$cargo_home_physical" "$root" "$root_physical" \
    "$(dirname "$home")" "$(dirname "$home_physical")"; do
    [[ "$path" == / || " ${paths[*]-} " == *" $path "* ]] || paths+=("$path")
  done
  for path in "${paths[@]}"; do
    if ((windows)); then
      # 区切りは\と/のどちらでも書かれ、ドライブの文字やフォルダの名前の大文字小文字も揃わない
      path="$(cygpath -w "$path")"
      patterns+=("$path\\" "$(cygpath -m "$path")/")
    else
      patterns+=("$path/")
    fi
  done
  ((windows)) && grep_options+=(-i)
  found_list="$(mktemp)"
  for pattern in "${patterns[@]}"; do
    # grepは見つからないと1、読めないファイルがあると2で終わる。2は検査の漏れなので止める。
    status=0
    LC_ALL=C grep -r -l -Z -a -F "${grep_options[@]}" -- "$pattern" "$dir" >"$found_list" || status=$?
    if [[ "$status" -gt 1 ]]; then
      echo "検査できないファイルがあります(上のgrepのエラー)" >&2
      rm -f "$found_list"
      exit 1
    fi
    while IFS= read -r -d '' file; do
      echo "絶対パスが残っています: $pattern($file)" >&2
      found=1
    done <"$found_list"
  done
  rm -f "$found_list"
  return "$found"
}
