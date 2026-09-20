// 横版 logo 锁定：图标 + Kanso 字样，出 README 和官网用的 PNG。
//
// 用法：
//   node scripts/icon/logo.mjs            出正式文件（docs/logo*.png、docs/icon.png）
//   node scripts/icon/logo.mjs --try      只出候选，放在 docs/icon-lab/ 里比
//
// 字样用系统字体渲染成位图 —— 仓库不带字体文件，也就没有再编辑的余地；
// 要改字型改下面的 FONTS，重跑即可。

import { execFileSync } from 'node:child_process'
import { mkdirSync, mkdtempSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { CHROME, rasterize } from './lib.mjs'

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..', '..')
const DOCS = join(ROOT, 'docs')
const LAB = join(DOCS, 'icon-lab')
const MAC_SVG = join(DOCS, 'kanso-icon.svg')

// 字号和间距按 320 高的画布定，和旧 logo 的比例一致（图标占满高，字略小于图标）。
const H = 320

// 锁定里的图标按 320 直接栅格化，不是拿大图缩的。放临时目录：它是中间产物，
// src-tauri/icons 只留构建真正要的五个文件。
const MAC_PNG = join(mkdtempSync(join(tmpdir(), 'kanso-logo-')), `icon-${H}.png`)
rasterize(MAC_SVG, MAC_PNG, H)
// 注意：macOS 上没装 SF Pro Rounded 时 rounded 会静默回落成 SF Pro，两者出图一样。
// 旧 logo 是圆体，新图标偏几何，正式版用 display。
const FONTS = {
  rounded: '"SF Pro Rounded", ui-rounded, "Avenir Next", -apple-system, sans-serif',
  display: '-apple-system, "SF Pro Display", "Helvetica Neue", sans-serif',
}

/** 锁定的 HTML。wrap 是 inline-flex，宽度由内容决定，量出来就是裁切尺寸。 */
function html({ font, color, caret }) {
  const caretEl = caret
    ? `<i style="width:30px;height:126px;border-radius:7px;background:#FB923C;margin-left:26px;display:block"></i>`
    : ''
  return `<!doctype html><meta charset="utf-8">
<style>
  html,body{margin:0;padding:0;background:transparent}
  #wrap{display:inline-flex;align-items:center;height:${H}px}
  #word{font:700 172px/1 ${font};color:${color};letter-spacing:-0.03em;
        margin-left:14px;transform:translateY(-6px)}
</style>
<div id="wrap">
  <img src="file://${MAC_PNG}" width="${H}" height="${H}">
  <div id="word">Kanso</div>
  ${caretEl}
</div>
<script>
  // 量出内容尺寸交给外面当截图窗口用 —— 不量就只能靠猜，logo 四周会多出空白。
  const w = document.getElementById('wrap')
  document.title = w.offsetWidth + 'x' + w.offsetHeight
</script>`
}

function shoot(cfg, outPng) {
  const page = join(LAB, '_logo.html')
  mkdirSync(LAB, { recursive: true })
  writeFileSync(page, html(cfg))

  // 第一遍只为读出量好的尺寸，第二遍按这个尺寸截图。
  const dom = execFileSync(
    CHROME,
    ['--headless', '--disable-gpu', '--hide-scrollbars', '--dump-dom', `file://${page}`],
    { encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] },
  )
  const m = dom.match(/<title>(\d+)x(\d+)<\/title>/)
  if (!m) throw new Error('量不到 logo 尺寸，页面可能没渲染出来')

  execFileSync(
    CHROME,
    [
      '--headless',
      '--disable-gpu',
      '--hide-scrollbars',
      '--force-device-scale-factor=1',
      '--default-background-color=00000000',
      `--screenshot=${outPng}`,
      `--window-size=${m[1]},${m[2]}`,
      `file://${page}`,
    ],
    { stdio: 'ignore' },
  )
  return `${m[1]}x${m[2]}`
}

const INK = '#0F1B2E' // 浅底上的字：近黑的蓝，比纯黑柔和，和图标底色同一支
const SNOW = '#F2F6FC' // 深底上的字

if (process.argv.includes('--try')) {
  const tries = [
    ['logo-rounded-ink', { font: FONTS.rounded, color: INK, caret: true }],
    ['logo-display-ink', { font: FONTS.display, color: INK, caret: true }],
    ['logo-rounded-blue', { font: FONTS.rounded, color: '#2563EB', caret: true }],
    ['logo-rounded-plain', { font: FONTS.rounded, color: INK, caret: false }],
  ]
  for (const [name, cfg] of tries) {
    console.log(`  ${name}.png  ${shoot(cfg, join(LAB, `${name}.png`))}`)
  }
} else {
  const cfg = { font: FONTS.display, caret: true }
  console.log(`docs/logo.png       ${shoot({ ...cfg, color: INK }, join(DOCS, 'logo.png'))}`)
  console.log(`docs/logo-dark.png  ${shoot({ ...cfg, color: SNOW }, join(DOCS, 'logo-dark.png'))}`)
  // 官网的 favicon 和页头图标，256 就够，同样按尺寸重新栅格化而不是缩图。
  rasterize(MAC_SVG, join(DOCS, 'icon.png'), 256)
  console.log('docs/icon.png       256x256')
}
