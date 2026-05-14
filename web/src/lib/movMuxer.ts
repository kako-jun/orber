// orber#192 — JS-only MOV/QuickTime muxer for PNG-codec video.
//
// 役割: 透過 PNG フレーム列を MOV container に詰めるだけ。実エンコードは
// 行わない (各 sample = PNG bytes をそのまま mdat に並べる)。これにより
// ffmpeg.wasm (~30MB) への依存を完全に外せる。
//
// MOV (QuickTime File Format) は ISO/IEC 14496-12 ベースの atom (box) tree。
// PNG codec の video track には次の atom を含める:
//
//   ftyp ('qt  ')
//   moov
//     mvhd               // movie header
//     trak
//       tkhd             // track header (width / height)
//       mdia
//         mdhd           // media header (timescale = fps, duration = N)
//         hdlr ('vide')  // handler (video)
//         minf
//           vmhd         // video media header
//           dinf > dref  // self-reference
//           stbl
//             stsd       // sample description ('png ' codec、depth=32, color_table=-1)
//             stts       // sample timing (1 entry: N samples, delta=1)
//             stsc       // sample-to-chunk (1 entry: chunk1, N samples/chunk)
//             stsz       // sample sizes (variable, N entries = PNG byte counts)
//             stco       // chunk offset (1 entry: absolute mdat data offset)
//   mdat                 // PNG bytes concatenated
//
// `stss` (sync sample atom) は **意図的に省略**。QuickTime spec: 「stss が
// 存在しない場合、全 sample が sync sample とみなされる」。PNG codec は frame
// 間予測なしで全 frame keyframe なので、stss を書かないのが ffmpeg movenc.c の
// 慣例でもある。出力ファイルサイズも僅かに小さい。
//
// 単一 chunk 戦略: stsc 1 entry / stco 1 entry にして N sample を 1 chunk にまとめる。
// stsz の各 sample size を累積することで decoder は各 sample 位置を導出する。
// ffmpeg 出力は per-sample chunk (N chunks) を使うが、player 互換性は同等で
// atom tree がコンパクトになる。

// 各 atom のサイズは N (フレーム数) のみに依存するため、moov の総サイズと
// mdat の data offset は事前に計算できる。これにより stco の chunk offset を
// 後付け patch ではなく直接書き込める。
//
// サイズ計算 (atom ヘッダ 8 = size(4) + type(4) を含む):
//   ftyp: 20             (size + 'ftyp' + major + minor + 1 compat brand)
//   mvhd: 108            (8 + 100 body)
//   tkhd: 92             (8 + 84 body)
//   mdhd: 32             (8 + 24 body)
//   hdlr: 33             (8 + 25 body; component_name は length 0 の Pascal string)
//   vmhd: 20             (8 + 12 body)
//   dref: 28             (8 + 4 ver_flags + 4 entry_count + 12 url entry)
//   dinf: 36             (8 + dref)
//   stsd: 102            (8 + 4 ver_flags + 4 entry_count + 86 PNG sample desc)
//   stts: 24             (8 + 4 + 4 + 8)
//   stsc: 28             (8 + 4 + 4 + 12)
//   stsz: 20 + 4N        (8 + 12 header + 4 × N)
//   stco: 20             (8 + 4 + 4 + 4)
//   stbl: 202 + 4N       (8 + stsd + stts + stsc + stsz + stco)
//   minf: 266 + 4N       (8 + vmhd + dinf + stbl)
//   mdia: 339 + 4N       (8 + mdhd + hdlr + minf)
//   trak: 439 + 4N       (8 + tkhd + mdia)
//   moov: 555 + 4N       (8 + mvhd + trak)
//   mdat: 8 + S          (S = Σ PNG bytes)
//   total: 583 + 4N + S
//
// dataOffset (first PNG byte の絶対位置) = 20 + (555 + 4N) + 8 = 583 + 4N

export function muxPngFramesToMov(
  frames: Uint8Array[],
  width: number,
  height: number,
  fps: number,
): Uint8Array {
  if (frames.length === 0) {
    throw new Error(`frames must be > 0, got ${frames.length}`);
  }
  if (width <= 0 || width > 0xffff || height <= 0 || height > 0xffff) {
    throw new Error(`invalid dimensions: ${width}x${height}`);
  }
  if (fps <= 0 || fps > 0x7fffffff) {
    throw new Error(`invalid fps: ${fps}`);
  }

  const N = frames.length;
  let mdatBodySize = 0;
  for (const f of frames) mdatBodySize += f.length;

  const moovSize = 555 + 4 * N;
  const dataOffset = 20 + moovSize + 8; // = 583 + 4N
  const total = 20 + moovSize + 8 + mdatBodySize;
  if (total > 0xffffffff) {
    // 32-bit size fields / chunk offsets が overflow する規模。これ 1 つで
    // (a) `mdat` の size 4-byte 上限、(b) `stco` の chunk_offset 4-byte 上限の
    // 両方を同時に保護する (mdat の絶対位置は total 未満なので)。orber は
    // 解像度 540x960 × 192 frame × 透過 PNG ≒ 60-70MB が想定で、本番では
    // 触れない領域。万一来たら早期に弾く。
    throw new Error(`output too large for 32-bit MOV (${total} bytes)`);
  }

  const out = new Uint8Array(total);
  const w = new Writer(out);

  // ---- ftyp ---------------------------------------------------------------
  w.beginBox('ftyp');
  w.str4('qt  ');         // major brand
  w.u32(0x00000200);      // minor version (QuickTime convention)
  w.str4('qt  ');         // compatible brand
  w.endBox(20);

  // ---- moov ---------------------------------------------------------------
  w.beginBox('moov');

  //   mvhd
  w.beginBox('mvhd');
  w.u32(0);               // version (0) + flags (0)
  w.u32(0);               // creation_time
  w.u32(0);               // modification_time
  w.u32(fps);             // time_scale
  w.u32(N);               // duration (in time_scale units; N samples × 1)
  w.u32(0x00010000);      // preferred_rate (1.0)
  w.u16(0x0100);          // preferred_volume (1.0)
  w.zeros(10);            // reserved
  w.matrixIdentity();     // 9 × 32-bit fixed-point matrix
  w.zeros(24);            // preview / poster / selection / current_time
  w.u32(2);               // next_track_id
  w.endBox(108);

  //   trak
  w.beginBox('trak');

  //     tkhd
  w.beginBox('tkhd');
  w.u32(0x00000007);      // version 0 + flags (enabled | inMovie | inPreview)
  w.u32(0);               // creation_time
  w.u32(0);               // modification_time
  w.u32(1);               // track_id
  w.u32(0);               // reserved
  w.u32(N);               // duration (in mvhd timescale; we use fps for both)
  w.zeros(8);             // reserved
  w.u16(0);               // layer
  w.u16(0);               // alternate_group
  w.u16(0);               // volume (0 for video)
  w.u16(0);               // reserved
  w.matrixIdentity();
  w.u32(width << 16);     // track width (16.16 fixed)
  w.u32(height << 16);    // track height (16.16 fixed)
  w.endBox(92);

  //     mdia
  w.beginBox('mdia');

  //       mdhd
  w.beginBox('mdhd');
  w.u32(0);               // version + flags
  w.u32(0);               // creation_time
  w.u32(0);               // modification_time
  w.u32(fps);             // time_scale (media)
  w.u32(N);               // duration (in media timescale)
  w.u16(0x55c4);          // language ('und'、ISO 639-2 packed: 'u'<<10|'n'<<5|'d' 各文字 -0x60)
  w.u16(0);               // quality
  w.endBox(32);

  //       hdlr (video)
  w.beginBox('hdlr');
  w.u32(0);               // version + flags
  w.str4('mhlr');         // component_type (QuickTime: 'mhlr' for media handler)
  w.str4('vide');         // component_subtype
  w.u32(0);               // component_manufacturer
  w.u32(0);               // component_flags
  w.u32(0);               // component_flags_mask
  w.u8(0);                // component_name (Pascal string length = 0)
  w.endBox(33);

  //       minf
  w.beginBox('minf');

  //         vmhd
  w.beginBox('vmhd');
  w.u32(0x00000001);      // version 0 + flags (no_lean_ahead = 1)
  w.u16(0);               // graphics_mode (copy)
  w.u16(0); w.u16(0); w.u16(0); // op_color (r, g, b)
  w.endBox(20);

  //         dinf
  w.beginBox('dinf');
  //           dref
  w.beginBox('dref');
  w.u32(0);               // version + flags
  w.u32(1);               // entry_count
  // url entry: self-contained
  w.u32(12);              // entry size
  w.str4('url ');
  w.u32(0x00000001);      // version 0 + flags (self_contained = 1; no URL string)
  w.endBox(28);
  w.endBox(36);

  //         stbl
  w.beginBox('stbl');

  //           stsd (sample description: PNG)
  w.beginBox('stsd');
  w.u32(0);               // version + flags
  w.u32(1);               // entry_count
  // PNG sample description entry (QuickTime image desc, 86 bytes)
  w.u32(86);              // entry size
  w.str4('png ');         // codec FourCC
  w.zeros(6);             // reserved
  w.u16(1);               // data_reference_index
  // QuickTime video sample description extension
  w.u16(0);               // version
  w.u16(0);               // revision_level
  w.u32(0);               // vendor
  w.u32(0);               // temporal_quality
  w.u32(512);             // spatial_quality (lossless = 0x200)
  w.u16(width);
  w.u16(height);
  w.u32(0x00480000);      // horizontal resolution (72 dpi as 16.16)
  w.u32(0x00480000);      // vertical resolution
  w.u32(0);               // data_size
  w.u16(1);               // frame_count (per sample)
  // compressor_name (32-byte field): Pascal string "PNG" zero-padded.
  w.u8(3); w.str('PNG'); w.zeros(28);
  w.u16(32);              // depth (RGBA)
  w.u16(0xffff);          // color_table_id (-1 = no color table)
  w.endBox(102);

  //           stts (sample timing)
  w.beginBox('stts');
  w.u32(0);               // version + flags
  w.u32(1);               // entry_count
  w.u32(N);               // sample_count
  w.u32(1);               // sample_delta (1 media-tick per frame)
  w.endBox(24);

  //           stsc (sample-to-chunk)
  w.beginBox('stsc');
  w.u32(0);               // version + flags
  w.u32(1);               // entry_count
  w.u32(1);               // first_chunk
  w.u32(N);               // samples_per_chunk
  w.u32(1);               // sample_description_index
  w.endBox(28);

  //           stsz (sample sizes)
  w.beginBox('stsz');
  w.u32(0);               // version + flags
  w.u32(0);               // sample_size (0 = variable; per-sample table follows)
  w.u32(N);               // sample_count
  for (const f of frames) w.u32(f.length);
  w.endBox(20 + 4 * N);

  //           stco (chunk offset; 1 entry)
  w.beginBox('stco');
  w.u32(0);               // version + flags
  w.u32(1);               // entry_count
  w.u32(dataOffset);      // chunk_offset (絶対 file offset → mdat data 先頭)
  w.endBox(20);

  // (stss は省略 — 全 frame keyframe → 暗黙 sync。ffmpeg movenc も同様)

  w.endBox(202 + 4 * N);   // stbl
  w.endBox(266 + 4 * N);   // minf
  w.endBox(339 + 4 * N);   // mdia
  w.endBox(439 + 4 * N);   // trak
  w.endBox(moovSize);      // moov

  // ---- mdat ---------------------------------------------------------------
  w.u32(8 + mdatBodySize);
  w.str4('mdat');
  for (const f of frames) w.bytes(f);

  return out;
}

// Atom サイズは事前計算済みなので Writer は単純な前進ポインタで十分。
// `beginBox` / `endBox` は型 FourCC を書き込みつつ、書き終わり時に事前計算
// サイズと実際の前進量が一致しているかを assert する (実装ミスを早期発見)。
class Writer {
  private buf: Uint8Array;
  private pos = 0;
  private boxStack: { type: string; start: number }[] = [];

  constructor(buf: Uint8Array) {
    this.buf = buf;
  }

  u8(n: number): void {
    this.buf[this.pos++] = n & 0xff;
  }
  u16(n: number): void {
    this.buf[this.pos++] = (n >>> 8) & 0xff;
    this.buf[this.pos++] = n & 0xff;
  }
  u32(n: number): void {
    this.buf[this.pos++] = (n >>> 24) & 0xff;
    this.buf[this.pos++] = (n >>> 16) & 0xff;
    this.buf[this.pos++] = (n >>> 8) & 0xff;
    this.buf[this.pos++] = n & 0xff;
  }
  str4(s: string): void {
    if (s.length !== 4) throw new Error(`str4 expects length 4, got "${s}"`);
    for (let i = 0; i < 4; i++) this.buf[this.pos++] = s.charCodeAt(i) & 0xff;
  }
  str(s: string): void {
    for (let i = 0; i < s.length; i++) this.buf[this.pos++] = s.charCodeAt(i) & 0xff;
  }
  zeros(n: number): void {
    // buf is zero-initialized; just advance.
    this.pos += n;
  }
  bytes(b: Uint8Array): void {
    this.buf.set(b, this.pos);
    this.pos += b.length;
  }
  matrixIdentity(): void {
    // QuickTime matrix: 3×3 of 16.16 fixed except column 3 which is 2.30 fixed.
    // Identity: { 0x10000, 0, 0, 0, 0x10000, 0, 0, 0, 0x40000000 }.
    this.u32(0x00010000); this.u32(0); this.u32(0);
    this.u32(0); this.u32(0x00010000); this.u32(0);
    this.u32(0); this.u32(0); this.u32(0x40000000);
  }

  beginBox(type: string): void {
    this.boxStack.push({ type, start: this.pos });
    this.u32(0);          // size placeholder
    this.str4(type);
  }
  endBox(expectedSize: number): void {
    const top = this.boxStack.pop();
    if (!top) throw new Error('endBox without matching beginBox');
    const written = this.pos - top.start;
    if (written !== expectedSize) {
      throw new Error(
        `box ${top.type} size mismatch: expected ${expectedSize}, wrote ${written}`,
      );
    }
    // Patch the size prefix in-place.
    this.buf[top.start] = (expectedSize >>> 24) & 0xff;
    this.buf[top.start + 1] = (expectedSize >>> 16) & 0xff;
    this.buf[top.start + 2] = (expectedSize >>> 8) & 0xff;
    this.buf[top.start + 3] = expectedSize & 0xff;
  }
}
