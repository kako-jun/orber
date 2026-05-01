// orber#112 — WebGL2 fragment shader による per-pixel orb 描画。
// orber#55 Phase B — Glyph shape (アウトライン fill) 経路と softness 軸を追加。
//
// `orber-wasm` の `get_render_data` で得た Float32Array をそのまま uniform に
// 流し、fragment shader 1 pass で全 orb の Source-Over 合成を行う。CPU 経路
// (`crates/core::animate::render_frame_with_params`) と同じ数式・同じ per-orb
// パラメータを使うので、視覚パリティは「最終的な見た目が同じ」が保たれる。
//
// shape == "glyph" のときは、`u_glyph_sdf` (256x256 SDF texture) を `(cx, cy)`
// 中心 + 半径 r の正方領域で sampling し、Circle と同じ falloff に流す。Circle 経路は
// 既存の rim/soft グラデを維持し、`if u_shape_id == 0` 分岐で texture lookup を
// skip して regression を避ける。
// softness の alpha_mul は per-orb opacity に乗算（CPU 経路と同式）、
// blur_offset は wasm 側で base_blur に積算済みなので shader はそのまま使う。
//
// アーキテクチャ:
//   1. setup() で program / VAO / FBO 関連の使い回しリソースを 1 度だけ作る
//   2. setRenderData(buf) で per-orb uniform を 1 度アップロード（フレーム間で
//      不変なので 192 frame の動画化でも 1 回だけ）
//   3. renderFrame(t) で u_t だけ書き換えて drawArrays 1 発
//
// 仕様の数式は CPU 経路と完全一致させる:
//   - extent = 1 + 2 * r_normalized
//   - r_normalized = base_radius_unit * sqrt(weight) * 1.10 / progress_axis_pixels
//   - advance_steps = fract(cycle * speed_mult * t)
//   - pos = mod(phase * extent + advance_steps * extent, extent) - r_normalized
//   - 3 軸独立呼吸: radius ±10%, blur ±15, opacity ±5%
//   - rim: 3-stop alpha gradient (mid_stop = clamp(1 - blur*0.8, 0.05, 0.95))
//          center_a = opacity, mid_a = opacity * 80/255 ≈ 0.3137
//   - soft: 2-stop alpha gradient (hold_stop = clamp(1 - blur, 0.05, 0.95))
//   - Source-Over: out.rgb = src.rgb * src.a + out.rgb * (1 - src.a)
//                  out.a   = src.a + out.a * (1 - src.a)
//
// review S1: CPU 側 (crates/core::orb) は alpha を `(opacity * 255).round() as u8`
// と `(opacity * 80).round() as u8` で 1/255 ステップに量子化してから tiny-skia
// に渡している。本 shader は raw float のまま blend する。差分は最大 ≤ 1/255
// (≒ 0.4% の輝度差) で肉眼識別不能。kako-jun 合意の「最終的な見た目が同じ」
// 合格ラインを守る前提で量子化は省略している。

/// uniform 配列の上限。`crates/core::animate::MAX_ORB_COUNT = 1024` ほど大きく
/// する必要はなく、GUI 経路では `random_batch_specs` の count_range
/// (COUNT_MAX = 50) が事実上の上限。バッファ余裕を持たせて 64 とする。
// SYNC WITH crates/wasm/src/lib.rs::GL_RENDERER_MAX_ORBS
const MAX_ORBS = 64;

const HEADER_WORDS = 16;
const PER_ORB_WORDS = 16;

/// Glyph SDF テクスチャの解像度（縦 = 横）。
/// wasm `get_glyph_sdf` の `size` 引数と一致させる必要がある。
/// 256 は GUI のプレビュー (360x640 / 640x360) でも十分な精度を保てる。
export const GLYPH_SDF_SIZE = 256;

const VS = `#version 300 es
in vec2 a_pos;
void main() {
  gl_Position = vec4(a_pos, 0.0, 1.0);
}`;

// fragment shader: per-pixel に全 orb をループして Source-Over で合成する。
// 仕様の数式 (extent / pos / 呼吸 / rim/soft グラデ) を 1:1 で再現。
const FS = `#version 300 es
precision highp float;
out vec4 outColor;

const float TAU = 6.28318530718;
const float BREATH_RADIUS_MAX_FACTOR = 1.10;

uniform vec2 u_resolution;
uniform float u_t;             // [0, 1)
uniform vec4 u_bg;             // straight rgba (0..1)
uniform float u_base_radius;   // px
uniform float u_base_blur;     // 0..1
uniform float u_direction;     // 0=LR, 1=RL, 2=TB, 3=BT
uniform float u_cycle;         // 1=VerySlow, 2=Slow, 3=Mid, 4=Fast
uniform int u_n_orbs;
// Phase B (#55):
uniform float u_alpha_mul;     // softness.alpha_mul (Mid=1.0)
uniform int u_shape_id;        // 0=Circle, 1=Glyph
uniform sampler2D u_glyph_sdf;

// per-orb uniforms (length MAX_ORBS = 64). Float で詰める。
uniform vec4 u_orb_color[${MAX_ORBS}];     // (r, g, b, weight)
uniform vec4 u_orb_phase[${MAX_ORBS}];     // (phase, phi_radius, phi_blur, phi_opacity)
uniform vec4 u_orb_misc[${MAX_ORBS}];      // (cross_axis, style_bit, speed_mult, _)
uniform vec4 u_orb_rot[${MAX_ORBS}];       // (base_angle, rot_speed_signed, _, _)

float clampf(float x, float a, float b) { return min(max(x, a), b); }

float falloff_curve(float style_bit, float r, float blur, float opacity) {
  if (opacity <= 0.0 || r >= 1.0) return 0.0;
  r = max(r, 0.0);
  if (style_bit < 0.5) {
    float center_a = opacity;
    float mid_a = opacity * (80.0 / 255.0);
    float mid_stop = clampf(1.0 - blur * 0.8, 0.05, 0.95);
    if (r <= mid_stop) {
      float u = (mid_stop > 0.0) ? (r / mid_stop) : 1.0;
      return mix(center_a, mid_a, u);
    }
    float denom = max(1.0 - mid_stop, 1e-6);
    float u = (r - mid_stop) / denom;
    return mix(mid_a, 0.0, u);
  }
  float hold_stop = clampf(1.0 - blur, 0.05, 0.95);
  if (r <= hold_stop) return opacity;
  float denom = max(1.0 - hold_stop, 1e-6);
  float u = (r - hold_stop) / denom;
  return mix(opacity, 0.0, u);
}

void main() {
  vec2 px = gl_FragCoord.xy;
  // N4: shader-internal comments are kept English for RenderDoc / Spector.js
  // capture readability (multibyte source comments may not survive extraction).
  // gl_FragCoord origin is bottom-left, but CPU path uses top-left
  // (image::RgbaImage). Flip y to match.
  px.y = u_resolution.y - px.y;

  // 進行軸長 (LR/RL=width, TB/BT=height)
  float progress_axis = (u_direction < 1.5) ? u_resolution.x : u_resolution.y;

  // 背景塗り (straight alpha)
  vec3 acc_rgb = u_bg.rgb;
  float acc_a = u_bg.a;

  for (int i = 0; i < ${MAX_ORBS}; i++) {
    if (i >= u_n_orbs) break;

    vec4 col = u_orb_color[i];
    vec4 ph = u_orb_phase[i];
    vec4 misc = u_orb_misc[i];
    vec4 rot = u_orb_rot[i];

    float weight = col.w;
    float phase = ph.x;
    float phi_radius = ph.y;
    float phi_blur = ph.z;
    float phi_opacity = ph.w;
    float cross_axis = misc.x;
    float style_bit = misc.y;       // 0=rim, 1=soft
    float speed_mult = misc.z;
    float base_angle = rot.x;
    float rot_speed_signed = rot.y;

    float r_pixels_max = u_base_radius * sqrt(max(weight, 0.0)) * BREATH_RADIUS_MAX_FACTOR;
    float r_normalized = (progress_axis > 0.0) ? (r_pixels_max / progress_axis) : 0.0;
    float extent = 1.0 + 2.0 * r_normalized;

    float advance_steps = fract(u_cycle * speed_mult * u_t);
    float raw = phase * extent + advance_steps * extent;
    // GLSL mod() never returns a negative value (mod(x, y) = x - y * floor(x/y)),
    // matching the Rust rem_euclid result so the resulting pos is identical to the CPU path.
    float pos = mod(raw, extent) - r_normalized;

    float nx, ny;
    if (u_direction < 0.5) {        // LR
      nx = pos; ny = cross_axis;
    } else if (u_direction < 1.5) { // RL
      nx = 1.0 - pos; ny = cross_axis;
    } else if (u_direction < 2.5) { // TB
      nx = cross_axis; ny = pos;
    } else {                        // BT
      nx = cross_axis; ny = 1.0 - pos;
    }

    float t_frac = fract(u_t);
    float radius_factor = 1.0 + 0.10 * sin(TAU * t_frac + phi_radius);
    float blur_delta = 0.15 * sin(TAU * t_frac + phi_blur);
    float opacity_factor = 1.0 + 0.05 * sin(TAU * t_frac + phi_opacity);

    float radius = u_base_radius * sqrt(max(weight, 0.0)) * radius_factor;
    if (radius <= 0.0) continue;

    float blur = clampf(u_base_blur + blur_delta, 0.0, 1.0);
    // Phase B (#55): softness.alpha_mul を per-orb opacity に乗算（CPU 経路と同式）。
    // Mid なら u_alpha_mul = 1.0 で既存挙動と完全同値。
    float opacity = clampf(opacity_factor * u_alpha_mul, 0.0, 1.0);

    float cx = nx * u_resolution.x;
    float cy = ny * u_resolution.y;

    // alpha 計算: shape == 0 (Circle) は既存の r=distance/radius、shape == 1
    // (Glyph) は SDF sampling 後に同じ falloff_curve へ流す。
    float alpha = 0.0;
    if (u_shape_id == 1) {
      vec2 local = px - vec2(cx, cy);
      float angle = base_angle + u_t * rot_speed_signed * TAU * u_cycle;
      float c = cos(angle);
      float s = sin(angle);
      vec2 rotated = vec2(c * local.x - s * local.y, s * local.x + c * local.y);
      vec2 uv = rotated / (2.0 * radius) + 0.5;
      if (uv.x >= 0.0 && uv.x <= 1.0 && uv.y >= 0.0 && uv.y <= 1.0) {
        float sdf01 = texture(u_glyph_sdf, uv).r;
        float signed_unit = sdf01 * 2.0 - 1.0;
        float r = 1.0 - signed_unit;
        alpha = falloff_curve(style_bit, r, blur, opacity);
      }
    } else {
      float dist = distance(px, vec2(cx, cy));
      float r = dist / radius;
      alpha = falloff_curve(style_bit, r, blur, opacity);
    }

    if (alpha > 0.0) {
      // Source-Over (straight alpha)
      // out.rgb = src.rgb * src.a + out.rgb * (1 - src.a)
      // out.a   = src.a + out.a * (1 - src.a)
      vec3 src_rgb = col.rgb;
      float one_minus_a = 1.0 - alpha;
      acc_rgb = src_rgb * alpha + acc_rgb * one_minus_a;
      acc_a = alpha + acc_a * one_minus_a;
    }
  }

  outColor = vec4(acc_rgb, acc_a);
}`;

export interface GlRenderer {
  /** 解像度を変更する。canvas のサイズは呼び出し側で予め変更しておくこと。 */
  setResolution(width: number, height: number): void;
  /** wasm の get_render_data 出力を 1 回だけ uniform に流す。 */
  setRenderData(data: Float32Array): void;
  /**
   * Glyph SDF テクスチャをアップロードする。
   * `mask` は長さ `size * size` の `Uint8Array`（各バイトが SDF 0..255）。
   * shape == "glyph" のときに必須。Circle のときは呼ばなくても安全だが、
   * 既存テクスチャを上書きしても害はない。同じ glyph + size なら呼び出し側で
   * キャッシュして再 upload を避けることを推奨。
   */
  setGlyphSdf(mask: Uint8Array, size: number): void;
  /** u_t を書き換えて 1 フレーム描画する。 */
  renderFrame(t: number): void;
  /** リソース解放（canvas を捨てるとき）。 */
  dispose(): void;
}

type AnyCanvas = HTMLCanvasElement | OffscreenCanvas;

export function createGlRenderer(canvas: AnyCanvas): GlRenderer {
  // alpha=true: 出力に straight alpha を残したいので背景透過 canvas で取る。
  // OffscreenCanvas からの transferToImageBitmap / VideoFrame(canvas) は
  // canvas のピクセルをそのままコピーする。
  const gl = canvas.getContext('webgl2', {
    alpha: true,
    antialias: false,
    premultipliedAlpha: false,
    preserveDrawingBuffer: false,
  }) as WebGL2RenderingContext | null;
  if (!gl) {
    throw new Error('WebGL2 context could not be created');
  }

  function compile(type: number, src: string): WebGLShader {
    const sh = gl!.createShader(type)!;
    gl!.shaderSource(sh, src);
    gl!.compileShader(sh);
    if (!gl!.getShaderParameter(sh, gl!.COMPILE_STATUS)) {
      const info = gl!.getShaderInfoLog(sh) ?? '<no log>';
      gl!.deleteShader(sh);
      throw new Error(`shader compile failed: ${info}`);
    }
    return sh;
  }

  const vs = compile(gl.VERTEX_SHADER, VS);
  const fs = compile(gl.FRAGMENT_SHADER, FS);
  const prog = gl.createProgram()!;
  gl.attachShader(prog, vs);
  gl.attachShader(prog, fs);
  gl.linkProgram(prog);
  if (!gl.getProgramParameter(prog, gl.LINK_STATUS)) {
    const info = gl.getProgramInfoLog(prog) ?? '<no log>';
    throw new Error(`program link failed: ${info}`);
  }
  gl.useProgram(prog);
  // shader を program に紐付けたら個別オブジェクトは破棄してよい。
  gl.deleteShader(vs);
  gl.deleteShader(fs);

  // full-screen triangle (covers viewport without scissor)
  const vao = gl.createVertexArray()!;
  gl.bindVertexArray(vao);
  const vbo = gl.createBuffer()!;
  gl.bindBuffer(gl.ARRAY_BUFFER, vbo);
  gl.bufferData(
    gl.ARRAY_BUFFER,
    new Float32Array([-1, -1, 3, -1, -1, 3]),
    gl.STATIC_DRAW,
  );
  const aPos = gl.getAttribLocation(prog, 'a_pos');
  gl.enableVertexAttribArray(aPos);
  gl.vertexAttribPointer(aPos, 2, gl.FLOAT, false, 0, 0);

  const uLoc = {
    resolution: gl.getUniformLocation(prog, 'u_resolution'),
    t: gl.getUniformLocation(prog, 'u_t'),
    bg: gl.getUniformLocation(prog, 'u_bg'),
    baseRadius: gl.getUniformLocation(prog, 'u_base_radius'),
    baseBlur: gl.getUniformLocation(prog, 'u_base_blur'),
    direction: gl.getUniformLocation(prog, 'u_direction'),
    cycle: gl.getUniformLocation(prog, 'u_cycle'),
    nOrbs: gl.getUniformLocation(prog, 'u_n_orbs'),
    alphaMul: gl.getUniformLocation(prog, 'u_alpha_mul'),
    shapeId: gl.getUniformLocation(prog, 'u_shape_id'),
    glyphMask: gl.getUniformLocation(prog, 'u_glyph_sdf'),
    orbColor: gl.getUniformLocation(prog, 'u_orb_color'),
    orbPhase: gl.getUniformLocation(prog, 'u_orb_phase'),
    orbMisc: gl.getUniformLocation(prog, 'u_orb_misc'),
    orbRot: gl.getUniformLocation(prog, 'u_orb_rot'),
  };

  // u_glyph_sdf は texture unit 0 に固定。Circle 経路でも
  // shader 側で `if u_shape_id == 0` で sampling を skip するため、初期は
  // 空 (1x1 黒) のテクスチャをバインドしておけば Circle 経路で uninitialized
  // sampler 警告が出ない。setGlyphSdf が呼ばれたら中身が差し替わる。
  const glyphTex = gl.createTexture()!;
  gl.activeTexture(gl.TEXTURE0);
  gl.bindTexture(gl.TEXTURE_2D, glyphTex);
  // 初期 1x1 alpha=0 dummy。`gl.LUMINANCE` も使えるが WebGL2 では `R8` 内部
  // フォーマットが推奨。R チャネルだけ使い、shader 側は texture(...).a で
  // 取る運用に統一する（R8 は alpha チャネルが常に 1.0 に見えるので、
  // 形状マスクとして R チャネルの値が欲しい）。よって shader 側は
  // texture(...).r を使うのが正しい。これに合わせて shader 側も .r に統一する。
  gl.texImage2D(
    gl.TEXTURE_2D,
    0,
    gl.R8,
    1,
    1,
    0,
    gl.RED,
    gl.UNSIGNED_BYTE,
    new Uint8Array([0]),
  );
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
  gl.uniform1i(uLoc.glyphMask, 0);

  // 使い回しバッファ。MAX_ORBS × 4 vec4 1 軸ぶんずつ。
  const colorBuf = new Float32Array(MAX_ORBS * 4);
  const phaseBuf = new Float32Array(MAX_ORBS * 4);
  const miscBuf = new Float32Array(MAX_ORBS * 4);
  const rotBuf = new Float32Array(MAX_ORBS * 4);

  // blend は使わない（fragment 内で per-orb Source-Over を完結させるため、
  // GL の blend で重ねると 2 重ブレンドになる）。
  gl.disable(gl.BLEND);

  let curWidth = 0;
  let curHeight = 0;

  function setResolution(width: number, height: number): void {
    curWidth = width;
    curHeight = height;
    gl!.viewport(0, 0, width, height);
    gl!.uniform2f(uLoc.resolution, width, height);
  }

  function setRenderData(data: Float32Array): void {
    if (data.length < HEADER_WORDS) {
      throw new Error(`render data too short: ${data.length} < ${HEADER_WORDS}`);
    }
    const bgR = data[0];
    const bgG = data[1];
    const bgB = data[2];
    const bgA = data[3];
    const baseRadius = data[4];
    const baseBlur = data[5];
    const directionId = data[6];
    const cycle = data[7];
    const nOrbs = data[8] | 0;
    // Phase B (#55): header[9] = alpha_mul, header[10] = shape_id (0=Circle, 1=Glyph)。
    const alphaMul = data[9];
    const shapeId = data[10] | 0;

    if (nOrbs > MAX_ORBS) {
      throw new Error(`n_orbs ${nOrbs} exceeds MAX_ORBS=${MAX_ORBS}`);
    }
    const expected = HEADER_WORDS + PER_ORB_WORDS * nOrbs;
    if (data.length < expected) {
      throw new Error(
        `render data length mismatch: got ${data.length}, expected at least ${expected}`,
      );
    }

    gl!.uniform4f(uLoc.bg, bgR, bgG, bgB, bgA);
    gl!.uniform1f(uLoc.baseRadius, baseRadius);
    gl!.uniform1f(uLoc.baseBlur, baseBlur);
    gl!.uniform1f(uLoc.direction, directionId);
    gl!.uniform1f(uLoc.cycle, cycle);
    gl!.uniform1i(uLoc.nOrbs, nOrbs);
    gl!.uniform1f(uLoc.alphaMul, alphaMul);
    gl!.uniform1i(uLoc.shapeId, shapeId);

    // per-orb を 3 本の vec4 配列に詰め直す。余り (i >= nOrbs) は 0 詰め
    // のままで shader 側で `i >= u_n_orbs` ガードしているので使われない。
    colorBuf.fill(0);
    phaseBuf.fill(0);
    miscBuf.fill(0);
    rotBuf.fill(0);
    for (let i = 0; i < nOrbs; i++) {
      const off = HEADER_WORDS + PER_ORB_WORDS * i;
      // color rgb + weight
      colorBuf[i * 4 + 0] = data[off + 0];
      colorBuf[i * 4 + 1] = data[off + 1];
      colorBuf[i * 4 + 2] = data[off + 2];
      colorBuf[i * 4 + 3] = data[off + 3];
      // phase / phi_radius / phi_blur / phi_opacity
      phaseBuf[i * 4 + 0] = data[off + 4];
      phaseBuf[i * 4 + 1] = data[off + 5];
      phaseBuf[i * 4 + 2] = data[off + 6];
      phaseBuf[i * 4 + 3] = data[off + 7];
      // cross_axis / style_bit / speed_mult / _
      miscBuf[i * 4 + 0] = data[off + 8];
      miscBuf[i * 4 + 1] = data[off + 9];
      miscBuf[i * 4 + 2] = data[off + 10];
      miscBuf[i * 4 + 3] = 0;
      rotBuf[i * 4 + 0] = data[off + 11];
      rotBuf[i * 4 + 1] = data[off + 12];
      rotBuf[i * 4 + 2] = 0;
      rotBuf[i * 4 + 3] = 0;
    }
    gl!.uniform4fv(uLoc.orbColor, colorBuf);
    gl!.uniform4fv(uLoc.orbPhase, phaseBuf);
    gl!.uniform4fv(uLoc.orbMisc, miscBuf);
    gl!.uniform4fv(uLoc.orbRot, rotBuf);
  }

  function setGlyphSdf(mask: Uint8Array, size: number): void {
    if (mask.length !== size * size) {
      throw new Error(
        `glyph sdf length mismatch: got ${mask.length}, expected ${size * size}`,
      );
    }
    gl!.activeTexture(gl!.TEXTURE0);
    gl!.bindTexture(gl!.TEXTURE_2D, glyphTex);
    // R8 / RED 1 ch でアップロード。`UNPACK_FLIP_Y_WEBGL` は default false で
    // OK。CPU 経路 (render_glyph_sdf) は左上原点で行優先に書き出している
    // ので、shader 側の UV (top-left = (0,0)) と一致する。
    gl!.pixelStorei(gl!.UNPACK_ALIGNMENT, 1);
    gl!.texImage2D(
      gl!.TEXTURE_2D,
      0,
      gl!.R8,
      size,
      size,
      0,
      gl!.RED,
      gl!.UNSIGNED_BYTE,
      mask,
    );
  }

  function renderFrame(t: number): void {
    if (curWidth === 0 || curHeight === 0) {
      throw new Error('setResolution must be called before renderFrame');
    }
    gl!.uniform1f(uLoc.t, t);
    // 背景は shader 内で u_bg を書き出すので clear 不要。
    gl!.drawArrays(gl!.TRIANGLES, 0, 3);
  }

  function dispose(): void {
    gl!.deleteTexture(glyphTex);
    gl!.deleteBuffer(vbo);
    gl!.deleteVertexArray(vao);
    gl!.deleteProgram(prog);
  }

  return { setResolution, setRenderData, setGlyphSdf, renderFrame, dispose };
}
