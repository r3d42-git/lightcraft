# Abschaltbarer Helligkeitsabgleich für lokale Entfernung

Stand: 05.11.2026 · Lokaler Abgleich auf Basis der Cloud-Option R3D.48.
Die produktive Übernahme zum Alltagstest ist vom Nutzer beauftragt. Bekannte
Qualitätsgrenzen bleiben bestehen; keine allgemeine Qualitäts-/Paritätszusage.
Der tatsächlich installierte Stand ist im separaten Installationsnachweis festzuhalten.

## Bedienung

Unter **Entfernen → Verarbeitung → Lokal auf diesem Mac** steht ebenfalls
**Helligkeit an Umgebung angleichen** zur Verfügung. Die Option ist standardmäßig
aktiv und gilt für die Sitzung. Sie bleibt beim Anbieterwechsel erhalten.
Der lokale Abgleich verarbeitet keine Daten in der Cloud und benötigt keinen
weiteren Modelllauf. Unverändertes und abgeglichenes Ergebnis stammen aus derselben
Inferenz und lassen sich am Entwurf umschalten. Erst **KI-Ergebnis übernehmen**
speichert die gewählte Variante. Vorhandene Rasterretuschen werden nicht geändert.

ObjectClear arbeitet standardmäßig direkt, einschließlich der verfügbaren
Schatten-/Reflexionsabdeckung. **Untergrund detailliert rekonstruieren** schaltet
die zusätzliche RORem-Stufe bewusst zu. Die Wahl gilt für neue Retuschen.
Der Patch wird in szenenlinearem RGB im RAW-Entwicklungsablauf gespeichert;
Original-RAW, Bayerdaten und bestehende Retuschen werden nicht überschrieben.
Weitere Entwicklungseinstellungen bleiben auf den gespeicherten Patch anwendbar.
Es gibt keinen Apple-Fotos-/JPEG-Rückweg und keine Nachbartextur-Übertragung.

Die Option wirkt sowohl auf die direkte lokale Entfernung als auch auf die
ObjectClear → RORem-Kette. Bei der Kette wird erst das fertige rekonstruierte
Ergebnis innerhalb der endgültigen Effektabdeckung abgeglichen. Beide Varianten
behalten dieselbe Maske, Schutzpinsel, Geometrie und Modell-/Stufenprovenienz.
Die abgeglichene Variante trägt im Modellnamen zusätzlich
`lighting=local-frequency-v2`; Modellrevision und Stufenidentitäten bleiben exakt
erhalten, damit auch ältere Versionen die gespeicherten Rasterpatches öffnen können.

## Lokale Methode und Grenzen

Lokale Worker liefern außerhalb der erlaubten Füllung bereits Originalpixel.
Deshalb wird der künstliche Rand dieser Zusammensetzung nicht als Bildstruktur
gefiltert. Innerhalb der Maske wird die großflächige generierte Helligkeit mit
maskierten gewichteten affinen Gauß-Fits geschätzt (σ=32, Radius128 Rasterpixel).
Die verbleibende feinere Struktur wird mit einer aus der Umgebung fortgesetzten
Helligkeitsfläche kombiniert. Separate Randfits (Radius12, σ=6) verwenden dafür
Original außen und diese feinere erzeugte Struktur innen; eine harmonische Lösung
setzt die Randwerte in die Füllung fort. Originalpixel innerhalb der Füllung sind
keine Referenz. Geschützte und außerhalb liegende Pixel bleiben exakt erhalten.

Damit werden auch breite generierte Helligkeitswolken im Inneren korrigiert.
Die feinere Maserung bleibt erzeugt und kann weiterhin vom Original abweichen.
Echte nichtlineare Lichtverläufe und Materialkanten innerhalb der Maske lassen
sich aus dem Außenrand nicht zuverlässig erkennen; der Abgleich kann solche
bereits passenden Bildinhalte verschlechtern. Dann ausschalten. Ein einzelner
Bildtest beweist keine allgemeine Verbesserung. Am Bildrand fehlt außerdem
Außenkontext auf einer Seite; dort werden auch einfache Lichtverläufe nicht
zwingend exakt fortgesetzt. Die erste lokale Randkorrektur
(`local-boundary-v1`) ließ breite innere Lichtfehler bestehen; ihre gespeicherten
Patches bleiben unverändert lesbar und werden nicht neu berechnet.

Fehlen genügend zweidimensionale Stützpunkte, ist die Grenze zu komplex oder
würde sich ein Kanal um mehr als 0,125 × HDR-Skalierung ändern, wird kein
abgeglichenes Ergebnis behauptet. Die App zeigt den Fehler; Ausschalten gibt den
bereits vorhandenen unveränderten Entwurf frei. Abbruch, Foto-/Anbieterwechsel und
Übernahme verwerfen das temporäre Variantenpaar. Neustart und Export gespeicherter
Retuschen funktionieren weiterhin ohne Modelle.

## Schnittstellen

`aiRemove.matchLighting {"enabled":true|false}` und der optionale
`matchLighting`-Parameter von `aiRemove.start` gelten für beide Anbieter.
`aiRemove.status.matchLighting` meldet die Sitzungswahl. Umschalten ist während
Inferenz oder Objektauswahl gesperrt. Im Browser ist lokale Inferenz unverfügbar.
Der vorhandene Cloud-Algorithmus und dessen Übertragungsablauf bleiben erhalten.
