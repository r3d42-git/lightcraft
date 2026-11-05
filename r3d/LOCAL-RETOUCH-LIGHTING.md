# Abschaltbarer Helligkeitsabgleich für lokale Entfernung

Stand: 05.11.2026 · Neuer Entwicklungskandidat auf Basis der bereits abgenommenen
Cloud-Option R3D.48. Persönliche Abnahme und produktive Übernahme dieser lokalen
Erweiterung sind gesondert festzuhalten. Keine allgemeine Qualitäts-/Paritätszusage.

## Bedienung

Unter **Entfernen → Verarbeitung → Lokal auf diesem Mac** steht ebenfalls
**Helligkeit an Umgebung angleichen** zur Verfügung. Die Option ist standardmäßig
aktiv und gilt für die Sitzung. Sie bleibt beim Anbieterwechsel erhalten.
Der lokale Abgleich verarbeitet keine Daten in der Cloud und benötigt keinen
weiteren Modelllauf. Unverändertes und abgeglichenes Ergebnis stammen aus derselben
Inferenz und lassen sich am Entwurf umschalten. Erst **KI-Ergebnis übernehmen**
speichert die gewählte Variante. Vorhandene Rasterretuschen werden nicht geändert.

Die Option wirkt sowohl auf die direkte lokale Entfernung als auch auf die
ObjectClear → RORem-Kette. Bei der Kette wird erst das fertige rekonstruierte
Ergebnis innerhalb der endgültigen Effektabdeckung abgeglichen. Beide Varianten
behalten dieselbe Maske, Schutzpinsel, Geometrie und Modell-/Stufenprovenienz.
Die abgeglichene Variante trägt im Modellnamen zusätzlich
`lighting=local-boundary-v1`; Modellrevision und Stufenidentitäten bleiben exakt
erhalten, damit auch ältere Versionen die gespeicherten Rasterpatches öffnen können.

## Lokale Methode und Grenzen

Lokale Worker liefern außerhalb der erlaubten Füllung bereits Originalpixel.
Deshalb wird der künstliche Rand dieser Zusammensetzung nicht als Bildstruktur
gefiltert. Stattdessen werden die Helligkeitsverläufe auf beiden Seiten getrennt
mit gewichteten lokalen Ebenen geschätzt (fester Radius 12 Rasterpixel, σ=6).
Ihre Differenz an der Grenze wird als glatte additive RGB-Korrektur in die
Füllung fortgesetzt. Referenzdaten des Originals werden ausschließlich außerhalb
der erlaubten Füllung einschließlich geschützter Bereiche verwendet.

Die generierte innere Struktur bleibt bestehen. Unterschiedliche Maserung,
Materialkanten oder breite Lichtfehler im Inneren können weiterhin sichtbar sein.
Ein einzelner Bildtest beweist keine allgemeine Verbesserung. Bei ungeeigneten
Licht-/Materialgrenzen kann der Abgleich unpassend sein; dann ausschalten.

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
