// Mock-only approximation of core's space theme derivation (`crates/sta-core/src/theme.rs`,
// recipe in docs/research/arc_spec.md §5). The real colors always come from core via
// `UiState.spaces[].colors`, `UiState.themePresets[].colors` and `invoke('theme.colors')`;
// this module only exists so mock mode can answer `theme.colors` for arbitrary themes and so the
// hand-written fixtures have realistic values. No DOM access: importable from Node too.

/** Built-in presets (arc_spec §5 "Preset swatches"). */
export const PRESETS = [
  { name: 'Dusk', theme: { hue: 300, hue2: 340, chroma: 0.06 } },
  { name: 'Lagoon', theme: { hue: 190, hue2: 230, chroma: 0.06 } },
  { name: 'Ember', theme: { hue: 50, hue2: 20, chroma: 0.07 } },
  { name: 'Moss', theme: { hue: 130, hue2: 150, chroma: 0.05 } },
  { name: 'Slate', theme: { hue: 255, hue2: 255, chroma: 0.015 } },
  { name: 'Rose', theme: { hue: 10, hue2: 350, chroma: 0.06 } },
  { name: 'Sky', theme: { hue: 245, hue2: 275, chroma: 0.06 } },
  { name: 'Sand', theme: { hue: 80, hue2: 60, chroma: 0.05 } },
];

/** OKLCH → linear sRGB (Björn Ottosson's OKLab matrices). */
function oklchToLinearRgb(L, C, H) {
  const h = (H * Math.PI) / 180;
  const a = C * Math.cos(h);
  const b = C * Math.sin(h);
  const l = (L + 0.3963377774 * a + 0.2158037573 * b) ** 3;
  const m = (L - 0.1055613458 * a - 0.0638541728 * b) ** 3;
  const s = (L - 0.0894841775 * a - 1.291485548 * b) ** 3;
  return [
    4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
    -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
    -0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s,
  ];
}

const inGamut = (rgb) => rgb.every((c) => c >= -1e-4 && c <= 1 + 1e-4);

function toHex(rgb) {
  return (
    '#' +
    rgb
      .map((c) => {
        const x = Math.min(1, Math.max(0, c));
        const g = x <= 0.0031308 ? 12.92 * x : 1.055 * x ** (1 / 2.4) - 0.055;
        return Math.round(g * 255)
          .toString(16)
          .padStart(2, '0');
      })
      .join('')
  );
}

/** `#rrggbb` for an OKLCH color, reducing chroma until it fits the sRGB gamut. */
export function oklchHex(L, C, H) {
  let rgb = oklchToLinearRgb(L, C, H);
  if (!inGamut(rgb)) {
    let lo = 0;
    let hi = C;
    for (let i = 0; i < 24; i++) {
      const mid = (lo + hi) / 2;
      if (inGamut(oklchToLinearRgb(L, mid, H))) lo = mid;
      else hi = mid;
    }
    rgb = oklchToLinearRgb(L, lo, H);
  }
  return toHex(rgb);
}

const finite = (v, d) => (Number.isFinite(v) ? v : d);
const wrapHue = (h) => ((finite(h, 0) % 360) + 360) % 360;

/** Hue interpolation along the shortest arc. */
function mixHue(h1, h2, t) {
  let d = h2 - h1;
  if (d > 180) d -= 360;
  if (d < -180) d += 360;
  return wrapHue(h1 + d * t);
}

/**
 * ThemeColors for a `{hue, hue2, chroma}` theme, shaped exactly like the Rust `ThemeColors`.
 * @param {{hue:number, hue2:number, chroma:number}} theme
 * @param {boolean} dark
 */
export function deriveThemeColors(theme, dark) {
  const h1 = wrapHue(theme?.hue);
  const h2 = wrapHue(theme?.hue2);
  const userC = Math.min(0.08, Math.max(0, finite(theme?.chroma, 0.06)));
  const c = Math.min(userC, dark ? 0.06 : 0.07);
  const [l1, l2] = dark ? [0.25, 0.2] : [0.93, 0.89];
  const t = 0.6;
  const frame = oklchHex(l1 + (l2 - l1) * t, c, mixHue(h1, h2, t));
  const gradientStart = oklchHex(l1, c, h1);
  const gradientEnd = oklchHex(l2, c, h2);
  if (dark) {
    return {
      frame,
      gradientStart,
      gradientEnd,
      accent: oklchHex(0.74, 0.13, h1),
      text: '#f3f2f7',
      textMuted: 'rgba(243,242,247,0.62)',
      hover: 'rgba(255,255,255,0.07)',
      pressed: 'rgba(255,255,255,0.11)',
      activeRow: 'rgba(255,255,255,0.13)',
      divider: 'rgba(255,255,255,0.08)',
      surface: '#232228',
      border: 'rgba(255,255,255,0.10)',
    };
  }
  return {
    frame,
    gradientStart,
    gradientEnd,
    accent: oklchHex(0.58, 0.15, h1),
    text: '#1c1b20',
    textMuted: 'rgba(28,27,32,0.62)',
    hover: 'rgba(0,0,0,0.05)',
    pressed: 'rgba(0,0,0,0.09)',
    activeRow: 'rgba(255,255,255,0.78)',
    divider: 'rgba(0,0,0,0.08)',
    surface: '#ffffff',
    border: 'rgba(0,0,0,0.10)',
  };
}

/** `UiState.themePresets` for a mode. */
export function presetViews(dark) {
  return PRESETS.map((p) => ({ name: p.name, theme: { ...p.theme }, colors: deriveThemeColors(p.theme, dark) }));
}
