# Solstice site design

The site looks like the app: a dark studio, the icon's charcoal and blue, and the amber of the
cycle bar used sparingly. Dark is the default; a light scheme follows the visitor's system setting.
Tokens live as CSS variables in `style.css`; pages use the names, never raw values.

## Colour

| Token | Dark | Light | Role |
|---|---|---|---|
| `--bg` | `#141418` | `#f6f6f8` | Page background |
| `--surface` | `#1e1e23` | `#ffffff` | Bands, code blocks, cards |
| `--raised` | `#26262c` | `#ececf0` | Nav, hover fills, ruler |
| `--border` | `#3d3d45` | `#d6d6dd` | Lines and outlines |
| `--text` | `#ededf0` | `#1b1b20` | Body and headings |
| `--muted` | `#a3a3ad` | `#5c5c66` | Secondary text, captions |
| `--link` | `#62a0ea` | `#1c63c4` | Links |
| `--primary` | `#1c63c4` | `#1c63c4` | Primary buttons (white text, 5.6:1) |
| `--primary-hover` | `#3584e4` | `#3584e4` | Primary button hover |
| `--accent` | `#f5a524` | `#c47c00` | The cycle bar, focus rings, one highlight per section at most |
| `--on-accent` | `#1e1e1e` | `#1e1e1e` | Text on amber |

All text pairs meet 4.5:1; `--muted` on `--bg` is 7.4:1 dark and 6.4:1 light. No gradients on
text or backgrounds.

## Typography

- **IBM Plex Sans** for everything, weights 400, 500 and 600. **IBM Plex Mono** 500 for the
  LCD-style figures, bar numbers and commands.
- Scale (rem, line height): 0.8125/1.4 captions · 1/1.6 body · 1.125/1.6 lead · 1.375/1.3 h3 ·
  1.75/1.2 h2 · clamp(2.25, 5vw, 3.5)/1.05 h1.
- Body text 16px minimum; prose at most 68ch.

## Spacing and layout

- 4px base: 4, 8, 12, 16, 24, 32, 48, 64, 96.
- Content width 1120px with a 16px gutter on phones and 32px from tablets up.
- Sections are 96px apart on desktop, 64px on phones. Features alternate text and screenshot
  left and right rather than sitting in a grid of identical cards.

## Shape and depth

- Radius: 6px for buttons and inputs, 12px for screenshot frames and cards (GNOME's window radius).
- One shadow, for screenshots only: `0 24px 48px -12px rgb(0 0 0 / 0.5)` (lighter in light mode).

## Components

- **Buttons:** primary (blue fill, white text), secondary (1px `--border` outline, `--text`),
  quiet (text only, `--link`). Hover lightens the fill or adds `--raised`; focus is a 2px
  `--accent` outline offset 2px; disabled is 50% opacity.
- **Links:** `--link`, underlined in prose, underline on hover elsewhere.
- **Nav:** one row: icon and name on the left; Features, Install and GitHub on the right.
- **Ruler:** the site's signature. A strip of bar numbers and ticks in Plex Mono with an amber
  cycle bar across part of it, used at the top of the hero and as section dividers.
- **Code:** commands in `--surface` blocks with Plex Mono and a copy button.
- **Footer:** SolarFlare Developments, the licence and the GitHub link.

## Imagery and icons

- Real screenshots of Solstice only, in the dark theme, framed with the 12px radius and the
  shadow. No stock photos, illustrations or mock-ups.
- Icons: the app icon, and simple inline SVG strokes where a symbol is needed. No emoji.
