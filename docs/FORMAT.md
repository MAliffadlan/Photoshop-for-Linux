# Project format

A `.mectov` file is a ZIP archive containing `manifest.json` and lossless PNG assets. Version 1 uses `format: "me.silverl.mectov"`, a `document` object, and a `pixel_layers` list. Layer images are stored at `images/<UUID>.png`; masks use `images/<UUID>.mask.png`. Pixel bytes are excluded from JSON.

The document records canvas dimensions, DPI, layer order, IDs, parent groups, clipping references, opacity, blending, visibility, locks, transforms, adjustment parameters, and optional live shape styles, including line endpoints and width. Layers are stored bottom to top; each folder's subtree is composited together in hierarchy order. Masks have enabled/linked flags and an optional independent placement transform.

Text layers also record an optional `text` object with UTF-8 content, font family, pixel size, RGBA color, bold, italic, underline, and strikethrough flags, an optional list of `colorRuns` for letters in their own colour (see version 8), and an optional paragraph `box` (see version 7). Their PNG assets preserve the rendered appearance when fonts are unavailable on another machine. Fonts are discovered from the system and are not embedded in the project; editing unavailable fonts uses the bundled Inter Variable fallback. Text is limited to 16 KiB and font sizes to 1–1024 pixels. Older version 1 files without text metadata remain supported.

Transforms retain original source pixels. Optional perspective corners are normalized coordinates before affine scale/rotation/flip. Channel adjustments retain separate RGB curves/levels and seven hue ranges; adjustment layers also carry Photoshop's Black & White weights and tint, Color Balance's nine shifts with its luminosity flag, Invert, and Compositor 1.2.3's Add Noise, Gaussian Blur and Motion Blur. Layers blend with Photoshop's twenty-four modes, stored by name, so documents stay readable when a mode is added. Selections, current multi-selection, clipboard contents, and undo/redo snapshots are not serialized.

Saving validates the document, writes a sibling temporary archive, flushes it, and atomically replaces the destination. Loading validates IDs, hierarchy, clipping cycles, transforms, dimensions, metadata size, decompressed asset size, and aggregate image/mask budgets. Assets are decoded in memory, never extracted using archive paths. `.comp` package reads reject symlinked assets and paths outside the package.

Projects written before the rename carry an earlier identifier and the previous project extension. The reader accepts both identifiers and both extensions, so those projects load unchanged; saving always writes the `me.silverl.mectov` identifier and the `.mectov` extension.

Limits: 30,000 pixels per canvas/image dimension, 200 megapixels for one image, 800 megapixels for a whole document measured as the canvas plus every layer and mask, 10,000 layers, 64 nested group levels, 4 MiB manifest JSON, and 1 GiB encoded asset files. Compositor's own ceilings are 200 and 800 megapixels; mectov lowers them to what the machine can hold, so a project saved on a large machine can be refused by a small one, with the size and the memory it found named in the error.

The importer accepts Compositor package versions 1–10, including Swift's alternating-key enum dictionaries, individual color channels, mask placement/link flags, grain parameters, live shape styles and line geometry, alignment guides, layer effects, Photoshop's blend modes, the Black & White, Color Balance and Invert adjustments, and 1.2.3's Add Noise, Gaussian Blur and Motion Blur. Versions 8 and 9 both cover Compositor 1.2.1 and 1.2.2, which kept version 8; 1.2.3 and 1.2.4 write version 9, which is the version that allows the blur and noise adjustment layers. Import is one-way: Save creates a `.mectov` file and leaves the `.comp` package untouched. Compositor 1.3.2 writes version 10, which can colour some letters of a text layer differently; a text layer in a package arrives as the PNG Compositor rendered it, because the importer reads no text metadata at any version, so a version 10 package opens exactly as version 9 does.

## Guides, layer effects, and line shapes (versions 3–5)

Documents carrying alignment guides or layer effects are written as version 3, so older readers do not silently drop them. Documents containing live line shapes are written as version 5. The reader accepts versions 1–10, with version 6 covering the layout grid, version 7 the paragraph text box, version 8 the letter colours, version 9 the colour grading wheels and version 10 the effects and calibration; ordinary documents remain version 1, documents with embedded RAW remain version 2, and documents with 1.2.3 blur/noise adjustment layers remain version 4. The adjustment kinds and blend modes added with Compositor 1.2.2 ride in the version their document already uses, as they do in `.comp` version 8: an older reader reports the unknown kind instead of dropping the layer. Compositor 1.2.3's blur and noise adjustment layers raise the file to version 4, because they redraw the backdrop in a pass of their own and a reader that does not know them cannot draw the document at all.

An optional `guides` list holds the alignment guides a Compositor project placed: each entry has an `axis` (`Horizontal` or `Vertical`) and a `position` in document pixels, a Y for horizontal guides and an X for vertical ones. Canvas Size and Image Size move or scale them, Crop drops the ones pushed off the canvas, and Flip Canvas mirrors only the guides running perpendicular to the flip. Up to 1,024 guides are stored, and Clear Guides in the View menu removes them. Rulers create, move, and delete these guides, and a guide dragged onto a ruler or released outside the canvas is removed.

A layer's optional `effects` object preserves Compositor's layer effects: `stroke`, `shadow`, `color_overlay`, `inner_shadow`, `outer_glow` and 1.2.3's `inner_glow`, each with its own geometry, color and opacity, plus an `enabled` flag that hides an effect rather than deleting it. Reading validates the source's ranges: sizes and blurs up to 500 px, shadow distances up to 5,000 px, angles within ±360°, and colors and opacities within 0–1. Effects stay with the layer through undo and project round trips and are rendered in source space before normal layer compositing.

Live line shapes store normalized `start` and `end` points plus `line_width`; the PNG asset is a cached raster for compatibility and is regenerated when the layer is resized.

## Embedded RAW (version 2)

Documents containing RAW layers are written as version 2, preventing older readers from silently dropping the source. Ordinary documents continue to use version 1, and the reader supports both versions.

## Coloured letters (version 8)

A document whose text layers colour some letters differently is written as version 8, so an older reader refuses the file instead of drawing every letter in one colour. A document whose text has no coloured range keeps the version it already had, and clearing the colours drops a document back to the version it had before.

The optional `colorRuns` list inside a layer's `text` holds runs of `start` and `length`, counted in UTF-16 code units from the start of `content`, each with a `color` of `[r, g, b, a]` bytes. Runs are stored sorted, do not overlap, are not empty, and end within the content; a text layer is limited to 1,024 of them. Letters no run covers take the text's own `color`, and a run's alpha is the text's, as it is for every other pixel of the layer. Compositor 1.3.2 stores the same runs in its own format as `location`, `length` and `red`/`green`/`blue` in 0–1.

## Colour grading wheels (version 9)

A document whose camera layers carry a colour grading is written as version 9. This is the one version bump that cannot be recovered from the saved pixels: a RAW layer keeps the original file and is developed again when the project opens, so a reader without the wheels would show the picture ungraded rather than refuse it. A document whose wheels are all neutral keeps the version it already had, and a wheel that only names a hue, with no tint and no luminance shift, counts as neutral.

The wheels live in the layer's `raw.settings` beside the rest of the develop settings: `grading` holds `shadows`, `midtones`, `highlights` and `global`, each a `hue` in degrees, a `saturation` from 0 to 100 and a `luminance` from −100 to 100, plus a `blending` from 0 to 100 and a `balance` from −100 to 100. `blending` reads as 50 rather than 0, because a tonal wheel has to reach its neighbours to grade the range it is given, and a file written before the wheels existed reads back with that neutral blend.

## Camera Raw effects and calibration (version 10)

A document whose camera layers also carry glow, the post-crop vignette, grain or calibration is written as version 10, the same reason the grading wheels are version 9: the layer keeps the original file and is developed again when the project opens, so a reader without these settings would show the picture as it was shot rather than refuse it. A document whose effects and calibration are all at their defaults keeps the version it already had.

Those settings ride in the layer's `raw.settings` beside the wheels: `glow` (0–100) with `glowStyle` (`Diffusion`, `Bloom` or `Halation`), `glowRange`, `glowSpread` and `glowWarmth` (−100–100); `vignetteAmount` (−100–100) with `vignetteStyle` (`HighlightPriority`, `ColorPriority` or `PaintOverlay`), `vignetteMidpoint` and `vignetteFeather` (0–100, both reading 50 rather than 0), `vignetteRoundness` (−100–100) and `vignetteHighlights` (0–100); `grainAmount`, `grainSize` (reading 25) and `grainRoughness` (reading 50), all 0–100; and `calibration` with its `process` (version 1 to 6, reading 6) plus a `shadowTint` and a hue and saturation for each of the three primaries, all −100–100. A calibration whose seven sliders are all at zero keeps the version the document already had, whatever its process says, because the process only decides how hard the sliders push.

Compositor's Paint Overlay vignette is the plain vignette inside the develop pipeline; the painted one belongs to its standalone Vignette filter, which mectov already has.

## Backdrop blurs (version 4)

A document containing an Add Noise, Gaussian Blur or Motion Blur adjustment layer is written as version 4, and the reader accepts it. The three kinds are the ones Compositor 1.2.3 added, which its validator only allows in `.comp` version 9; how they import and draw is described in [PORTING.md](PORTING.md).

A RAW image layer has an optional `raw` object containing the original filename, camera metadata, and validated `DevelopSettings`. Its original bytes are stored in `raw/<layer UUID>.nef` (also for NRW sources); the current developed render stays in `images/<layer UUID>.png`. Loading restores the image without decoding the sensor data. Reopening Develop decodes the embedded bytes, so moving/deleting the original file does not break the project. Missing sources, invalid settings, and oversized sources fail validation. RAW byte buffers are shared by layer duplicates and history snapshots, and included in history's memory accounting.

The archive allows up to 30,001 entries to accommodate an image, mask and RAW source for each layer. Aggregate embedded RAW bytes are limited to 512 MiB. Develop settings store crop/overlay coordinates relative to the full oriented image; curve knots and all numeric parameters are finite and range-checked. Develop's transient preview, comparison view, clipping indicators, worker state, and local undo history are not serialized.

## Layout grid (version 6)

A document carrying a layout grid is written as version 6, so an older reader reports the unknown field instead of drawing a document whose alignment is no longer described. The reader accepts versions 1–8; every version below 6 is exactly what it accepted before, and documents without a grid keep the version they already had.

The document's optional `grid` object holds `spacing` in document pixels (1–5,000, default 32) and `subdivisions` per cell (1–100, default 1). The minor step the canvas draws and snaps to is `spacing / subdivisions`, coarsened by powers of ten only while a cell would stay under two screen pixels, so the file always stores the values the user typed. Showing, hiding, and configuring the grid are one undoable edit each, stored in the document rather than in the app's settings, so two windows on the same project can disagree about visibility without rewriting it. Opening a project that carries a grid shows it; opening one without hides it.

## Paragraph text boxes (version 7)

A document whose text layers carry a paragraph box is written as version 7, so an older reader refuses the file instead of drawing text it cannot lay out. A document whose text has no box keeps the version it already had, so plain multiline text stays version 1.

The optional `box` object inside a layer's `text` holds `width` (the wrap width in document pixels, 1–30,000, default 512), `min_height` (space reserved above the text, 0–30,000, default 0), `align` (`Left`, `Center`, or `Right`, default `Left`), `line_spacing` (a multiple of the font size, 0.5–4, default 1.3), and `paragraph_spacing` (extra document pixels after each hard line break, 0–2,000, default 0). Stored fields the file leaves out fall back to those defaults, so a box written with only a width still loads.

The layer of a boxed text layer is the box, not the ink inside it: the PNG asset is exactly `width` by `max(min_height, wrapped text height)`, and the transform carries that size, so the box has an area to click and to resize. A type layer imported from PSD or PSB arrives the same way, with its box sized from the pixels Photoshop drew, which are kept until the text is edited. Alignment and spacing live inside the box, which is why a text layer without one keeps the tight ink raster and the appearance it always had. Resizing such a layer re-wraps the text at the new width rather than scaling the cached raster; canvas and image resizes follow the same rule, and a box too large for the raster budget keeps its pixels instead of failing the edit.
