# Changelog

All notable changes to Hoplix, grouped by version in the style of [Keep a Changelog](https://keepachangelog.com/). The section of each version is used as its release notes. Changes on `main` that are not released yet are collected under [Unreleased] and move into the section of the next version.

Alle wichtigen Änderungen an Hoplix, gegliedert nach Version im Stil von [Keep a Changelog](https://keepachangelog.com/de/). Der Abschnitt einer Version wird als ihre Release-Notizen verwendet. Änderungen auf `main`, die noch nicht veröffentlicht sind, stehen unter [Unreleased] und wandern mit der nächsten Version in deren Abschnitt.

## [Unreleased]

### English

#### Added

- Portable mode: with the file `hoplix.portable` next to the program (included in the portable ZIP), Hoplix keeps all configuration in the `config` folder beside it; saved passwords then go to the file encrypted with a master password by default.
- `.deb` and `.rpm` packages for Linux with a menu entry and icons.
- Optional *Permissions* and *Owner* columns in the file list (*Settings → General*).
- Search results can be copied to another tab or pane with the copy buttons or by dragging them there.

### Deutsch

#### Neu

- Portabler Modus: Mit der Datei `hoplix.portable` neben dem Programm (in der portablen ZIP enthalten) speichert Hoplix die gesamte Konfiguration im Ordner `config` daneben; gespeicherte Passwörter landen dann standardmäßig in der mit einem Master-Passwort verschlüsselten Datei.
- Pakete `.deb` und `.rpm` für Linux mit Menüeintrag und Icons.
- Optionale Spalten *Berechtigungen* und *Besitzer* in der Dateiliste (*Einstellungen → Allgemein*).
- Suchergebnisse lassen sich mit den Kopier-Buttons oder durch Ziehen in einen anderen Tab oder Bereich kopieren.

## [0.5.0] – 2026-11-26

### English

#### Added

- Filter field above the file list (Ctrl+F): part of a name or `*`/`?` wildcards, shows "x of y items"; Esc clears it.
- Back and forward through visited folders per tab (Alt+←/→, mouse buttons, ← → buttons).
- Bookmarks for local folders and per saved site (Ctrl+B), with a list next to the path field, editing, the command palette and site export.
- Search in subfolders (Ctrl+Shift+F) for local and remote tabs, with depth limit, live results and stop; a double-click opens the folder of a result.
- Properties dialog (Alt+Enter) with size (folders on demand), modification time, owner, group and permissions; change permissions on SFTP, FTP (`SITE CHMOD`) and Linux, also recursively for files or folders.
- Preview pane (F3) next to the file list: type, size and date of the selected file and the beginning of text files (UTF-8, UTF-16, Windows-1252); files on servers are read into memory only, larger ones on request (*Settings → Preview*).
- Drop files and folders from Explorer or your file manager onto a tab or pane to upload or copy them there.
- Copy path (Ctrl+Shift+C) copies the paths of the selection, as `sftp://user@host/path` for servers and without passwords.
- Unfinished transfers are kept and come back paused after a restart; transfers to a server wait for its connection and continue with *Connect*.
- Transfer history in the Transfers panel with filter, copy, CSV and text export.
- Optional checksum check after each file (SHA-256 locally; FTP HASH/XSHA256/XSHA1/XMD5/XCRC; SFTP check-file/md5-hash), otherwise a size check; mismatches fail and are copied again on *Retry*.
- `hoplix-cli chmod [-r]`, `hoplix-cli find` with `--depth`, and `--verify` for `get`, `put`, `cp` and `sync`; `stat` shows permissions, owner and group.

#### Changed

- The preview pane leaves room for the file list: the Name column always stays visible, Type and Modified hide first in narrow panes, and the divider stops before the list.
- Long paths in the transfer history and queue stay on one line, shortened in the middle with the file name visible and the full text as a tooltip.
- In small windows and in German, the action row above the file list wraps onto a second line and the path field gets its own line instead of running into the neighbouring pane or shrinking to nothing.

### Deutsch

#### Neu

- Filterfeld über der Dateiliste (Strg+F): Teil eines Namens oder Platzhalter `*`/`?`, zeigt „x von y Einträgen“; Esc leert es.
- Zurück und vorwärts durch die besuchten Ordner je Tab (Alt+←/→, Maustasten, Buttons ← →).
- Lesezeichen für lokale Ordner und je gespeichertem Server (Strg+B), mit Liste neben dem Pfadfeld, Bearbeiten, Befehlspalette und Serverexport.
- Suche in Unterordnern (Strg+Umschalt+F) für lokale und Server-Tabs, mit Tiefenlimit, laufenden Ergebnissen und Stopp; ein Doppelklick öffnet den Ordner eines Treffers.
- Eigenschaften-Dialog (Alt+Eingabe) mit Größe (Ordner auf Wunsch), Änderungszeit, Besitzer, Gruppe und Berechtigungen; Berechtigungen ändern bei SFTP, FTP (`SITE CHMOD`) und unter Linux, auch rekursiv für Dateien oder Ordner.
- Vorschau (F3) neben der Dateiliste: Typ, Größe und Datum der ausgewählten Datei und der Anfang von Textdateien (UTF-8, UTF-16, Windows-1252); Dateien auf Servern werden nur in den Arbeitsspeicher gelesen, größere nur auf Wunsch (*Einstellungen → Vorschau*).
- Dateien und Ordner aus dem Explorer bzw. der Dateiverwaltung auf einen Tab oder Bereich ziehen, um sie dorthin hochzuladen oder zu kopieren.
- Pfad kopieren (Strg+Umschalt+C) kopiert die Pfade der Auswahl, für Server als `sftp://benutzer@host/pfad` und ohne Passwörter.
- Unfertige Übertragungen bleiben erhalten und sind nach einem Neustart pausiert wieder da; Übertragungen zu einem Server warten auf dessen Verbindung und laufen mit *Verbinden* weiter.
- Übertragungsverlauf im Bereich Übertragungen mit Filter, Kopieren sowie Export als CSV und Text.
- Optionale Prüfsummenprüfung nach jeder Datei (SHA-256 lokal; FTP HASH/XSHA256/XSHA1/XMD5/XCRC; SFTP check-file/md5-hash), sonst Größenprüfung; Abweichungen schlagen fehl und werden mit *Wiederholen* neu kopiert.
- `hoplix-cli chmod [-r]`, `hoplix-cli find` mit `--depth` sowie `--verify` für `get`, `put`, `cp` und `sync`; `stat` zeigt Berechtigungen, Besitzer und Gruppe.

#### Geändert

- Die Vorschau lässt der Dateiliste Platz: Die Namensspalte bleibt immer sichtbar, in schmalen Bereichen werden zuerst Typ und Geändert ausgeblendet, und der Teiler stoppt vor der Liste.
- Lange Pfade in Übertragungsverlauf und Warteschlange bleiben einzeilig, in der Mitte gekürzt mit sichtbarem Dateinamen und vollem Text als Tooltip.
- In kleinen Fenstern und auf Deutsch bricht die Aktionszeile über der Dateiliste in eine zweite Zeile um, und das Pfadfeld bekommt eine eigene Zeile, statt in den Nachbarbereich zu ragen oder auf null zu schrumpfen.

## [0.4.0] – 2026-11-26

### English

#### Added

- Sign in to SFTP servers with the SSH agent (Windows OpenSSH agent, Pageant or `SSH_AUTH_SOCK`), on the Connect page and in the site editor, with clear messages when no agent runs or no key is accepted.
- Connect through an SSH jump host (SFTP) and through a SOCKS5 or HTTP CONNECT proxy (SFTP, FTP and FTPS in passive mode), on the Connect page and in the site editor; their passwords can be saved like the site password and are included when switching the password store, exporting and deleting sites.
- Advanced site settings: connection timeout, keep-alive interval, FTP active or passive mode, "use the server's address for passive mode", file name encoding UTF-8 or Windows-1252/Latin-1, and parallel transfers per site.
- Recent connections on the Connect page (last 10, never passwords), also in the command palette; they include jump host and proxy and can be switched off in *Settings → Connections*.
- Export and import of the site list, optionally with saved passwords in an encrypted file.
- `hoplix-cli`: `--agent`, `--jump`, `--jump-key`, `--jump-password-env`, `--jump-agent`, `--proxy`, `--proxy-user`, `--proxy-password-env`, and `sites export` / `sites import`; saved sites use their jump host and proxy.

#### Changed

- The transfer queue respects a per-site limit in addition to the overall and per-direction limits; waiting transfers show why.
- In the site manager, Import and Export have their own row below the list.

#### Security

- A server key or certificate you confirm only applies to the host and port it was shown for; with a jump host, every hop is confirmed separately and "Always trust" only saves the confirmed host.

### Deutsch

#### Neu

- Anmeldung an SFTP-Servern über den SSH-Agenten (Windows-OpenSSH-Agent, Pageant oder `SSH_AUTH_SOCK`) auf der Verbindungsseite und im Server-Editor, mit klaren Meldungen, wenn kein Agent läuft oder kein Schlüssel angenommen wird.
- Verbindung über einen SSH-Jump-Host (SFTP) und über einen SOCKS5- oder HTTP-CONNECT-Proxy (SFTP, FTP und FTPS im passiven Modus), auf der Verbindungsseite und im Server-Editor; ihre Passwörter lassen sich wie das Server-Passwort speichern und werden beim Wechsel des Passwortspeichers, beim Export und beim Löschen berücksichtigt.
- Erweiterte Server-Einstellungen: Verbindungs-Timeout, Keep-alive-Abstand, aktiver oder passiver FTP-Modus, „Adresse des Servers für den passiven Modus verwenden“, Zeichensatz der Dateinamen UTF-8 oder Windows-1252/Latin-1 und gleichzeitige Übertragungen pro Server.
- Letzte Verbindungen auf der Seite „Verbinden“ (die letzten 10, nie Passwörter), auch in der Befehlspalette; sie enthalten Jump-Host und Proxy und lassen sich unter *Einstellungen → Verbindungen* abschalten.
- Export und Import der Serverliste, wahlweise mit gespeicherten Passwörtern in einer verschlüsselten Datei.
- `hoplix-cli`: `--agent`, `--jump`, `--jump-key`, `--jump-password-env`, `--jump-agent`, `--proxy`, `--proxy-user`, `--proxy-password-env` sowie `sites export` / `sites import`; gespeicherte Server nutzen ihren Jump-Host und Proxy.

#### Geändert

- Die Übertragungswarteschlange beachtet zusätzlich zur Gesamtgrenze und zu den Grenzen je Richtung eine Grenze pro Server; wartende Übertragungen zeigen den Grund.
- In der Server-Verwaltung haben Importieren und Exportieren eine eigene Zeile unter der Liste.

#### Sicherheit

- Ein bestätigter Serverschlüssel oder ein bestätigtes Zertifikat gilt nur für den Host und Port, für den es angezeigt wurde; bei einem Jump-Host wird jeder Hop einzeln bestätigt, und „Immer vertrauen“ speichert nur den bestätigten Host.

## [0.3.0] – 2026-11-24

### English

#### Added

- Update check: Hoplix looks for a new version once a day in the background (can be switched off in *Settings → Updates*), or right away with *Check now*. Nothing is downloaded or installed.
- When a new version is available, Hoplix tells you once and keeps a small link to the release page in the status bar until you have installed it.
- Separate limits for parallel downloads and uploads next to the overall limit; a copy between two servers counts as both.
- Download and upload speed limits in KiB/s or MiB/s, shared by all running transfers of that direction and applied right away.
- Adjustable retries after a dropped connection (0 to 10, default 3) and first retry delay (1 to 60 seconds, doubling).
- Option to switch off automatic reconnecting of dropped SFTP sessions.
- `hoplix-cli`: `--retries`, `--retry-delay`, `--limit-download` and `--limit-upload` for `get`, `put`, `cp` and `sync`.

#### Changed

- The settings page is grouped into General, Transfers, Connections, Passwords, Editing and Updates and scrolls in small windows.
- The editor setting accepts a command line with arguments, for example `code --wait %1`; `%1` stands for the file, otherwise the file is added at the end.
- An active comparison follows the active tab: switching, opening or closing tabs compares the newly active tab, and the comparison ends when no second tab is left.

#### Fixed

- Temporary edit folders left behind after a crash are removed the next time Hoplix starts.

### Deutsch

#### Neu

- Update-Prüfung: Hoplix sucht einmal am Tag im Hintergrund nach einer neuen Version (abschaltbar unter *Einstellungen → Updates*) oder sofort mit *Jetzt prüfen*. Es wird nichts heruntergeladen oder installiert.
- Gibt es eine neue Version, weist Hoplix einmal darauf hin und zeigt danach einen kleinen Link zur Release-Seite in der Statusleiste, bis sie installiert ist.
- Eigene Grenzen für gleichzeitige Downloads und Uploads neben der Gesamtgrenze; eine Kopie zwischen zwei Servern zählt für beide.
- Tempolimits für Downloads und Uploads in KiB/s oder MiB/s, gemeinsam für alle laufenden Übertragungen einer Richtung und sofort wirksam.
- Einstellbare Wiederholungen nach einem Verbindungsabbruch (0 bis 10, Standard 3) und erste Wartezeit (1 bis 60 Sekunden, jede weitere doppelt so lang).
- Automatisches Neuverbinden abgebrochener SFTP-Sitzungen lässt sich abschalten.
- `hoplix-cli`: `--retries`, `--retry-delay`, `--limit-download` und `--limit-upload` für `get`, `put`, `cp` und `sync`.

#### Geändert

- Die Einstellungsseite ist in Allgemein, Übertragungen, Verbindungen, Passwörter, Bearbeiten und Updates gegliedert und scrollt bei kleinen Fenstern.
- Die Editor-Einstellung nimmt eine Befehlszeile mit Argumenten, zum Beispiel `code --wait %1`; `%1` steht für die Datei, sonst wird die Datei am Ende angehängt.
- Ein laufender Vergleich folgt dem aktiven Tab: Wechseln, Öffnen oder Schließen von Tabs vergleicht den neu aktiven Tab, und der Vergleich endet, wenn kein zweiter Tab mehr übrig ist.

#### Behoben

- Nach einem Absturz zurückgebliebene temporäre Bearbeitungsordner werden beim nächsten Start gelöscht.

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
