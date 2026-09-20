#!/usr/bin/env python3
"""Builds assets/wordmark.svg. Run it from the root of the repository."""
# so the file needs no font on the viewer's machine and carries no font licence.
# Grid: cap height 100, y=0 is the cap line, y=100 is the baseline.

L = {}
L['B'] = (63, "M0 0 L38 0 C52 0 63 6 63 18 C63 30 55 36 47 38 C57 40 65 48 65 62 C65 78 53 100 38 100 L0 100 Z"
               "M20 18 L36 18 C43 18 46 22 46 27 C46 33 43 37 36 37 L20 37 Z"
               "M20 60 L37 60 C44 60 48 65 48 71 C48 77 44 82 37 82 L20 82 Z")
L['U'] = (62, "M0 0 L20 0 L20 62 C20 74 25 82 31 82 C37 82 42 74 42 62 L42 0 L62 0 L62 62 "
               "C62 84 49 100 31 100 C13 100 0 84 0 62 Z")
L['R'] = (64, "M0 0 L38 0 C53 0 63 9 63 25 C63 38 56 47 46 51 L66 100 L43 100 L28 56 L20 56 L20 100 L0 100 Z"
               "M20 18 L36 18 C43 18 47 21 47 26 C47 32 43 36 36 36 L20 36 Z")
L['N'] = (64, "M0 0 L22 0 L44 58 L44 0 L64 0 L64 100 L42 100 L20 42 L20 100 L0 100 Z")
L['O'] = (66, "M33 0 C52 0 66 21 66 50 C66 79 52 100 33 100 C14 100 0 79 0 50 C0 21 14 0 33 0 Z"
               "M33 20 C24 20 20 34 20 50 C20 66 24 80 33 80 C42 80 46 66 46 50 C46 34 42 20 33 20 Z")
L['T'] = (58, "M0 0 L58 0 L58 20 L39 20 L39 100 L19 100 L19 20 L0 20 Z")

word, track = "BURNOUT", 13
adv = [L[c][0] for c in word]
total = sum(adv) + track * (len(word) - 1)

# badge geometry
pad_x, pad_y = 64, 32
bx, by = 60, 158
bw, bh = total + 2 * pad_x, 100 + 2 * pad_y
tx, ty = bx + pad_x, by + pad_y

glyphs, cursor = [], tx
for c in word:
    w, d = L[c]
    glyphs.append(f'<path transform="translate({cursor} {ty})" d="{d}"/>')
    cursor += w + track

W, H = bx * 2 + bw, 348
print("canvas", W, H, "badge", bx, by, bw, bh)

def lick(cx, w, h, lean, base):
    """A flame teardrop: sides bulge OUTWARD low down, then hook in to a sharp
    tip. Control points inside the base give straight-sided spikes, which read
    as a crown rather than as fire."""
    x0, x1 = cx - w / 2, cx + w / 2
    tip = cx + lean
    return (f"M{x0:.1f} {base:.1f} "
            f"C{x0-w*0.16:.1f} {base-h*0.36:.1f} {tip-w*0.34:.1f} {base-h*0.74:.1f} {tip:.1f} {base-h:.1f} "
            f"C{tip+w*0.32:.1f} {base-h*0.72:.1f} {x1+w*0.16:.1f} {base-h*0.36:.1f} {x1:.1f} {base:.1f} Z")

top = by + 12
# Bases overlap on purpose, and the spacing is irregular. Evenly spaced licks
# with gaps between them read as a row of separate flames.
SPEC = ((76, 58, 56, 7), (134, 52, 98, -6), (190, 62, 40, 9), (250, 56, 118, 10),
        (308, 54, 62, -7), (366, 60, 94, 8), (422, 50, 46, -5), (478, 58, 80, 7),
        (536, 52, 54, -6), (592, 56, 72, 6))
flames = [lick(bx + cx, w, h, lean, top) for cx, w, h, lean in SPEC]
bed = (bx + 34, top - 15, bw - 68, 17)
FIRE_TOP = top - 124

sparks = [(bx+104, 40, 5), (bx+bw/2-58, 20, 4), (bx+bw/2+88, 30, 6),
          (bx+bw-136, 16, 4), (bx+bw-52, 62, 5), (bx+52, 74, 4),
          (bx+bw/2+18, 58, 3), (bx+bw-210, 48, 4)]

svg = f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" width="{W}" height="{H}" role="img" aria-label="Burnout">
  <title>Burnout</title>
  <defs>
    <linearGradient id="fire" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#FDE047"/>
      <stop offset="0.38" stop-color="#FB923C"/>
      <stop offset="0.74" stop-color="#EF4444"/>
      <stop offset="1" stop-color="#C2185B"/>
    </linearGradient>
    <linearGradient id="fireText" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#FEF08A"/>
      <stop offset="0.42" stop-color="#FB923C"/>
      <stop offset="1" stop-color="#E11D48"/>
    </linearGradient>
    <linearGradient id="lick" gradientUnits="userSpaceOnUse" x1="0" y1="{top}" x2="0" y2="{FIRE_TOP}">
      <stop offset="0" stop-color="#FDE047"/>
      <stop offset="0.34" stop-color="#FBBF24"/>
      <stop offset="0.66" stop-color="#F97316"/>
      <stop offset="1" stop-color="#EF4444"/>
    </linearGradient>
  </defs>

  <rect x="{bx}" y="{by}" width="{bw}" height="{bh}" rx="46" fill="none" stroke="url(#fire)" stroke-width="11"/>

  <rect x="{bx-40}" y="{by+40}" width="34" height="22" rx="8" fill="none" stroke="url(#fire)" stroke-width="10"/>
  <rect x="{bx-40}" y="{by+bh-62}" width="34" height="22" rx="8" fill="none" stroke="url(#fire)" stroke-width="10"/>
  <rect x="{bx+bw+6}" y="{by+40}" width="34" height="22" rx="8" fill="none" stroke="url(#fire)" stroke-width="10"/>
  <rect x="{bx+bw+6}" y="{by+bh-62}" width="34" height="22" rx="8" fill="none" stroke="url(#fire)" stroke-width="10"/>

  <g fill="url(#lick)">
    <rect x="{bed[0]}" y="{bed[1]}" width="{bed[2]}" height="{bed[3]}" rx="11"/>
    {chr(10).join(f'    <path d="{d}"/>' for d in flames)}
  </g>


  <g fill="url(#fireText)" fill-rule="evenodd">
    {chr(10).join("    " + g for g in glyphs)}
  </g>

  <g fill="#F97316">
    {chr(10).join(f'    <circle cx="{x}" cy="{y}" r="{r}" opacity="{0.9 - i*0.1:.2f}"/>' for i, (x, y, r) in enumerate(sparks))}
  </g>
</svg>
'''
open("assets/wordmark.svg", "w").write(svg)
print("written")
