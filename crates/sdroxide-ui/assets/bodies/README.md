# Body maps

`moon.jpg` — 2048×1024 equirectangular albedo map of the Moon, −180°…180° east
longitude with 0° at the centre and +90°…−90° latitude, matching the sphere
mesh's UVs and `sdroxide_solar::ephem::moon_basis`'s frame directly.

Source: NASA/Goddard Scientific Visualization Studio, *CGI Moon Kit*
(<https://svs.gsfc.nasa.gov/4720>), `lroc_color_poles_2k.tif` — the LRO Wide
Angle Camera global albedo mosaic. NASA imagery is public domain.

Re-encoded to JPEG at quality 88 (3.2 MB TIFF → 0.5 MB) because it is a
photograph: PNG of the same data is four times the size for no visible gain,
and this is the one body in the view whose real surface everybody already
knows by sight.
