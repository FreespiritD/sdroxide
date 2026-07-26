#!/usr/bin/env python3
"""Rasterise the globe's land and country-border maps from Natural Earth.

Both outputs are equirectangular, row-major, x = lon -180..180 and
y = lat +90..-90 — the same convention as `sdroxide_types::worldmask`, so the
3D globe and the flat FT8 map place a coastline at the same coordinates. The
globe simply gets four times the resolution and a border layer the flat map has
no room for.

    land.png     4 320 x 2 160, 1-bit  — land = 1, ocean and lakes = 0
    borders.png  4 320 x 2 160, 8-bit  — antialiased coverage of the
                                          international boundary lines

Sources (all public domain, Natural Earth 1:10m):

    ne_10m_land, ne_10m_lakes, ne_10m_admin_0_boundary_lines_land

Run from anywhere; it downloads into a temporary directory and writes the two
PNGs next to itself.

    python3 make_earth_maps.py
"""

import io
import os
import struct
import sys
import urllib.request
import zipfile

from PIL import Image, ImageDraw

# Output grid: 1/12 deg, ~9 km at the equator — twice the flat map's 2160x1080
# in each axis, and comfortably inside the 8192-texel limit a GPU is allowed to
# have (the globe uploads these as textures, with a mip chain).
W, H = 4320, 2160
# Supersampling factor for the rasteriser. Land is thresholded back to 1 bit,
# so this only decides where the coastline lands; borders keep the coverage as
# an 8-bit alpha, which is what makes a one-pixel line readable at any zoom.
SS = 3

BASE = "https://naciscdn.org/naturalearth/10m"
LAYERS = {
    "land": f"{BASE}/physical/ne_10m_land.zip",
    "lakes": f"{BASE}/physical/ne_10m_lakes.zip",
    "borders": f"{BASE}/cultural/ne_10m_admin_0_boundary_lines_land.zip",
}


def fetch(name: str, url: str, cache: str) -> bytes:
    """The .shp member of a Natural Earth zip, downloaded once and cached."""
    path = os.path.join(cache, name + ".shp")
    if os.path.exists(path):
        return open(path, "rb").read()
    print(f"fetching {url}", file=sys.stderr)
    with urllib.request.urlopen(url, timeout=180) as r:
        blob = r.read()
    with zipfile.ZipFile(io.BytesIO(blob)) as z:
        member = next(n for n in z.namelist() if n.endswith(".shp"))
        shp = z.read(member)
    open(path, "wb").write(shp)
    return shp


def shapes(shp: bytes):
    """Yield each record's rings as lists of (lon, lat).

    Handles the two shape types Natural Earth uses here: 3 (polyline) and
    5 (polygon). Both store the same parts/points layout, so one reader does.
    """
    pos = 100  # file header
    while pos + 8 <= len(shp):
        _, words = struct.unpack_from(">ii", shp, pos)
        pos += 8
        end = pos + words * 2
        (kind,) = struct.unpack_from("<i", shp, pos)
        if kind in (3, 5):
            n_parts, n_points = struct.unpack_from("<ii", shp, pos + 36)
            parts = struct.unpack_from(f"<{n_parts}i", shp, pos + 44)
            off = pos + 44 + n_parts * 4
            pts = struct.unpack_from(f"<{2 * n_points}d", shp, off)
            bounds = list(parts) + [n_points]
            rings = []
            for k in range(n_parts):
                a, b = bounds[k], bounds[k + 1]
                rings.append([(pts[2 * i], pts[2 * i + 1]) for i in range(a, b)])
            yield rings
        pos = end


def project(ring):
    """Lon/lat degrees to supersampled pixel coordinates."""
    sw, sh = W * SS, H * SS
    return [
        (((lon + 180.0) / 360.0) * sw, ((90.0 - lat) / 180.0) * sh) for lon, lat in ring
    ]


def signed_area(ring) -> float:
    """Shoelace area in lon/lat. Negative is clockwise, which in the shapefile
    convention is an outer ring; positive rings are holes."""
    a = 0.0
    for (x0, y0), (x1, y1) in zip(ring, ring[1:] + ring[:1]):
        a += x0 * y1 - x1 * y0
    return a * 0.5


def rasterise_polygons(shp: bytes, img: Image.Image, fill: int):
    """Fill every outer ring, then punch out every hole."""
    draw = ImageDraw.Draw(img)
    holes = []
    for rings in shapes(shp):
        for ring in rings:
            if len(ring) < 3:
                continue
            if signed_area(ring) > 0:
                holes.append(ring)
            else:
                draw.polygon(project(ring), fill=fill)
    for ring in holes:
        draw.polygon(project(ring), fill=0)


def rasterise_lines(shp: bytes, img: Image.Image, width: int):
    draw = ImageDraw.Draw(img)
    for rings in shapes(shp):
        for ring in rings:
            if len(ring) < 2:
                continue
            draw.line(project(ring), fill=255, width=width, joint="curve")


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    cache = os.environ.get("NE_CACHE", "/tmp/naturalearth")
    os.makedirs(cache, exist_ok=True)
    sw, sh = W * SS, H * SS

    land = Image.new("L", (sw, sh), 0)
    rasterise_polygons(fetch("land", LAYERS["land"], cache), land, 255)
    # Inland water is not land: without this the Caspian and the Great Lakes
    # read as continent, which on a globe is immediately wrong.
    rasterise_polygons(fetch("lakes", LAYERS["lakes"], cache), land, 0)
    land = land.resize((W, H), Image.BOX)
    land = land.point(lambda v: 255 if v >= 128 else 0).convert("1")
    land.save(os.path.join(here, "land.png"), optimize=True)

    borders = Image.new("L", (sw, sh), 0)
    # One output pixel wide once downsampled — any thicker and the borders
    # dominate the coastline they sit next to.
    rasterise_lines(fetch("borders", LAYERS["borders"], cache), borders, SS)
    borders = borders.resize((W, H), Image.BOX)
    borders.save(os.path.join(here, "borders.png"), optimize=True)

    for name in ("land.png", "borders.png"):
        p = os.path.join(here, name)
        print(f"{name}: {os.path.getsize(p) / 1024:.0f} kB", file=sys.stderr)


if __name__ == "__main__":
    main()
