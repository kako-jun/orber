// orber#192 — encodeAlphaVideoWasm + movMuxer の単体テスト。
//
// ffmpeg.wasm は撤去済み (JS-only MOV muxer に置換)。テストも mock 経由の
// 引数検証ではなく、出力 MOV の byte-level 構造を直接読み解く方式に切り替えた。

import { beforeEach, describe, expect, it } from 'vitest';

// ---- MOV parser helpers ---------------------------------------------------

function asciiAt(buf: Uint8Array, off: number, len = 4): string {
  let s = '';
  for (let i = 0; i < len; i++) s += String.fromCharCode(buf[off + i]);
  return s;
}
function u16At(buf: Uint8Array, off: number): number {
  return (buf[off] << 8) | buf[off + 1];
}
function u32At(buf: Uint8Array, off: number): number {
  // unsigned right shift で 32-bit unsigned 化
  return (
    ((buf[off] << 24) |
      (buf[off + 1] << 16) |
      (buf[off + 2] << 8) |
      buf[off + 3]) >>>
    0
  );
}

// box (atom) tree を再帰的に index する。同じ名前 type の box が複数あっても
// 各 lookup は先頭一致でよい (今回の MOV は各 box 1 つずつ)。
interface Box {
  type: string;
  start: number;
  size: number;
  payloadStart: number;
  payloadEnd: number;
}
function listBoxes(buf: Uint8Array, start: number, end: number): Box[] {
  const out: Box[] = [];
  let off = start;
  while (off + 8 <= end) {
    const size = u32At(buf, off);
    const type = asciiAt(buf, off + 4);
    if (size < 8 || off + size > end) break;
    out.push({
      type,
      start: off,
      size,
      payloadStart: off + 8,
      payloadEnd: off + size,
    });
    off += size;
  }
  return out;
}
function findBox(boxes: Box[], type: string): Box {
  const b = boxes.find((x) => x.type === type);
  if (!b) throw new Error(`box not found: ${type}`);
  return b;
}
function childrenOf(buf: Uint8Array, b: Box): Box[] {
  return listBoxes(buf, b.payloadStart, b.payloadEnd);
}

// ---- test data builder ----------------------------------------------------

// PNG signature を頭につけたダミー frame。muxer は中身を解釈しないので
// signature を入れなくても動くが、復号 round-trip を視覚的に分かりやすくする。
const PNG_SIG = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
function makeFrame(byte: number, payloadSize = 16): Uint8Array {
  const f = new Uint8Array(PNG_SIG.length + payloadSize);
  f.set(PNG_SIG, 0);
  for (let i = 0; i < payloadSize; i++) f[PNG_SIG.length + i] = byte;
  return f;
}

beforeEach(() => {
  // 各テストでモジュールをフレッシュに読み直す (状態を持たない実装なので形式的)。
  // 必要に応じてここで vi.resetModules() を呼ぶこともできる。
});

describe('encodeAnimationAlphaWasm', () => {
  it('1 フレーム入力で video/quicktime (MOV) の Blob を返す (正常系)', async () => {
    const { encodeAnimationAlphaWasm } = await import('./encodeAlphaVideoWasm');
    const blob = await encodeAnimationAlphaWasm([makeFrame(1)], 32, 32);
    expect(blob).toBeInstanceOf(Blob);
    expect(blob.type).toBe('video/quicktime');
    expect(blob.size).toBeGreaterThan(0);
  });

  it('frames.length === 0 では Error を投げる', async () => {
    const mod = await import('./encodeAlphaVideoWasm');
    await expect(mod.encodeAnimationAlphaWasm([], 16, 16)).rejects.toThrow(
      /frames must be > 0/,
    );
  });

  it('onProgress は開始 (0, total) と完了 (total, total) の 2 回呼ばれる', async () => {
    const { encodeAnimationAlphaWasm } = await import('./encodeAlphaVideoWasm');
    const seen: Array<[number, number]> = [];
    await encodeAnimationAlphaWasm(
      [makeFrame(1), makeFrame(2), makeFrame(3)],
      16,
      16,
      24,
      (f, t) => seen.push([f, t]),
    );
    expect(seen).toEqual([
      [0, 3],
      [3, 3],
    ]);
  });

  it('onProgress 未指定でも throw しない', async () => {
    const { encodeAnimationAlphaWasm } = await import('./encodeAlphaVideoWasm');
    await expect(
      encodeAnimationAlphaWasm([makeFrame(1)], 16, 16),
    ).resolves.toBeInstanceOf(Blob);
  });
});

describe('movMuxer (byte-level)', () => {
  it('ftyp は brand "qt  " で始まる', async () => {
    const { muxPngFramesToMov } = await import('./movMuxer');
    const mov = muxPngFramesToMov([makeFrame(1)], 32, 64, 24);
    const top = listBoxes(mov, 0, mov.length);
    const ftyp = findBox(top, 'ftyp');
    expect(ftyp.start).toBe(0);
    expect(ftyp.size).toBe(20);
    expect(asciiAt(mov, ftyp.payloadStart, 4)).toBe('qt  '); // major brand
    expect(u32At(mov, ftyp.payloadStart + 4)).toBe(0x00000200); // minor version
    expect(asciiAt(mov, ftyp.payloadStart + 8, 4)).toBe('qt  '); // compat brand
  });

  it('top-level atom 順序は ftyp → moov → mdat', async () => {
    const { muxPngFramesToMov } = await import('./movMuxer');
    const mov = muxPngFramesToMov([makeFrame(1), makeFrame(2)], 32, 64, 24);
    const top = listBoxes(mov, 0, mov.length);
    expect(top.map((b) => b.type)).toEqual(['ftyp', 'moov', 'mdat']);
  });

  it('mvhd の time_scale は fps、duration は frame 数', async () => {
    const { muxPngFramesToMov } = await import('./movMuxer');
    const N = 5;
    const fps = 30;
    const mov = muxPngFramesToMov(
      Array.from({ length: N }, (_, i) => makeFrame(i + 1)),
      64,
      32,
      fps,
    );
    const moov = findBox(listBoxes(mov, 0, mov.length), 'moov');
    const mvhd = findBox(childrenOf(mov, moov), 'mvhd');
    // version+flags (4) + creation (4) + modification (4) → timescale at +12
    expect(u32At(mov, mvhd.payloadStart + 12)).toBe(fps);
    expect(u32At(mov, mvhd.payloadStart + 16)).toBe(N);
  });

  it('tkhd の track_width / track_height は 16.16 fixed で与えた width × height', async () => {
    const { muxPngFramesToMov } = await import('./movMuxer');
    const W = 540;
    const H = 960;
    const mov = muxPngFramesToMov([makeFrame(1)], W, H, 24);
    const top = listBoxes(mov, 0, mov.length);
    const moov = findBox(top, 'moov');
    const trak = findBox(childrenOf(mov, moov), 'trak');
    const tkhd = findBox(childrenOf(mov, trak), 'tkhd');
    // 末尾 8 byte = track_width(4) + track_height(4)
    const wOff = tkhd.payloadEnd - 8;
    expect(u32At(mov, wOff) >>> 16).toBe(W);
    expect(u32At(mov, wOff + 4) >>> 16).toBe(H);
  });

  it('hdlr の component_subtype は "vide"', async () => {
    const { muxPngFramesToMov } = await import('./movMuxer');
    const mov = muxPngFramesToMov([makeFrame(1)], 16, 16, 24);
    const top = listBoxes(mov, 0, mov.length);
    const moov = findBox(top, 'moov');
    const trak = findBox(childrenOf(mov, moov), 'trak');
    const mdia = findBox(childrenOf(mov, trak), 'mdia');
    const hdlr = findBox(childrenOf(mov, mdia), 'hdlr');
    // payload: ver_flags(4) + component_type(4) + component_subtype(4)
    expect(asciiAt(mov, hdlr.payloadStart + 4, 4)).toBe('mhlr');
    expect(asciiAt(mov, hdlr.payloadStart + 8, 4)).toBe('vide');
  });

  it('stsd のサンプル記述は PNG codec (rgba, depth=32, color_table=-1)', async () => {
    const { muxPngFramesToMov } = await import('./movMuxer');
    const W = 128;
    const H = 96;
    const mov = muxPngFramesToMov([makeFrame(1)], W, H, 24);
    const top = listBoxes(mov, 0, mov.length);
    const moov = findBox(top, 'moov');
    const trak = findBox(childrenOf(mov, moov), 'trak');
    const mdia = findBox(childrenOf(mov, trak), 'mdia');
    const minf = findBox(childrenOf(mov, mdia), 'minf');
    const stbl = findBox(childrenOf(mov, minf), 'stbl');
    const stsd = findBox(childrenOf(mov, stbl), 'stsd');
    // payload: ver_flags(4) + entry_count(4) → 各エントリは entry_size(4) + 'png '(4) ...
    expect(u32At(mov, stsd.payloadStart + 4)).toBe(1);
    const entryStart = stsd.payloadStart + 8;
    expect(u32At(mov, entryStart)).toBe(86); // entry size
    expect(asciiAt(mov, entryStart + 4, 4)).toBe('png ');
    // width / height: entryStart + 32, +34 (16 header + reserved(6) + dri(2) + ver(2)+rev(2)+vendor(4)+tq(4)+sq(4) = 24 → +8 header → +32 absolute)
    expect(u16At(mov, entryStart + 32)).toBe(W);
    expect(u16At(mov, entryStart + 34)).toBe(H);
    // depth (2) at entryStart + 82 (= 86 - 4)
    expect(u16At(mov, entryStart + 82)).toBe(32);
    // color_table_id (signed -1) = 0xFFFF
    expect(u16At(mov, entryStart + 84)).toBe(0xffff);
  });

  it('stts は (sample_count=N, sample_delta=1) 1 entry', async () => {
    const { muxPngFramesToMov } = await import('./movMuxer');
    const N = 7;
    const mov = muxPngFramesToMov(
      Array.from({ length: N }, () => makeFrame(1)),
      16,
      16,
      24,
    );
    const top = listBoxes(mov, 0, mov.length);
    const moov = findBox(top, 'moov');
    const trak = findBox(childrenOf(mov, moov), 'trak');
    const mdia = findBox(childrenOf(mov, trak), 'mdia');
    const minf = findBox(childrenOf(mov, mdia), 'minf');
    const stbl = findBox(childrenOf(mov, minf), 'stbl');
    const stts = findBox(childrenOf(mov, stbl), 'stts');
    expect(u32At(mov, stts.payloadStart + 4)).toBe(1); // entry_count
    expect(u32At(mov, stts.payloadStart + 8)).toBe(N); // sample_count
    expect(u32At(mov, stts.payloadStart + 12)).toBe(1); // sample_delta
  });

  it('stsz の各 sample size は入力 PNG 長と一致する', async () => {
    const { muxPngFramesToMov } = await import('./movMuxer');
    const frames = [makeFrame(1, 8), makeFrame(2, 16), makeFrame(3, 32)];
    const mov = muxPngFramesToMov(frames, 16, 16, 24);
    const top = listBoxes(mov, 0, mov.length);
    const moov = findBox(top, 'moov');
    const trak = findBox(childrenOf(mov, moov), 'trak');
    const mdia = findBox(childrenOf(mov, trak), 'mdia');
    const minf = findBox(childrenOf(mov, mdia), 'minf');
    const stbl = findBox(childrenOf(mov, minf), 'stbl');
    const stsz = findBox(childrenOf(mov, stbl), 'stsz');
    // payload: ver_flags(4) + sample_size(4=0) + sample_count(4) + N×size(4)
    expect(u32At(mov, stsz.payloadStart + 4)).toBe(0);
    expect(u32At(mov, stsz.payloadStart + 8)).toBe(frames.length);
    for (let i = 0; i < frames.length; i++) {
      expect(u32At(mov, stsz.payloadStart + 12 + i * 4)).toBe(frames[i].length);
    }
  });

  it('stco の chunk offset は mdat の data 先頭を指す (= file 内で frame bytes と一致)', async () => {
    const { muxPngFramesToMov } = await import('./movMuxer');
    const frames = [makeFrame(0xaa, 8), makeFrame(0xbb, 12), makeFrame(0xcc, 4)];
    const mov = muxPngFramesToMov(frames, 16, 16, 24);
    const top = listBoxes(mov, 0, mov.length);
    const moov = findBox(top, 'moov');
    const trak = findBox(childrenOf(mov, moov), 'trak');
    const mdia = findBox(childrenOf(mov, trak), 'mdia');
    const minf = findBox(childrenOf(mov, mdia), 'minf');
    const stbl = findBox(childrenOf(mov, minf), 'stbl');
    const stco = findBox(childrenOf(mov, stbl), 'stco');
    expect(u32At(mov, stco.payloadStart + 4)).toBe(1); // entry_count
    const dataOffset = u32At(mov, stco.payloadStart + 8);

    const mdat = findBox(top, 'mdat');
    expect(dataOffset).toBe(mdat.payloadStart);

    // mdat data 先頭の 8 byte は frame[0] の先頭 (PNG signature) と一致
    for (let i = 0; i < PNG_SIG.length; i++) {
      expect(mov[dataOffset + i]).toBe(PNG_SIG[i]);
    }
  });

  it('mdat には全 frame bytes が順番通り連結される', async () => {
    const { muxPngFramesToMov } = await import('./movMuxer');
    const frames = [makeFrame(0x11, 4), makeFrame(0x22, 6), makeFrame(0x33, 8)];
    const mov = muxPngFramesToMov(frames, 16, 16, 24);
    const top = listBoxes(mov, 0, mov.length);
    const mdat = findBox(top, 'mdat');
    let off = mdat.payloadStart;
    for (const f of frames) {
      for (let i = 0; i < f.length; i++) {
        expect(mov[off + i]).toBe(f[i]);
      }
      off += f.length;
    }
    expect(off).toBe(mdat.payloadEnd);
  });

  it('総ファイルサイズは 583 + 4N + Σ(frame size) (#192 設計式)', async () => {
    const { muxPngFramesToMov } = await import('./movMuxer');
    const frames = [makeFrame(1, 10), makeFrame(2, 20), makeFrame(3, 30)];
    const mov = muxPngFramesToMov(frames, 32, 32, 24);
    const sum = frames.reduce((s, f) => s + f.length, 0);
    expect(mov.length).toBe(583 + 4 * frames.length + sum);
  });

  it('width 0 や負値は早期 reject', async () => {
    const { muxPngFramesToMov } = await import('./movMuxer');
    expect(() => muxPngFramesToMov([makeFrame(1)], 0, 16, 24)).toThrow(
      /invalid dimensions/,
    );
    expect(() => muxPngFramesToMov([makeFrame(1)], 16, 0, 24)).toThrow(
      /invalid dimensions/,
    );
  });

  it('fps 0 / 負値は早期 reject', async () => {
    const { muxPngFramesToMov } = await import('./movMuxer');
    expect(() => muxPngFramesToMov([makeFrame(1)], 16, 16, 0)).toThrow(
      /invalid fps/,
    );
  });

  it('frames === 0 は早期 reject', async () => {
    const { muxPngFramesToMov } = await import('./movMuxer');
    expect(() => muxPngFramesToMov([], 16, 16, 24)).toThrow(/frames must be > 0/);
  });

  it('192 frame (実運用想定) でも各 atom が走査でき、サイズ整合', async () => {
    const { muxPngFramesToMov } = await import('./movMuxer');
    const N = 192;
    const frames = Array.from({ length: N }, (_, i) => makeFrame(i & 0xff, 64));
    const mov = muxPngFramesToMov(frames, 540, 960, 24);
    const top = listBoxes(mov, 0, mov.length);
    expect(top.map((b) => b.type)).toEqual(['ftyp', 'moov', 'mdat']);
    const moov = findBox(top, 'moov');
    expect(moov.size).toBe(555 + 4 * N);
    const mdat = findBox(top, 'mdat');
    expect(mdat.size).toBe(8 + N * (PNG_SIG.length + 64));
  });
});
