/** @type {import('tailwindcss').Config} */
export default {
  content: ['./src/**/*.{astro,html,js,jsx,ts,tsx,vue,svelte}'],
  theme: {
    extend: {
      colors: {
        // DESIGN.md §2 — black-canvas gothic, no accent hue
        bg: '#000000',
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
