#!/bin/bash
# 在 Linux 上打开发行用的 x64 压缩包：Release 构建 GPUI 与 MCP 转发程序，
# 连同 GPL 许可、第三方声明和运行说明打成 tar.gz，并写 SHA-256。
# 须在仓库根目录执行。系统库由调用方事先安装（见 .github/workflows/release.yml）。
set -euo pipefail

if [ "$(uname -s)" != Linux ]; then
  echo "linux 包只在 Linux 上构建。" >&2
  exit 1
fi
if [ ! -f Cargo.toml ] || [ ! -f crates/kanso-gpui/Cargo.toml ]; then
  echo "请在仓库根目录运行 scripts/linux-package.sh。" >&2
  exit 1
fi

version=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
if [ -z "$version" ]; then
  echo "无法从 Cargo.toml 读取版本号。" >&2
  exit 1
fi

echo "==> 构建 kanso-gpui 与 kanso-mcp（release）"
cargo build --release --locked -p kanso-gpui -p kanso-mcp

release_dir=target/release
gpui=$release_dir/kanso-gpui
helper=$release_dir/kanso-mcp
for path in "$gpui" "$helper"; do
  if [ ! -f "$path" ]; then
    echo "找不到 $path" >&2
    exit 1
  fi
done

name="Kanso-${version}-Linux-x64"
out=target/linux-package
stage=$out/$name
rm -rf "$stage"
mkdir -p "$stage"
cp "$gpui" "$stage/Kanso"
cp "$helper" "$stage/kanso-mcp"
strip --strip-unneeded "$stage/Kanso" "$stage/kanso-mcp"
chmod 755 "$stage/Kanso" "$stage/kanso-mcp"
cp LICENSE "$stage/LICENSE.txt"

for bin in "$stage/Kanso" "$stage/kanso-mcp"; do
  if ldd "$bin" | grep -q 'not found'; then
    echo "$bin 缺少动态库：" >&2
    ldd "$bin" >&2
    exit 1
  fi
done

glibc=$(ldd --version | head -1)
cat > "$stage/README.txt" << EOF
Kanso ${version}（Linux x64）

解压后在桌面会话里运行 ./Kanso。同目录的 kanso-mcp 是 MCP 转发程序，需与 Kanso 放在一起。
数据目录：\$XDG_DATA_HOME/Kanso；未设置时为 ~/.local/share/Kanso。
需要 X11 或 Wayland、可用的 Vulkan 驱动，以及构建机同等或更新的 glibc（${glibc}）。
密码存在 Secret Service（GNOME Keyring、KWallet 或其他兼容服务）。
界面字体使用系统里的 DejaVu 与 Noto CJK，不随压缩包分发。

Debian / Ubuntu 运行库：
  sudo apt-get install libxcb1 libxkbcommon0 libxkbcommon-x11-0 libwayland-client0 libvulkan1 libfontconfig1 libfreetype6 fonts-dejavu-core fonts-noto-cjk

本压缩包由 CI 在 Ubuntu 上构建，未签名。

Kanso ${version} for Linux x64

Run ./Kanso from this directory. kanso-mcp must stay beside it.
Data: \$XDG_DATA_HOME/Kanso, or ~/.local/share/Kanso.
Requires an X11 or Wayland session, a Vulkan driver, and glibc at least as new as the build machine (${glibc}).
Passwords use Secret Service. Fonts come from the system (DejaVu and Noto CJK), not this archive.
The archive is built by CI on Ubuntu and is not signed.

动态库 / dynamic libraries (ldd Kanso):
$(ldd "$stage/Kanso" | sed 's/^/  /')
EOF

echo "==> 生成第三方声明"
python3 - "$stage/THIRD_PARTY_NOTICES.linux.md" "$version" << 'PY'
import sys
from collections import defaultdict
from pathlib import Path
import json
import subprocess

destination, version = sys.argv[1], sys.argv[2]
metadata = json.loads(
    subprocess.check_output(
        [
            "cargo",
            "metadata",
            "--format-version",
            "1",
            "--locked",
            "--filter-platform",
            "x86_64-unknown-linux-gnu",
        ]
    )
)
packages = {package["id"]: package for package in metadata["packages"]}
nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
roots = [
    package_id
    for package_id, package in packages.items()
    if package["name"] in {"kanso-gpui", "kanso-mcp"} and not package.get("source")
]
if len(roots) != 2:
    sys.exit(f"未找到 kanso-gpui 与 kanso-mcp 的依赖根：{roots}")

seen = set()
stack = list(roots)
while stack:
    package_id = stack.pop()
    if package_id in seen:
        continue
    seen.add(package_id)
    node = nodes.get(package_id)
    if node is None:
        continue
    for dep in node.get("deps", []):
        kinds = {(item.get("kind") or "normal") for item in dep.get("dep_kinds", [])}
        if not kinds or kinds & {"normal", "build"}:
            stack.append(dep["pkg"])

third_party = [
    packages[package_id]
    for package_id in seen
    if packages[package_id].get("source")
]
third_party.sort(key=lambda package: (package["name"], package["version"]))

license_dir = Path("xtask/licenses")
standards = {
    name: (license_dir / f"{name}.txt").read_text(encoding="utf-8")
    for name in ("MIT", "Apache-2.0", "MPL-2.0", "CC0-1.0", "Zlib")
}


def own_licenses(package):
    root = Path(package["manifest_path"]).parent
    found = []
    names = [root / package["license_file"]] if package.get("license_file") else []
    names.extend(path for path in root.iterdir() if path.is_file())
    seen_paths = set()
    for path in names:
        if path in seen_paths or not path.is_file():
            continue
        seen_paths.add(path)
        upper = path.name.upper()
        if not upper.startswith(("LICENSE", "LICENCE", "COPYING", "NOTICE")):
            continue
        text = path.read_text(encoding="utf-8", errors="replace").strip()
        if text:
            found.append((path.name, text))
    return found


def standard_texts(spdx):
    tokens = []
    for chunk in spdx.replace("/", " ").replace("(", " ").replace(")", " ").split():
        if chunk in standards and chunk not in tokens:
            tokens.append(chunk)
    return [(token, standards[token].strip()) for token in tokens]


groups = defaultdict(list)
mpl = []
for package in third_party:
    identity = f"{package['name']} {package['version']}"
    texts = own_licenses(package)
    has_license = any(
        not name.upper().startswith("NOTICE") for name, _text in texts
    )
    if not has_license:
        texts.extend(standard_texts(package.get("license") or ""))
        has_license = any(
            not name.upper().startswith("NOTICE") for name, _text in texts
        )
    if not has_license:
        sys.exit(f"{identity} 没有许可文本（SPDX：{package.get('license')}）")
    for name, text in texts:
        groups[(name, text)].append(identity)
    if "MPL-2.0" in (package.get("license") or ""):
        mpl.append(identity)

lines = [
    "# Third-party notices (Linux)",
    "",
    "Generated while building the Linux archive. Covers `kanso-gpui` and `kanso-mcp`",
    "for x86_64-unknown-linux-gnu, including build dependencies.",
    "",
    f"Kanso itself is GPL-3.0-or-later. Source for this build is git tag `v{version}`",
    "at https://github.com/chhuax/Kanso.",
    "",
    "## Crates",
    "",
    "| Crate | Version | License | Repository |",
    "| --- | --- | --- | --- |",
]
for package in third_party:
    license_name = (package.get("license") or "").replace("|", "/")
    repository = package.get("repository") or package.get("homepage") or ""
    lines.append(
        f"| {package['name']} | {package['version']} | {license_name} | {repository} |"
    )

if mpl:
    lines.extend(
        [
            "",
            "## MPL-2.0 source",
            "",
            "These crates are licensed under the Mozilla Public License 2.0.",
            "The source for the exact version linked into this build is the version",
            f"recorded in Cargo.lock of Kanso `v{version}` (https://github.com/chhuax/Kanso)",
            "and the repository listed in the table above.",
            "",
        ]
    )
    lines.extend(f"- {identity}" for identity in mpl)

lines.extend(
    [
        "",
        "## Linux GPUI icons",
        "",
        "Source: Lucide Icons, revision `149eba0aea48178a5ab1efe2c701de1da94f4be8`.",
        "",
        "Location: `crates/kanso-gpui/assets/icons`.",
        "",
        Path("crates/kanso-gpui/assets/icons/LICENSE").read_text(encoding="utf-8").strip(),
        "",
        "## License texts",
        "",
    ]
)
for (name, text), users in sorted(groups.items(), key=lambda item: item[0][0]):
    lines.append(f"### {name}")
    lines.append("")
    lines.append(", ".join(sorted(users)))
    lines.append("")
    fence = "`" * max(3, text.count("`") + 1)
    lines.append(fence)
    lines.append(text)
    lines.append(fence)
    lines.append("")

Path(destination).write_text("\n".join(lines) + "\n", encoding="utf-8")
print(f"第三方声明：{len(third_party)} 个 crate，其中 MPL-2.0 {len(mpl)} 个")
PY

archive=$out/${name}.tar.gz
rm -f "$archive" "$archive.sha256"
tar -C "$out" --owner=0 --group=0 -czf "$archive" "$name"
(
  cd "$out"
  sha256sum "${name}.tar.gz" > "${name}.tar.gz.sha256"
  cat "${name}.tar.gz.sha256"
)
echo "产物：$archive（$(wc -c < "$archive") 字节）"
