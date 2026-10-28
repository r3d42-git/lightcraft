# R3D: Abnahmeplan für Entrauschung pro Foto

Die Erweiterung wird zuerst in einer getrennten Testfassung mit eigener Konfiguration
und Testbibliothek geprüft. Die installierte App, normale Bibliothek und Originalfotos
werden dafür nicht verwendet. Testeingaben sind die vorhandenen zwölf autorisierten
Sony-/Canon-Kopien. Die Modelle liegen bereits separat und SHA-geprüft vor.

## Automatische Nachweise

- Alte positive Entrauschungsstände einschließlich ausgeschalteter Zustände, Historie
  und Versionen behalten das frühere Verfahren. Alben, Stapel, Bearbeitungszeit und
  Historiencursor bleiben erhalten; fehlgeschlagene Journalmigration ist wiederholbar.
- Zwei Fotos können verschiedene Verfahren behalten. Neuer Standard wirkt nur auf
  neue Zuordnungen. Auswahl bei ausgeschalteter Entrauschung schaltet sie nicht ein.
- Rückgängig/Wiederholen, Neustart und Detail-Copy/Paste erhalten die Zuordnung.
- Bild-/Modellwechsel während eines Jobs übernimmt kein verspätetes fremdes Ergebnis.
  Vorschau und Export verwenden den passenden Modellcache; Export berücksichtigt
  einen gespeicherten Bearbeitungsstand statt späterer Änderungen am Foto.
- Fehlendes, entferntes, ersetztes oder verändertes Modell erzeugt einen klaren Fehler
  ohne stillen Wechsel zu BM3D. Normale Bilder ohne Entrauschung bleiben renderbar.
- Vollständige Projekt-CI; echte Metal-Bildläufe und isolierte Kontrollkanal-/UI-Prüfung.

## Persönliche Prüfung der Testfassung

Diese Ergebnisse werden getrennt von Agententests erfasst. Frühere BM3D-Abnahmen
werden nicht als ungeprüft bezeichnet und nicht durch diese neue Abnahme ersetzt.

1. Im Detailbereich BM3D, LightCraft und RawNIND wählen. Bei S01/S04 Haut und Stoff,
   bei S06 Metall, bei S11/S12 Ornamente und feine Oberflächen in 100 % ansehen.
   Stärke 50/100 wechseln; Vergleichsmaßstab sind die bereits erstellten Tafeln.
2. Zwei Fotos auf verschiedene Verfahren setzen, mehrfach wechseln und zurückkehren.
   Aus/An erhält Methode und Stärke; Wechsel bei ausgeschaltetem Entrauschen lässt
   es aus. Rückgängig und Wiederholen betreffen nur das beabsichtigte Foto.
3. In den Einstellungen den Standard ändern. Bestehende Zuordnungen bleiben bestehen;
   ein noch nie entrauschtes Foto bekommt beim Einschalten den neuen Standard.
4. Testfassung schließen und erneut öffnen. Zuordnungen und Bilder prüfen. Je einen
   Export mit verschiedenen Verfahren mit Vorschau/Ausschnitt vergleichen.
5. In der Testkonfiguration ein verwendetes Modell entfernen: Hinweis beim Foto und
   Fehler beim Export, kein anderer Look als Ersatz. Nach Wiederinstallation oder
   bewusster neuer Wahl funktioniert die Berechnung wieder.

Vor einer späteren normalen Installation werden die bis dahin bestätigten Ergebnisse
und verbleibenden Punkte abgeglichen. Katalogformat 10 benötigt eine getrennte
Testbibliothek; ein normaler Katalogwechsel erfolgt erst mit passender Sicherung.
