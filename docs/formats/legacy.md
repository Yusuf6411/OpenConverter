# SQLite and legacy Office files

## SQLite (.sqlite, .db)
Read and written as a workbook: each table is a sheet, the first row is the header. The reader walks the B-trees itself (table leaf and interior pages, overflow chains, varint records); the writer lays out 4096-byte pages and builds the schema table. Column types are chosen from the cells when `detect_types` is on. Checked with Python's `sqlite3` (`PRAGMA integrity_check` is `ok` on our output, and files from sqlite3 read back equal). Not done: indexes, views, triggers, WAL, encryption, tables without a rowid.

## Compound files
`oc-archive::cfb` reads the Microsoft compound file container (FAT, mini-FAT, directory tree) with limits on sector chains and stream sizes, so loops and huge sizes are errors.

## XLS (BIFF8)
Read only. Sheets, shared strings (including strings split over CONTINUE records), numbers, RK values, booleans and dates (by number format). Formulas give their cached value. Encrypted files report `encrypted`.

## DOC
Read only. The piece table gives the text; fields become links, table markers become tables, page and line breaks are kept, and the summary information gives title, subject and author. Styles, pictures, footnotes, headers and formatting are not carried (note `doc.text_only`).

## PPT
Read only. Slide text in slide order: title text becomes a heading, body text a list. Layout, pictures, tables, shapes and notes are not carried (note `ppt.text_only`).

All three route through the document model, so a DOC or PPT converts to Markdown, HTML, DOCX, ODT, RTF, EPUB, PDF or pictures.
