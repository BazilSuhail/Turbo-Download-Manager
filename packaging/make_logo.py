"""Generate every TDM logo file from the master image, assets/tdm-logo.png.

Run from the project root after changing the logo:

    python packaging/make_logo.py

Writes:
  packaging/icons/      32, 128, 256 and 512 px PNG, icon.ico
  packaging/windows/    installer bitmaps (24-bit BMP on a dark background)
  extension/icons/      toolbar icons, 16, 32, 48 and 128 px
  assets/               raw RGBA for the window icon (64 px) and header mark (32 px)

Needs Pillow (pip install pillow). The app itself does not need it.
"""

from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
MASTER = ROOT / "assets" / "tdm-logo.png"
BACKGROUND = (0x1A, 0x17, 0x14)  # warm charcoal, behind the logo in the BMPs

# Installer bitmaps: NSIS and WiX need 24-bit BMP, so the logo sits on a solid colour.
BITMAPS = {
    "sidebar.bmp": (164, 314, 120),  # (width, height, logo size)
    "header.bmp": (150, 57, 44),
    "wix-banner.bmp": (493, 58, 46),
    "wix-dialog.bmp": (493, 312, 180),
}


def square_master() -> Image.Image:
    """The logo centred on a transparent square, so resizing keeps its proportions."""
    logo = Image.open(MASTER).convert("RGBA")
    side = max(logo.size)
    canvas = Image.new("RGBA", (side, side), (0, 0, 0, 0))
    canvas.paste(logo, ((side - logo.width) // 2, (side - logo.height) // 2), logo)
    return canvas


def sized(master: Image.Image, px: int) -> Image.Image:
    return master.resize((px, px), Image.LANCZOS)


def main() -> None:
    master = square_master()
    big = master.resize((1024, 1024), Image.LANCZOS)  # ICO and ICNS write their own sizes from this

    icons = ROOT / "packaging" / "icons"
    icons.mkdir(parents=True, exist_ok=True)
    for px in (32, 128, 256, 512):
        name = "icon.png" if px == 512 else f"{px}x{px}.png"
        sized(master, px).save(icons / name)
    big.save(icons / "icon.ico", format="ICO", sizes=[(16, 16), (32, 32), (48, 48), (256, 256)])

    windows = ROOT / "packaging" / "windows"
    for name, (w, h, logo_px) in BITMAPS.items():
        sheet = Image.new("RGBA", (w, h), BACKGROUND + (255,))
        logo = sized(master, logo_px)
        sheet.alpha_composite(logo, ((w - logo_px) // 2, (h - logo_px) // 2))
        sheet.convert("RGB").save(windows / name, format="BMP")

    extension = ROOT / "extension" / "icons"
    extension.mkdir(parents=True, exist_ok=True)
    for px in (16, 32, 48, 128):
        sized(master, px).save(extension / f"icon{px}.png")

    assets = ROOT / "assets"
    (assets / "app-icon-64.rgba").write_bytes(sized(master, 64).tobytes())
    (assets / "app-icon-32.rgba").write_bytes(sized(master, 32).tobytes())
    print("logo files written")


if __name__ == "__main__":
    main()
