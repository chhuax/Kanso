// 出正式图标资产：矢量源 → src-tauri/icons。
//
// 用法：
//   node scripts/icon/emit.mjs             出选定方案（enso 深色版）
//   node scripts/icon/emit.mjs moongate    换一个方案出
//
// 只出这五个文件，就是 tauri.conf.json 的 bundle.icon 列的那几个：
//   32x32.png / 128x128.png / 128x128@2x.png   窗口图标；32x32 还被 commands.rs
//                                              用 include_bytes! 编进二进制当拖拽预览图
//   icon.icns                                  macOS 应用包
//   icon.ico                                   Windows 安装包
//
// 这是个桌面应用，没有 android / ios / Windows Store 的目标。`tauri icon` 会一口气
// 生成五十多个各平台图标，所以这里让它输出到临时目录，只取回一个 icon.ico ——
// 多层 ico 的编码器只有它有，其余尺寸都由矢量直接栅格化，比缩图清楚。
//
// 两份源图，排版不同：
//   macOS 版  本体内缩到 824/1024、烘焙投影 —— 程序坞里图标本来就比画布小一圈，
//             不留这圈边，Kanso 会显得比旁边的 App 大。只用于 icon.icns。
//   满幅版    本体撑满 1024、不带投影 —— Windows 任务栏自己会加阴影，图上再画一层
//             就是重影；内缩则会让图标凭空小一圈。用于 ico 和几张 PNG。

import { execFileSync } from 'node:child_process'
import { copyFileSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { concepts, rasterize, render } from './lib.mjs'

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..', '..')
const ICONS = join(ROOT, 'src-tauri', 'icons')
const DOCS = join(ROOT, 'docs')

const name = process.argv[2] || 'enso'
if (!concepts[name]) {
  console.error(`没有这个方案：${name}（可选：${Object.keys(concepts).join(', ')}）`)
  process.exit(1)
}

const work = mkdtempSync(join(tmpdir(), 'kanso-icon-'))

// 矢量源。改设计改 lib.mjs 里的方案定义，不要直接改这两个 svg。
const macSvg = join(DOCS, 'kanso-icon.svg')
const flatSvg = join(DOCS, 'kanso-icon-flat.svg')
writeFileSync(macSvg, render(name, { dark: true }))
writeFileSync(flatSvg, render(name, { dark: true, flat: true }))
console.log('docs/kanso-icon.svg  docs/kanso-icon-flat.svg')

// icon.ico：借 tauri icon 的编码器，产物落在临时目录，只取这一个文件。
const sourcePng = join(work, 'app-icon.png')
const tauriOut = join(work, 'icons')
rasterize(flatSvg, sourcePng)
mkdirSync(tauriOut, { recursive: true })
execFileSync('npx', ['tauri', 'icon', sourcePng, '-o', tauriOut], { cwd: ROOT, stdio: 'ignore' })
copyFileSync(join(tauriOut, 'icon.ico'), join(ICONS, 'icon.ico'))
console.log('src-tauri/icons/icon.ico')

// 窗口图标：按目标尺寸重新栅格化矢量。tauri icon 是把 1024 逐级缩下去的，
// 32px 以下笔锋会糊没。
for (const [file, size] of [
  ['32x32.png', 32],
  ['128x128.png', 128],
  ['128x128@2x.png', 256],
]) {
  rasterize(flatSvg, join(ICONS, file), size)
}
console.log('src-tauri/icons/32x32.png  128x128.png  128x128@2x.png')

// icns 的每一层同样按尺寸重出，用 macOS 版。
// iconutil 只认这套文件名，少一档 Finder 就会去缩相邻的层，边缘会发毛。
const iconset = join(work, 'Kanso.iconset')
mkdirSync(iconset, { recursive: true })
for (const [base, size] of [
  ['16x16', 16],
  ['16x16@2x', 32],
  ['32x32', 32],
  ['32x32@2x', 64],
  ['128x128', 128],
  ['128x128@2x', 256],
  ['256x256', 256],
  ['256x256@2x', 512],
  ['512x512', 512],
  ['512x512@2x', 1024],
]) {
  rasterize(macSvg, join(iconset, `icon_${base}.png`), size)
}
execFileSync('iconutil', ['-c', 'icns', iconset, '-o', join(ICONS, 'icon.icns')])
console.log('src-tauri/icons/icon.icns')

rmSync(work, { recursive: true, force: true })
