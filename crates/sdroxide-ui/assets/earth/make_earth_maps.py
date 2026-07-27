#!/usr/bin/env python3
"""Rasterise the globe's land and country-border maps from Natural Earth.

Both outputs are equirectangular, row-major, x = lon -180..180 and
y = lat +90..-90 — the same convention as `sdroxide_types::worldmask`, so the
3D globe and the flat FT8 map place a coastline at the same coordinates.

    land.png     8 192 x 4 096, 8-bit  — *coverage*: the fraction of each texel
                                          that is land, 0 = open ocean or lake,
                                          255 = solid ground
    borders.png  4 320 x 2 160, 8-bit  — antialiased coverage of the
                                          international boundary lines

Coverage, not a 1-bit mask, is the whole point of the land map. The shader
draws the shoreline as the field's half-way contour and strokes it a fixed
number of *pixels* wide, so where that contour falls inside a texel is what the
eye reads as sharpness. Thresholding would round it to the texel grid and put a
staircase on every coast; keeping the supersampled coverage places it to a
fraction of a texel and the same contour comes out smooth at any zoom.

The land grid is 1/22.75 deg, ~4.9 km at the equator — four times the flat FT8
map's 2160x1080 in each axis, because the globe's camera can fly down to the
surface and a panel-sized rectangle's grid runs out long before it does. The
borders stay at 1/12 deg: they are drawn as one-texel lines rather than as a
filled region, so resolution buys them far less than it buys the coast, and
they cost the same GPU memory per texel.

Sources (all public domain, Natural Earth 1:10m):

    ne_10m_land, ne_10m_lakes, ne_10m_admin_0_boundary_lines_land

Run from anywhere; it downloads into a temporary directory and writes the two
PNGs next to itself. Peak memory is a few hundred MB — the land pass
supersamples to 32768x16384 before averaging down.

    python3 make_earth_maps.py
"""

import io
import os
import struct
import sys
import urllib.request
import zipfile

from PIL import Image, ImageDraw

LAND_W, LAND_H = 8192, 4096
BORDER_W, BORDER_H = 4320, 2160
# Supersampling factors. Both outputs keep the coverage as an 8-bit alpha, so
# these decide how finely a shoreline can be placed inside a texel and how
# readable a one-pixel border line is. Land gets the finer grid because its
# coverage is what the shader's contour is reconstructed from: SS² + 1 distinct
# levels, and the contour wobbles by about half a level's worth of a texel.
LAND_SS = 4
BORDER_SS = 3

CACHE = os.environ.get("NE_CACHE", "/tmp/naturalearth")
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


def project(ring, size):
    """Lon/lat degrees to supersampled pixel coordinates for one output grid."""
    sw, sh = size
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
                draw.polygon(project(ring, img.size), fill=fill)
    for ring in holes:
        draw.polygon(project(ring, img.size), fill=0)


def rasterise_lines(shp: bytes, img: Image.Image, width: int):
    draw = ImageDraw.Draw(img)
    for rings in shapes(shp):
        for ring in rings:
            if len(ring) < 2:
                continue
            draw.line(project(ring, img.size), fill=255, width=width, joint="curve")


def build_land() -> Image.Image:
    land = Image.new("L", (LAND_W * LAND_SS, LAND_H * LAND_SS), 0)
    rasterise_polygons(fetch("land", LAYERS["land"], CACHE), land, 255)
    # Inland water is not land: without this the Caspian and the Great Lakes
    # read as continent, which on a globe is immediately wrong.
    rasterise_polygons(fetch("lakes", LAYERS["lakes"], CACHE), land, 0)
    # BOX is an exact box filter over the SS x SS subsamples, so every texel
    # comes out holding the fraction of itself that is land — which is what the
    # shader's contour needs.
    return land.resize((LAND_W, LAND_H), Image.BOX)


def build_borders() -> Image.Image:
    borders = Image.new("L", (BORDER_W * BORDER_SS, BORDER_H * BORDER_SS), 0)
    # One output pixel wide once downsampled — any thicker and the borders
    # dominate the coastline they sit next to.
    rasterise_lines(fetch("borders", LAYERS["borders"], CACHE), borders, BORDER_SS)
    return borders.resize((BORDER_W, BORDER_H), Image.BOX)


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    os.makedirs(CACHE, exist_ok=True)
    for name, build in (("land.png", build_land), ("borders.png", build_borders)):
        path = os.path.join(here, name)
        build().save(path, optimize=True)
        print(f"{name}: {os.path.getsize(path) / 1024:.0f} kB", file=sys.stderr)


if __name__ == "__main__":
    main()
