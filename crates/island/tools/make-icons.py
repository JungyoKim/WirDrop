"""Builds crates/island/assets/F7Icons.ttf: the Framework7 Icons glyphs WirDrop's island draws, each at a
private-use code point (U+E000 + its index in ICONS), so DirectWrite draws one without ligatures.

    pip install fonttools
    python make-icons.py path/to/Framework7Icons-Regular.ttf

Framework7 Icons (MIT, https://github.com/framework7io/framework7-icons) is ligature-only: its
character map has letters, and each icon is a ligature of its name.
"""
import sys
from pathlib import Path

from fontTools import subset
from fontTools.ttLib import TTFont
from fontTools.ttLib.tables._c_m_a_p import cmap_format_4

# The order is the code points' and src/icons.rs's.
ICONS = [
    "checkmark_circle_fill", "xmark_circle_fill", "exclamationmark_triangle_fill",
    "arrow_down_circle_fill", "arrow_up_circle_fill", "device_phone_portrait",
    "device_tablet_portrait", "device_laptop", "desktopcomputer", "person_crop_circle_fill",
    "folder_fill", "doc_fill", "photo_fill", "film_fill", "link", "checkmark", "xmark",
    "person_crop_circle_badge_checkmark", "arrow_up_right_square", "wifi_slash",
    "dot_radiowaves_left_right", "hourglass", "info_circle_fill", "lock_fill", "bell_slash_fill",
    "music_note_2", "doc_text_fill", "archivebox_fill", "arrow_clockwise", "tray_arrow_down_fill",
    "square_arrow_up_fill", "person_fill",
]


def ligatures(font):
    cmap = font.getBestCmap()
    char = {glyph: chr(code) for code, glyph in cmap.items()}
    found = {}
    for lookup in font["GSUB"].table.LookupList.Lookup:
        for table in lookup.SubTable:
            if lookup.LookupType == 7:
                table = table.ExtSubTable
            for first, ligs in getattr(table, "ligatures", {}).items():
                for lig in ligs:
                    name = "".join(char.get(g, "?") for g in [first, *lig.Component])
                    found[name] = lig.LigGlyph
    return found


def main(src):
    font = TTFont(src)
    glyphs = ligatures(font)
    missing = [n for n in ICONS if n not in glyphs]
    if missing:
        sys.exit(f"not in the font: {missing}")
    picked = [glyphs[n] for n in ICONS]
    options = subset.Options()
    options.layout_features = []
    options.glyph_names = True
    options.name_IDs = ["*"]
    options.notdef_outline = True
    subsetter = subset.Subsetter(options)
    subsetter.populate(glyphs=picked)
    subsetter.subset(font)
    table = cmap_format_4(4)
    table.platformID, table.platEncID, table.language = 3, 1, 0
    table.cmap = {0xE000 + i: g for i, g in enumerate(picked)}
    font["cmap"].tables = [table]
    if "GSUB" in font:
        del font["GSUB"]
    out = Path(__file__).resolve().parent.parent / "assets" / "F7Icons.ttf"
    font.save(out)
    print(f"{out}: {len(picked)} icons, {out.stat().st_size} bytes")


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "Framework7Icons-Regular.ttf")

