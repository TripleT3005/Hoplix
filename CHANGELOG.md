# Changelog

All notable changes are documented here. The format follows [Keep a Changelog](https://keepachangelog.com/).

## [0.2.1] – 2026-11-24

### English

#### Security

- `hoplix-cli sites` no longer shows user names or which sites have a saved password, so the list can be shared or logged without revealing accounts.
- The encrypted password file (master password) now creates its salts and nonces directly from the system's random number generator, without an intermediate buffer.

### Deutsch

#### Sicherheit

- `hoplix-cli sites` zeigt keine Benutzernamen mehr und auch nicht, für welche Server ein Passwort gespeichert ist; die Liste lässt sich so teilen oder protokollieren, ohne Konten preiszugeben.
- Die verschlüsselte Passwortdatei (Master-Passwort) erzeugt Salts und Nonces jetzt direkt aus dem Zufallszahlengenerator des Systems, ohne Zwischenpuffer.

## [0.2.0] – 2026-11-24

### English

#### Added

- Open files with the default program (double-click, Enter) or edit them in an editor of your choice (F4); files from a server are downloaded to a temporary folder, watched for changes and uploaded again on request or automatically.
- Compare two folders (Ctrl+D): differences are marked in both file lists, optionally including subfolders, with a filter for only the differences.
- Synchronise two folders (Ctrl+Shift+D) in one direction or both ways, as update or mirror, optionally deleting files missing on the source, with exclude patterns and a preview before starting; copied files keep their modification time.
- `hoplix-cli compare` and `hoplix-cli sync` (with `--dry-run`, `--delete`, `--mirror`, `--size-only`, `--skip-hidden` and `--exclude`).
- The licenses of the libraries used are included with the program (`THIRD-PARTY-LICENSES.txt`).

### Deutsch

#### Neu

- Dateien mit dem Standardprogramm öffnen (Doppelklick, Eingabetaste) oder in einem frei wählbaren Editor bearbeiten (F4); Dateien vom Server werden in einen temporären Ordner heruntergeladen, auf Änderungen beobachtet und auf Wunsch oder automatisch wieder hochgeladen.
- Zwei Ordner vergleichen (Strg+D): Unterschiede werden in beiden Dateilisten markiert, auf Wunsch mit Unterordnern und mit einem Filter für nur die Unterschiede.
- Zwei Ordner synchronisieren (Strg+Umschalt+D) in eine oder beide Richtungen, als Aktualisieren oder Spiegeln, auf Wunsch mit Löschen von Dateien, die in der Quelle fehlen, mit Ausschlussmustern und Vorschau vor dem Start; kopierte Dateien behalten ihre Änderungszeit.
- `hoplix-cli compare` und `hoplix-cli sync` (mit `--dry-run`, `--delete`, `--mirror`, `--size-only`, `--skip-hidden` und `--exclude`).
- Die Lizenzen der verwendeten Bibliotheken liegen dem Programm bei (`THIRD-PARTY-LICENSES.txt`).

## [0.1.0] – 2026-11-24

### English

#### Added

- Browse local folders in tabs, sort by name, type, size or modification date.
- Connect to SFTP servers with a password or key file; unknown server keys are shown for confirmation, changed keys are refused.
- Connect to FTP and FTPS servers (explicit and implicit TLS); untrusted certificates are shown with their fingerprint for confirmation, plain FTP shows a warning.
- Create, rename and delete files and folders, locally and on the server; select several entries with Ctrl and Shift.
- Transfer files and folders between any two tabs or panes (upload, download, server to server) by drag and drop or with *Copy to…*, several at the same time.
- Transfer panel with progress, speed and remaining time; pause, resume, cancel and retry transfers, which continue where they stopped and retry automatically after connection errors.
- Ask what to do with existing files (overwrite, resume, keep both, skip), optionally for all of them.
- Site manager with nested folders: save, edit, duplicate and delete servers with their remote and local start folders and notes, and connect with a double-click.
- Save passwords and key passphrases in the Windows Credential Manager or the Linux system keyring, or encrypted with a master password.
- Command-line tool `hoplix-cli` for listing, downloading, uploading and copying between servers (also whole folders with `-r`, with resume and progress), creating, moving and deleting, also with saved sites (`hoplix-cli sites`, `site://name/path`).
- Keyboard shortcuts for the file list (Enter, Backspace, F2, F5, Del, arrows with Shift, Ctrl+A, Ctrl+T/W, Ctrl+Tab) and a command palette (Ctrl+K) that also connects to saved sites.
- *Modified* column with the date of the last change in local time, sortable.
- Open tabs and the window size and position are restored after a restart; tabs of saved sites come back unconnected and connect with one click.
- SFTP connections reconnect automatically after a dropped session, so transfers and browsing continue.
- Light and dark appearance, following the system or chosen manually.
- Modern (tabs) and classic (two panes) layout.
- English and German interface.
- App icon, version information in the programs and a title bar in the colours of the current theme on Windows.

#### Fixed

- Dragging files from the right pane onto the left pane in the classic layout now copies them.
- Clicking a command in the command palette closes the palette.
- Switching between light and dark from the command palette also recolours the title bar on Windows.
- Long fingerprints and paths in dialogs wrap instead of being cut off, and long setting values stay on one line.
- The copy buttons no longer move when the selection changes.
- Tab and Shift+Tab move between input fields.
- Changing the connection details on the Connect page no longer keeps them linked to a saved site.
- The file list scrolls only when the selected entry leaves the visible area, instead of jumping.
- Secondary and delete buttons and notices are easier to read in both themes.

### Deutsch

#### Neu

- Lokale Ordner in Tabs durchsuchen, nach Name, Typ, Größe oder Änderungsdatum sortieren.
- Verbindung zu SFTP-Servern mit Passwort oder Schlüsseldatei; unbekannte Serverschlüssel werden zur Bestätigung angezeigt, geänderte abgelehnt.
- Verbindung zu FTP- und FTPS-Servern (explizites und implizites TLS); nicht vertrauenswürdige Zertifikate werden mit Fingerabdruck zur Bestätigung angezeigt, bei reinem FTP erscheint eine Warnung.
- Dateien und Ordner anlegen, umbenennen und löschen, lokal und auf dem Server; mehrere Einträge mit Strg und Umschalt auswählen.
- Dateien und Ordner zwischen zwei beliebigen Tabs oder Bereichen übertragen (Hochladen, Herunterladen, von Server zu Server), per Ziehen und Ablegen oder über *Kopieren nach …*, mehrere gleichzeitig.
- Bereich für Übertragungen mit Fortschritt, Geschwindigkeit und Restzeit; Übertragungen pausieren, fortsetzen, abbrechen und wiederholen, sie machen dort weiter, wo sie aufgehört haben, und versuchen es nach Verbindungsfehlern selbst erneut.
- Nachfrage bei vorhandenen Dateien (überschreiben, fortsetzen, beide behalten, überspringen), auf Wunsch für alle.
- Server-Verwaltung mit verschachtelten Ordnern: Server mit Start-Ordnern auf dem Server und lokal sowie Notizen speichern, bearbeiten, duplizieren und löschen, Verbinden per Doppelklick.
- Passwörter und Passphrasen in der Windows-Anmeldeinformationsverwaltung bzw. im Schlüsselbund unter Linux speichern oder mit einem Master-Passwort verschlüsselt.
- Kommandozeilenwerkzeug `hoplix-cli` zum Auflisten, Herunter- und Hochladen sowie Kopieren zwischen Servern (auch ganze Ordner mit `-r`, mit Fortsetzen und Fortschritt), Anlegen, Verschieben und Löschen, auch mit gespeicherten Servern (`hoplix-cli sites`, `site://Name/Pfad`).
- Tastenkürzel für die Dateiliste (Eingabe, Rücktaste, F2, F5, Entf, Pfeiltasten mit Umschalt, Strg+A, Strg+T/W, Strg+Tab) und eine Befehlspalette (Strg+K), die auch mit gespeicherten Servern verbindet.
- Spalte *Geändert* mit dem Datum der letzten Änderung in Ortszeit, sortierbar.
- Offene Tabs sowie Größe und Position des Fensters werden nach einem Neustart wiederhergestellt; Tabs gespeicherter Server kommen unverbunden zurück und verbinden sich mit einem Klick.
- SFTP-Verbindungen bauen sich nach einem Abbruch selbst wieder auf, Übertragungen und Durchsuchen laufen weiter.
- Helle und dunkle Darstellung, wie im System oder fest gewählt.
- Moderne (Tabs) und klassische (zwei Bereiche) Ansicht.
- Oberfläche auf Englisch und Deutsch.
- Programmsymbol, Versionsinfo in den Programmen und unter Windows eine Titelleiste in den Farben des aktuellen Designs.

#### Behoben

- Dateien, die in der klassischen Ansicht vom rechten in den linken Bereich gezogen werden, werden jetzt kopiert.
- Ein Klick auf einen Befehl in der Befehlspalette schließt die Palette.
- Das Umschalten zwischen hell und dunkel über die Befehlspalette färbt unter Windows auch die Titelleiste um.
- Lange Fingerabdrücke und Pfade in Dialogen werden umgebrochen statt abgeschnitten, lange Werte in den Einstellungen bleiben auf einer Zeile.
- Die Kopierknöpfe verschieben sich nicht mehr, wenn sich die Auswahl ändert.
- Tab und Umschalt+Tab wechseln zwischen den Eingabefeldern.
- Werden die Verbindungsdaten auf der Verbindungsseite geändert, sind sie nicht mehr mit einem gespeicherten Server verknüpft.
- Die Dateiliste scrollt nur noch, wenn der ausgewählte Eintrag den sichtbaren Bereich verlässt, statt zu springen.
- Nebenknöpfe, Löschknöpfe und Hinweise sind in beiden Designs besser lesbar.
