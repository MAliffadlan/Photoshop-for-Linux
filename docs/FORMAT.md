# Project format

A `.mectov` file is a ZIP archive containing `manifest.json` and lossless PNG assets. Version 1 uses `format: "me.silverl.mectov"`, a `document` object, and a `pixel_layers` list. Layer images are stored at `images/<UUID>.png`; masks use `images/<UUID>.mask.png`. Pixel bytes are excluded from JSON.

The document records canvas dimensions, DPI, layer order, IDs, parent groups, clipping references, opacity, blending, visibility, locks, transforms, adjustment parameters, and optional live shape styles, including line endpoints and width. Layers are stored bottom to top; each folder's subtree is composited together in hierarchy order. Masks have enabled/linked flags and an optional independent placement transform.

Text layers also record an optional `text` object with UTF-8 content, font family, pixel size, RGBA color, and bold, italic, underline, and strikethrough flags. Their PNG assets preserve the rendered appearance when fonts are unavailable on another machine. Fonts are discovered from the system and are not embedded in the project; editing unavailable fonts uses the bundled Inter Variable fallback. Text is limited to 16 KiB and font sizes to 1–1024 pixels. Older version 1 files without text metadata remain supported.

Transforms retain original source pixels. Optional perspective corners are normalized coordinates before affine scale/rotation/flip. Channel adjustments retain separate RGB curves/levels and seven hue ranges; adjustment layers also carry Photoshop's Black & White weights and tint, Color Balance's nine shifts with its luminosity flag, Invert, and Compositor 1.2.3's Add Noise, Gaussian Blur and Motion Blur. Layers blend with Photoshop's twenty-four modes, stored by name, so documents stay readable when a mode is added. Selections, current multi-selection, clipboard contents, and undo/redo snapshots are not serialized.

Saving validates the document, writes a sibling temporary archive, flushes it, and atomically replaces the destination. Loading validates IDs, hierarchy, clipping cycles, transforms, dimensions, metadata size, decompressed asset size, and aggregate image/mask budgets. Assets are decoded in memory, never extracted using archive paths. `.comp` package reads reject symlinked assets and paths outside the package.

Projects written before the rename carry an earlier identifier and the previous project extension. The reader accepts both identifiers and both extensions, so those projects load unchanged; saving always writes the `me.silverl.mectov` identifier and the `.mectov` extension.

Limits: 30,000 pixels per canvas/image dimension, 100 megapixels per canvas, 100 megapixels of layer assets plus 100 megapixels of masks, 10,000 layers, 64 nested group levels, 4 MiB manifest JSON, and 512 MiB encoded asset files.

The importer accepts Compositor package versions 1–9, including Swift's alternating-key enum dictionaries, individual color channels, mask placement/link flags, grain parameters, live shape styles and line geometry, alignment guides, layer effects, Photoshop's blend modes, the Black & White, Color Balance and Invert adjustments, and 1.2.3's Add Noise, Gaussian Blur and Motion Blur. Versions 8 and 9 both cover Compositor 1.2.1 and 1.2.2, which kept version 8; 1.2.3 and 1.2.4 write version 9, which is the version that allows the blur and noise adjustment layers. Import is one-way: Save creates a `.mectov` file and leaves the `.comp` package untouched.

## Guides, layer effects, and line shapes (versions 3–5)

Documents carrying alignment guides or layer effects are written as version 3, so older readers do not silently drop them. Documents containing live line shapes are written as version 5. The reader accepts versions 1–6, with version 6 covering the layout grid; ordinary documents remain version 1, documents with embedded RAW remain version 2, and documents with 1.2.3 blur/noise adjustment layers remain version 4. The adjustment kinds and blend modes added with Compositor 1.2.2 ride in the version their document already uses, as they do in `.comp` version 8: an older reader reports the unknown kind instead of dropping the layer. Compositor 1.2.3's blur and noise adjustment layers raise the file to version 4, because they redraw the backdrop in a pass of their own and a reader that does not know them cannot draw the document at all.

An optional `guides` list holds the alignment guides a Compositor project placed: each entry has an `axis` (`Horizontal` or `Vertical`) and a `position` in document pixels, a Y for horizontal guides and an X for vertical ones. Canvas Size and Image Size move or scale them, Crop drops the ones pushed off the canvas, and Flip Canvas mirrors only the guides running perpendicular to the flip. Up to 1,024 guides are stored, and Clear Guides in the View menu removes them. Rulers create, move, and delete these guides, and a guide dragged onto a ruler or released outside the canvas is removed.

A layer's optional `effects` object preserves Compositor's layer effects: `stroke`, `shadow`, `color_overlay`, `inner_shadow`, `outer_glow` and 1.2.3's `inner_glow`, each with its own geometry, color and opacity, plus an `enabled` flag that hides an effect rather than deleting it. Reading validates the source's ranges: sizes and blurs up to 500 px, shadow distances up to 5,000 px, angles within ±360°, and colors and opacities within 0–1. Effects stay with the layer through undo and project round trips and are rendered in source space before normal layer compositing.

Live line shapes store normalized `start` and `end` points plus `line_width`; the PNG asset is a cached raster for compatibility and is regenerated when the layer is resized.

## Embedded RAW (version 2)

Documents containing RAW layers are written as version 2, preventing older readers from silently dropping the source. Ordinary documents continue to use version 1, and the reader supports both versions.

## Backdrop blurs (version 4)

A document containing an Add Noise, Gaussian Blur or Motion Blur adjustment layer is written as version 4, and the reader accepts it. The three kinds are the ones Compositor 1.2.3 added, which its validator only allows in `.comp` version 9; how they import and draw is described in [PORTING.md](PORTING.md).

A RAW image layer has an optional `raw` object containing the original filename, camera metadata, and validated `DevelopSettings`. Its original bytes are stored in `raw/<layer UUID>.nef` (also for NRW sources); the current developed render stays in `images/<layer UUID>.png`. Loading restores the image without decoding the sensor data. Reopening Develop decodes the embedded bytes, so moving/deleting the original file does not break the project. Missing sources, invalid settings, and oversized sources fail validation. RAW byte buffers are shared by layer duplicates and history snapshots, and included in history's memory accounting.

The archive allows up to 30,001 entries to accommodate an image, mask and RAW source for each layer. Aggregate embedded RAW bytes are limited to 512 MiB. Develop settings store crop/overlay coordinates relative to the full oriented image; curve knots and all numeric parameters are finite and range-checked. Develop's transient preview, comparison view, clipping indicators, worker state, and local undo history are not serialized.

## Layout grid (version 6)

A document carrying a layout grid is written as version 6, so an older reader reports the unknown field instead of drawing a document whose alignment is no longer described. The reader accepts versions 1–6; every version below 6 is exactly what it accepted before, and documents without a grid keep the version they already had.

The document's optional `grid` object holds `spacing` in document pixels (1–5,000, default 32) and `subdivisions` per cell (1–100, default 1). The minor step the canvas draws and snaps to is `spacing / subdivisions`, coarsened by powers of ten only while a cell would stay under two screen pixels, so the file always stores the values the user typed. Showing, hiding, and configuring the grid are one undoable edit each, stored in the document rather than in the app's settings, so two windows on the same project can disagree about visibility without rewriting it. Opening a project that carries a grid shows it; opening one without hides it.
