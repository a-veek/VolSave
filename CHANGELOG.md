# Changelog

All notable changes to VolSave. Downloads: [Releases](../../releases).
*Deutsch: [unten](#deutsch) · Español: [abajo](#español).*

## 0.2.8 – 2026-11-11
- For other front-ends: copying a whole disk onto another disk without an
  image in between (`plan_clone`, `ClonePlan::run`): read like a disk backup
  (VSS snapshots, used blocks), written and verified like a disk restore;
  the target is taken offline because it gets the same disk ID. The command
  `clone` is only offered by editions that enable it.
- The restore of a disk image shares its code with the clone; no change in
  behaviour.
- Fixed: on FAT and exFAT volumes the page, swap and hibernation files and
  the FilesNotToBackup entries were left out at the wrong place (shifted by
  the file system tables), so other data could be missing from the image.
  Images made with --keep-system-files --files-not-to-backup none were not
  affected; NTFS and ReFS were never affected.
- Before a disk restore or clone writes, it checks that the target is still
  the disk that was shown for confirmation (size, sector size, model); a
  disk plugged in since may have taken its number.
- Damaged or crafted image files cannot make the reader allocate huge
  amounts of memory or miscount blocks; a read error while opening an image
  is reported as such instead of "index rebuilt"; a missing middle part of
  a split image is named instead of silently shortening the image.
- A differential backup whose full image is not next to its hash list
  gets a warning: it can only be restored together with that full image.
- --hash never replaces an existing hash list without --overwrite.
- A volume restore refuses a too small target before locking it, and
  Ctrl+C during the final verification says that the data is complete but
  unchecked instead of asking to restore again.
- A VSS writer complaining after the backup is complete gives a warning,
  not a failure.
- --log takes its file as --log=FILE, so `volsave --log backup ...` no
  longer eats the command name.
- volsave.exe carries a Windows manifest and version information
  (product, version, description); without them Windows Defender treated
  the bare file as suspicious and removed it.
- Faster: zstd keeps one context per thread instead of making one per
  block; restore and verify reuse their buffers; cloning a fragmented
  volume no longer slows down with the number of fragments.

## 0.2.7 – 2026-11-09
- For other front-ends: a storage that encrypts its images (`ImageStore::encrypts`)
  gets a fitting warning for a BitLocker volume saved decrypted (only the
  restored copy lacks BitLocker); the free VolSave is unchanged.

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

### 0.2.8 – 11.11.2026
- Für andere Oberflächen: einen ganzen Datenträger ohne Image direkt auf
  einen anderen kopieren (`plan_clone`, `ClonePlan::run`): gelesen wie eine
  Disk-Sicherung (VSS-Snapshots, belegte Blöcke), geschrieben und geprüft
  wie ein Disk-Restore; das Ziel wird offline genommen, weil es dieselbe
  Platten-ID bekommt. Den Befehl `clone` bieten nur Editionen an, die ihn
  freischalten.
- Der Restore eines Disk-Images teilt seinen Code mit dem Klonen; keine
  Verhaltensänderung.
- Behoben: Auf FAT- und exFAT-Volumes wurden Auslagerungs-, Swap- und
  Ruhezustandsdatei sowie die FilesNotToBackup-Einträge an der falschen
  Stelle ausgelassen (um die Dateisystemtabellen verschoben), sodass andere
  Daten im Image fehlen konnten. Images mit --keep-system-files
  --files-not-to-backup none waren nicht betroffen; NTFS und ReFS nie.
- Vor dem Schreiben prüfen Disk-Restore und Klonen, dass das Ziel noch die
  Platte ist, die zur Bestätigung gezeigt wurde (Größe, Sektorgröße,
  Modell); eine inzwischen angesteckte Platte kann ihre Nummer übernommen
  haben.
- Beschädigte oder manipulierte Image-Dateien können den Leser nicht zu
  riesigen Speicheranforderungen oder falschen Blockzahlen bringen; ein
  Lesefehler beim Öffnen wird als solcher gemeldet statt als „Index neu
  aufgebaut“; ein fehlender mittlerer Teil eines aufgeteilten Images wird
  benannt statt das Image stillschweigend zu verkürzen.
- Eine Differenz-Sicherung, deren Voll-Image nicht neben seiner Hashliste
  liegt, bekommt eine Warnung: Sie lässt sich nur zusammen mit diesem
  Voll-Image wiederherstellen.
- --hash ersetzt eine vorhandene Hashliste nie ohne --overwrite.
- Ein Volume-Restore lehnt ein zu kleines Ziel ab, bevor es gesperrt wird,
  und Strg+C während der abschließenden Prüfung meldet, dass die Daten
  vollständig, aber ungeprüft sind, statt zum erneuten Restore zu raten.
- Ein VSS-Writer, der sich nach abgeschlossener Sicherung beschwert, ergibt
  eine Warnung, keinen Fehlschlag.
- --log nimmt seine Datei als --log=DATEI, damit `volsave --log backup ...`
  nicht mehr den Befehlsnamen schluckt.
- volsave.exe trägt ein Windows-Manifest und Versionsinformationen
  (Produkt, Version, Beschreibung); ohne sie hielt der Windows Defender die
  nackte Datei für verdächtig und entfernte sie.
- Schneller: zstd behält einen Kontext pro Thread statt einen pro Block;
  Restore und Prüfung verwenden ihre Puffer wieder; das Klonen eines
  fragmentierten Volumes wird nicht mehr mit der Zahl der Fragmente
  langsamer.

### 0.2.7 – 09.11.2026
- Für andere Oberflächen: Verschlüsselt der Speicher seine Images
  (`ImageStore::encrypts`), passt die Warnung zu einem entschlüsselt
  gesicherten BitLocker-Volume (nur die wiederhergestellte Kopie hat kein
  BitLocker); die freie VolSave bleibt unverändert.

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

### 0.2.8 – 11/11/2026
- Para otras interfaces: copiar un disco completo directamente a otro, sin
  imagen intermedia (`plan_clone`, `ClonePlan::run`): se lee como una copia
  de disco (instantáneas VSS, bloques usados) y se escribe y verifica como
  una restauración de disco; el destino se pone fuera de línea porque recibe
  el mismo ID de disco. El comando `clone` solo lo ofrecen las ediciones que
  lo activan.
- La restauración de una imagen de disco comparte su código con la
  clonación; sin cambios de comportamiento.
- Corregido: en volúmenes FAT y exFAT, los archivos de paginación,
  intercambio e hibernación y las entradas de FilesNotToBackup se omitían
  en el lugar equivocado (desplazados por las tablas del sistema de
  archivos), por lo que podían faltar otros datos en la imagen. Las
  imágenes hechas con --keep-system-files --files-not-to-backup none no se
  vieron afectadas; NTFS y ReFS nunca.
- Antes de escribir, la restauración de disco y la clonación comprueban que
  el destino sigue siendo el disco mostrado para confirmar (tamaño, tamaño
  de sector, modelo); un disco conectado después puede haber tomado su
  número.
- Los archivos de imagen dañados o manipulados no pueden hacer que el
  lector reserve enormes cantidades de memoria ni cuente mal los bloques;
  un error de lectura al abrir se indica como tal en lugar de «índice
  reconstruido»; una parte intermedia ausente de una imagen dividida se
  nombra en lugar de acortar la imagen en silencio.
- Una copia diferencial cuya imagen completa no está junto a su lista de
  hashes recibe una advertencia: solo se puede restaurar junto con esa
  imagen completa.
- --hash nunca sustituye una lista de hashes existente sin --overwrite.
- Una restauración de volumen rechaza un destino demasiado pequeño antes de
  bloquearlo, y Ctrl+C durante la verificación final indica que los datos
  están completos pero sin comprobar, en lugar de pedir restaurar de nuevo.
- Un escritor VSS que se queja tras completar la copia produce una
  advertencia, no un fallo.
- --log toma su archivo como --log=ARCHIVO, para que `volsave --log backup
  ...` ya no se trague el nombre del comando.
- volsave.exe lleva un manifiesto de Windows e información de versión
  (producto, versión, descripción); sin ellos Windows Defender consideraba
  sospechoso el archivo desnudo y lo eliminaba.
- Más rápido: zstd mantiene un contexto por hilo en lugar de uno por
  bloque; restaurar y verificar reutilizan sus búferes; clonar un volumen
  fragmentado ya no se ralentiza con el número de fragmentos.

### 0.2.7 – 09/11/2026
- Para otras interfaces: si el almacén cifra sus imágenes
  (`ImageStore::encrypts`), la advertencia sobre un volumen BitLocker
  guardado descifrado se adapta (solo la copia restaurada queda sin
  BitLocker); el VolSave gratuito no cambia.

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
