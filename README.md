# VolSave

Disk and volume imaging for Windows, written in Rust. Backups use the
Volume Shadow Copy Service (VSS); images are compressed with zstd.

Status: early development. The command line tool backs up, verifies,
restores and merges single volumes and whole disks (partition table, boot
code and all partitions); a whole-disk clone of Windows 10 has been restored
and booted. The GUI is not started yet.

## Layout

| Crate | Purpose |
| --- | --- |
| `crates/volsave-format` | Image format: header, blocks, index, split files, diff hash lists ([spec](docs/FORMAT.md)) |
| `crates/volsave-core` | Platform-independent engine: used-range planning, parallel backup pipeline, restore |
| `crates/volsave-win` | Windows: volumes, disks, VSS snapshots, allocation bitmaps, raw device I/O |
| `crates/volsave-ops` | Operations shared by the command line tool and GUIs: backup, restore, opening images |
| `crates/volsave-cli` | Command line tool `volsave` |

Network targets (SMB, FTP, SFTP), encryption and a graphical interface are part of
the separate commercial product VolSave Pro, which builds on these crates.

## Building

```sh
cargo test
cargo build --release --target x86_64-pc-windows-gnu   # Windows EXE from Linux (needs mingw-w64)
```

## Usage (Windows, as administrator)

```
volsave list
volsave exclusions C:                              # what FilesNotToBackup leaves out
volsave backup C: E:\backup\c.dimg                 # also writes E:\backup\c.dhsh
volsave backup C: E:\backup\c-mon.dimg --diff E:\backup\c.dhsh
volsave verify E:\backup\c.dimg
volsave restore E:\backup\c-mon.dimg F:            # finds c.dimg by its id
volsave merge E:\backup\c-mon.dimg E:\backup\c-new.dimg

volsave backup HD0 E:\backup\disk0.dimg            # whole disk 0
volsave info E:\backup\disk0.dimg                  # shows the partitions
volsave restore E:\backup\disk0.dimg HD1 --verify  # clone onto disk 1, read back
volsave restore E:\backup\disk0.dimg F: --partition 2
```

A restored disk keeps the disk ID of the original. While both disks are
attached Windows keeps the clone offline; do not set it online there, or
Windows gives it a new ID and it no longer starts. Remove the original disk
and boot from the clone.

Commands that work on an image append a log next to it (`c.dimg` -> `c.log`);
`--log FILE` writes elsewhere, `--no-log` turns it off, `--log-level debug`
adds details such as every VSS call. Exit codes: 0 = OK, 1 = error,
2 = OK with warnings.

The `zstd` crate compiles the zstd C library, so a C compiler is required
(`build-essential` on Debian/Ubuntu, MSVC Build Tools on Windows).

## License

The command line tool and the libraries in this repository are free
software under the GNU General Public License, version 3 or later
([LICENSE](LICENSE)). The planned graphical interface will be a separate,
commercial product.
