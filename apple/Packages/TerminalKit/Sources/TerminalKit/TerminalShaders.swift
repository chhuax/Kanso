import Metal
import os

/// 着色器源码与 GPU 实例结构。
///
/// 源码放在 Swift 字符串里，启动时用 `makeLibrary(source:)` 在运行时编译：`swift build` 不会编译 `.metal` 文件
/// （SwiftPM 的 Metal 支持有限，且 `xcodebuild` 处理 package 资源里的 .metal 要额外配置 metallib 打包），
/// 而 TerminalKit 又要能被 `swift build` 单独编译。代价是首次创建渲染器时要付一次编译耗时，
/// 耗时记录在 `MetalResources.shaderCompileSeconds`，以后优化启动时间时可以改为预编译 metallib。
enum TerminalShaders {
    static let source = """
    #include <metal_stdlib>
    using namespace metal;

    struct Uniforms {
        float2 viewport; // 渲染目标尺寸，设备像素
    };

    // 背景色块与装饰共用：style 0 实心，1 波浪，2 点线，3 虚线，4 圆角矩形（y 为半径）。
    struct RectInstance {
        float4 rect;   // x, y, w, h（设备像素，原点左上）
        float4 color;  // sRGB 编码值，非预乘
        float4 params; // style, thickness, onLength, period
    };

    struct GlyphInstance {
        float4 rect;   // 位图在屏幕上的 x, y, w, h（设备像素）
        float4 uv;     // u0, v0, u1, v1
        float4 color;  // 前景色；彩色字形只用 alpha
    };

    static float2 corner(uint vid) {
        // 两个三角形拼成的单位正方形。
        const float2 c[6] = { float2(0, 0), float2(1, 0), float2(0, 1), float2(1, 0), float2(1, 1), float2(0, 1) };
        return c[vid];
    }

    static float4 toClip(float2 px, float2 viewport) {
        return float4(px.x / viewport.x * 2.0 - 1.0, 1.0 - px.y / viewport.y * 2.0, 0.0, 1.0);
    }

    struct RectOut {
        float4 position [[position]];
        float4 color [[flat]];
        float4 params [[flat]];
        float2 size [[flat]];
        float2 local;
    };

    vertex RectOut rect_vertex(uint vid [[vertex_id]], uint iid [[instance_id]],
                               constant RectInstance *instances [[buffer(0)]],
                               constant Uniforms &u [[buffer(1)]]) {
        RectInstance r = instances[iid];
        float2 c = corner(vid);
        RectOut out;
        out.position = toClip(r.rect.xy + c * r.rect.zw, u.viewport);
        out.color = r.color;
        out.params = r.params;
        out.size = r.rect.zw;
        out.local = c * r.rect.zw;
        return out;
    }

    fragment float4 rect_fragment(RectOut in [[stage_in]]) {
        int style = int(in.params.x + 0.5);
        float t = in.params.y;
        float coverage = 1.0;
        if (style == 1) {
            // 波浪线：一个周期一条正弦曲线，用到曲线的垂直距离做抗锯齿。
            // 周期等于单元格宽度，所以相邻单元格的波形首尾相接。
            float amplitude = max((in.size.y - t) * 0.5, 0.0);
            float yc = in.size.y * 0.5 + amplitude * sin(in.local.x / in.params.w * 6.2831853);
            coverage = clamp(t * 0.5 + 0.5 - abs(in.local.y - yc), 0.0, 1.0);
        } else if (style == 2 || style == 3) {
            // 点线 / 虚线：按整数像素列判断，保持边缘锐利。
            float x = floor(in.local.x);
            coverage = (fmod(x, in.params.w) < in.params.z) ? 1.0 : 0.0;
        } else if (style == 4) {
            float2 q = abs(in.local - in.size * 0.5) - (in.size * 0.5 - t);
            float distance = length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - t;
            coverage = clamp(0.5 - distance, 0.0, 1.0);
        }
        float a = in.color.a * coverage;
        return float4(in.color.rgb * a, a);
    }

    struct GlyphOut {
        float4 position [[position]];
        float2 uv;
        float4 color [[flat]];
    };

    vertex GlyphOut glyph_vertex(uint vid [[vertex_id]], uint iid [[instance_id]],
                                 constant GlyphInstance *instances [[buffer(0)]],
                                 constant Uniforms &u [[buffer(1)]]) {
        GlyphInstance g = instances[iid];
        float2 c = corner(vid);
        GlyphOut out;
        out.position = toClip(g.rect.xy + c * g.rect.zw, u.viewport);
        out.uv = mix(g.uv.xy, g.uv.zw, c);
        out.color = g.color;
        return out;
    }

    constexpr sampler atlasSampler(coord::normalized, filter::nearest, address::clamp_to_edge);

    // 灰度图集：纹理里是覆盖率，乘前景色。输出预乘 alpha，与管线的 (one, oneMinusSourceAlpha) 混合配合。
    fragment float4 glyph_gray_fragment(GlyphOut in [[stage_in]], texture2d<float> atlas [[texture(0)]]) {
        float coverage = atlas.sample(atlasSampler, in.uv).r;
        float a = coverage * in.color.a;
        return float4(in.color.rgb * a, a);
    }

    // 彩色图集：纹理已经是预乘 alpha 的 RGBA，只需要整体乘不透明度（dim 用）。
    fragment float4 glyph_color_fragment(GlyphOut in [[stage_in]], texture2d<float> atlas [[texture(0)]]) {
        return atlas.sample(atlasSampler, in.uv) * in.color.a;
    }
    """
}

/// Swift 侧的实例结构，布局必须与上面 MSL 的 `RectInstance` / `GlyphInstance` 逐字段一致
/// （三个 `float4`，48 字节，16 字节对齐）。
struct RectInstance {
    var rect: SIMD4<Float>
    var color: SIMD4<Float>
    var params: SIMD4<Float>
}

struct GlyphInstance {
    var rect: SIMD4<Float>
    var uv: SIMD4<Float>
    var color: SIMD4<Float>
}

/// 每个 `MTLDevice` 一份的 Metal 资源：着色器库与渲染管线。
///
/// 多个终端视图（多标签）共用，所以着色器只在进程内编译一次。只在主线程使用。
@MainActor
final class MetalResources {
    static let colorPixelFormat = MTLPixelFormat.bgra8Unorm

    let device: MTLDevice
    let queue: MTLCommandQueue
    let rectPipeline: MTLRenderPipelineState
    let grayGlyphPipeline: MTLRenderPipelineState
    let colorGlyphPipeline: MTLRenderPipelineState
    /// 编译着色器库与创建管线的总耗时（秒），记录下来供优化启动时间时参考。
    let shaderCompileSeconds: Double

    private static var cache: [ObjectIdentifier: MetalResources] = [:]

    static func shared(for device: MTLDevice) -> MetalResources? {
        let id = ObjectIdentifier(device)
        if let hit = cache[id] { return hit }
        guard let made = MetalResources(device: device) else { return nil }
        cache[id] = made
        return made
    }

    private init?(device: MTLDevice) {
        let log = Logger(subsystem: Bundle.main.bundleIdentifier ?? "com.chhuax.kanso", category: "renderer")
        let start = DispatchTime.now().uptimeNanoseconds
        guard let queue = device.makeCommandQueue() else { return nil }
        let library: MTLLibrary
        do {
            library = try device.makeLibrary(source: TerminalShaders.source, options: nil)
        } catch {
            log.error("着色器编译失败：\(error.localizedDescription)")
            return nil
        }

        func pipeline(_ name: String, _ vertex: String, _ fragment: String) -> MTLRenderPipelineState? {
            let desc = MTLRenderPipelineDescriptor()
            desc.label = name
            desc.vertexFunction = library.makeFunction(name: vertex)
            desc.fragmentFunction = library.makeFunction(name: fragment)
            let attachment = desc.colorAttachments[0]!
            attachment.pixelFormat = Self.colorPixelFormat
            // 片段着色器输出预乘 alpha：结果 = src + dst * (1 - src.a)。
            attachment.isBlendingEnabled = true
            attachment.rgbBlendOperation = .add
            attachment.alphaBlendOperation = .add
            attachment.sourceRGBBlendFactor = .one
            attachment.sourceAlphaBlendFactor = .one
            attachment.destinationRGBBlendFactor = .oneMinusSourceAlpha
            attachment.destinationAlphaBlendFactor = .oneMinusSourceAlpha
            do {
                return try device.makeRenderPipelineState(descriptor: desc)
            } catch {
                log.error("创建管线 \(name) 失败：\(error.localizedDescription)")
                return nil
            }
        }
        guard let rect = pipeline("rect", "rect_vertex", "rect_fragment"),
              let gray = pipeline("glyph.gray", "glyph_vertex", "glyph_gray_fragment"),
              let color = pipeline("glyph.color", "glyph_vertex", "glyph_color_fragment")
        else { return nil }

        self.device = device
        self.queue = queue
        rectPipeline = rect
        grayGlyphPipeline = gray
        colorGlyphPipeline = color
        shaderCompileSeconds = Double(DispatchTime.now().uptimeNanoseconds - start) / 1e9
        log.info("着色器编译与管线创建耗时 \(self.shaderCompileSeconds * 1000, format: .fixed(precision: 1)) ms")
    }
}
