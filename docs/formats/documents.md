# Documents

Every document format is read into one model (`oc-document::model`: metadata, blocks, pictures) and
written from it, so any pair of formats converts through the same path, and plain text and PDF text
join in. Block types: heading (1-6), paragraph, list (ordered or not, start number, task state), quote,
code block, table (header row, column alignment), rule, page break. Inlines: text, emphasis, strong,
strike, underline, superscript, subscript, code, link, picture, line break. Raw HTML is kept only for
HTML and Markdown output.

| Format | Read | Write | Notes |
|---|---|---|---|
| Markdown | yes | yes | Own parser. CommonMark 0.31 spec examples: 655 of 655. GFM spec: 656 of 672 (the rest are CommonMark 0.29 emphasis differences, task-list markup and a few autolink edge cases). Front matter gives title, author, date, language; `data:` pictures become pictures of the document. |
| HTML | yes | yes | Own tolerant reader (not the full WHATWG tree builder: no foster parenting or adoption agency, tables and lists are repaired by simple rules), complete entity table. Writer produces one self-contained page. |
| DOCX | yes | yes | Styles (heading levels through `outlineLvl` and style names), numbering, runs, hyperlinks, fields, pictures, footnotes (moved to a list at the end), tables, text boxes. Checked against python-docx and LibreOffice. |
| ODT | yes | yes | Automatic and named styles, lists, tables, pictures, footnotes, metadata. Checked against LibreOffice both ways. |
| RTF | yes | yes | Control words, groups, destinations skipped when unknown, `\uN` with skip counts, code pages through `oc-text`, fonts (monospace detection), fields (hyperlinks), lists (`\listtext`), tables, PNG and JPEG pictures, `\info`. The writer includes a list table so Word and LibreOffice show real lists. |
| EPUB | yes | yes | Reader follows container, package and reading order, loads pictures from the package. Writer produces EPUB 3 with an EPUB 2 NCX, chapters split at the top heading (level 1, or 2 when there are no level 1 headings), `mimetype` stored first. |
| FB2 | yes | no | Title info, sections, poems, epigraphs, notes (moved to a list at the end), pictures from `<binary>`; the declared encoding is honoured. |
| PPTX | yes | no | Slide titles become level-1 headings, body placeholders become lists, tables and pictures are kept; slide order follows the presentation part. |
| ODP | yes | no | The same for OpenDocument slides. |

## PDF output

Documents, text and slide decks become PDF through our own layout engine (`oc-engine::docpdf`):
paragraphs with greedy line breaking, headings in a sans face with bookmarks (the PDF outline), nested
lists and quotes, code blocks, tables (column widths from the content, header row repeated on each page),
pictures (JPEG stored as is, others as Flate with a soft mask), links as annotations, document language
and title. Fonts are found on the computer (`oc-font::db`), subset and embedded as TrueType CID fonts
with a `ToUnicode` map so the text can be selected and searched. Options: `page_size` (a4, letter),
`margin` (points), `font_size`, `font` (serif, sans, mono or a family name). The first page can also be
written as a picture (`md -> png`).

## Not done

- No complex-script shaping, no bidi reordering, no UAX #14 line breaking, no hyphenation, no
  justification, no Knuth-Plass: lines are broken greedily at spaces, and Arabic, Hebrew and Indic text
  comes out left to right and unjoined (the PDF writer says so in a note). M6.13 is not done.
- No bundled fallback fonts: PDF output needs an embeddable TrueType font on the computer (Liberation,
  DejaVu, Noto, Arial, Times New Roman, ...). Without one the conversion fails with a message saying so.
  OpenType fonts with CFF outlines are not embedded (so most CJK fonts are skipped and the characters
  show as blanks with a loss note).
- No footnotes, headers and footers, columns, or page numbers in the PDF layout; footnotes of DOCX, ODT and
  FB2 are listed at the end. No PDF/A, no tagged PDF, no SVG output of documents.
- DOCX, ODT and RTF write a plain structure: no page setup, no table of contents, no tracked changes,
  comments, text colours, fonts, sizes or alignment beyond table cells. Readers drop the same things.
- DOCX and ODT: fields other than hyperlinks, equations, drawings, charts and SmartArt are not read.
- PPTX and ODP: layout, shapes, charts, animations and speaker notes are not read; slides do not render as
  slides (the PDF has one section per slide, not a picture of it). PPTX and ODP are not written.
- EPUB: the table of contents is by chapter only, covers are not written, DRM is not handled, `.fb2.zip`
  and `.epub` with fixed-layout pages are read as text.
- The document UI (M6.15: outline inspector, page preview, PDF/A) is not done; the document options show
  in the generic option cards.
