# Lokale KI-Retusche – privater Mac-Prototyp

Stand 30.10.2026. Persönlicher Nutzerauftrag: lokale, non-destruktive
Objektentfernung auf dem Mac Studio direkt aus LightCraft, ohne zusätzlich zu
startendes Programm. Hintergrundtausch und generatives Ersetzen bleiben spätere
Ausbaustufen. LM Studio ist für die jetzige Funktion nicht nötig.

## Bedienung und Verhalten

Im Entfernen-Werkzeug ist „KI entfernen“ standardmäßig aktiv. Das Objekt mit
einem zusammenhängenden Pinselstrich übermalen und die Entfernung starten.
Die cyanfarbene Auswahl ist vorübergehend und verändert das Foto noch nicht.
Ein neuer Strich ersetzt die bisherige Auswahl; mehrere getrennte Striche werden
noch nicht addiert. Der klassische Entfernen-/Heilen-/Klonmodus bleibt erreichbar.

Die entwickelte Vorschau erscheint im Hauptfoto einschließlich Weißabgleich,
Tonwerten und Geometrie. Originalansicht, Verwerfen und erneute Variante sind
vor dem Übernehmen möglich. Erst Übernehmen speichert eine Retusche. Undo/Redo,
Deaktivieren und Löschen bleiben möglich. Ein Foto-, Bibliotheks- oder
Bearbeitungswechsel verwirft veraltete Ergebnisse.

LightCraft startet und beendet seinen lokalen Modellprozess selbst. Abbruch
beendet den Kindprozess; Timeout nach 600 Sekunden. Es gibt keinen Netzwerkserver,
keine Laufzeit-Downloads und keine Fotoübertragung. Vorhandene Ergebnisse lassen
sich auch ohne Modell neu anzeigen und exportieren.

## Modell und Datenpfad

ObjectClear ist der Standard, LaMa und RORem bleiben zum Vergleich auswählbar.
ObjectClear: Modell `jixin0101/ObjectClear`, Revision
`c73af80888dbd519819d0a521b5c8d0f3cda6859`; ergänzende offizielle Pipeline
`c052d91ecd8772744a5ab97527441c813ab83009`. FP16-Modelldateien werden auf diesem Mac
mit FP32 auf Metal berechnet. Pipelinequellen werden vor dem Import gegen ihre
SHA256-Werte geprüft. Standard: 20 Schritte, CFG 2,5, erste Variante Seed 42.
Weitere Modellstarts erhalten andere Seeds.

Die Auswahl bekommt einen quadratischen Kontextausschnitt bis 1024 Pixel;
ObjectClear verarbeitet intern 512 Pixel. Die Quellfarben werden aus linearem
Rec.2020 mit gespeicherter Helligkeitsskalierung nach sRGB und zurück geführt.
Eine harmonische Fortsetzung der Randdifferenzen gleicht lokale Farb- und
Helligkeitsabweichungen in linearem RGB ab. Rust erzwingt die ursprüngliche
Pinselbegrenzung mit ausschließlich nach innen laufender 15-%-Überblendung;
Modelländerungen außerhalb der Auswahl werden verworfen.

Gespeichert werden komprimierte lineare RGBA-Patches, Quellkoordinaten,
Modellrevision und Seed. Die Komposition erfolgt vor Geometrie, Weißabgleich
und Tonwerten. Der Katalog verwendet Format 11. Retuschen werden nicht über
Presets oder Auto-Sync auf andere Fotos übertragen. Alte Kreis-Auswahlen behalten
bei Deserialisierung ihre Semantik; neue Auswahlen verbinden die Punkte lückenlos.
Der Pipeline-Cache unterscheidet beide Maskenformen.

## Eigenständige Test-App

`r3d/build-ai-dev.sh` erstellt mit den vorhandenen Projekt-Packagingwerkzeugen:

`/Volumes/Media/codex/lightcraft/test-runtime/ai-retouch-20261030/LightCraft KI Test.app`

Die eigene Kennung ist `local.r3d.lightcraft.ai-retouch-dev`. Die App enthält
CPython 3.11, alle Modellabhängigkeiten, ObjectClear/LaMa/RORem, Lizenzen und ein
Dateihash-Manifest in `Contents/Resources/ai-retouch`. Keine externe Python-
Installation und kein Laufzeitordner sind beim Start nötig. Der Test-Build ist
lokal ad-hoc signiert und strikt geprüft; keine notarisierten Release-Artefakte.
Das Modellpaket umfasst logisch etwa 14,7 GiB. APFS-Klone begrenzen Kopieraufwand;
dies ist keine Aussage über die auf anderen Datenträgern benötigte Größe.

Private Bibliothek, Konfiguration und RAW-Kopie liegen unter:

`/Users/r3d/Library/Application Support/LightCraft KI Test 20261030/`

Der Buildpfad ist über `R3D_AI_TEST_DATA` überschreibbar. Die normale produktive
App, Bibliothek und Konfiguration wurden nicht verändert. Bei einem normalen
LaunchServices-Start blockierten Zugriffe auf externe Test-Datenpfade in
Dateisystemaufrufen; mit internen Testdaten funktioniert derselbe App-Code.
Die konkrete macOS-Berechtigungsursache ist nicht bewiesen. Auch Testexporte
werden deshalb intern geschrieben und anschließend in den Nachweisordner kopiert.

`tools/ai_retouch/requirements.txt` hält die Hauptabhängigkeiten fest.
`package_runtime.py` prüft eingebettete Pfade und Mach-O-Abhängigkeiten,
signiert native Dateien und ersetzt eine vorhandene Test-App erst nach Prüfung.
Die private Build-Laufzeit `test-runtime/ai-retouch-dev` enthält zusätzlich die
FLUX-Forschungsmodelle; diese werden nicht in die Test-App aufgenommen.

## Qualitätsstand und bestätigte Prüfungen

Die isolierte Kopie von `20141005_a7_Mannheim_DSC00136.ARW` wurde durch die native
App verarbeitet und in 4000×6000 gerendert. ObjectClear entfernt die Person in
diesem Beispiel deutlich plausibler als die bisherigen RORem-Varianten; die
beanstandete helle Silhouette ist im großen Vergleich nicht mehr sichtbar.
Das ist keine persönliche Nutzerabnahme und kein allgemeiner Nachweis von
iPhone-Parität. Verdeckte Strukturen werden vom Modell erfunden.

Drei zusätzliche FLUX.1-Fill-Versuche mit frisch bezogenen Originalgewichten
waren bei diesem Motiv schlechter: zusätzliche Pfosten/Blöcke beziehungsweise
eine neue Person. Keine Comfy-Dateien verwendet. Gewichthashes und die bewusste
Verwendung des offiziellen Diffusers-FP16-VAE sind im Forschungsnachweis getrennt
dokumentiert. FLUX ist deshalb nicht der Produktstandard.

Bestätigt: eingebettete Inferenz nach normalem App-Start, Vorschau ohne Änderung
der gespeicherten Entwicklung, Übernehmen, Abbruch mit Prozessende, Undo/Redo,
Wiederöffnen und persistierte Ergebnisse. Ein vollständiger Vorgang dauerte
22,3 Sekunden einschließlich Modellstart; Einzelmessung, kein Benchmark.
Python-Protokoll-/Farbtests, Rust-Regressionen für Maskengrenzen, Cache,
Persistenz, Abbruch und UI sind vorhanden. Der aktuelle vollständige CI-Bericht
ist grün: alle acht Gates bestanden, Metal auf Apple M5 Max. 172 Capability-Skips
(137 Korpus, 27 Schriften, 8 Modelle) sind ausdrücklich dokumentiert;
fehlende RAW-Korpora, optionale Schriften und Modelle gelten nicht als geprüft.
Ein späterer Nutzerbefund deckte eine fehlende Vorschau auf profilierten Monitoren
auf: Der Texturvergleich verwendete einen Render-Key ohne Monitorprofil. Die
Korrektur berücksichtigt das Profil vor dem Vergleich; ein DisplayP3-Test
reproduziert den Fehler im alten Stand und besteht mit der Korrektur. Native
Vogel-/Vergleichsschalter-Nachweise liegen separat unter `preview-fix/`.

Aktuelle UI-Aufnahmen während der Abwesenheit sind ausdrücklich Headless-Render,
keine Bildschirmaufnahmen des macOS-Compositors.

## Verbleibende Grenzen

- Erzeugung bei aktiver nichtaffiner Optik-/Perspektivkorrektur wird derzeit
  abgelehnt. Vorhandene Patches folgen späteren Geometrieänderungen.
- Ein zusammenhängender Auswahlstrich; automatische Objekt-/Schattenauswahl,
  additive Striche, Hintergrundtausch und generatives Ersetzen sind noch offen.
- SDR-Modellkonvertierung begrenzt negative und außerhalb sRGB liegende Werte.
  Allgemeine RAW-/HDR-Qualität ist nicht abgenommen.
- Höchstens acht Patches und insgesamt zwei Millionen gespeicherte Patchpixel.
  Der Modellstart benötigt vorübergehend eine vollständige Quellbildkopie.
- Die Entwurfsvorschau rendert das Gesamtbild; bei starkem Zoom kann sie weicher
  sein als das anschließend übernommene Vollauflösungsergebnis.
- Persönlicher Qualitätsvergleich und Freigabe einer produktiven Installation
  stehen aus. Eine öffentliche Veröffentlichung ist nicht beauftragt.

Nachweise: `/Volumes/Media/codex/lightcraft/evidence/ai-retouch-20261030/`,
insbesondere `integrated/comparison-detail.png`, `integrated/native-flow.json`,
`integrated/normal-launch-internal-test-data.json` und `model-comparison/`.
