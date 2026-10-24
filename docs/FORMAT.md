# VolSave image format, version 1

An image is one logical byte stream. It may be split into several files
(`name.dimg`, `name.dimg.002`, `name.dimg.003`, …) that are concatenated
in order. All integers are little endian.

```
FileHeader | Block* | Index | Footer
```

Blocks are appended in ascending volume order and never overlap. Volume
ranges without a block were not in use at backup time (or, in a
differential image, unchanged since the base image).

## File header

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 8 | magic `VSAVEIMG` |
| 8 | 4 | format version (1) |
| 12 | 4 | header length in bytes (= 96 + metadata length) |
| 16 | 16 | image id (UUID) |
| 32 | 16 | base image id (UUID); all zero for a full image |
| 48 | 4 | flags: bit 0 = differential; other bits must be 0 |
| 52 | 4 | block size (largest raw block, 1 byte to 16 MiB; default 1 MiB) |
| 56 | 4 | hash chunk size for diff hash lists (power of two; default 64 KiB) |
| 60 | 4 | metadata length *m* (at most 16 MiB) |
| 64 | *m* | metadata, UTF-8 JSON (see below) |
| 64 + *m* | 32 | BLAKE3 of bytes 0 … 64 + *m* |

Metadata is a JSON object. Readers ignore unknown fields and use defaults
for missing ones:

```json
{
  "tool": "VolSave 0.1.0",
  "created_unix": 1760000000,
  "comment": null,
  "source": {
    "kind": "volume",
    "device": "\\\\?\\Volume{...}\\",
    "size_bytes": 512105932800,
    "sector_size": 512,
    "filesystem": "NTFS",
    "label": "System",
    "disk_number": 0,
    "partition_number": 3,
    "partition_offset": 290455552,
    "hostname": "PC01"
  },
  "base_image_name": null
}
```

## Block

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 4 | tag `BLK1` |
| 4 | 4 | encoding: 0 = stored, 1 = zstd, 2 = all zeros |
| 8 | 8 | byte offset of the data within the source volume or disk |
| 16 | 4 | raw length (1 … 16 MiB, at most the header's block size) |
| 20 | 4 | stored length: = raw length (stored), < raw length (zstd), 0 (zeros) |
| 24 | 32 | BLAKE3 of the raw data |
| 56 | 4 | CRC-32 (IEEE) of bytes 0 … 56 |
| 60 | stored length | payload |

A zstd payload is a single zstd frame. Writers store a block uncompressed
when compression does not make it smaller.

## Index

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 4 | tag `IDX1` |
| 4 | 8 | entry count *n* |
| 12 | 20 × *n* | entries: volume offset (u64), stream position of the block header (u64), raw length (u32) |
| 12 + 20*n* | 4 | CRC-32 of all preceding index bytes |

## Footer (last 32 bytes of the stream)

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 8 | magic `VSAVEEND` |
| 8 | 8 | stream position of the index |
| 16 | 8 | index length in bytes |
| 24 | 4 | CRC-32 of footer bytes 0 … 24 |
| 28 | 4 | reserved, 0 |

The index must end exactly where the footer begins. If footer or index are
missing or damaged, readers rebuild the index by walking the blocks from the
end of the header and stop at the first block that is damaged or cut short.

## Diff hash list (`.dhsh`)

Written next to a full image. A differential backup hashes the current
volume chunk by chunk, compares with this list and stores only chunks whose
hash differs. Chunks without any used cluster are marked unused (all zero)
and never stored. The differential image records the full image id as its
base image id; `base_image_name` in its metadata names the full image file.

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 8 | magic `VSAVEHSH` |
| 8 | 4 | format version (1) |
| 12 | 4 | chunk size (power of two) |
| 16 | 16 | id of the image the list belongs to |
| 32 | 8 | volume size in bytes |
| 40 | 8 | chunk count (= volume size / chunk size, rounded up) |
| 48 | 16 × count | first 16 bytes of BLAKE3 per chunk; all zero = chunk not in use |
| … | 32 | BLAKE3 of all preceding bytes |
