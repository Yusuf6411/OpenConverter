# SVG (read only)

Crates: `oc-svg` (DOM, CSS, renderer), `oc-raster` (drawing), `oc-font` (text outlines), `oc-engine/src/svg.rs` (routes). An SVG is drawn at its own size, or at `width`/`height`, and then written as PNG, JPEG, WebP, BMP, TIFF, GIF, QOI or ICO. `.svgz` (gzip) is accepted.

## What is drawn
- Shapes: `rect` (with `rx`/`ry`), `circle`, `ellipse`, `line`, `polyline`, `polygon`, `path` (all commands, arcs included).
- Painting: fill and stroke with colours (hex, `rgb()`, `hsl()`, names, `currentColor`), opacity, fill rules, caps, joins, miter limit, dashes, `paint-order`, `vector-effect` is not read.
- Paint servers: linear and radial gradients (with focal point and `fr`, `spreadMethod`, `gradientTransform`, `href` inheritance, both unit systems) and patterns (`viewBox`, `patternTransform`, both unit systems).
- Structure: `g`, `a`, `switch`, `use`, `symbol`, nested `svg`, `viewBox`, `preserveAspectRatio`, `transform`, `display`, `visibility`.
- Styling: presentation attributes, `<style>` sheets (type, class, id, attribute, descendant, child, sibling and structural pseudo-class selectors, specificity, `!important`) and `style` attributes.
- Compositing: group opacity, `mix-blend-mode`, `isolation`, `clip-path` (with `clipPathUnits`, per-child `clip-rule`, nested clips), `mask` (luminance and alpha, `maskUnits`, `maskContentUnits`).
- Markers: `marker-start/mid/end`, `markerUnits`, `orient` (angle, `auto`, `auto-start-reverse`), `viewBox`, overflow clipping.
- Filters: `feGaussianBlur`, `feOffset`, `feFlood`, `feMerge`, `feBlend`, `feComposite` (all operators), `feColorMatrix`, `feComponentTransfer`, `feMorphology`, `feDropShadow`, primitive subregions, `linearRGB` and `sRGB` working spaces, and the CSS functions `blur`, `brightness`, `contrast`, `grayscale`, `sepia`, `saturate`, `hue-rotate`, `invert`, `opacity`, `drop-shadow`.
- Images: `data:` URIs of PNG, JPEG, GIF, WebP, BMP, and nested SVG.
- Text: `text`, `tspan`, per-character `x/y/dx/dy/rotate`, `text-anchor`, `letter-spacing`, `word-spacing`, underline/overline/line-through, fill and stroke, using installed fonts found by our own font database.

## How it was checked
`tools/svg_interop.py` draws 19 pictures (`tools/svg_cases.py`) with us and with Chromium and compares them; all 19 are within the bound (under 2 % of pixels differing by more than 48/255, mean difference under 3; text is allowed 6 % and 8 because glyph anti-aliasing differs). A fuzz target (`fuzz/fuzz_targets/svg.rs`) and hostile-input tests cover reference cycles, deep nesting, huge sizes and element-count blow-ups.

## Not done
- External references: only `data:` URIs are used for images; nothing is fetched and no other file is read.
- Text: layout uses advance widths and `kern` pairs; there is no OpenType shaping (ligatures, complex scripts, bidi), `textPath` is drawn along a straight line, `text-decoration` thickness is approximate, web fonts (`@font-face`) are ignored, and text inside a `clipPath` clips nothing. A note is attached whenever text is drawn.
- Filters: `feTurbulence`, `feImage`, `feTile`, `feConvolveMatrix`, `feDisplacementMap`, `feDiffuseLighting` and `feSpecularLighting` pass their input through with a note; `BackgroundImage`, `FillPaint` and `StrokePaint` inputs are transparent; edge modes are not applied.
- Animation (SMIL, CSS), scripting, `foreignObject`, `<view>` fragments, `vector-effect`, `context-fill`, `shape-rendering` hints, and `color-interpolation` for gradients are not supported.
- Output is not byte-identical to any browser; edges differ by anti-aliasing and filters by rounding.
