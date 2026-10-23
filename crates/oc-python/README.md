# openconverter (Python)

```python
import openconverter as oc

r = oc.convert("photo.png", "photo.webp", quality=80)
print(r.route, r.bytes_in, "->", r.bytes_out)
for n in r.notes:            # what the conversion changed or could not keep
    print(n["severity"], n["code"], n["message"])

oc.info("photo.png")         # {'format': 'png', 'width': 640, ...}
oc.formats()                 # every format, with extensions and read/write support
```

Failures raise `openconverter.ConversionError` with a stable `.code`. Keyword arguments are the format options listed by `openconverter formats`. Conversions release the GIL. See `docs/python.md` in the repository.
