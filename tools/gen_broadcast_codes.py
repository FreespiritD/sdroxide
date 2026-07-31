#!/usr/bin/env python3
"""Refresh the compiled-in EiBi lookup tables and the offline fallback schedule.

sdroxide downloads EiBi's seasonal schedule at runtime and parses it itself (see
`sdroxide_types::broadcast::parse_schedule`), so this tool does *not* convert
schedule rows — doing that here as well would be a second implementation to keep
in step with the first. It only produces the two things that cannot be fetched
usefully at runtime:

  broadcast_codes.json  the transmitter-site coordinates and the language,
                        country and target-area names, all of which live in
                        EiBi's human-readable README rather than in the machine
                        -readable schedule. They change very rarely.

  sked-fallback.csv     one season's schedule verbatim, so a first run with no
                        network still labels the waterfall. Parsed by exactly
                        the same Rust code as a downloaded file.

Run it when a new README lands, or to move the fallback to a newer season:

    tools/gen_broadcast_codes.py --season b26
"""

import argparse
import json
import re
import sys
import urllib.request
from pathlib import Path

EIBI = "http://www.eibispace.de/dx"
OUT_DIR = Path(__file__).resolve().parent.parent / "crates/sdroxide-types/src"

TARGET_PREFIX = {"C": "Central", "E": "East", "N": "North", "S": "South", "W": "West"}


def fetch(url, local):
    if local:
        return Path(local).read_bytes()
    with urllib.request.urlopen(url, timeout=180) as r:
        return r.read()


def parse_dms(text):
    """`07S54`, `146E46'05"` -> signed degrees."""
    m = re.match(r"(\d+)([NSEW])(\d+)(?:'(\d+)\")?", text)
    if not m:
        return None
    deg, hemi, minutes, secs = m.groups()
    v = int(deg) + int(minutes) / 60 + (int(secs) / 3600 if secs else 0)
    return -v if hemi in "SW" else v


def section(readme, start_marker, end_marker=None):
    """The body of a README section.

    Every heading appears twice — once in the table of contents, once over the
    table itself — so the start is found from the end of the file and the
    terminator from there forwards.
    """
    start = readme.rindex(start_marker)
    end = readme.index(end_marker, start) if end_marker else len(readme)
    return readme[start:end]


def parse_sites(readme):
    """Site-code table -> {"CCC-x": [name, lat, lon]}, or "CCC" where a country
    has just the one site."""
    sites = {}
    country = None
    for line in section(readme, "IV) Transmitter site codes.").splitlines():
        m = re.match(r"^   ([A-Z]{1,3}):\s*(.*)$", line)
        if m:
            country, rest = m.group(1), m.group(2)
        elif re.match(r"^ {6,}\S", line) and country:
            rest = line.strip()
        else:
            continue
        coord = re.search(
            r"(\d+[NS]\d+(?:'\d+\")?)\s*-\s*(\d+[EW]\d+(?:'\d+\")?)", rest
        )
        if not coord:
            continue
        lat, lon = parse_dms(coord.group(1)), parse_dms(coord.group(2))
        if lat is None or lon is None:
            continue
        head = rest[: coord.start()].strip().rstrip(",").strip()
        # Site codes are one or two characters (the README says so). Bounding the
        # match matters: without it "Koror-Babeldaob" reads as code "Koror".
        sm = re.match(r"^([A-Za-z0-9]{1,2})-(.*)$", head)
        suffix, name = (sm.group(1), sm.group(2)) if sm else ("", head)
        name = re.sub(r"\(.*?\)", "", name)
        name = re.sub(r"\b\d+\s*x?\s*\d*\s*kW\b", "", name, flags=re.I).strip(" ,-")
        name = re.sub(r'\s*"\d+"\s*$', "", name).strip()
        if name:
            key = f"{country}-{suffix}" if suffix else country
            sites[key] = [name, round(lat, 4), round(lon, 4)]
    return sites


def parse_two_column(readme, start_marker, end_marker, pattern):
    out = {}
    for line in section(readme, start_marker, end_marker).splitlines():
        m = re.match(pattern, line)
        if m:
            out.setdefault(m.group(1), m.group(2).strip())
    return out


def parse_languages(readme):
    raw = parse_two_column(
        readme, "I) Language codes.", "II) Country codes.", r"^   (\S+)\s\s+(.+)$"
    )
    out = {}
    for code, desc in raw.items():
        # "Arabic (300m)  [arb]" -> "Arabic"; "Amoy: S China ..." -> "Amoy".
        name = re.split(r"[:(\[]", desc)[0].strip().rstrip(",")
        if name and not name.startswith("-"):
            out[code] = name
    return out


def parse_countries(readme):
    return parse_two_column(
        readme, "II) Country codes.", "III) Target-area codes.",
        r"^   ([A-Z]{1,3})\s\s+([A-Za-z].+)$",
    )


def parse_targets(readme, countries):
    raw = parse_two_column(
        readme, "III) Target-area codes.", "IV) Transmitter site codes.",
        r"^   (\S+)\s+-\s+(.+)$",
    )
    direct = {
        k: v.split("(")[0].strip().rstrip(",") for k, v in raw.items() if ".." not in k
    }
    # `C..`, `E..`, `N..`, `S..` and `W..` are prefixes to be glued onto another
    # code, which is how `SAs` means South Asia. Expanded here rather than in the
    # Rust so the runtime only has to do a map lookup.
    out = dict(direct)
    for prefix, word in TARGET_PREFIX.items():
        for code, name in direct.items():
            out.setdefault(f"{prefix}{code}", f"{word} {name}")
    for code, name in countries.items():
        out.setdefault(code, name.split("(")[0].strip())
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--season", default="a26", help="season for the fallback schedule")
    ap.add_argument("--readme", help="local README.TXT instead of downloading")
    ap.add_argument("--sked", help="local sked-XNN.csv instead of downloading")
    args = ap.parse_args()

    readme = fetch(f"{EIBI}/README.TXT", args.readme).decode("latin-1")
    countries = parse_countries(readme)
    codes = {
        "source": "EiBi README (https://www.eibispace.de/dx/README.TXT)",
        "sites": parse_sites(readme),
        "languages": parse_languages(readme),
        "countries": countries,
        "targets": parse_targets(readme, countries),
    }
    for name, table in codes.items():
        if isinstance(table, dict):
            print(f"  {name}: {len(table)}", file=sys.stderr)
    if len(codes["sites"]) < 500 or len(codes["countries"]) < 100:
        sys.exit("README parsed suspiciously thin — has its layout changed?")

    (OUT_DIR / "broadcast_codes.json").write_text(
        json.dumps(codes, ensure_ascii=False, indent=0, sort_keys=True) + "\n"
    )

    sked = fetch(f"{EIBI}/sked-{args.season}.csv", args.sked)
    if sked.count(b"\n") < 1000:
        sys.exit(f"sked-{args.season}.csv looks truncated")
    # EiBi publishes latin-1; `include_str!` needs UTF-8. Downloads are decoded
    # the same way at runtime, so both paths see identical text.
    text = sked.decode("latin-1")
    (OUT_DIR / "sked-fallback.csv").write_text(text, encoding="utf-8")

    print(
        f"wrote broadcast_codes.json and sked-fallback.csv "
        f"({args.season.upper()}, {text.count(chr(10))} lines)",
        file=sys.stderr,
    )


if __name__ == "__main__":
    main()
