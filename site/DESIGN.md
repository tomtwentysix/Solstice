# Solstice site design

The site looks like the app: a dark studio, the icon's charcoal and blue, and the amber of the
cycle bar used sparingly. Pages follow the visitor's light or dark setting, but `.stage` bands
(the hero and closing calls to action) are always dark, because they stand for the studio itself.

The message, in order: free and open source, made for Linux, easy to record a band with, and
familiar to people coming from other DAWs. Electronic music tools stay on the Features page.
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
  clamp(1.875, 3.6vw, 2.75)/1.1 h2 · clamp(2.5, 6.5vw, 4.75)/1 h1 (3.5 on inner pages).
- Body text 17px; prose at most 68ch.

## Spacing and layout

- 4px base: 4, 8, 12, 16, 24, 32, 48, 64, 96.
- Content width 1200px with a 16px gutter on phones and 32px from tablets up.
- Sections are 120px apart on desktop, 72px on phones.
- The home page tells one session as numbered steps: on desktop the step text sticks beside
  its screenshot, with an amber bar on its left like a region on the timeline. Other features
  alternate text and screenshot rather than sitting in a grid of identical cards.
- Pages: Home, Features (a hub of cards), eight feature pages (recording, guitar, editing,
  mixing, mastering, plugins, projects, switching) and Download. Switching talks about DAWs in
  general; Logic Pro is named only where a fact needs it (the project bundle import).
  Feature pages share a strip of
  links under their heading, long-form sections of at most 68ch beside or above screenshots,
  tables for reference detail, and Previous/Next links at the end.
- Copy describes what the code does, in the app's own UI labels. Anything not built (a movable
  punch range, time-stretching loops) isn't mentioned.

## Shape and depth

- Radius: 6px for buttons and inputs, 12px for screenshot frames and cards (GNOME's window radius).
- One shadow, for screenshots only: `0 32px 64px -16px rgb(0 0 0 / 0.6)` (lighter in light mode).
- Screenshots rise into place as they scroll in, where the browser has scroll-driven animations
  and the visitor hasn't asked for reduced motion.

## Components

- **Buttons:** primary (blue fill, white text), secondary (1px `--border` outline, `--text`),
  quiet (text only, `--link`). Hover lightens the fill or adds `--raised`; focus is a 2px
  `--accent` outline offset 2px; disabled is 50% opacity.
- **Links:** `--link`, underlined in prose, underline on hover elsewhere.
- **Nav:** one row: icon and name on the left; How it works, Features, Switching and a
  Download button on the right. The current page gets an amber underline; How it works and
  Switching hide on phones.
- **Pillars:** four claims in a row divided by ruler-style lines, each numbered like a bar.
- **Terms:** a grid of definition cells for concepts shared with other DAWs, with `kbd` keys below.
- **Ruler:** the site's signature. A strip of bar numbers and ticks in Plex Mono with an amber
  cycle bar across part of it, used at the top of the hero and as section dividers.
- **Code:** commands in `--surface` blocks with Plex Mono and a copy button.
- **Footer:** SolarFlare Developments, the licence, GitHub, release notes, and the Apple
  trademark notice, since the pages name Logic Pro and Final Cut Pro.

## Imagery and icons

- Real screenshots of Solstice only, in the dark theme, framed with the 12px radius and the
  shadow. No stock photos, illustrations or mock-ups.
- Icons: the app icon, and simple inline SVG strokes where a symbol is needed. No emoji.
