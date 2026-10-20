# Format compatibility

Generated from the engine's own registry (`tools/gen_compat.py`). Every format listed is implemented by OpenConverter's own code.

## Text

| Format | Extensions | Read | Write | Converts to | Options |
| --- | --- | --- | --- | --- | --- |
| Plain text (`txt`) | .txt .text .log .md | yes | yes | `txt`, `tar`, `tar.gz`, `tar.bz2`, `tar.xz`, `tar.zst`, `tar.lz4`, `tar.lzma`, `zip`, `7z`, `cpio`, `ar`, `gz`, `bz2`, `xz`, `zst`, `lz4`, `lzma` | `from_encoding`, `to_encoding`, `line_ending`, `bom`, `lossy` |

## Images

| Format | Extensions | Read | Write | Converts to | Options |
| --- | --- | --- | --- | --- | --- |
| PNG (`png`) | .png .apng | yes | yes | `png`, `jpeg`, `bmp`, `tga`, `qoi`, `pnm`, `gif`, `ico`, `icns`, `dds`, `psd`, `hdr`, `exr`, `tiff`, `webp`, `tar`, `tar.gz`, `tar.bz2`, `tar.xz`, `tar.zst`, `tar.lz4`, `tar.lzma`, `zip`, `7z`, `cpio`, `ar`, `gz`, `bz2`, `xz`, `zst`, `lz4`, `lzma` | `level`, `adaptive_filter`, `background`, `width`, `height`, `rotate`, `flip`, `frames`, `profile`, `intent` |
| JPEG (`jpeg`) | .jpg .jpeg .jpe .jfif | yes | yes | `png`, `jpeg`, `bmp`, `tga`, `qoi`, `pnm`, `gif`, `ico`, `icns`, `dds`, `psd`, `hdr`, `exr`, `tiff`, `webp`, `tar`, `tar.gz`, `tar.bz2`, `tar.xz`, `tar.zst`, `tar.lz4`, `tar.lzma`, `zip`, `7z`, `cpio`, `ar`, `gz`, `bz2`, `xz`, `zst`, `lz4`, `lzma` | `quality`, `subsampling`, `optimize`, `progressive`, `lossless`, `trellis`, `background`, `width`, `height`, `rotate`, `flip`, `frames`, `profile`, `intent` |
| BMP (`bmp`) | .bmp .dib | yes | yes | `png`, `jpeg`, `bmp`, `tga`, `qoi`, `pnm`, `gif`, `ico`, `icns`, `dds`, `psd`, `hdr`, `exr`, `tiff`, `webp`, `tar`, `tar.gz`, `tar.bz2`, `tar.xz`, `tar.zst`, `tar.lz4`, `tar.lzma`, `zip`, `7z`, `cpio`, `ar`, `gz`, `bz2`, `xz`, `zst`, `lz4`, `lzma` | `background`, `width`, `height`, `rotate`, `flip`, `frames`, `profile`, `intent` |
| TGA (`tga`) | .tga .icb .vda .vst | yes | yes | `png`, `jpeg`, `bmp`, `tga`, `qoi`, `pnm`, `gif`, `ico`, `icns`, `dds`, `psd`, `hdr`, `exr`, `tiff`, `webp`, `tar`, `tar.gz`, `tar.bz2`, `tar.xz`, `tar.zst`, `tar.lz4`, `tar.lzma`, `zip`, `7z`, `cpio`, `ar`, `gz`, `bz2`, `xz`, `zst`, `lz4`, `lzma` | `rle`, `background`, `width`, `height`, `rotate`, `flip`, `frames`, `profile`, `intent` |
| QOI (`qoi`) | .qoi | yes | yes | `png`, `jpeg`, `bmp`, `tga`, `qoi`, `pnm`, `gif`, `ico`, `icns`, `dds`, `psd`, `hdr`, `exr`, `tiff`, `webp`, `tar`, `tar.gz`, `tar.bz2`, `tar.xz`, `tar.zst`, `tar.lz4`, `tar.lzma`, `zip`, `7z`, `cpio`, `ar`, `gz`, `bz2`, `xz`, `zst`, `lz4`, `lzma` | `background`, `width`, `height`, `rotate`, `flip`, `frames`, `profile`, `intent` |
| Netpbm (`pnm`) | .pnm .pbm .pgm .ppm .pam | yes | yes | `png`, `jpeg`, `bmp`, `tga`, `qoi`, `pnm`, `gif`, `ico`, `icns`, `dds`, `psd`, `hdr`, `exr`, `tiff`, `webp`, `tar`, `tar.gz`, `tar.bz2`, `tar.xz`, `tar.zst`, `tar.lz4`, `tar.lzma`, `zip`, `7z`, `cpio`, `ar`, `gz`, `bz2`, `xz`, `zst`, `lz4`, `lzma` | `ascii`, `background`, `width`, `height`, `rotate`, `flip`, `frames`, `profile`, `intent` |
| GIF (`gif`) | .gif | yes | yes | `png`, `jpeg`, `bmp`, `tga`, `qoi`, `pnm`, `gif`, `ico`, `icns`, `dds`, `psd`, `hdr`, `exr`, `tiff`, `webp`, `tar`, `tar.gz`, `tar.bz2`, `tar.xz`, `tar.zst`, `tar.lz4`, `tar.lzma`, `zip`, `7z`, `cpio`, `ar`, `gz`, `bz2`, `xz`, `zst`, `lz4`, `lzma` | `dither`, `interlace`, `background`, `width`, `height`, `rotate`, `flip`, `frames`, `profile`, `intent` |
| ICO / CUR (`ico`) | .ico .cur | yes | yes | `png`, `jpeg`, `bmp`, `tga`, `qoi`, `pnm`, `gif`, `ico`, `icns`, `dds`, `psd`, `hdr`, `exr`, `tiff`, `webp`, `tar`, `tar.gz`, `tar.bz2`, `tar.xz`, `tar.zst`, `tar.lz4`, `tar.lzma`, `zip`, `7z`, `cpio`, `ar`, `gz`, `bz2`, `xz`, `zst`, `lz4`, `lzma` | `sizes`, `background`, `width`, `height`, `rotate`, `flip`, `frames`, `profile`, `intent` |
| ICNS (`icns`) | .icns | yes | yes | `png`, `jpeg`, `bmp`, `tga`, `qoi`, `pnm`, `gif`, `ico`, `icns`, `dds`, `psd`, `hdr`, `exr`, `tiff`, `webp`, `tar`, `tar.gz`, `tar.bz2`, `tar.xz`, `tar.zst`, `tar.lz4`, `tar.lzma`, `zip`, `7z`, `cpio`, `ar`, `gz`, `bz2`, `xz`, `zst`, `lz4`, `lzma` | `sizes`, `background`, `width`, `height`, `rotate`, `flip`, `frames`, `profile`, `intent` |
| DDS (`dds`) | .dds | yes | yes | `png`, `jpeg`, `bmp`, `tga`, `qoi`, `pnm`, `gif`, `ico`, `icns`, `dds`, `psd`, `hdr`, `exr`, `tiff`, `webp`, `tar`, `tar.gz`, `tar.bz2`, `tar.xz`, `tar.zst`, `tar.lz4`, `tar.lzma`, `zip`, `7z`, `cpio`, `ar`, `gz`, `bz2`, `xz`, `zst`, `lz4`, `lzma` | `compression`, `background`, `width`, `height`, `rotate`, `flip`, `frames`, `profile`, `intent` |
| PSD (`psd`) | .psd .psb | yes | yes | `png`, `jpeg`, `bmp`, `tga`, `qoi`, `pnm`, `gif`, `ico`, `icns`, `dds`, `psd`, `hdr`, `exr`, `tiff`, `webp`, `tar`, `tar.gz`, `tar.bz2`, `tar.xz`, `tar.zst`, `tar.lz4`, `tar.lzma`, `zip`, `7z`, `cpio`, `ar`, `gz`, `bz2`, `xz`, `zst`, `lz4`, `lzma` | `rle`, `background`, `width`, `height`, `rotate`, `flip`, `frames`, `profile`, `intent` |
| Radiance HDR (`hdr`) | .hdr .pic | yes | yes | `png`, `jpeg`, `bmp`, `tga`, `qoi`, `pnm`, `gif`, `ico`, `icns`, `dds`, `psd`, `hdr`, `exr`, `tiff`, `webp`, `tar`, `tar.gz`, `tar.bz2`, `tar.xz`, `tar.zst`, `tar.lz4`, `tar.lzma`, `zip`, `7z`, `cpio`, `ar`, `gz`, `bz2`, `xz`, `zst`, `lz4`, `lzma` | `background`, `width`, `height`, `rotate`, `flip`, `frames`, `profile`, `intent` |
| OpenEXR (`exr`) | .exr | yes | yes | `png`, `jpeg`, `bmp`, `tga`, `qoi`, `pnm`, `gif`, `ico`, `icns`, `dds`, `psd`, `hdr`, `exr`, `tiff`, `webp`, `tar`, `tar.gz`, `tar.bz2`, `tar.xz`, `tar.zst`, `tar.lz4`, `tar.lzma`, `zip`, `7z`, `cpio`, `ar`, `gz`, `bz2`, `xz`, `zst`, `lz4`, `lzma` | `compression`, `background`, `width`, `height`, `rotate`, `flip`, `frames`, `profile`, `intent` |
| TIFF (`tiff`) | .tif .tiff | yes | yes | `png`, `jpeg`, `bmp`, `tga`, `qoi`, `pnm`, `gif`, `ico`, `icns`, `dds`, `psd`, `hdr`, `exr`, `tiff`, `webp`, `tar`, `tar.gz`, `tar.bz2`, `tar.xz`, `tar.zst`, `tar.lz4`, `tar.lzma`, `zip`, `7z`, `cpio`, `ar`, `gz`, `bz2`, `xz`, `zst`, `lz4`, `lzma` | `compression`, `predictor`, `background`, `width`, `height`, `rotate`, `flip`, `frames`, `profile`, `intent` |
| WebP (`webp`) | .webp | yes | yes | `png`, `jpeg`, `bmp`, `tga`, `qoi`, `pnm`, `gif`, `ico`, `icns`, `dds`, `psd`, `hdr`, `exr`, `tiff`, `webp`, `tar`, `tar.gz`, `tar.bz2`, `tar.xz`, `tar.zst`, `tar.lz4`, `tar.lzma`, `zip`, `7z`, `cpio`, `ar`, `gz`, `bz2`, `xz`, `zst`, `lz4`, `lzma` | `lossless`, `quality`, `effort`, `clean_transparent`, `background`, `width`, `height`, `rotate`, `flip`, `frames`, `profile`, `intent` |

## Tabular data

| Format | Extensions | Read | Write | Converts to | Options |
| --- | --- | --- | --- | --- | --- |
| CSV (`csv`) | .csv | yes | yes | `csv`, `tsv`, `json`, `xlsx`, `ods`, `tar`, `tar.gz`, `tar.bz2`, `tar.xz`, `tar.zst`, `tar.lz4`, `tar.lzma`, `zip`, `7z`, `cpio`, `ar`, `gz`, `bz2`, `xz`, `zst`, `lz4`, `lzma` | `source_delimiter`, `has_header`, `source_encoding`, `sheet`, `delimiter`, `newline`, `bom` |
| TSV (`tsv`) | .tsv .tab | yes | yes | `csv`, `tsv`, `json`, `xlsx`, `ods`, `tar`, `tar.gz`, `tar.bz2`, `tar.xz`, `tar.zst`, `tar.lz4`, `tar.lzma`, `zip`, `7z`, `cpio`, `ar`, `gz`, `bz2`, `xz`, `zst`, `lz4`, `lzma` | `source_delimiter`, `has_header`, `source_encoding`, `sheet`, `newline`, `bom` |
| JSON table (`json`) | .json | yes | yes | `csv`, `tsv`, `json`, `xlsx`, `ods`, `tar`, `tar.gz`, `tar.bz2`, `tar.xz`, `tar.zst`, `tar.lz4`, `tar.lzma`, `zip`, `7z`, `cpio`, `ar`, `gz`, `bz2`, `xz`, `zst`, `lz4`, `lzma` | `source_delimiter`, `has_header`, `source_encoding`, `sheet`, `indent`, `ensure_ascii` |
| XLSX (`xlsx`) | .xlsx .xlsm | yes | yes | `csv`, `tsv`, `json`, `xlsx`, `ods`, `tar`, `tar.gz`, `tar.bz2`, `tar.xz`, `tar.zst`, `tar.lz4`, `tar.lzma`, `zip`, `7z`, `cpio`, `ar`, `gz`, `bz2`, `xz`, `zst`, `lz4`, `lzma` | `source_delimiter`, `has_header`, `source_encoding`, `sheet`, `sheet_name`, `detect_types`, `detect_dates`, `style_header` |
| ODS (`ods`) | .ods | yes | yes | `csv`, `tsv`, `json`, `xlsx`, `ods`, `tar`, `tar.gz`, `tar.bz2`, `tar.xz`, `tar.zst`, `tar.lz4`, `tar.lzma`, `zip`, `7z`, `cpio`, `ar`, `gz`, `bz2`, `xz`, `zst`, `lz4`, `lzma` | `source_delimiter`, `has_header`, `source_encoding`, `sheet`, `sheet_name`, `detect_types`, `detect_dates`, `style_header` |

## Archives

| Format | Extensions | Read | Write | Converts to | Options |
| --- | --- | --- | --- | --- | --- |
| TAR (`tar`) | .tar | yes | yes | `tar`, `tar.gz`, `tar.bz2`, `tar.xz`, `tar.zst`, `tar.lz4`, `tar.lzma`, `zip`, `7z`, `cpio`, `ar`, `gz`, `bz2`, `xz`, `zst`, `lz4`, `lzma` | `source_password` |
| TAR + gzip (`tar.gz`) | .tar.gz .tgz | yes | yes | `tar`, `tar.gz`, `tar.bz2`, `tar.xz`, `tar.zst`, `tar.lz4`, `tar.lzma`, `zip`, `7z`, `cpio`, `ar`, `gz`, `bz2`, `xz`, `zst`, `lz4`, `lzma` | `source_password`, `level` |
| TAR + bzip2 (`tar.bz2`) | .tar.bz2 .tbz2 .tbz | yes | yes | `tar`, `tar.gz`, `tar.bz2`, `tar.xz`, `tar.zst`, `tar.lz4`, `tar.lzma`, `zip`, `7z`, `cpio`, `ar`, `gz`, `bz2`, `xz`, `zst`, `lz4`, `lzma` | `source_password`, `level` |
| TAR + xz (`tar.xz`) | .tar.xz .txz | yes | yes | `tar`, `tar.gz`, `tar.bz2`, `tar.xz`, `tar.zst`, `tar.lz4`, `tar.lzma`, `zip`, `7z`, `cpio`, `ar`, `gz`, `bz2`, `xz`, `zst`, `lz4`, `lzma` | `source_password`, `level` |
| TAR + Zstandard (`tar.zst`) | .tar.zst .tzst | yes | yes | `tar`, `tar.gz`, `tar.bz2`, `tar.xz`, `tar.zst`, `tar.lz4`, `tar.lzma`, `zip`, `7z`, `cpio`, `ar`, `gz`, `bz2`, `xz`, `zst`, `lz4`, `lzma` | `source_password`, `level` |
| TAR + LZ4 (`tar.lz4`) | .tar.lz4 | yes | yes | `tar`, `tar.gz`, `tar.bz2`, `tar.xz`, `tar.zst`, `tar.lz4`, `tar.lzma`, `zip`, `7z`, `cpio`, `ar`, `gz`, `bz2`, `xz`, `zst`, `lz4`, `lzma` | `source_password`, `level` |
| TAR + LZMA (`tar.lzma`) | .tar.lzma .tlz | yes | yes | `tar`, `tar.gz`, `tar.bz2`, `tar.xz`, `tar.zst`, `tar.lz4`, `tar.lzma`, `zip`, `7z`, `cpio`, `ar`, `gz`, `bz2`, `xz`, `zst`, `lz4`, `lzma` | `source_password`, `level` |
| TAR + compress (`tar.z`) | .tar.z .taz | yes | no | `tar`, `tar.gz`, `tar.bz2`, `tar.xz`, `tar.zst`, `tar.lz4`, `tar.lzma`, `zip`, `7z`, `cpio`, `ar`, `gz`, `bz2`, `xz`, `zst`, `lz4`, `lzma` | `source_password` |
| ZIP (`zip`) | .zip | yes | yes | `tar`, `tar.gz`, `tar.bz2`, `tar.xz`, `tar.zst`, `tar.lz4`, `tar.lzma`, `zip`, `7z`, `cpio`, `ar`, `gz`, `bz2`, `xz`, `zst`, `lz4`, `lzma` | `source_password`, `level`, `method`, `password`, `encryption`, `comment` |
| 7z (`7z`) | .7z | yes | yes | `tar`, `tar.gz`, `tar.bz2`, `tar.xz`, `tar.zst`, `tar.lz4`, `tar.lzma`, `zip`, `7z`, `cpio`, `ar`, `gz`, `bz2`, `xz`, `zst`, `lz4`, `lzma` | `source_password`, `level`, `method`, `password` |
| CPIO (`cpio`) | .cpio | yes | yes | `tar`, `tar.gz`, `tar.bz2`, `tar.xz`, `tar.zst`, `tar.lz4`, `tar.lzma`, `zip`, `7z`, `cpio`, `ar`, `gz`, `bz2`, `xz`, `zst`, `lz4`, `lzma` | `source_password` |
| ar archive (`ar`) | .ar .a .deb | yes | yes | `tar`, `tar.gz`, `tar.bz2`, `tar.xz`, `tar.zst`, `tar.lz4`, `tar.lzma`, `zip`, `7z`, `cpio`, `ar`, `gz`, `bz2`, `xz`, `zst`, `lz4`, `lzma` | `source_password` |
| ISO 9660 image (`iso`) | .iso | yes | no | `tar`, `tar.gz`, `tar.bz2`, `tar.xz`, `tar.zst`, `tar.lz4`, `tar.lzma`, `zip`, `7z`, `cpio`, `ar`, `gz`, `bz2`, `xz`, `zst`, `lz4`, `lzma` | `source_password` |
| Cabinet (`cab`) | .cab | yes | no | `tar`, `tar.gz`, `tar.bz2`, `tar.xz`, `tar.zst`, `tar.lz4`, `tar.lzma`, `zip`, `7z`, `cpio`, `ar`, `gz`, `bz2`, `xz`, `zst`, `lz4`, `lzma` | `source_password` |
| gzip (`gz`) | .gz | yes | yes | `tar`, `tar.gz`, `tar.bz2`, `tar.xz`, `tar.zst`, `tar.lz4`, `tar.lzma`, `zip`, `7z`, `cpio`, `ar`, `gz`, `bz2`, `xz`, `zst`, `lz4`, `lzma` | `source_password`, `level` |
| bzip2 (`bz2`) | .bz2 | yes | yes | `tar`, `tar.gz`, `tar.bz2`, `tar.xz`, `tar.zst`, `tar.lz4`, `tar.lzma`, `zip`, `7z`, `cpio`, `ar`, `gz`, `bz2`, `xz`, `zst`, `lz4`, `lzma` | `source_password`, `level` |
| xz (`xz`) | .xz | yes | yes | `tar`, `tar.gz`, `tar.bz2`, `tar.xz`, `tar.zst`, `tar.lz4`, `tar.lzma`, `zip`, `7z`, `cpio`, `ar`, `gz`, `bz2`, `xz`, `zst`, `lz4`, `lzma` | `source_password`, `level` |
| Zstandard (`zst`) | .zst | yes | yes | `tar`, `tar.gz`, `tar.bz2`, `tar.xz`, `tar.zst`, `tar.lz4`, `tar.lzma`, `zip`, `7z`, `cpio`, `ar`, `gz`, `bz2`, `xz`, `zst`, `lz4`, `lzma` | `source_password`, `level` |
| LZ4 (`lz4`) | .lz4 | yes | yes | `tar`, `tar.gz`, `tar.bz2`, `tar.xz`, `tar.zst`, `tar.lz4`, `tar.lzma`, `zip`, `7z`, `cpio`, `ar`, `gz`, `bz2`, `xz`, `zst`, `lz4`, `lzma` | `source_password`, `level` |
| LZMA (`lzma`) | .lzma | yes | yes | `tar`, `tar.gz`, `tar.bz2`, `tar.xz`, `tar.zst`, `tar.lz4`, `tar.lzma`, `zip`, `7z`, `cpio`, `ar`, `gz`, `bz2`, `xz`, `zst`, `lz4`, `lzma` | `source_password`, `level` |
| compress (.Z) (`z`) | .z | yes | no | `tar`, `tar.gz`, `tar.bz2`, `tar.xz`, `tar.zst`, `tar.lz4`, `tar.lzma`, `zip`, `7z`, `cpio`, `ar`, `gz`, `bz2`, `xz`, `zst`, `lz4`, `lzma` | `source_password` |

