# rsnap

Disk and volume imaging for Windows, written in Rust. Backups use the
Volume Shadow Copy Service (VSS); images are compressed with zstd.

Status: early development. Command line backup (full and differential), restore and merge of single volumes are implemented but not yet tested with administrator rights; the GUI is not started yet.

## Layout

| Crate | Purpose |
| --- | --- |
| `crates/rsnap-format` | Image format: header, blocks, index, split files, diff hash lists ([spec](docs/FORMAT.md)) |
| `crates/rsnap-core` | Platform-independent engine: used-range planning, parallel backup pipeline, restore |
| `crates/rsnap-win` | Windows: volumes, disks, VSS snapshots, allocation bitmaps, raw device I/O |
| `crates/rsnap-cli` | Command line tool `rsnap` |

Planned: disk images, storage targets (SMB, FTP,
SFTP), Slint GUI.

## Building

```sh
cargo test
cargo build --release --target x86_64-pc-windows-gnu   # Windows EXE from Linux (needs mingw-w64)
```

## Usage (Windows, as administrator)

```
rsnap list
rsnap exclusions C:                                # what FilesNotToBackup leaves out
rsnap backup C: E:\backup\c.dimg                   # also writes E:\backup\c.dhsh
rsnap backup C: E:\backup\c-mon.dimg --diff E:\backup\c.dhsh
rsnap verify E:\backup\c.dimg
rsnap restore E:\backup\c-mon.dimg F:              # finds c.dimg by its id
rsnap merge E:\backup\c-mon.dimg E:\backup\c-new.dimg
```

Commands that work on an image append a log next to it (`c.dimg` -> `c.log`);
`--log FILE` writes elsewhere, `--no-log` turns it off, `--log-level debug`
adds details such as every VSS call. Exit codes: 0 = OK, 1 = error,
2 = OK with warnings.

The `zstd` crate compiles the zstd C library, so a C compiler is required
(`build-essential` on Debian/Ubuntu, MSVC Build Tools on Windows).
