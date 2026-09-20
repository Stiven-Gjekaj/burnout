#!/usr/bin/env python3
"""Builds assets/wordmark.svg. Run it from the root of the repository.

The banner is a terminal: a 5x7 pixel face on a 12px grid, a prompt, a block
cursor that blinks, and scanlines over the top. The letters are drawn here
rather than set in a font, so the file needs no font on the viewer's machine
and carries no font licence.
"""

CELL = 12          # one pixel of the face
COLS, ROWS = 5, 7  # every glyph is this size
ADVANCE = (COLS + 1) * CELL

FACE = {
    'B': ("11110", "10001", "10001", "11110", "10001", "10001", "11110"),
    'U': ("10001", "10001", "10001", "10001", "10001", "10001", "01110"),
    'R': ("11110", "10001", "10001", "11110", "10100", "10010", "10001"),
    'N': ("10001", "11001", "10101", "10101", "10011", "10001", "10001"),
    'O': ("01110", "10001", "10001", "10001", "10001", "10001", "01110"),
    'T': ("11111", "00100", "00100", "00100", "00100", "00100", "00100"),
}

# A prompt chevron, one cell per step. These are (row, col) like every other
# cell here, not (x, y).
CHEVRON = ((0, 0), (1, 1), (2, 2), (3, 3), (4, 2), (5, 1), (6, 0))


def blocks(cells):
    """Merge each row's runs into one rect. A rect per cell says the same
    thing in five times the bytes."""
    out, cells = [], sorted(cells)
    i = 0
    while i < len(cells):
        row, col = cells[i]
        run = 1
        while i + run < len(cells) and cells[i + run] == (row, col + run):
            run += 1
        out.append(f"M{col*CELL} {row*CELL}h{run*CELL}v{CELL}h-{run*CELL}z")
        i += run
    return "".join(out)


def word(text):
    cells = []
    for n, ch in enumerate(text):
        for row, bits in enumerate(FACE[ch]):
            for col, bit in enumerate(bits):
                if bit == "1":
                    cells.append((row, col + n * (COLS + 1)))
    return blocks(cells)


WORD = "BURNOUT"
PAD, TOP = 48, 48
text_x = PAD + 8 * CELL
text_w = len(WORD) * ADVANCE - CELL
cur_x = text_x + text_w + 2 * CELL
W = cur_x + 4 * CELL + PAD
H = TOP * 2 + ROWS * CELL

svg = f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" width="{W}" height="{H}" role="img" aria-label="Burnout">
  <title>Burnout</title>

  <defs>
    <!-- Ember red through to the pale yellow at the base of a flame. The
         shift runs along the word, so the last letter is the hottest. -->
    <linearGradient id="ink" x1="0" y1="0" x2="1" y2="0.35">
      <stop offset="0" stop-color="#dc2626"/>
      <stop offset="0.42" stop-color="#f97316"/>
      <stop offset="0.75" stop-color="#fbbf24"/>
      <stop offset="1" stop-color="#facc15"/>
    </linearGradient>

    <filter id="glow" x="-20%" y="-40%" width="140%" height="180%">
      <feGaussianBlur stdDeviation="6" result="blur"/>
      <feMerge>
        <feMergeNode in="blur"/>
        <feMergeNode in="SourceGraphic"/>
      </feMerge>
    </filter>

    <!-- One scanline, tiled. A rule drawn per line would be a hundred
         elements that all say the same thing. -->
    <pattern id="scanlines" width="4" height="4" patternUnits="userSpaceOnUse">
      <rect width="4" height="2" fill="#000" opacity="0.28"/>
    </pattern>
  </defs>

  <rect width="{W}" height="{H}" rx="16" fill="#0c0706"/>
  <rect x="6" y="6" width="{W-12}" height="{H-12}" rx="11"
        fill="none" stroke="#f97316" stroke-opacity="0.28" stroke-width="2"/>

  <g filter="url(#glow)">
    <path d="{blocks(CHEVRON)}" fill="#f97316" opacity="0.55"
          transform="translate({PAD} {TOP})"/>
    <path d="{word(WORD)}" fill="url(#ink)" transform="translate({text_x} {TOP})"/>
    <rect x="{cur_x}" y="{TOP}" width="{4*CELL}"
          height="{ROWS*CELL}" fill="#fbbf24" opacity="0.85">
      <animate attributeName="opacity" values="0.85;0.85;0.1;0.1;0.85"
               dur="1.2s" repeatCount="indefinite"/>
    </rect>
  </g>

  <rect width="{W}" height="{H}" rx="16" fill="url(#scanlines)"
        opacity="0.5" pointer-events="none"/>
</svg>
'''
open("assets/wordmark.svg", "w").write(svg)
print(f"written {W}x{H}")
