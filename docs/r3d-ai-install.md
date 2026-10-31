# KI-Entfernung selbst einrichten (Apple Silicon)

LaMa, RORem und ObjectClear sind optionale, getrennte Zusatzpakete. Die LightCraft
R3D-App enthält keine Modellgewichte. Sie benötigt zum Ausführen dieser Pakete
weder Python noch einen Server; Fotos bleiben lokal. Der native Einrichtungshelfer
benötigt ebenfalls kein Python, Homebrew, Git oder Xcode.

## Voraussetzungen und Downloads

- Mac mit Apple Silicon, macOS 13.4 oder neuer; eine LightCraft-R3D-Version mit den
  passenden freigegebenen Laufzeitbindungen (ab Quellstand `a520892d`). Dies ist
  eine Fork-Funktion, keine Zusage zur unveränderten Upstream-App.
- Separates LightCraft-KI-Einrichtungspaket mit `lightcraft-model-setup`,
  `model-recipes`, `Runtimes` und vollständigen Lizenzhinweisen. Der Quellcode des
  Einrichtungshelfers allein enthält **keine** der nativen Modelllaufzeiten.
- Platz für die gewählten Originaldownloads, die vorbereiteten Modellpakete und
  die spätere Kopie in die KI-Ablage. Die Vorbereitung löscht keine Downloads.
  LaMa benötigt ca. 208 MB, RORem ca. 6,94 GB, ObjectClear ca. 7,83 GB reine
  Modelldaten; für alle drei mit den Zwischenkopien mindestens 50 GB freihalten.
  Laufzeit- und Inferenzspeicher kommt hinzu. RORem und ObjectClear nutzen Metal;
  LaMa nutzt die CPU. Keine Intel-/Windows-/Linux-Unterstützung dieser Pakete.

Nur die festgelegten Revisionen und Dateinamen verwenden. `main`, beliebige
andere Varianten oder Git-LFS-Zeigerdateien werden nicht akzeptiert.

| Modell | Originalquelle und feste Revision |
|---|---|
| LaMa | [Carve/LaMa-ONNX](https://huggingface.co/Carve/LaMa-ONNX/tree/c3c0c9e468934d62e79c329e35d82dd09ff8c444) |
| RORem | [LetsThink/RORem](https://huggingface.co/LetsThink/RORem/tree/22d02de974c8dc92c717cd3431f8b0fe0eeecc3b) |
| ObjectClear | [jixin0101/ObjectClear](https://huggingface.co/jixin0101/ObjectClear/tree/c73af80888dbd519819d0a521b5c8d0f3cda6859) |
| ObjectClear ergänzende Dateien | [zjx0101/ObjectClear](https://github.com/zjx0101/ObjectClear/tree/c052d91ecd8772744a5ab97527441c813ab83009) |

Die genaue benötigte Dateiliste steht in `model-recipes/<modell>.json`. Die
Hugging-Face-Dateien in ihrer ursprünglichen Ordnerstruktur speichern. Bei
ObjectClear zusätzlich den Quellordner der oben angegebenen GitHub-Revision
entpacken. Darin enthaltene Python-Dateien werden nur als fest geprüfte
Paketbestandteile kopiert, **nicht ausgeführt**.

Der Helfer kann alternativ ein lesbares Downloadskript erzeugen. Erst das selbst
gestartete Skript lädt die benötigten Dateien direkt von den Originalquellen.
Die App lädt nichts automatisch herunter. Vorher die Bedingungen der Quellen
lesen; ein eigener Download hebt deren Einschränkungen nicht auf.

## Einrichtung

1. Einrichtungspaket entpacken und `KI einrichten.command` öffnen. Bei der ersten
   Einrichtung **0 – Neue KI-Ablage einrichten** wählen und einen neuen dauerhaften
   Ordner angeben. Der Helfer prüft und kopiert die drei nativen Laufzeiten in
   diesen Ordner und erzeugt die neue Registry. Ein vorhandener Ordner wird nicht
   überschrieben. Noch keine Gewichte werden installiert.
2. Den Starter erneut öffnen und ein Modell wählen. Optional zuerst das
   Downloadskript erzeugen und selbst starten, beispielsweise mit
   `LIGHTCRAFT_ACCEPT_MODEL_LICENSES=yes /bin/sh /Pfad/zum/download.sh`.
   Die Downloads landen neben dem Skript in `snapshot/` und bei ObjectClear
   zusätzlich `objectclear-source/`. Jeder Modelldownload benötigt einen eigenen
   neuen Ordner. Ohne Zustimmung zu den Quellenbedingungen startet kein Download.
3. Den vollständig heruntergeladenen Modellordner wählen; bei ObjectClear auch
   den entpackten GitHub-Quellordner. Einen Zielordner für das neue Paket wählen.
   Jede benötigte Datei wird nach Größe und SHA256 geprüft. Ein vorhandenes
   Ziel wird nicht ersetzt. Es werden keine Modellskripte ausgeführt.
4. In LightCraft im Entfernen-Werkzeug **Modelle verwalten…** öffnen. Bei neuer
   Einrichtung mit **Bestehende KI-Ablage wählen…** den in Schritt 1 erzeugten
   Ordner wählen. Keine fremde `registry.json` kopieren oder bearbeiten. Die App
   prüft die Laufzeiten nochmals gegen ihre eigenen Vertrauensbindungen.
5. **KI-Paket importieren…**: das neu vorbereitete **Modellpaket** wählen
   (den Ordner mit `package.json`). Für jedes gewünschte Modell wiederholen.
   Bei einer schon vorhandenen KI-Ablage statt Schritt 1 zuerst den jeweils
   passenden Laufzeitordner aus `Runtimes` importieren, dann das Modellpaket.
   Bereits registrierte Revisionen nicht erneut importieren; **Prüfen** verwenden.
6. Bei einer vorhandenen Python-Einrichtung unter **Laufzeit** ausdrücklich
   **Native Laufzeit (Rust)** wählen. Bestehende Einstellungen werden durch den
   Import nicht automatisch umgestellt. Bei neuer nativer Ablage wird die
   passende importierte native Laufzeit verwendet.
7. Im Werkzeug das gewünschte Modell auswählen, eine kleine Auswahl testen,
   Vorschau prüfen und erst dann übernehmen.

Nach erfolgreichem Import und Test können die heruntergeladenen und vorbereiteten
Zwischenkopien gelöscht werden. Den aktiven KI-Ordner und seine `registry.json`
erhalten und sichern; die App verwendet die beim Import dort abgelegten Kopien.
App-Updates kopieren diese Daten nicht erneut. Gespeicherte Retuschen lassen sich
auch ohne Modelle weiter anzeigen und exportieren.

## Bedingungen der optionalen Pakete

ObjectClear und sein davon abgeleiteter nativer Zusatz sind nach
[S-Lab License 1.0](https://github.com/zjx0101/ObjectClear/blob/c052d91ecd8772744a5ab97527441c813ab83009/LICENSE)
für **nichtkommerzielle Zwecke** vorgesehen. Die getrennte Installation erlaubt
keine kommerzielle Nutzung. Für andere Zwecke die Rechteinhaber kontaktieren.
Der Zusatz wird nicht unter die MIT-/Apache-Lizenz der App umgedeutet.

LaMa nennt Apache-2.0. RORem nennt Apache-2.0; für seine SDXL-Inpainting-Basis
sind zusätzlich die CreativeML Open RAIL++-M-Bedingungen relevant. Entsprechende
SDXL-Basisbedingungen liegen auch den ObjectClear-Hinweisen bei. Vollständige
Hinweise und bekannte Unklarheiten zu Gewichten stehen unter `Model-Notices`.
Die Modellrechte sind unabhängig von der Lizenz des Installationshelfers.

## Probleme

- **Falsche Prüfsumme / Größe:** genaue Revision und richtigen Dateinamen prüfen;
  die Datei vollständig erneut von der Originalquelle laden. HTML-Fehlerseiten
  und kleine LFS-Zeiger sind keine Gewichte. Nicht `package.json` anpassen.
- **Unbekannte native Laufzeit / inkompatibles Modell:** App und Zusatzpakete
  müssen zusammenpassen. Vertrauensbindungen niemals abschalten oder selbst
  durch Registry-Einträge ersetzen.
- **Symbolischer Link:** für die Einrichtung echte Snapshot-Dateien verwenden,
  keine verlinkten Hugging-Face-Cache-Dateien.
- **Abbruch / zu wenig Platz:** der aktive KI-Ordner bleibt unverändert. Einen
  anderen Zielordner mit genügend Platz wählen und erneut vorbereiten.
- **Langsame Berechnung:** RORem ist im aktuellen nativen F32-Pfad langsamer als
  der frühere Python-F16-Pfad. Native Ausführung ist kein Geschwindigkeitsversprechen.
- **macOS blockiert ein Paket:** nur ein zur App passendes, verifiziertes
  Veröffentlichungspaket verwenden. Keine Quarantäneattribute entfernen und
  keine Systemeinstellungen durch Installationsskripte abschalten.

## Stand der Vorbereitung

In diesem Fork werden Änderungen **committed und gepusht, keine Releases
erstellt**. Ein möglicher späterer Upstream-PR ist ein gesonderter Auftrag;
für den 31.10.2026 ist kein PR vorgesehen. Aus der Vorbereitung folgt kein
Signierungs-, Notarisierungs- oder Veröffentlichungsauftrag.

Der lokale Einrichtungskandidat ist ein **Entwicklungsartefakt**, kein öffentlicher
oder notarisierter Download. Die bisherigen Laufzeitpakete tragen historische
Prototyp-Metadaten; diese werden beim Vorbereiten nicht heimlich geändert. Die
persönliche App bleibt unverändert. Eine hypothetische spätere Verteilung der
Zusatzpakete wäre getrennt zu prüfen; sie ist kein geplanter Fork-Release.
Technisch gilt unabhängig davon: Signieren verändert die Binärhashes. Eine
veränderte Laufzeit braucht deshalb passende neue App-Vertrauensbindungen.
