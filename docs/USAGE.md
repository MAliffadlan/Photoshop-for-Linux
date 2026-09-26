# Using mectov

## Install and launch

mectov runs on Linux with Wayland or X11 and working Vulkan drivers. Mesa software Vulkan can also run the editor. Native file dialogs use the desktop portal; install the portal backend for your desktop if dialogs do not appear.

Download the package for your distribution from [Releases](https://github.com/MAliffadlan/Photoshop-for-Linux/releases). GitHub releases provide x86_64 builds.

On Debian or Ubuntu, install the downloaded `.deb` with APT so runtime dependencies are installed too:

```sh
sudo apt install ./mectov-*-linux-x86_64.deb
```

On Fedora or another compatible RPM distribution using DNF:

```sh
sudo dnf install ./mectov-*-linux-x86_64.rpm
```

Install one version at a time. These packages add mectov to the application menu and provide the `mectov` command. Remove them with `sudo apt remove mectov` or `sudo dnf remove mectov`.

For any other distribution, download the `mectov-<version>-linux-<architecture>.AppImage`, make it executable, and run it. It is self-contained and needs no installation, but it must be started from a terminal or from the file manager's "run as program" action:

```sh
chmod +x mectov-*-linux-x86_64.AppImage
./mectov-*-linux-x86_64.AppImage --demo
```

For a portable `mectov-<version>-linux-<architecture>.tar.gz` archive, extract it and run:

```sh
scripts/install.sh                 # installs under ~/.local
~/.local/bin/mectov
```

The installer adds a desktop launcher, icons, and the `.mectov` file association. Use `scripts/install.sh /custom/prefix` to choose another location, and add the installation prefix's `bin` directory to `PATH`. You can also run `bin/mectov` directly from the extracted archive.

Each download has a matching `.sha256` file. From the download directory, verify it with `sha256sum --check <package-file>.sha256`.

The separate `mectov-<version>-source.tar.gz` download is for rebuilding the application. The binary packages include a `SOURCES.md` notice under `share/doc/mectov` (`/usr/share/doc/mectov` for Debian/RPM installations) with a link to the matching source release.

Once `mectov` is on `PATH`:

```sh
mectov --demo
mectov photograph.png composition.mectov
```

To compile the application or produce a release archive, see the [development guide](DEVELOPMENT.md). Archives built on newer distributions may require a newer glibc; build from source on your target distribution if needed.

## Workspace

mectov has a charcoal theme, contextual controls above the canvas, a vertical tool rail, document tabs, and a Layers panel. The menu bar shares the titlebar with the window controls. Drag the titlebar to move the window, double-click to maximize, or drag an edge to resize.

## Editing tools

- **Layers:** folders, Photoshop's 24 blend modes in its own grouped order, opacity, visibility, locks, drag reordering/nesting, duplication, merge, clipping masks, linked or independent raster masks, live layer effects, and copy/paste of whole layers or folders when no selection is active. **Shift-+** and **Shift−** step the active layer's blend mode along that order.
- **Transforms:** move, scale, rotate, flip, free perspective distortion, numeric controls, shared transforms for several layers or folders, and snapping to edges and centers. Original source pixels remain available during transforms. Move / Transform has **Ignore Transparent Pixels** checked by default to select only at visible pixels; uncheck it to select and drag anywhere inside a layer's bounds.
- **Selections:** rectangle, ellipse, freehand/polygonal lasso, contiguous/global magic wand, add/subtract/intersect, inverse, feather, expand/contract, outline movement, and moving or duplicating selected pixels.
- **Paint:** brush with adjustable smoothing, eraser, aligned/unaligned clone stamp with layer/all-layer sampling, spot healing, blur/smudge, gradients, rectangles, rounded rectangles, ellipses, lines, and eyedropper. Live shapes redraw at the new size until their pixels are edited.
- **Text:** editable multiline text layers, searchable installed font families, size and color, bold, italic, underline, and strikethrough. Click with the Text tool (T) to place or edit text; double-click a text layer to reopen its live preview. Move and transform text with the Move tool.
- **Paragraph text boxes:** turn on **Paragraph box** in the text dialog to wrap text to a width instead of letting it run. Set the wrap width, alignment (left, center, right), line spacing, the gap between paragraphs, and a minimum height the box reserves. The box grows downward as the text needs more room, never shorter than its content. The Text tool outlines the box, the tool header carries a **Box width** number for re-flowing it, and dragging a side handle with the Move tool re-wraps the text as you drag rather than stretching it. A text layer without a box keeps the tight text it always had.
- **Adjustments:** editable Hue/Saturation color ranges, per-channel Levels and Curves, Exposure, Gradient Map, Grain, Add Noise (flat or bell-shaped, colored or monochromatic, on its own seed), Gaussian Blur, Motion Blur, Invert, Black & White (a weight per color family, with an optional sepia or cyanotype tint) and Color Balance (a separate shift for shadows, midtones and highlights, with Preserve Luminosity). Apply directly or add an adjustment layer, with live preview and selection coverage.
- **Backdrop blurs:** a Gaussian Blur or Motion Blur adjustment layer draws everything beneath it again, softened, the way Compositor's does; its opacity, mask and clipping decide how much of the softened backdrop replaces the sharp one, and layers above it are left alone. Apply one directly to a layer instead and it blurs that layer's own pixels. Either way the radius and distance count document pixels, so a zoomed-out preview softens proportionally less.
- **Filters:** Gaussian and Motion Blur with expanded bounds, Add Noise, selectable-color Vignette, Bloom / Glow, Tonal Contrast, Lens Correction, content-aware fill, and edge-color background removal. Filtering and expensive retouching run in cancellable workers.
- **RAW Develop:** camera RAW — Nikon NEF/NRW, Canon CR2/CR3/CRW, Sony ARW, Fujifilm RAF, DNG, Olympus ORF and the rest of the formats Rawler decodes — opens in a dedicated Develop workspace. Adjust white balance, exposure, tone curves, HSL, monochrome and split toning, noise reduction, sharpening, manual lens correction, crop, and brush/gradient masks. Compare before/after and inspect clipping or full-resolution detail. Develop creates an embedded RAW layer; double-click it to edit the original RAW again. Save `.mectov` to retain the source and adjustments, or export a 16-bit sRGB TIFF directly from Develop. See [RAW workflow and limits](RAW.md).
- **Documents:** independent tab histories, crop with ratio presets, Trim, canvas/image size, high-quality downsampling, pixel grid, alignment guides (see below), pasting copied images or image files as layers, Copy Merged, and save-on-close prompts. Undo retains up to 64 steps with a 512 MiB asset budget, keeping at least one step.
- **Rulers, guides, and the layout grid:** **View > Show Rulers** (Ctrl+R) draws document coordinates along the top and left of the canvas. Drag out of a ruler to pull a guide onto the canvas, drag an existing guide with the Move tool to move it, and release one over a ruler or outside the canvas to delete it. **View > Show Layout Grid** draws a grid over the canvas; **View > Grid Settings…** sets the cell spacing in pixels and how many subdivisions each cell carries. The grid is stored in the document, so it saves, undoes, and travels with the project.
- **Snapping:** with **View > Snap** on, moving or transforming a layer locks its edges, center, or bounds to the canvas, other layers, guides, and the grid. **View > Snap To** turns each of those targets off individually. Snapped lines are drawn while you drag: magenta for layers, cyan for guides and the grid. Hold Ctrl to suspend snapping for one drag.

See [keyboard shortcuts](SHORTCUTS.md) for tool and command bindings.

## Files and export

Use **File → Open Compositor Project…** to import an original `.comp` folder package. Save it as `.mectov` to keep editing on Linux. Image export supports PNG, JPEG, TIFF, and WebP; JPEG has a quality preview and PNG/JPEG carry print resolution.

Projects saved by earlier versions still open and load normally; the next save writes the current `.mectov` format alongside any file you choose.

HEIC/HEIF import uses the optional `heif-convert` executable from `libheif-examples`. SVG and SVGZ import is rendered directly in Rust at their intrinsic dimensions, through File → Open, Import Image as Layer, drag-and-drop, file-manager paste, or the command line. SVG external file and URL resources are not loaded; embedded `data:` resources are supported. PSD and PSB import accepts 8-bit RGB documents, including layers, pass-through groups, masks, clipping, visibility, opacity, supported blend modes, and Invert adjustment layers; raw and PackBits channel data are supported. Type layers import as editable text with their wording, font, size, colour and alignment; a layer with several styles takes the first run's, and Photoshop's own raster is kept until the text is edited. A type layer whose text cannot be read is kept as a pixel layer and reported in the status bar. ZIP compression, vector masks, smart objects, layer effects, non-default Blend If ranges, group-based clipping, and unsupported adjustments are reported as errors rather than silently flattened. PSD/PSB opens as a new editable mectov project and is saved as `.mectov`, never over the source file. Camera RAW import uses the bundled Rawler library and needs no external converter: Nikon NEF/NRW, Canon CR2/CR3/CRM/CRW, Sony ARW/ARI, Fujifilm RAF, Panasonic RW2, Olympus ORF, Pentax PEF, Samsung SRW, DNG, and the other rawler formats. Other image formats are decoded directly in Rust. See the [project format](FORMAT.md) for details about saved documents.

## Current limits

Document size follows the machine. One image may be up to 30,000 pixels per side and 200 megapixels, and a whole document — the canvas plus every layer and mask — up to 800 megapixels, but both ceilings are lowered to a share of the memory the machine reports, and the New Document dialog says the ceiling in force. A machine with 2 GiB of memory allows about 134 megapixels for one image and 402 for a document; from 4 GiB upwards the full 200 and 800 are allowed. Layer effects are baked through float buffers that need about 40 bytes for every pixel of the layer, so they are available up to one eightieth of the machine's memory in pixels — 27 megapixels on a 2 GiB machine, 107 on 8 GiB, and every layer the editor accepts from 16 GiB upwards. A layer past that is refused by name rather than drawn without its effects. A project saved on a large machine may be refused by a small one, again with the size and the memory in the message.

The photo editor uses an 8-bit sRGB raster pipeline. RAW Develop uses floating-point camera data and offers direct 16-bit TIFF output with an sRGB profile; its photo-layer render uses the existing 8-bit pipeline. Imported raster ICC profiles are not converted or preserved. `.comp` versions 1–9 can be imported, including alignment guides, layer effects, and live line shapes; a package from Compositor 1.3.2 is version 10 and is refused, because it can colour some letters of a text layer differently and mectov carries one colour per text layer. mectov writes its own project format and does not write the original macOS format. Selections and undo history are session state and are not saved in project archives.

The tool settings Compositor 1.2.1 keeps are kept here too: Auto Select, Show Transform Controls and Snap to Canvas and Layers keep their last values between runs. Command and tool shortcut remapping is available from **Help → Keyboard Shortcuts**; bindings persist between runs and can be exported or imported as JSON. Remove Background uses a border-color matte, intended for simple backgrounds, instead of Apple's foreground-recognition service. Content-aware fill and healing use a portable texture-matching implementation, so results differ from Compositor. Layer effects are rendered in source space and composited through the existing CPU/GPU pipeline. The canvas preview is capped at 4096 pixels per side; filter Apply uses full layer dimensions and export uses full document dimensions. Imports, saves, and raster adjustments can temporarily occupy the UI thread. Vulkan is the verified rendering path; OpenGL surface availability depends on the driver.
