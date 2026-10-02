#!/usr/bin/env bash
# uur 开发构建脚本（Arch Linux / CachyOS）
# 用途：拉取本修复分支后，在 Linux 机器上一键构建并安装到 ~/.local。
# 用法：./dev-build.sh          # 构建并安装
#       ./dev-build.sh check   # 只跑 fmt/clippy/test（不安装）
set -euo pipefail

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
readonly root
cd "$root"

# ---- 1. 构建依赖（参考 packaging/aur/PKGBUILD 的 makedepends）----
#   rust            cargo 工具链（Rust 主程序）
#   gcc / pkgconf   编译与 pkg-config
#   mingw-w64-gcc   交叉编译 Windows 侧 hook DLL（uur-hook.dll 等）
#   git
missing=()
for tool in cargo git x86_64-w64-mingw32-gcc pkg-config; do
    command -v "$tool" >/dev/null 2>&1 || missing+=("$tool")
done
if ((${#missing[@]})); then
    printf '缺少构建工具: %s\n执行安装（需要 sudo）:\n' "${missing[*]}" >&2
    printf '  sudo pacman -S --needed rust gcc pkgconf mingw-w64-gcc git\n' >&2
    exit 1
fi

if [[ ${1:-} == check ]]; then
    ./scripts/check.sh
    exit 0
fi

# ---- 2. 三个构建产物 ----
# a) Windows 侧 hook（uur-hook.dll / wevtapi.dll / 各 proxy exe）
./hook/build.sh
# b) PipeWire 采集助手（uur-pw-capture）
./capture/build.sh
# c) Rust 主程序
cargo build --release

# ---- 3. 安装到 ~/.local（bin/lib/hook/图标/桌面入口）----
./scripts/install-user.sh

printf '\n完成。提示：\n'
printf '  - 运行前确认 ~/.config/uur/config.toml 里 input_backend = "xtest"\n'
printf '  - 实测点：右键（应弹右键菜单而非创建便签）、Insert、PrtSc/SysRq\n'
