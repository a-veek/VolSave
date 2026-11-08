# VolSave

*Deutsche Beschreibung: [unten](#deutsch). Changes per version: [CHANGELOG](CHANGELOG.md).*

Disk and volume imaging for Windows, written in Rust. Backups use the
Volume Shadow Copy Service (VSS); images are compressed with zstd.

Status: early development, but in use. The command line tool backs up,
verifies, restores and merges single volumes and whole disks (partition
table, boot code and all partitions). Whole-disk clones of Windows 10 and 11
(BIOS/MBR and UEFI/GPT) have been restored and booted, in virtual machines
and on real hardware, also from Windows PE.

Download: `volsave.exe` under [Releases](../../releases); it runs on Windows
10/11 and in Windows PE, without installation.

## Layout

| Crate | Purpose |
| --- | --- |
| `crates/volsave-format` | Image format: header, blocks, index, split files, diff hash lists ([spec](docs/FORMAT.md)) |
| `crates/volsave-core` | Platform-independent engine: used-range planning, parallel backup pipeline, restore |
| `crates/volsave-win` | Windows: volumes, disks, VSS snapshots, allocation bitmaps, raw device I/O |
| `crates/volsave-ops` | Operations shared by the command line tool and GUIs: backup, restore, opening images |
| `crates/volsave-cli` | Command line tool `volsave`; also a library, so VolSave Pro offers the same commands |

A graphical interface is part of the separate commercial product VolSave
Pro, which builds on these crates; encrypted images and network targets
(SMB, FTP, SFTP) are planned there.

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
volsave backup HD0 E:\backup\disk0.dimg --all-sectors  # every sector (unknown layouts)
volsave info E:\backup\disk0.dimg                  # shows the partitions
volsave restore E:\backup\disk0.dimg HD1 --verify  # clone onto disk 1, read back
volsave restore E:\backup\disk0.dimg F: --partition 2
```

A restored disk keeps the disk ID of the original. While both disks are
attached Windows keeps the clone offline; do not set it online there, or
Windows gives it a new ID and it no longer starts. Remove the original disk
and boot from the clone.

Differential images (`--diff`) store only what changed since a full backup
and build on its hash list; there are no incremental chains. In Windows PE
volumes are read directly without VSS: the installed Windows is not running.
`restore --verify` reads the target back and compares it with the image.
Existing images are only replaced with `--overwrite`; the full image of a
`--diff` backup and the images a command reads are never overwritten.

BitLocker volumes that are unlocked (always the case for the running
Windows) are saved decrypted through a VSS snapshot: only used blocks,
compressed and consistent. The image then holds the data unencrypted, so keep
it safe, and a restored copy is not encrypted until BitLocker is turned on
again. `--bitlocker-raw` copies such partitions as encrypted sectors instead
(every sector, hardly compressible, no snapshot). Locked volumes, e.g. in
Windows PE without `manage-bde -unlock`, can only be copied that way.

Commands that work on an image append a log next to it (`c.dimg` -> `c.log`);
`--log FILE` writes elsewhere, `--no-log` turns it off, `--log-level debug`
adds details such as every VSS call. Exit codes: 0 = OK, 1 = error,
2 = OK with warnings.

The `zstd` crate compiles the zstd C library, so a C compiler is required
(`build-essential` on Debian/Ubuntu, MSVC Build Tools on Windows).

## License

The command line tool and the libraries in this repository are free
software under the GNU General Public License, version 3 or later
([LICENSE](LICENSE)). The graphical interface VolSave Pro is a separate,
commercial product.

---

## Deutsch

VolSave sichert Datenträger und Volumes unter Windows, geschrieben in Rust.
Gesichert wird über den Volumeschattenkopie-Dienst (VSS), die Images werden
mit zstd komprimiert.

Stand: frühe Entwicklung, aber im Einsatz. Das Kommandozeilenprogramm
sichert, prüft, stellt wieder her und führt zusammen, sowohl einzelne
Volumes als auch ganze Datenträger (Partitionstabelle, Bootcode und alle
Partitionen). Klone ganzer Datenträger mit Windows 10 und 11 (BIOS/MBR und
UEFI/GPT) wurden wiederhergestellt und gestartet, in virtuellen Maschinen
und auf echter Hardware, auch aus Windows PE heraus.

Download: `volsave.exe` unter [Releases](../../releases). Läuft ohne
Installation unter Windows 10/11 und in Windows PE.

Eine grafische Oberfläche gehört zum getrennten, kommerziellen Produkt
VolSave Pro, das auf diesen Bibliotheken aufbaut; verschlüsselte Images und
Netzwerkziele (SMB, FTP, SFTP) sind dort geplant.

### Verwendung (Windows, als Administrator)

Die Befehle stehen oben unter „Usage“. Kurz:

- `volsave list` zeigt Datenträger und Volumes.
- `volsave backup C: E:\backup\c.dimg` sichert Laufwerk C:, dazu entsteht
  die Hashliste `c.dhsh`.
- `--diff E:\backup\c.dhsh` erzeugt ein Differenz-Image: nur was sich seit
  der Vollsicherung geändert hat. Es gibt keine inkrementellen Ketten.
- `volsave backup HD0 …` sichert den ganzen Datenträger 0; `--all-sectors`
  kopiert jeden Sektor (für unbekannte Aufteilungen).
- `volsave verify` prüft ein Image, `volsave info` zeigt seinen Inhalt.
- `volsave restore … HD1 --verify` spielt ein Image zurück und liest das
  Ziel danach zum Vergleich wieder ein; `--partition 2` stellt nur eine
  Partition aus einem Datenträger-Image wieder her.
- Ein vorhandenes Image wird nur mit `--overwrite` ersetzt; das Voll-Image
  eines `--diff` und die Images, die ein Befehl liest, werden nie
  überschrieben.
- `volsave --help` zeigt Beispiele.

Ein wiederhergestellter Datenträger behält die Datenträgerkennung des
Originals. Solange beide angeschlossen sind, lässt Windows den Klon offline;
dort nicht online schalten, sonst vergibt Windows eine neue Kennung und er
startet nicht mehr. Original abklemmen und vom Klon starten.

In Windows PE werden Volumes direkt ohne VSS gelesen, weil das installierte
Windows nicht läuft.

BitLocker-Volumes, die entsperrt sind (beim laufenden Windows immer),
werden entschlüsselt über einen VSS-Snapshot gesichert: nur belegte Blöcke,
komprimiert und konsistent. Das Image enthält die Daten dann
**unverschlüsselt**, also sicher aufbewahren; eine wiederhergestellte Kopie
ist unverschlüsselt, bis BitLocker wieder eingeschaltet wird.
`--bitlocker-raw` kopiert solche Partitionen stattdessen als verschlüsselte
Sektoren (jeder Sektor, kaum komprimierbar, ohne Snapshot). Gesperrte
Volumes, etwa in Windows PE ohne `manage-bde -unlock`, lassen sich nur so
kopieren.

Befehle, die mit einem Image arbeiten, schreiben ein Protokoll daneben
(`c.dimg` -> `c.log`); `--log DATEI` schreibt woandershin, `--no-log`
schaltet es ab, `--log-level debug` protokolliert Details wie jeden
VSS-Aufruf. Rückgabewerte: 0 = OK, 1 = Fehler, 2 = OK mit Warnungen.

### Lizenz

Das Kommandozeilenprogramm und die Bibliotheken in diesem Repository sind
freie Software unter der GNU General Public License, Version 3 oder später
([LICENSE](LICENSE)). Die grafische Oberfläche VolSave Pro ist ein
getrenntes, kommerzielles Produkt.
