// ZenTerm 应用图标的矢量定义。
//
// 图标不是手画的位图，而是在这里拼出 SVG、再用无头 Chrome 栅格化。
// 之所以不手写 SVG 文件：macOS 的图标外形是超椭圆而不是圆角矩形，路径必须算出来，
// 而同一段路径要被外形、裁剪、内描边三处复用。
//
// build.mjs 用它出方案对比图，emit.mjs 用它出正式资产。

import { execFileSync } from 'node:child_process'
import { readFileSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

export const CHROME =
  process.env.CHROME || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome'

// ---------------------------------------------------------------- 画布与外形

// Apple 的 macOS 图标模板：1024 画布里，图标本体是居中的 824×824，
// 四周留出的 100px 是投影用的安全区。改这两个数会让图标在程序坞里和别的 App 不一样大。
export const CANVAS = 1024
const BODY = 824

/**
 * 生成 macOS 风格圆角方形（超椭圆 |x/a|^n + |y/b|^n = 1）的路径。
 *
 * n = 5 是公认最接近 Apple 连续曲率圆角的取值；用普通 rx 圆角矩形在 1024px 下
 * 能看出拐角"鼓"出来。采样点取 720 个，弦高误差远小于半个像素，肉眼无棱角。
 */
function squircle(cx, cy, half, n = 5, steps = 720) {
  const pts = []
  for (let i = 0; i < steps; i++) {
    const t = (i / steps) * Math.PI * 2
    const c = Math.cos(t)
    const s = Math.sin(t)
    const x = cx + half * Math.sign(c) * Math.abs(c) ** (2 / n)
    const y = cy + half * Math.sign(s) * Math.abs(s) ** (2 / n)
    pts.push(`${x.toFixed(2)},${y.toFixed(2)}`)
  }
  return `M${pts[0]}L${pts.slice(1).join('L')}Z`
}

/**
 * 满幅模式开关：true 时图标本体撑满 1024 画布、不带投影。
 *
 * macOS 要的是 824 内缩加烘焙投影，Windows / Linux / Android 要的是满幅无投影 ——
 * 同一份设计必须出两种排版。用模块级开关而不是给十个方案挨个加参数：
 * 渲染是同步的，render() 进出之间不会有别的调用插进来。
 */
let bleed = false

/** 按当前模式取本体半宽和路径。 */
function body() {
  const half = bleed ? CANVAS / 2 : BODY / 2
  return { half, path: squircle(CANVAS / 2, CANVAS / 2, half), scale: half / (BODY / 2) }
}

/** 出图的唯一入口：选方案、选深浅、选排版。 */
export function render(name, { dark = true, flat = false } = {}) {
  bleed = flat
  try {
    return concepts[name](dark)
  } finally {
    bleed = false
  }
}

/** 极坐标取点，角度用数学习惯（0° 在右、逆时针为正），y 轴已按屏幕坐标翻转。 */
function polar(cx, cy, r, deg) {
  const rad = (deg * Math.PI) / 180
  return [cx + r * Math.cos(rad), cy - r * Math.sin(rad)]
}

/**
 * 禅圆的笔触：一条粗细沿程变化的圆弧，收笔处渐细成飞白。
 *
 * 必须画成填充闭合路径而不是 stroke —— SVG 的描边宽度是常量，等宽的圆环会被
 * 读成"加载转圈"，粗细变化才是毛笔。路径由外缘正向采样、内缘反向采样闭合而成。
 */
function brushArc({ cx, cy, r, from, sweep, width, steps = 240 }) {
  const outer = []
  const inner = []
  for (let i = 0; i <= steps; i++) {
    const t = i / steps
    const deg = from - sweep * t // 顺时针行笔
    // 起笔饱满、中段略涨、收笔剩三成，是毛笔提按的大致比例。
    const w = width * (0.86 - 0.56 * t + 0.26 * Math.sin(Math.PI * t))
    // 半径的微小起伏让圆不至于像机器画的，幅度压在 1% 以内，缩小后看不出抖动。
    const rr = r * (1 + 0.01 * Math.sin(((deg * Math.PI) / 180) * 2 + 0.8))
    const [ox, oy] = polar(cx, cy, rr + w / 2, deg)
    const [ix, iy] = polar(cx, cy, rr - w / 2, deg)
    outer.push(`${ox.toFixed(1)},${oy.toFixed(1)}`)
    inner.push(`${ix.toFixed(1)},${iy.toFixed(1)}`)
  }
  inner.reverse()
  return `M${outer[0]}L${outer.slice(1).join('L')}L${inner.join('L')}Z`
}

// ---------------------------------------------------------------- 配色

const C = {
  night: ['#16273F', '#080E1A'], // 深色底：夜蓝 → 近黑
  paper: ['#FFFFFF', '#E7EDF6'], // 浅色底：纸白 → 冷灰
  blue: ['#5CC8F5', '#2563EB'], // 主色：青 → ZenTerm 品牌蓝
  amber: '#FB923C', // 强调色：只给光标，全图唯一的暖色
  ink: '#0B1220',
}

/**
 * 套上图标外壳：背景、内高光、投影。
 * inner 是画在本体内部的内容，defs 是该方案自己的渐变定义。
 */
function frame({ id, defs = '', bg = C.night, inner, dark = true }) {
  const { path, scale } = body()
  const edge = dark ? 'rgba(255,255,255,0.14)' : 'rgba(15,23,42,0.10)'
  // 满幅版不烘焙投影：Windows / Android 由系统自己加阴影，图上再画一层会重影。
  const shadow = bleed
    ? `<path d="${path}" fill="url(#${id}-bg)"/>`
    : `<g filter="url(#${id}-shadow)"><path d="${path}" fill="url(#${id}-bg)"/></g>`
  // 方案坐标都是按 824 本体画的，满幅时整体放大，构图比例才不变。
  const open = scale === 1 ? '' : `<g transform="translate(512 512) scale(${scale.toFixed(4)}) translate(-512 -512)">`
  const close = scale === 1 ? '' : '</g>'
  return `<svg xmlns="http://www.w3.org/2000/svg" width="${CANVAS}" height="${CANVAS}" viewBox="0 0 ${CANVAS} ${CANVAS}">
  <defs>
    <linearGradient id="${id}-bg" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="${bg[0]}"/><stop offset="1" stop-color="${bg[1]}"/>
    </linearGradient>
    <radialGradient id="${id}-glow" cx="0.5" cy="0.18" r="0.9">
      <stop offset="0" stop-color="#FFFFFF" stop-opacity="${dark ? 0.16 : 0.9}"/>
      <stop offset="1" stop-color="#FFFFFF" stop-opacity="0"/>
    </radialGradient>
    <filter id="${id}-shadow" x="-20%" y="-20%" width="140%" height="140%">
      <feDropShadow dx="0" dy="14" stdDeviation="16" flood-color="#0A1020" flood-opacity="0.34"/>
    </filter>
    <clipPath id="${id}-clip"><path d="${path}"/></clipPath>
${defs}
  </defs>
  ${shadow}
  <g clip-path="url(#${id}-clip)">
    <path d="${path}" fill="url(#${id}-glow)"/>
    ${open}
${inner}
    ${close}
  </g>
  <path d="${path}" fill="none" stroke="${edge}" stroke-width="3"/>
</svg>`
}

/** 主色渐变（左上亮、右下深），各方案共用同一角度以保持一致的光源。 */
function blueGrad(id, from = C.blue[0], to = C.blue[1]) {
  return `    <linearGradient id="${id}" x1="0.1" y1="0" x2="0.9" y2="1">
      <stop offset="0" stop-color="${from}"/><stop offset="1" stop-color="${to}"/>
    </linearGradient>`
}

/** 圆角矩形路径。石头和它的每一圈轮廓都是这个形，只是逐圈放大。 */
function roundRect(cx, cy, hw, hh, r) {
  const rr = Math.min(r, hw, hh)
  const x = cx - hw
  const y = cy - hh
  const w = hw * 2
  const h = hh * 2
  return `M${x + rr} ${y}h${w - rr * 2}a${rr} ${rr} 0 0 1 ${rr} ${rr}v${h - rr * 2}a${rr} ${rr} 0 0 1 ${-rr} ${rr}h${-(w - rr * 2)}a${rr} ${rr} 0 0 1 ${-rr} ${-rr}v${-(h - rr * 2)}a${rr} ${rr} 0 0 1 ${rr} ${-rr}Z`
}

/**
 * 一块石头外围的等距轮廓圈。
 *
 * 轮廓是把石形整体外扩，不是套同心圆 —— 圆角矩形外扩 d 之后还是圆角矩形，
 * 只是圆角半径也加 d。这就是耙纹贴着石头走的形状。
 */
function contours(stone) {
  return stone.gaps.map((d) => ({
    hw: stone.hw + d,
    hh: stone.hh + d,
    r: stone.r + d,
    path: roundRect(stone.x, stone.y, stone.hw + d, stone.hh + d, stone.r + d),
  }))
}

/**
 * 直纹被最外圈挡住的那一段有多宽（半宽，0 表示这一行压根碰不到石头）。
 * 分两段算：轮廓的直边部分整段都挡，圆角部分按圆弧收窄。
 */
function blockedHalfWidth(ring, dy) {
  const a = Math.abs(dy)
  if (a >= ring.hh) return 0
  if (a <= ring.hh - ring.r) return ring.hw
  const t = a - (ring.hh - ring.r)
  return ring.hw - ring.r + Math.sqrt(Math.max(0, ring.r * ring.r - t * t))
}

/**
 * 耙出一庭沙。
 *
 * 石头周围是几圈贴着石形的轮廓，轮廓之外是笔直的纹；直纹走到最外圈就断开，
 * 不穿过去也不鼓包 —— 上一版让整条线鼓起来，出来是波浪和信纸，不是庭。
 * rows 给每条直纹的 y 和右端点（右端点不等长时，读起来就是一屏终端输出）。
 */
function raked({ stones, rows, x0 = -40, x1 = 1064, clear = 16 }) {
  const ringsOf = stones.map((s) => ({ stone: s, rings: contours(s) }))
  const ringPaths = ringsOf.flatMap(({ rings }) => rings.map((g) => g.path))

  const linePaths = rows.map(({ y, end = x1 }) => {
    // 先收集这一行被各块石头挡住的区间，再从整条线里挖掉。
    const blocks = []
    for (const { stone, rings } of ringsOf) {
      const outer = rings[rings.length - 1]
      const half = blockedHalfWidth(outer, y - stone.y)
      if (half > 0) blocks.push([stone.x - half - clear, stone.x + half + clear])
    }
    blocks.sort((a, b) => a[0] - b[0])

    const segs = []
    let cursor = x0
    for (const [bs, be] of blocks) {
      if (bs > cursor) segs.push([cursor, Math.min(bs, end)])
      cursor = Math.max(cursor, be)
    }
    if (cursor < end) segs.push([cursor, end])
    return segs
      .filter(([s, e]) => e - s > 24)
      .map(([s, e]) => `M${s.toFixed(0)} ${y} H${e.toFixed(0)}`)
      .join(' ')
  })

  return { ringPaths, linePaths: linePaths.filter(Boolean) }
}

/** 一庭沙纹画成 SVG：轮廓圈比直纹亮一点，眼睛才知道纹是绕着石头走的。 */
function sandSvg({ ringPaths, linePaths }, { dark, width = 13 }) {
  const color = dark ? '#8FC3F0' : '#2563EB'
  const lines = linePaths
    .map(
      (d) =>
        `    <path d="${d}" fill="none" stroke="${color}" stroke-opacity="${dark ? 0.3 : 0.26}" stroke-width="${width}" stroke-linecap="round"/>`,
    )
    .join('\n')
  const rings = ringPaths
    .map(
      (d) =>
        `    <path d="${d}" fill="none" stroke="${color}" stroke-opacity="${dark ? 0.42 : 0.34}" stroke-width="${width}"/>`,
    )
    .join('\n')
  return `${lines}\n${rings}`
}

/** 卧石：一块压扁的不规则石头，配立着的光标（立石）用。 */
function rock(cx, cy, w, h) {
  const shape = [
    [-0.5, 0.06],
    [-0.36, -0.36],
    [0.02, -0.5],
    [0.42, -0.28],
    [0.5, 0.12],
    [0.26, 0.42],
    [-0.3, 0.44],
  ]
  return `M${shape.map(([sx, sy]) => `${(cx + sx * w).toFixed(1)},${(cy + sy * h).toFixed(1)}`).join('L')}Z`
}

/** 终端提示符：一个 › 和一个光标块，是多个方案共用的语义零件。 */
function prompt({ x, y, scale = 1, chevron = '#FFFFFF', caret = C.amber, gap = 46 }) {
  const arm = 54 * scale
  const w = 26 * scale
  const cw = 74 * scale
  const ch = 118 * scale
  const g = gap * scale
  return `    <path d="M${x} ${y - arm} L${x + arm * 0.82} ${y} L${x} ${y + arm}"
      fill="none" stroke="${chevron}" stroke-width="${w}" stroke-linecap="round" stroke-linejoin="round"/>
    <rect x="${x + arm * 0.82 + g}" y="${y - ch / 2}" width="${cw}" height="${ch}" rx="${w / 2}" fill="${caret}"/>`
}

// ---------------------------------------------------------------- 方案
//
// enso 是选定方案，其余保留在这里是为了将来还能比较、还能回头看，
// 以及让 build.mjs 出的对比图始终是完整的。

export const concepts = {
  // 禅圆：一笔未合拢的圆，缺口处正是提示符等待输入的地方。
  enso: (dark = true) => {
    const cx = CANVAS / 2
    const cy = CANVAS / 2
    const r = 250
    const width = 74
    // 缺口居于右上 50°，张口 46°，是禅圆"未完成"的那一口气。
    const from = 27
    const sweep = 314
    const [capX, capY] = polar(cx, cy, r * 1.01, from)
    return frame({
      id: 'enso',
      dark,
      bg: dark ? C.night : C.paper,
      defs: blueGrad('enso-ring', ...(dark ? C.blue : ['#3B82F6', '#1D4ED8'])),
      // 起笔处补一个圆点当笔尖：brushArc 是填充路径，端头是平的，不补会像被切了一刀。
      inner: `    <g fill="url(#enso-ring)">
      <path d="${brushArc({ cx, cy, r, from, sweep, width })}"/>
      <circle cx="${capX.toFixed(1)}" cy="${capY.toFixed(1)}" r="${(width * 0.86) / 2}"/>
    </g>
${prompt({ x: cx - 116, y: cy, scale: 1.05, chevron: dark ? '#FFFFFF' : C.ink })}`,
    })
  },

  // Z_：名字本身被敲进终端，光标停在它后面。
  z: (dark = true) =>
    frame({
      id: 'z',
      dark,
      bg: dark ? C.night : C.paper,
      defs: blueGrad('z-stroke', ...(dark ? C.blue : ['#3B82F6', '#1D4ED8'])),
      // 字与光标线作为一组做视觉居中：组的上下边界是 268 和 756，中心正好落在 512。
      inner: `    <path d="M312 268 H712 L312 608 H712"
      fill="none" stroke="url(#z-stroke)" stroke-width="88"
      stroke-linecap="round" stroke-linejoin="round"/>
    <rect x="312" y="690" width="400" height="66" rx="33" fill="${C.amber}"/>`,
    }),

  // 提示符：最朴素、也最经得起缩到 16px 的做法。
  prompt: (dark = true) =>
    frame({
      id: 'prompt',
      dark,
      bg: dark ? C.night : C.paper,
      inner: prompt({
        x: 330,
        y: CANVAS / 2,
        scale: 1.85,
        chevron: dark ? '#E8F1FF' : C.ink,
      }),
    }),

  // 月洞门：实心圆里挖出提示符。solid 形在 16px 下比圆环稳，是 enso 的"能缩"版本。
  moongate: (dark = true) => {
    const cx = CANVAS / 2
    const cy = CANVAS / 2
    const r = 276
    const arm = 74
    const w = 30
    const x0 = cx - 118
    const caretX = x0 + arm * 0.82 + 62
    return frame({
      id: 'mg',
      dark,
      bg: dark ? C.night : C.paper,
      defs: `${blueGrad('mg-disc', ...(dark ? C.blue : ['#3B82F6', '#1D4ED8']))}
    <mask id="mg-mask">
      <circle cx="${cx}" cy="${cy}" r="${r}" fill="#fff"/>
      <path d="M${x0} ${cy - arm} L${x0 + arm * 0.82} ${cy} L${x0} ${cy + arm}"
        fill="none" stroke="#000" stroke-width="${w * 2.3}" stroke-linecap="round" stroke-linejoin="round"/>
      <rect x="${caretX - 12}" y="${cy - 92}" width="124" height="184" rx="26" fill="#000"/>
    </mask>`,
      // 光标块比挖空的洞小一圈，留出的暗边让它像是嵌在门里而不是贴上去的。
      inner: `    <circle cx="${cx}" cy="${cy}" r="${r}" fill="url(#mg-disc)" mask="url(#mg-mask)"/>
    <rect x="${caretX}" y="${cy - 80}" width="100" height="160" rx="18" fill="${C.amber}"/>`,
    })
  },

  // 跳板：Z 的四个折点就是 ProxyJump 的四跳，落点那一跳是暖色。远看是字母，近看是链路。
  hop: (dark = true) => {
    const nodes = [
      [306, 292],
      [718, 292],
      [306, 628],
      [718, 628],
    ]
    const dots = nodes
      .map(
        ([x, y], i) =>
          `    <circle cx="${x}" cy="${y}" r="58" fill="${i === 3 ? C.amber : dark ? '#0E1A2B' : '#FFFFFF'}" ${
            i === 3 ? '' : `stroke="url(#hop-line)" stroke-width="34"`
          }/>`,
      )
      .join('\n')
    return frame({
      id: 'hop',
      dark,
      bg: dark ? C.night : C.paper,
      defs: blueGrad('hop-line', ...(dark ? C.blue : ['#3B82F6', '#1D4ED8'])),
      inner: `    <path d="M306 292 H718 L306 628 H718" fill="none" stroke="url(#hop-line)"
      stroke-width="46" stroke-linecap="round" stroke-linejoin="round"/>
${dots}`,
    })
  },

  // 三石：一块压一块的平衡，最上面那块立着的是光标。禅味最足，终端味最淡。
  stones: (dark = true) =>
    frame({
      id: 'stones',
      dark,
      bg: dark ? C.night : C.paper,
      defs: blueGrad('stones-fill', ...(dark ? C.blue : ['#3B82F6', '#1D4ED8'])),
      inner: `    <rect x="302" y="620" width="420" height="118" rx="59" fill="url(#stones-fill)"/>
    <rect x="362" y="492" width="300" height="108" rx="54" fill="url(#stones-fill)" opacity="0.82"/>
    <rect x="452" y="286" width="120" height="190" rx="26" fill="${C.amber}"/>`,
    }),

  // 一个光标：产品主张的字面翻译 —— 只留必需的那一个东西，背后是淡到几乎看不见的禅圆。
  caret: (dark = true) =>
    frame({
      id: 'caret',
      dark,
      bg: dark ? C.night : C.paper,
      inner: `    <circle cx="512" cy="512" r="300" fill="none"
      stroke="${dark ? '#FFFFFF' : C.ink}" stroke-opacity="0.09" stroke-width="44"/>
    <rect x="412" y="332" width="200" height="360" rx="34" fill="${C.amber}"/>`,
    }),


  // ---- 枯山水 ----
  // 第一版是三圈同心圆（涟漪，不是庭），第二版让直纹整体鼓包（波浪，还是不是庭）。
  // 这一版按真实的耙法来：石头四周几圈贴着石形外扩的轮廓，轮廓之外笔直，直纹走到圈边就断。
  // 光标块就是那块立石 —— 庭里唯一的实体，也是终端里唯一会动的东西。

  // 一石：满幅耙纹，石偏左，三圈轮廓。最接近龙安寺的读法。
  'garden-ring': (dark = true) => {
    const stone = { x: 430, y: 512, hw: 48, hh: 86, r: 18, gaps: [40, 92, 148] }
    const rows = [52, 156, 260, 364].flatMap((d) => [
      { y: stone.y - d },
      { y: stone.y + d },
    ])
    return frame({
      id: 'gr',
      dark,
      bg: dark ? C.night : C.paper,
      inner: `${sandSvg(raked({ stones: [stone], rows }), { dark, width: 13 })}
    <path d="${roundRect(stone.x, stone.y, stone.hw, stone.hh, stone.r)}" fill="${C.amber}"/>`,
    })
  },

  // 半庭：沙只铺下半，上面是留白。庭院的留白比沙纹本身更像禅，缩小后也比满幅干净。
  'garden-half': (dark = true) => {
    const stone = { x: 606, y: 648, hw: 46, hh: 82, r: 17, gaps: [40, 94] }
    const rows = [
      { y: 508 },
      { y: 588 },
      { y: 668 },
      { y: 748 },
      { y: 828 },
    ]
    return frame({
      id: 'gh',
      dark,
      bg: dark ? C.night : C.paper,
      // 沙面的边界：没有这条线，上半部只是"空"，不是"留白"。
      inner: `    <path d="M60 436 H964" stroke="${dark ? '#FFFFFF' : C.ink}" stroke-opacity="0.13" stroke-width="9"/>
${sandSvg(raked({ stones: [stone], rows }), { dark, width: 14 })}
    <path d="${roundRect(stone.x, stone.y, stone.hw, stone.hh, stone.r)}" fill="${C.amber}"/>`,
    })
  },

  // 耙纹即输出：沙纹长短不齐，就是一屏落定的终端输出；石头落在其中，纹绕开它。
  // 几个里唯一把"枯山水"和"终端"说成同一件事的。
  'garden-out': (dark = true) => {
    const stone = { x: 668, y: 512, hw: 46, hh: 82, r: 17, gaps: [38, 88, 140] }
    const ends = [864, 596, 892, 648, 820, 560, 876]
    const rows = [-312, -208, -104, 0, 104, 208, 312]
      .map((d, i) => ({ y: stone.y + d + 52, end: ends[i] }))
      .filter((r) => r.y > 90 && r.y < 934)
    return frame({
      id: 'go',
      dark,
      bg: dark ? C.night : C.paper,
      inner: `${sandSvg(raked({ stones: [stone], rows, x0: 164 }), { dark, width: 14 })}
    <path d="${roundRect(stone.x, stone.y, stone.hw, stone.hh, stone.r)}" fill="${C.amber}"/>`,
    })
  },

  // 二石：立石与卧石，枯山水里最常见的一组。卧石是蓝的真石头，立石是光标。
  // 沙的轮廓绕角石时本来就比石头本身圆 —— 所以棱角的石配圆转的轮廓是对的，不是偷懒。
  'garden-two': (dark = true) => {
    const upright = { x: 388, y: 420, hw: 44, hh: 78, r: 16, gaps: [38, 88] }
    const lying = { x: 646, y: 648, hw: 105, hh: 59, r: 40, gaps: [42, 96] }
    const rows = [140, 244, 348, 452, 556, 660, 764, 868].map((y) => ({ y }))
    return frame({
      id: 'gt',
      dark,
      bg: dark ? C.night : C.paper,
      defs: blueGrad('gt-rock', ...(dark ? C.blue : ['#3B82F6', '#1D4ED8'])),
      inner: `${sandSvg(raked({ stones: [upright, lying], rows }), { dark, width: 13 })}
    <path d="${rock(lying.x, lying.y, 206, 116)}" fill="url(#gt-rock)" stroke="url(#gt-rock)" stroke-width="22" stroke-linejoin="round"/>
    <path d="${roundRect(upright.x, upright.y, upright.hw, upright.hh, upright.r)}" fill="${C.amber}"/>`,
    })
  },

  // 一圈两纹：把庭减到不能再减。32px 下唯一还读得出"绕"这个动作的一版。
  'garden-min': (dark = true) => {
    const stone = { x: 478, y: 512, hw: 52, hh: 92, r: 19, gaps: [52] }
    const rows = [-232, -78, 78, 232].map((d) => ({ y: stone.y + d }))
    return frame({
      id: 'gm',
      dark,
      bg: dark ? C.night : C.paper,
      inner: `${sandSvg(raked({ stones: [stone], rows, clear: 22 }), { dark, width: 19 })}
    <path d="${roundRect(stone.x, stone.y, stone.hw, stone.hh, stone.r)}" fill="${C.amber}"/>`,
    })
  },

  // 窗：ZenTerm 自己的辨识特征 —— 左边那条时间戳栏 —— 直接画进图标。
  window: (dark = true) => {
    const ticks = [0, 1, 2]
      .map(
        (i) =>
          `    <rect x="232" y="${356 + i * 96}" width="46" height="16" rx="8" fill="${dark ? '#FFFFFF' : C.ink}" opacity="0.28"/>`,
      )
      .join('\n')
    return frame({
      id: 'win',
      dark,
      bg: dark ? C.night : C.paper,
      inner: `    <rect x="180" y="238" width="664" height="548" rx="58"
      fill="${dark ? 'rgba(255,255,255,0.06)' : 'rgba(15,23,42,0.05)'}"
      stroke="${dark ? 'rgba(255,255,255,0.18)' : 'rgba(15,23,42,0.14)'}" stroke-width="8"/>
${ticks}
    <rect x="306" y="330" width="7" height="364" rx="3.5" fill="${dark ? '#FFFFFF' : C.ink}" opacity="0.22"/>
${prompt({ x: 388, y: CANVAS / 2, scale: 1.2, chevron: dark ? '#E8F1FF' : C.ink })}`,
    })
  },

  // 石庭：三行落定的输出，末行留着光标。静，但仍然是终端。
  lines: (dark = true) => {
    const x = 268
    const h = 64
    const bars = [488, 364, 244]
      .map(
        (w, i) =>
          `    <rect x="${x}" y="${352 + i * 132}" width="${w}" height="${h}" rx="${h / 2}" fill="url(#lines-bar)" opacity="${1 - i * 0.18}"/>`,
      )
      .join('\n')
    return frame({
      id: 'lines',
      dark,
      bg: dark ? C.night : C.paper,
      defs: blueGrad('lines-bar', ...(dark ? C.blue : ['#3B82F6', '#1D4ED8'])),
      inner: `${bars}
    <rect x="${x + 288}" y="${602}" width="78" height="92" rx="14" fill="${C.amber}"/>`,
    })
  },
}

// ---------------------------------------------------------------- 渲染

/**
 * 用无头 Chrome 把 SVG 截成 PNG。
 *
 * 出小尺寸时必须改写 SVG 根节点的 width/height：只缩窗口的话 Chrome 会裁剪而不是缩放，
 * 因为根节点写死了 1024。改写之后是按 viewBox 重新栅格化一次，
 * 而不是把 1024 的位图缩下去 —— 16/32px 的图标这样才清楚。
 */
export function rasterize(svgPath, pngPath, size = CANVAS) {
  let src = svgPath
  if (size !== CANVAS) {
    src = join(tmpdir(), `zenterm-icon-${size}.svg`)
    writeFileSync(
      src,
      readFileSync(svgPath, 'utf8').replace(
        /width="\d+" height="\d+"/,
        `width="${size}" height="${size}"`,
      ),
    )
  }
  execFileSync(
    CHROME,
    [
      '--headless',
      '--disable-gpu',
      '--hide-scrollbars',
      '--force-device-scale-factor=1',
      '--default-background-color=00000000',
      `--screenshot=${pngPath}`,
      `--window-size=${size},${size}`,
      `file://${src}`,
    ],
    { stdio: 'ignore' },
  )
}
