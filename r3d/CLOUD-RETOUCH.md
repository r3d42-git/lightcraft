# Optionale ChatGPT-Retusche: Objekt- und Schattenentfernung

Stand: 05.11.2026 · Die Cloud-Option mit abschaltbarem Helligkeitsabgleich wurde
als R3D.48 persönlich abgenommen und privat installiert. Die Erweiterung des
Abgleichs auf lokale Modelle wird separat geprüft; siehe `LOCAL-RETOUCH-LIGHTING.md`.
Paritätsstatus und Zählwerte bleiben unverändert.

## Voraussetzungen und Kontingent

Die Desktop-App kann für **KI-Entfernen** optional ChatGPT über einen lokal
installierten Codex verwenden. Standard bleibt **Lokal auf diesem Mac**.
Codex muss mit dem gewünschten ChatGPT-Konto angemeldet sein; das Konto muss die
integrierte Bildgenerierung anbieten. Ein vorhandenes Pro-Abonnement wird über
diese Anmeldung genutzt. Die Anwendung fordert keinen API-Schlüssel an und hat
keinen API-Fallback. Verfügbarkeit und Kontingent hängen vom Konto ab.

Die integrierte Bildgenerierung zählt zum Codex-Kontingent des ChatGPT-Abonnements
und kann mehr davon verbrauchen als reine Textanfragen. Die offiziellen
[Hinweise zur Bildgenerierung](https://learn.chatgpt.com/docs/image-generation)
beschreiben diesen Verbrauch. Die lokale Verbindung verwendet das
[Codex App Server-Protokoll](https://learn.chatgpt.com/docs/app-server).
Es werden weder Credits gekauft noch eigene Abrechnungsdaten eingerichtet.

## Bedienung und übertragene Daten

1. Objekt lokal anklicken oder den gewünschten Bereich mit dem Pinsel auswählen.
   Die vorhandenen Korrektur- und Schutzpinsel bleiben verfügbar.
2. Unter **Verarbeitung** **ChatGPT über Codex** wählen. Der Wechsel erhält die
   Auswahl und verwirft einen bisherigen Entwurf. Während einer Berechnung oder
   Objektauswahl ist der Anbieterwechsel gesperrt.
3. Optional **Schatten und Spiegelungen einbeziehen** aktivieren. Dafür ist die
   unterstützte vertrauenswürdige lokale ObjectClear-Laufzeit erforderlich.
   Objektentfernung allein benötigt kein lokales Retuschemodell.
4. **Helligkeit an Umgebung angleichen** ist für neue Cloud-Berechnungen
   standardmäßig aktiv und lässt sich ausschalten. Die Wahl gilt nur für die
   aktuelle Sitzung. Kann bei Licht- oder Materialkanten unpassend wirken.
   Bei Bedarf ausschalten. Auch am fertigen Entwurf lässt sich zwischen
   abgeglichenem und unverändertem Cloud-Ergebnis umschalten, ohne eine weitere
   Cloud-Anfrage oder zusätzlichen Kontingentverbrauch. Vorhandene gespeicherte
   Retuschen werden dadurch nicht verändert.
5. **Mit ChatGPT entfernen** startet ausdrücklich die Übertragung an OpenAI.
   Gesendet werden ein vorbereiteter Bildausschnitt mit umgebendem Kontext und
   dessen Bearbeitungsmaske als neue PNG-Bilder, außerdem die Bearbeitungsanweisung.
   Die Bilder enthalten keine übernommenen EXIF-/GPS-Metadaten oder Katalogdaten.
   Der sichtbare Inhalt des Kontextausschnitts wird auch außerhalb der Maske
   übertragen; die Maske begrenzt die spätere lokale Änderung.
6. Vorschau prüfen und ausdrücklich übernehmen oder verwerfen. Die bestehende
   Original-/Entwurf-Ansicht und Rückgängig/Wiederholen bleiben verfügbar.

Die Bilddateien werden aus den vorbereiteten Pixeln neu kodiert. Die lokale
Temporärablage wird nach der Anfrage entfernt; das ist keine Aussage über die
Aufbewahrung der an OpenAI übertragenen Daten. Der Dienstzugriff erfolgt über
das integrierte gehostete Bildwerkzeug von Codex mit ChatGPT-Anmeldung.
Externe MCP-Verbindungen, eigene Provider-Endpunkte und API-Schlüssel-Abrechnung
werden für diesen Ablauf nicht zugelassen.

## Lokale Begrenzung und Schatten

Die vorhandene Vorbereitung berücksichtigt aufgelöste Objektivkorrektur und
Bildgeometrie. Bei aktivierten Effekten bestimmt ObjectClear lokal die
Objekt-/Schattenabdeckung. ChatGPT erhält zur Rekonstruktion die ursprünglichen
RGB-Pixel unter dieser Abdeckung; das von ObjectClear erzeugte Ergebnisbild wird
nicht als Zwischenbild übernommen. Lokales RORem wird hierfür nicht benötigt.
Die lokale Option **Untergrund detailliert rekonstruieren** gehört weiterhin zum
lokalen Anbieter und wird im ChatGPT-Modus ausgeblendet.

Die App begrenzt den endgültigen Rasterpatch lokal auf die erlaubte Maske bzw.
Effektabdeckung. Schutzpinsel und Pixel außerhalb dieser Abdeckung bleiben
unverändert. Ein Fehler oder Abbruch speichert keinen neuen Patch. Erst
**KI-Ergebnis übernehmen** schreibt die Retusche als bestehenden Rasterpatch mit
Provenienz; Öffnen und Export erfordern danach weder Codex noch KI-Modelle.
Vorhandene Retuschen werden nicht neu berechnet.

Der abschaltbare Helligkeitsabgleich erfolgt lokal auf dem vorbereiteten
Retuscheraster. Die mit σ = 32 Pixeln geglättete Beleuchtung des Modellbilds wird
innerhalb der erlaubten Füllung durch einen Verlauf aus den unveränderten
Randwerten ersetzt; die feinere erzeugte Struktur bleibt erhalten.
Nur die erlaubte Füllung wird korrigiert; Schutzpinsel und Pixel
außerhalb der Maske bleiben erhalten. Das ist keine bildspezifische
Randüberblendung. Der Abgleich kann Helligkeitsübergänge verbessern, garantiert
aber weder passende Textur noch korrekte Material- und Lichtkanten.

Für den aktuellen Entwurf werden das ursprüngliche Cloud-Ergebnis und die lokal
abgeglichene Variante aus derselben Cloud-Antwort vorübergehend gehalten. Die
Checkbox wechselt zwischen diesen Varianten und aktualisiert die Vorschau; sie
startet keine weitere Bildgenerierung. Während einer Berechnung oder
Objektauswahl ist die Checkbox gesperrt. Schlägt der Abgleich fehl, meldet die
App den Fehler und zeigt das ursprüngliche Ergebnis nicht als erfolgreich
abgeglichen an. Durch Ausschalten lässt sich das bereits empfangene
Cloud-Ergebnis ohne Abgleich verwenden.

Übernehmen, Verwerfen sowie Foto- oder Anbieterwechsel verwerfen das temporäre
Variantenpaar. Gespeicherte Patches bleiben unverändert.

Generierte Inhalte, Hintergrundstruktur und die räumliche Übereinstimmung
innerhalb der Maske sind nicht garantiert. Die lokale Begrenzung verhindert
keine erfundenen Details oder sichtbaren Übergänge innerhalb des erlaubten
Bereichs. Die Abnahme am Tischmotiv ist keine allgemeine Garantie für andere
Fotos. RAW-Entrauschung und SAM-Objektauswahl bleiben lokal.
Im WASM-Build ist der ChatGPT-Anbieter nicht verfügbar.

## Schnittstellen und Quellen im Projekt

- `aiRemove.provider {"provider":"local"|"chatgpt"}` wählt den Anbieter.
- `aiRemove.matchLighting {"enabled":true|false}` setzt den sitzungsbezogenen
  Helligkeitsabgleich; Standard ist `true`. `aiRemove.status` liefert dazu
  `matchLighting` als Boolean.
- `aiRemove.start` akzeptiert optional `provider`, `includeEffects` und
  `matchLighting`; im ChatGPT-Modus wird kein ausgewähltes lokales `modelId`
  mitgesendet.
- `aiRemove.status`, `aiRemove.cancel`, `aiRemove.accept` und die vorhandenen
  Vorschaukommandos behalten den bisherigen Entwurfsablauf.
- Umsetzung: `crates/engine/src/ai_remove/cloud.rs`,
  `crates/engine/src/ai_remove/codex_client.rs`,
  `crates/engine/src/ai_remove/runner.rs` und
  `crates/ui-egui/src/panels/right/crop.rs`.
