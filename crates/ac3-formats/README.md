# ac3-formats

Dependency-free, bounded readers for the recovered European Ace Combat 3 data.

- `psy::unpack(bytes)` validates packed and unpacked checksums, decompresses the
  executable, and returns its embedded entry/load/GP/stack fields.
- `bph::parse(header, body_size)` preserves every directory record and verifies
  raw BPB extents. `Entry::extent` is `None` for special records; unsupported
  streaming flags are rejected. `Extent::bytes(body)` rechecks slice bounds.
- `ulz::decompress(bytes)` decodes versions 0 and 2, reporting stream consumption.

Input limits are 2 MiB for PS-Y output, 32 MiB for Ulz output, and 65,536 BPH
entries. Errors carry the format, offset, kind, and a readable explanation.
Unknown PS-Y fields and special/streaming BPH records have no invented semantics.
These readers load data; they do not translate or execute the game's MIPS code.

The source of truth for these ports is the locally validated Python code in
`tools/unpack_psy.py` and `tools/unpack_archives.py`. The resident executable and
five raw overlay extents also matched independent PCSX-Redux memory captures.
Ulz output has a Python corpus comparison, not a separate emulator execution test.

Synthetic boundary tests need no game files:

```powershell
cargo test -p ac3-formats
```

Opt-in local comparisons use the existing recovery workspace without bundling
any assets:

```powershell
$env:AC3_DATA_DIR = (Get-Location).Path
cargo test -p ac3-formats --test local_data -- --ignored --nocapture
```
