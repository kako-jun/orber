/** @type {import('tailwindcss').Config} */
export default {
  content: ['./src/**/*.{astro,html,js,jsx,ts,tsx,vue,svelte}'],
  theme: {
    extend: {
      colors: {
        // DESIGN.md §2 — black-canvas gothic, no accent hue.
        // #126: 厳密な #000000 ではなく orber.png 右上 1px の実測値 #040404 に揃え、
        // PWA splash / theme-color / アイコン背景全てを単一値に集約 (SOT)。
        // 4 階調差は人間の目には不可視だが、OLED で icon を貼った時の境目を消す。
        bg: '#040404',
        fg: '#FFFFFF',
        fgMuted: 'rgba(255,255,255,0.55)',
        fgSubtle: 'rgba(255,255,255,0.32)',
        hairline: 'rgba(255,255,255,0.12)',
        glassBg: 'rgba(255,255,255,0.06)',
        glassBgHover: 'rgba(255,255,255,0.10)',
        glassBorder: 'rgba(255,255,255,0.12)',
        focusRing: 'rgba(255,255,255,0.7)',
      },
      fontFamily: {
        display: ['"Space Grotesk"', 'system-ui', 'sans-serif'],
        // Latin は Space Grotesk に統一 (DESIGN.md §3 改訂、ロゴ専用ではなく
        // UI 全体に拡張)。CJK 文字は Space Grotesk に収録されていないので
        // OS の日本語フォントへ自動フォールバックさせる。
        sans: [
          '"Space Grotesk"',
          'system-ui',
          '-apple-system',
          '"Segoe UI"',
          '"Hiragino Sans"',
          '"Yu Gothic"',
          'Meiryo',
          'sans-serif',
        ],
      },
      letterSpacing: {
        // ロゴ用 — DESIGN.md §3 (0.4em)
        logo: '0.4em',
      },
      backdropBlur: {
        glass: '12px',
      },
      // DESIGN.md §6 motion uses Tailwind defaults `duration-200 ease-out`.
      // No theme keys are added here for them.
    },
  },
  plugins: [],
};
