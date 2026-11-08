# Changelog

All notable changes to VolSave. Downloads: [Releases](../../releases).
*Deutsche Fassung: [unten](#deutsch).*

## 0.2.5 – 2026-11-07
- The command line tool is also a library (`volsave_cli::main` with an
  edition), so VolSave Pro ships the same commands as `volsave-pro-cli.exe`.
  No change in behaviour.

## 0.2.4 – 2026-11-07
- Existing images are no longer overwritten silently: `backup`, `merge` and
  `extract` refuse an existing target unless `--overwrite` is given.
- `backup --diff` never overwrites the full image the hash list belongs to,
  and no command writes over an image it reads.
- `volsave --help` shows examples: full backup, differential backup, verify,
  restore of volumes and disks.

## 0.2.3 – 2026-11-06
- Smaller EXE: release builds are stripped and use full LTO.
- README updated, German description added.

## 0.2.2 – 2026-11-06
- The BitLocker warning mentions that VolSave Pro can save encrypted images.

## 0.2.1 – 2026-11-06
- Unlocked BitLocker volumes are saved decrypted through a VSS snapshot:
  only used blocks, compressed and consistent, much faster than before. A
  warning says that the image holds the data unencrypted.
- `--bitlocker-raw` keeps the old behaviour (encrypted sector copy).
- Locked BitLocker volumes are copied sector by sector, with a hint to
  unlock them first (`manage-bde -unlock`).

## 0.2.0 – 2026-10-28
- Backup and restore moved into the shared crate `volsave-ops`.
- Windows PE: volumes are read directly, without VSS.
- Differential images get no hash list of their own.
- `--version` and the logs name the git commit.

## 0.1.1 – 2026-10-27
- `restore --verify` reads the target back and compares it with the image.
- After a backup the block bytes are reported.

## 0.1.0 – 2026-10-26
- First release: image format with zstd compression, backups through VSS,
  `FilesNotToBackup`, logging, whole-disk backup and restore (MBR and GPT),
  differential images, verify and merge. GPL-3.0-or-later.

---

## Deutsch

Alle wichtigen Änderungen an VolSave. Downloads unter [Releases](../../releases).

### 0.2.5 – 07.11.2026
- Das Kommandozeilenprogramm ist zusätzlich eine Bibliothek; VolSave Pro
  liefert damit dieselben Befehle als `volsave-pro-cli.exe` aus. Am
  Verhalten ändert sich nichts.

### 0.2.4 – 07.11.2026
- Vorhandene Images werden nicht mehr still überschrieben: `backup`, `merge`
  und `extract` lehnen ein vorhandenes Ziel ab, außer mit `--overwrite`.
- `backup --diff` überschreibt nie das Voll-Image, zu dem die Hashliste
  gehört, und kein Befehl schreibt über ein Image, das er liest.
- `volsave --help` zeigt Beispiele: Vollsicherung, Differenz-Sicherung,
  Prüfen, Wiederherstellen von Volumes und Datenträgern.

### 0.2.3 – 06.11.2026
- Kleinere EXE: Release-Builds ohne Symbole und mit voller LTO.
- README aktualisiert, deutsche Beschreibung ergänzt.

### 0.2.2 – 06.11.2026
- Die BitLocker-Warnung erwähnt, dass VolSave Pro verschlüsselte Images
  speichern kann.

### 0.2.1 – 06.11.2026
- Entsperrte BitLocker-Volumes werden entschlüsselt über einen
  VSS-Snapshot gesichert: nur belegte Blöcke, komprimiert und konsistent,
  viel schneller als vorher. Eine Warnung weist darauf hin, dass das Image
  die Daten unverschlüsselt enthält.
- `--bitlocker-raw` behält das alte Verhalten (verschlüsselte Sektorkopie).
- Gesperrte BitLocker-Volumes werden sektorweise kopiert, mit dem Hinweis,
  sie vorher zu entsperren (`manage-bde -unlock`).

### 0.2.0 – 28.10.2026
- Sichern und Wiederherstellen in die gemeinsame Bibliothek `volsave-ops`
  verschoben.
- Windows PE: Volumes werden direkt ohne VSS gelesen.
- Differenz-Images bekommen keine eigene Hashliste mehr.
- `--version` und die Protokolle nennen den Git-Commit.

### 0.1.1 – 27.10.2026
- `restore --verify` liest das Ziel zurück und vergleicht es mit dem Image.
- Nach einer Sicherung werden die Block-Bytes angezeigt.

### 0.1.0 – 26.10.2026
- Erste Version: Image-Format mit zstd-Kompression, Sicherung über VSS,
  `FilesNotToBackup`, Protokoll, Sichern und Wiederherstellen ganzer
  Datenträger (MBR und GPT), Differenz-Images, Prüfen und Zusammenführen.
  GPL-3.0-or-later.
