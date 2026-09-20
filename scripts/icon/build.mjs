// 图标方案对比图：把 lib.mjs 里的每个方案都渲染出来，并排看大图和程序坞里的真实尺寸。
//
// 用法：
//   node scripts/icon/build.mjs            全部方案
//   node scripts/icon/build.mjs enso       只看一个
//   CHROME=/path/to/chrome node ...        指定浏览器
//
// 产物在 docs/icon-lab/，对比图是 docs/icon-lab/_contact-sheet.png。

import { execFileSync } from 'node:child_process'
import { mkdirSync, writeFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { CHROME, concepts, rasterize } from './lib.mjs'

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..', '..')
const OUT = join(ROOT, 'docs', 'icon-lab')

mkdirSync(OUT, { recursive: true })

const only = process.argv[2]
const built = []
for (const [name, make] of Object.entries(concepts)) {
  if (only && !name.startsWith(only)) continue // 前缀匹配：garden 一次出齐所有枯山水
  for (const dark of [true, false]) {
    const id = `${name}-${dark ? 'dark' : 'light'}`
    const svgPath = join(OUT, `${id}.svg`)
    const pngPath = join(OUT, `${id}.png`)
    writeFileSync(svgPath, make(dark))
    rasterize(svgPath, pngPath)
    built.push({ id, pngPath })
    console.log(`  ${id}.png`)
  }
}

// 小尺寸那两张是同一个 PNG 让浏览器缩的，只用来快速判断"糊不糊"；
// 正式的小图标由 emit.mjs 按尺寸重新栅格化，不走这条路。
const sheetRows = built
  .map(
    ({ id, pngPath }) => `<div class="row">
  <div class="name">${id}</div>
  <img src="file://${pngPath}" width="190">
  <img src="file://${pngPath}" width="64">
  <img src="file://${pngPath}" width="32">
</div>`,
  )
  .join('\n')

const sheetHtml = `<!doctype html><meta charset="utf-8">
<style>
  body{margin:0;padding:28px 32px;background:#6B7B90;font:600 13px -apple-system,sans-serif;color:#0E1726}
  .grid{display:grid;grid-template-columns:repeat(3,1fr);gap:16px}
  .row{display:flex;align-items:center;gap:16px;background:linear-gradient(180deg,#9AA7B8,#7E8C9F);
       border-radius:16px;padding:12px 16px}
  .name{width:92px;font-family:ui-monospace,monospace;font-size:12px}
</style>
<div class="grid">${sheetRows}</div>`

const sheetPath = join(OUT, '_contact-sheet.html')
writeFileSync(sheetPath, sheetHtml)
execFileSync(
  CHROME,
  [
    '--headless',
    '--disable-gpu',
    '--hide-scrollbars',
    '--force-device-scale-factor=1.6',
    `--screenshot=${join(OUT, '_contact-sheet.png')}`,
    '--window-size=1400,1760',
    `file://${sheetPath}`,
  ],
  { stdio: 'ignore' },
)
console.log('\n对比图：docs/icon-lab/_contact-sheet.png')
