# Format compatibility

Generated from the engine's own registry (`tools/gen_compat.py`). Every format listed is implemented by OpenConverter's own code.

## Text

| Format | Extensions | Read | Write | Converts to | Options |
| --- | --- | --- | --- | --- | --- |
| Plain text (`txt`) | .txt .text .log .md | yes | yes | `txt` | `from_encoding`, `to_encoding`, `line_ending`, `bom`, `lossy` |

## Images

| Format | Extensions | Read | Write | Converts to | Options |
| --- | --- | --- | --- | --- | --- |
| PNG (`png`) | .png .apng | yes | yes | `png`, `jpeg`, `bmp`, `tga`, `qoi`, `pnm`, `gif`, `ico`, `icns`, `dds`, `psd`, `hdr`, `exr`, `tiff`, `webp` | `level`, `adaptive_filter`, `background`, `width`, `height`, `rotate`, `flip`, `frames`, `profile`, `intent` |
| JPEG (`jpeg`) | .jpg .jpeg .jpe .jfif | yes | yes | `png`, `jpeg`, `bmp`, `tga`, `qoi`, `pnm`, `gif`, `ico`, `icns`, `dds`, `psd`, `hdr`, `exr`, `tiff`, `webp` | `quality`, `subsampling`, `optimize`, `progressive`, `lossless`, `trellis`, `background`, `width`, `height`, `rotate`, `flip`, `frames`, `profile`, `intent` |
| BMP (`bmp`) | .bmp .dib | yes | yes | `png`, `jpeg`, `bmp`, `tga`, `qoi`, `pnm`, `gif`, `ico`, `icns`, `dds`, `psd`, `hdr`, `exr`, `tiff`, `webp` | `background`, `width`, `height`, `rotate`, `flip`, `frames`, `profile`, `intent` |
| TGA (`tga`) | .tga .icb .vda .vst | yes | yes | `png`, `jpeg`, `bmp`, `tga`, `qoi`, `pnm`, `gif`, `ico`, `icns`, `dds`, `psd`, `hdr`, `exr`, `tiff`, `webp` | `rle`, `background`, `width`, `height`, `rotate`, `flip`, `frames`, `profile`, `intent` |
| QOI (`qoi`) | .qoi | yes | yes | `png`, `jpeg`, `bmp`, `tga`, `qoi`, `pnm`, `gif`, `ico`, `icns`, `dds`, `psd`, `hdr`, `exr`, `tiff`, `webp` | `background`, `width`, `height`, `rotate`, `flip`, `frames`, `profile`, `intent` |
| Netpbm (`pnm`) | .pnm .pbm .pgm .ppm .pam | yes | yes | `png`, `jpeg`, `bmp`, `tga`, `qoi`, `pnm`, `gif`, `ico`, `icns`, `dds`, `psd`, `hdr`, `exr`, `tiff`, `webp` | `ascii`, `background`, `width`, `height`, `rotate`, `flip`, `frames`, `profile`, `intent` |
| GIF (`gif`) | .gif | yes | yes | `png`, `jpeg`, `bmp`, `tga`, `qoi`, `pnm`, `gif`, `ico`, `icns`, `dds`, `psd`, `hdr`, `exr`, `tiff`, `webp` | `dither`, `interlace`, `background`, `width`, `height`, `rotate`, `flip`, `frames`, `profile`, `intent` |
| ICO / CUR (`ico`) | .ico .cur | yes | yes | `png`, `jpeg`, `bmp`, `tga`, `qoi`, `pnm`, `gif`, `ico`, `icns`, `dds`, `psd`, `hdr`, `exr`, `tiff`, `webp` | `sizes`, `background`, `width`, `height`, `rotate`, `flip`, `frames`, `profile`, `intent` |
| ICNS (`icns`) | .icns | yes | yes | `png`, `jpeg`, `bmp`, `tga`, `qoi`, `pnm`, `gif`, `ico`, `icns`, `dds`, `psd`, `hdr`, `exr`, `tiff`, `webp` | `sizes`, `background`, `width`, `height`, `rotate`, `flip`, `frames`, `profile`, `intent` |
| DDS (`dds`) | .dds | yes | yes | `png`, `jpeg`, `bmp`, `tga`, `qoi`, `pnm`, `gif`, `ico`, `icns`, `dds`, `psd`, `hdr`, `exr`, `tiff`, `webp` | `compression`, `background`, `width`, `height`, `rotate`, `flip`, `frames`, `profile`, `intent` |
| PSD (`psd`) | .psd .psb | yes | yes | `png`, `jpeg`, `bmp`, `tga`, `qoi`, `pnm`, `gif`, `ico`, `icns`, `dds`, `psd`, `hdr`, `exr`, `tiff`, `webp` | `rle`, `background`, `width`, `height`, `rotate`, `flip`, `frames`, `profile`, `intent` |
| Radiance HDR (`hdr`) | .hdr .pic | yes | yes | `png`, `jpeg`, `bmp`, `tga`, `qoi`, `pnm`, `gif`, `ico`, `icns`, `dds`, `psd`, `hdr`, `exr`, `tiff`, `webp` | `background`, `width`, `height`, `rotate`, `flip`, `frames`, `profile`, `intent` |
| OpenEXR (`exr`) | .exr | yes | yes | `png`, `jpeg`, `bmp`, `tga`, `qoi`, `pnm`, `gif`, `ico`, `icns`, `dds`, `psd`, `hdr`, `exr`, `tiff`, `webp` | `compression`, `background`, `width`, `height`, `rotate`, `flip`, `frames`, `profile`, `intent` |
| TIFF (`tiff`) | .tif .tiff | yes | yes | `png`, `jpeg`, `bmp`, `tga`, `qoi`, `pnm`, `gif`, `ico`, `icns`, `dds`, `psd`, `hdr`, `exr`, `tiff`, `webp` | `compression`, `predictor`, `background`, `width`, `height`, `rotate`, `flip`, `frames`, `profile`, `intent` |
| WebP (`webp`) | .webp | yes | yes | `png`, `jpeg`, `bmp`, `tga`, `qoi`, `pnm`, `gif`, `ico`, `icns`, `dds`, `psd`, `hdr`, `exr`, `tiff`, `webp` | `lossless`, `quality`, `effort`, `clean_transparent`, `background`, `width`, `height`, `rotate`, `flip`, `frames`, `profile`, `intent` |

## Tabular data

| Format | Extensions | Read | Write | Converts to | Options |
| --- | --- | --- | --- | --- | --- |
| CSV (`csv`) | .csv | yes | yes | `csv`, `tsv`, `json` | `source_delimiter`, `has_header`, `source_encoding`, `delimiter`, `newline`, `bom` |
| TSV (`tsv`) | .tsv .tab | yes | yes | `csv`, `tsv`, `json` | `source_delimiter`, `has_header`, `source_encoding`, `newline`, `bom` |
| JSON table (`json`) | .json | yes | yes | `csv`, `tsv`, `json` | `source_delimiter`, `has_header`, `source_encoding`, `indent`, `ensure_ascii` |

