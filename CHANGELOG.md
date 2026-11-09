# Changelog

All notable changes to VolSave. Downloads: [Releases](../../releases).
*Deutsch: [unten](#deutsch) · Español: [abajo](#español).*

## 0.2.6 – 2026-11-09
- Encrypted images and hash lists are recognised: commands say that the
  file is encrypted instead of calling it not a valid image.
- For other front-ends: all image files and hash lists are reached through
  a replaceable storage interface (`ImageStore`), and an edition can add
  command-line options. No change in behaviour.

## 0.2.5 – 2026-11-07
- The command line is now also a library crate (`volsave_cli::main` with an
  edition), so other front-ends can reuse it. No change in behaviour.

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

### 0.2.6 – 09.11.2026
- Verschlüsselte Images und Hashlisten werden erkannt: Die Befehle melden,
  dass die Datei verschlüsselt ist, statt sie als ungültiges Image zu
  bezeichnen.
- Für andere Oberflächen: Alle Image-Dateien und Hashlisten werden über eine
  austauschbare Speicher-Schnittstelle (`ImageStore`) angesprochen, und eine
  Edition kann Kommandozeilen-Optionen ergänzen. Keine Verhaltensänderung.

### 0.2.5 – 07.11.2026
- Das Kommandozeilenprogramm ist jetzt zusätzlich eine Bibliothek
  (`volsave_cli::main` mit einer Edition); andere Oberflächen können sie
  nutzen. Am Verhalten ändert sich nichts.

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

---

## Español

Todos los cambios importantes de VolSave. Descargas en [Releases](../../releases).

### 0.2.6 – 09/11/2026
- Se reconocen las imágenes y listas de hashes cifradas: los comandos
  indican que el archivo está cifrado en lugar de decir que no es una imagen
  válida.
- Para otras interfaces: todos los archivos de imagen y listas de hashes se
  acceden mediante una interfaz de almacenamiento reemplazable
  (`ImageStore`), y una edición puede añadir opciones de línea de comandos.
  Sin cambios de comportamiento.

### 0.2.5 – 07/11/2026
- La línea de comandos es ahora también una biblioteca (`volsave_cli::main`
  con una edición); otras interfaces pueden reutilizarla. El comportamiento
  no cambia.

### 0.2.4 – 07/11/2026
- Las imágenes existentes ya no se sobrescriben sin aviso: `backup`, `merge`
  y `extract` rechazan un destino existente, salvo con `--overwrite`.
- `backup --diff` nunca sobrescribe la imagen completa a la que pertenece la
  lista de hashes, y ningún comando escribe sobre una imagen que lee.
- `volsave --help` muestra ejemplos: copia completa, copia diferencial,
  verificación, restauración de volúmenes y discos.

### 0.2.3 – 06/11/2026
- EXE más pequeño: compilaciones sin símbolos y con LTO completa.
- README actualizado, descripción en alemán añadida.

### 0.2.2 – 06/11/2026
- La advertencia de BitLocker menciona que VolSave Pro puede guardar
  imágenes cifradas.

### 0.2.1 – 06/11/2026
- Los volúmenes BitLocker desbloqueados se guardan descifrados mediante una
  instantánea VSS: solo los bloques usados, comprimidos y coherentes, mucho
  más rápido que antes. Una advertencia indica que la imagen contiene los
  datos sin cifrar.
- `--bitlocker-raw` mantiene el comportamiento anterior (copia cifrada
  sector a sector).
- Los volúmenes BitLocker bloqueados se copian sector a sector, con la
  indicación de desbloquearlos antes (`manage-bde -unlock`).

### 0.2.0 – 28/10/2026
- Copia y restauración trasladadas a la biblioteca común `volsave-ops`.
- Windows PE: los volúmenes se leen directamente, sin VSS.
- Las imágenes diferenciales ya no tienen lista de hashes propia.
- `--version` y los registros indican el commit de git.

### 0.1.1 – 27/10/2026
- `restore --verify` vuelve a leer el destino y lo compara con la imagen.
- Tras una copia se muestran los bytes de los bloques.

### 0.1.0 – 26/10/2026
- Primera versión: formato de imagen con compresión zstd, copias mediante
  VSS, `FilesNotToBackup`, registro, copia y restauración de discos
  completos (MBR y GPT), imágenes diferenciales, verificación y fusión.
  GPL-3.0-or-later.
