// orber#112 — WebGL2 fragment shader による per-pixel orb 描画。
//
// `orber-wasm` の `get_render_data` で得た Float32Array をそのまま uniform に
// 流し、fragment shader 1 pass で全 orb の Source-Over 合成を行う。CPU 経路
// (`crates/core::animate::render_frame_with_params`) と同じ数式・同じ per-orb
// パラメータを使うので、視覚パリティは「最終的な見た目が同じ」が保たれる。
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
const MAX_ORBS = 64;

const HEADER_WORDS = 16;
const PER_ORB_WORDS = 16;

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
uniform float u_cycle;         // 1 or 2
uniform int u_n_orbs;

// per-orb uniforms (length MAX_ORBS = 64). Float で詰める。
uniform vec4 u_orb_color[${MAX_ORBS}];     // (r, g, b, weight)
uniform vec4 u_orb_phase[${MAX_ORBS}];     // (phase, phi_radius, phi_blur, phi_opacity)
uniform vec4 u_orb_misc[${MAX_ORBS}];      // (cross_axis, style_bit, speed_mult, _)

float clampf(float x, float a, float b) { return min(max(x, a), b); }

void main() {
  vec2 px = gl_FragCoord.xy;
  // gl_FragCoord は左下原点。CPU 経路は左上原点 (image::RgbaImage) なので
  // y を反転して合わせる。
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

    float weight = col.w;
    float phase = ph.x;
    float phi_radius = ph.y;
    float phi_blur = ph.z;
    float phi_opacity = ph.w;
    float cross_axis = misc.x;
    float style_bit = misc.y;       // 0=rim, 1=soft
    float speed_mult = misc.z;

    float r_pixels_max = u_base_radius * sqrt(max(weight, 0.0)) * BREATH_RADIUS_MAX_FACTOR;
    float r_normalized = (progress_axis > 0.0) ? (r_pixels_max / progress_axis) : 0.0;
    float extent = 1.0 + 2.0 * r_normalized;

    float advance_steps = fract(u_cycle * speed_mult * u_t);
    float raw = phase * extent + advance_steps * extent;
    // GLSL の mod() は負を出さない (mod(x, y) = x - y * floor(x/y))。Rust の
    // rem_euclid と一致するので、Rust 側と同じ pos が出る。
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
    float opacity = clampf(opacity_factor, 0.0, 1.0);

    float cx = nx * u_resolution.x;
    float cy = ny * u_resolution.y;

    float dist = distance(px, vec2(cx, cy));
    float dnorm = dist / radius;        // 0..1 が orb 内、>1 は外

    // alpha 計算 (rim / soft)。center_alpha = opacity, mid_alpha = opacity * 80/255。
    float alpha = 0.0;
    if (dnorm < 1.0) {
      float center_a = opacity;
      float mid_a = opacity * (80.0 / 255.0);
      if (style_bit < 0.5) {
        // rim: 3-stop
        float mid_stop = clampf(1.0 - blur * 0.8, 0.05, 0.95);
        if (dnorm <= mid_stop) {
          float u = (mid_stop > 0.0) ? (dnorm / mid_stop) : 1.0;
          alpha = mix(center_a, mid_a, u);
        } else {
          float denom = max(1.0 - mid_stop, 1e-6);
          float u = (dnorm - mid_stop) / denom;
          alpha = mix(mid_a, 0.0, u);
        }
      } else {
        // soft: 2-stop (center .. hold_stop は center_a 一定 → hold_stop..1 で 0 へ)
        float hold_stop = clampf(1.0 - blur, 0.05, 0.95);
        if (dnorm <= hold_stop) {
          alpha = center_a;
        } else {
          float denom = max(1.0 - hold_stop, 1e-6);
          float u = (dnorm - hold_stop) / denom;
          alpha = mix(center_a, 0.0, u);
        }
      }
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
    orbColor: gl.getUniformLocation(prog, 'u_orb_color'),
    orbPhase: gl.getUniformLocation(prog, 'u_orb_phase'),
    orbMisc: gl.getUniformLocation(prog, 'u_orb_misc'),
  };

  // 使い回しバッファ。MAX_ORBS × 4 vec4 1 軸ぶんずつ。
  const colorBuf = new Float32Array(MAX_ORBS * 4);
  const phaseBuf = new Float32Array(MAX_ORBS * 4);
  const miscBuf = new Float32Array(MAX_ORBS * 4);

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

    // per-orb を 3 本の vec4 配列に詰め直す。余り (i >= nOrbs) は 0 詰め
    // のままで shader 側で `i >= u_n_orbs` ガードしているので使われない。
    colorBuf.fill(0);
    phaseBuf.fill(0);
    miscBuf.fill(0);
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
    }
    gl!.uniform4fv(uLoc.orbColor, colorBuf);
    gl!.uniform4fv(uLoc.orbPhase, phaseBuf);
    gl!.uniform4fv(uLoc.orbMisc, miscBuf);
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
    gl!.deleteBuffer(vbo);
    gl!.deleteVertexArray(vao);
    gl!.deleteProgram(prog);
  }

  return { setResolution, setRenderData, renderFrame, dispose };
}
