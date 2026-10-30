# Lokale Build- und Paketdaten

Vor lokalen Builds `source r3d/dev-env.sh` ausführen. Die R3D-Skripte erwarten
das separat eingehängte APFS-Volume `/Volumes/LightCraft Build`, das als Volume
von Time Machine ausgeschlossen ist. Fehlt es oder ist die Prüfung uneindeutig,
brechen die Skripte ab; sie legen keinen Ersatzordner auf Media an.

Eine Time-Machine-Ausnahme für einen Ordner auf Media verhindert nicht, dass
APFS-Snapshots des Media-Volumes dessen gelöschte temporäre Daten festhalten.
Deshalb gehören Cargo-Targets, temporäre Pakete, Test-Apps, große Renderdaten
und entpackte Rückroll-/Probeentpackungen auf das separate Build-Volume.

Vorgaben unter `$R3D_SCRATCH_ROOT/lightcraft/`:

- `CARGO_TARGET_DIR`: `target-$R3D_INSTANCE`
- `R3D_TMPDIR` (auch als `TMPDIR` exportiert): `tmp-$R3D_INSTANCE`
- `DIST`: `dist-$R3D_INSTANCE`
- `R3D_BACKUP_STAGING`: `backup-staging` für künftige Sicherungs-/Entpackskripte
- `R3D_AI_STAGING`, `R3D_AI_APP`, `R3D_AI_DIST` und `R3D_AI_TEST_APP`:
  Paketstaging und große lokale Apps auf demselben Volume

Explizite Pfade bleiben erhalten, müssen aber absolut innerhalb des geprüften
Scratch-Volumes liegen. Symlink-Ausbrüche und eingebettete andere Volumes werden
abgewiesen. Ein alternatives `R3D_SCRATCH_ROOT` muss selbst der Mountpoint eines
separaten, ausgeschlossenen APFS-Volumes sein. Das vom Login geerbte macOS-
`TMPDIR` wird bewusst durch `R3D_TMPDIR` ersetzt.

Kleine finale Binaries und Nachweise dürfen weiterhin unter `R3D_AI_ARTIFACTS`
auf Media liegen. Zusammengehörige Rückrollsätze direkt auf Scratch aufbauen
und dort probeentpacken; erst das verifizierte finale ZIP nach `backups/` auf
Media übernehmen. Dafür beim Entpacken mit `ditto` weiterhin `umask 022` nutzen.
Historische Nachweisskripte wurden nicht umgeschrieben. Produktive Konfiguration,
Bibliothek und Modelle werden durch diese Build-Umgebung nicht verschoben.

Die Prüfung verhindert Fehlkonfigurationen zum Startzeitpunkt. Das Volume während
eines Builds nicht aushängen und seine Time-Machine-Ausnahme nicht entfernen.
Direkte Cargo-Aufrufe ohne diese Umgebung sind davon nicht erfasst.
