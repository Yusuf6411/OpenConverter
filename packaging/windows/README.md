# Windows shell integration

The installer adds a classic context-menu entry through the registry
(`HKCU\Software\Classes\*\shell\OpenConverter`), which Windows 11 shows under
"Show more options".

Not done: the Windows 11 top-level menu entry. It needs an `IExplorerCommand` COM handler
shipped in a signed sparse MSIX package, which cannot be built or tested here.
