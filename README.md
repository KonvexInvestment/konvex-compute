# Konvex Compute

Lokale Software für macOS und Windows, die die GPU-Mietpreis-Märkte von Polymarket als Forward-Kurve darstellt, für H100, H200, B200 und B300. Startpunkt der Kurve ist der Ornn Compute Price Index (OCPI), auf den alle diese Polymarket-Märkte abrechnen.

Beispielprojekt aus dem [KI-Lab von Konvex Investment](https://konvexinvestment.de/ki-lab/). Die erste Fassung entstand mit Gemini Neon in der Antigravity CLI, die Überarbeitung mit Claude Code (Opus 5.5) und einem Review durch Gemini Neon. Technik: Tauri 2, Rust und SQLite, ohne Python, Node.js oder npm.

## Funktionen

- **Forward-Kurve mit Bändern:** links die Ornn-Spot-Historie, rechts die Polymarket-Forward-Kurve bis zum fernsten gelisteten Verfall, dazu Interquartilsband (`P25` bis `P75`), äußeres Band (`P10` bis `P90`), getrimmter Kern-Mean, Roh-Mean inklusive Tail und Skew.
- **Kurvenmathematik ([`math.rs`](src-tauri/src/math.rs)):** Gewichtete isotonische Regression (PAVA) glättet Spread-Zacken im Orderbuch. Daraus entstehen eine konsistente Survival Function für Quantile und Kern-Mean sowie die Wahrscheinlichkeits-Buckets (`PDF`) für das Verteilungs-Histogramm.
- **Tages-, Wochen- und Monatsvergleich (1T, 1W, 1M):** Beim ersten Start rekonstruiert die Software bis zu 14 Tage Forward-Kurven aus der Polymarket-CLOB-Historie (`/prices-history`). Danach speichert sie alle 15 Minuten einen eigenen Snapshot in SQLite. Nach 48 Stunden bleibt je GPU und Tag nur der letzte Stand erhalten. Lücken nach einer Pause füllt ein automatischer Backfill, höchstens alle 20 Stunden.
- **Strike-Leiter:** Ein Klick auf einen Strike lädt dessen 30-Tage-Wahrscheinlichkeitsverlauf aus der CLOB-API, in 14-Tage-Abschnitten, weil die API längere Fenster ablehnt.
- **Erklärungen in der Oberfläche:** Gepunktet unterstrichene Fachbegriffe erklären sich beim Überfahren mit der Maus, der Hauptchart hat eine Legende, Abruffehler erscheinen als kurze Hinweise mit aufklappbaren Details.
- **Alle GPUs im Vergleich:** Relative-Value-Matrix über die vier GPU-Generationen (Verhältnis zur H100, Kurvensteigung) und weitere Ornn-Spot-Indizes (`A100 SXM4`, `RTX 5090`, `RTX PRO 6000 WS`).
- **Finanzierungsregime ([`regime.rs`](src-tauri/src/regime.rs)):** verbindet Kreditaufschläge des Anleihemarkts und der Emittenten Oracle, CoreWeave und SpaceX mit der GPU-Forward-Kurve und erkennt Regimewechsel. Details im Abschnitt [Finanzierungsregime](#finanzierungsregime).
- **Data Store:** Übersicht über die lokale Datenbank, manueller Backfill und CSV-Export in den Download-Ordner.

## Warum der Roh-Mean bei fernen Verfällen über dem Median liegt

Ein GPU-Mietpreis ist nach unten bei `0 $` begrenzt, nach oben offen. Ferne binäre Polymarket-Kontrakte binden zudem lange Kapital, deshalb fallen weit entfernte Strikes kaum unter einen impliziten Zinsboden (Beispielwert: 8 bis 10 Cent). Der volle Erwartungswert (`Roh-Mean`) gewichtet diesen oberen Tail mit der ganzen Dollarbreite und steigt dadurch, während Median (`P50`) und Kern-Mean die zentrale Markterwartung zeigen.

## Finanzierungsregime

GPU-Rechenzentren entstehen überwiegend auf Kredit. Die Modellannahme: Die Finanzierung wirkt in zwei Richtungen auf den Mietpreis. Teure Finanzierung bremst neue Kapazität und stützt damit die Mieten bestehender GPUs (Angebotskanal). Geraten Betreiber oder große Abnehmer in eine Notlage, kommen Kapazität und Flotten auf den Markt, und die Mieten fallen mit dem Stress (Notlagenkanal). Welcher Kanal überwiegt, bestimmt das Regime.

**Kreditseite.** Der Stressindex mittelt zwei Blöcke:

- **Makroblock:** robuste z-Werte (Median und skalierte mittlere absolute Abweichung über drei Jahre, auf ±4 begrenzt) der ICE-BofA-Aufschläge für Hochzins gesamt, CCC und BBB.
- **KI-Block:** Für jeden Emittenten rechnet die Software aus dem Geldkurs von bis zu drei Anleihen die Rendite (30/360, halbjährlicher Kupon) und zieht die Rendite der US-Staatsanleihe gleicher Restlaufzeit ab. Bei mehreren Anleihen ist die Reihe verkettet, damit ein Wechsel im Korb keinen Sprung erzeugt. Der Spread wird logarithmisch ins Raster der ICE-BofA-Aufschläge je Ratingstufe (AAA bis CCC) eingeordnet. Der Block ist der durchschnittliche Abstand zwischen diesem Marktniveau und dem Agenturrating, in Stufen (eine Stufe entspricht etwa drei Notches). Beispielwert: Ein Emittent mit Agenturrating BBB, dessen Spread zwischen BB und B liegt, hat einen Abstand von etwa 1,5.

**GPU-Seite.** Aus den gespeicherten Kurvenständen entstehen je Tag Median und unteres Quartil bei fester Laufzeit von 90 Tagen, linear zwischen den Liefermonaten. Die Märkte rechnen gegen den Monatsdurchschnitt ab, deshalb zählt die Monatsmitte als Laufzeitpunkt, und ein Liefermonat fällt heraus, sobald er begonnen hat. Der Ornn-Spot geht nur als Basis zur nahen Kurve ein.

**Regime.** Stress beginnt ab einem Indexwert von 1,0 und endet erst unter 0,5. Ein gemeinsamer Rückgang heißt: Median und unteres Quartil fallen über 14 Tage um mindestens 5 % oder über 30 Tage um mindestens 8 %, und der Spot fällt mindestens so stark wie die Kurve.

| Regime | Kreditseite | GPU-Seite |
|---|---|---|
| Ruhig | normal | kein gemeinsamer Rückgang |
| Preisdruck ohne Kreditstress | normal | gemeinsamer Rückgang (Angebot, Generationswechsel) |
| Kreditstress, Mieten fest | Stress | kein gemeinsamer Rückgang (Angebotskanal überwiegt) |
| Übertragung auf die Mieten | Stress | gemeinsamer Rückgang (Notlagenkanal überwiegt) |

Ein neues Regime gilt erst nach drei Tagen in Folge. Eine Übertragung braucht sieben Tage, wenn weder die Bruchpunkt-Erkennung (Bayesian Online Changepoint Detection nach Adams und MacKay, 2007) in den täglichen Kursänderungen einen frischen Bruch meldet noch die Kopplung negativ ist. Die Kopplung ist eine Regression der 7-Tage-Änderung des nahen Medians auf die 7-Tage-Änderung des Stressindex eine Woche zuvor, über 90 Tage. Eine Vorwarnung erscheint im Regime „Kreditstress, Mieten fest“, wenn der Spot stärker fällt als die Kurve, die Kopplung negativ wird oder die Bruchpunkt-Erkennung anschlägt.

**Grenzen.** Alle Schwellen sind eigene Setzungen und nicht an historischen Krisen kalibriert. Die GPU-Seite braucht mindestens 15 Tage Kurvenhistorie, die Bruchpunkt-Erkennung mindestens 30 Beobachtungen. Der Spread zur Staatsanleihe ist eine Näherung an den optionsbereinigten Aufschlag der Indizes. Agenturratings ändern sich und werden in den Einstellungen gepflegt; die Voreinstellungen sind eigene Annahmen zum Stand November 2026. Ohne laufende TWS oder IB Gateway rechnet das Modell nur mit dem Makroblock.

## Voraussetzungen

### macOS

- macOS 14 oder neuer (getestet mit macOS 27.2)
- Apple Command Line Tools: `xcode-select --install`
- Rust über rustup (getestet mit Rust 1.95):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

### Windows

- Windows 10 oder 11 auf x64 (getestet mit Windows 11)
- Build Tools für Visual Studio 2022 mit der Workload „Desktopentwicklung mit C++“ (Compiler und Linker für Rust und SQLite)
- Rust über rustup (getestet mit Rust 1.96), Toolchain `stable-x86_64-pc-windows-msvc`
- WebView2-Runtime, unter Windows 11 vorinstalliert
- PowerShell-Ausführungsrichtlinie für das eigene Konto: `Set-ExecutionPolicy -Scope CurrentUser RemoteSigned`

```powershell
winget install --id Microsoft.VisualStudio.2022.BuildTools -e --override "--wait --passive --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
winget install --id Rustlang.Rustup -e
```

Node.js, npm, Python und API-Schlüssel sind auf beiden Systemen nicht nötig. Die Software ruft nur öffentliche Endpunkte ohne Anmeldung ab.

Optional für den KI-Block des Finanzierungsregimes: eine laufende Trader Workstation (TWS) oder ein IB Gateway von Interactive Brokers auf demselben Rechner, mit aktivierter API („Enable ActiveX and Socket Clients“). Die Software stellt nur lesende Anfragen; die Einstellung „Read-Only API“ kann aktiv bleiben. Welche Anleihen Kurse liefern, hängt von den Marktdaten-Berechtigungen des eigenen Kontos ab. Port und Client-ID lassen sich im Reiter „Finanzierung“ einstellen; ohne Angabe probiert die Software die Standard-Ports 7496, 7497, 4001 und 4002 und eine Client-ID aus 8300 bis 8399.

## Bauen und installieren

Quellcode per git holen:

```bash
git clone https://github.com/KonvexInvestment/konvex-compute.git
cd konvex-compute
```

Alternativ das ZIP von GitHub („Code“, „Download ZIP“) entpacken und im Terminal in den Ordner `konvex-compute-main` wechseln. Unter Windows tragen entpackte Dateien eine Download-Markierung; das Build-Skript braucht dann vorher einmal `Unblock-File .\build_windows.ps1`.

**macOS:** bauen und nach `/Applications/Konvex Compute.app` installieren:

```bash
bash build_mac_app.sh --install
```

**Windows (PowerShell):** bauen und nach `%LOCALAPPDATA%\Programs\Konvex Compute` installieren, mit Eintrag im Startmenü:

```powershell
.\build_windows.ps1 -Install
```

Der erste Build lädt die Rust-Abhängigkeiten und dauert einige Minuten. Ohne `--install` bzw. `-Install` bleibt das Ergebnis im Build-Ordner (`src-tauri/target/release/`). Die Unit-Tests laufen auf beiden Systemen mit:

```bash
cargo test --locked --manifest-path src-tauri/Cargo.toml
```

Updates kommen per `git pull`, danach das Build-Skript erneut ausführen.

## Gatekeeper und SmartScreen

Die Software ist weder von Apple notarisiert noch für Windows signiert. Auf dem Rechner, auf dem sie gebaut wurde, startet sie trotzdem ohne Warnung, weil selbst gebaute Programme keine Download-Markierung tragen. Wird die fertige `.app` oder `.exe` per Download, AirDrop oder Messenger auf einen anderen Rechner übertragen, blockiert Gatekeeper sie unter macOS, unter Windows warnt SmartScreen vor unbekannter Software. Deshalb gibt es kein fertiges Download-Paket, die Software wird auf jedem Rechner aus dem Quellcode gebaut.

## Lokale Daten

Alle Daten liegen nur auf dem eigenen Rechner:

- macOS: `~/Library/Application Support/de.konvexinvestment.compute/compute.db`
- Windows: `%APPDATA%\konvexinvestment\compute\data\compute.db`

CSV-Exporte nutzen Komma als Trennzeichen und Punkt als Dezimalzeichen, also das internationale Format, das Python und pandas direkt lesen. Excel mit deutscher Ländereinstellung öffnet sie per Doppelklick einspaltig; dort ist ein Import mit Komma als Trennzeichen und Punkt als Dezimalzeichen nötig.

Zum vollständigen Entfernen genügt es, die installierte Software (unter Windows zusätzlich den Startmenü-Eintrag) und den Datenordner zu löschen.

## Datenquellen und Bedingungen

Die Software fragt alle Quellen direkt vom eigenen Rechner ab, solange sie läuft: Kurven und Spot alle 15 Minuten, Zinskurve, Kreditaufschläge und Anleihekurse höchstens alle sechs Stunden. Das Repository enthält nur Code, keine Marktdaten.

- **Ornn Compute Price Index** ([data.ornn.com](https://data.ornn.com)), kostenlose API unter `api.ornnai.com`. Der OCPI ist der Abrechnungsindex aller Polymarket-GPU-Mietpreis-Märkte. Ornns [Nutzungsbedingungen](https://ornn.com/terms-of-use) erlauben die kostenlose API nur für die eigene private oder interne Nutzung und verlangen für das Speichern von Antworten und für abgeleitete Berechnungen eine schriftliche Erlaubnis. Ornn hat Konvex Investment im November 2026 schriftlich bestätigt, dass die Nutzung in dieser Software in Ordnung ist: Abruf vom eigenen Rechner, Speicherung nur lokal, Vergleich mit der Forward-Kurve, Ornn als genannte Quelle. Eine Weitergabe der Daten ist davon nicht gedeckt, Datenbank und CSV-Exporte bleiben auf dem eigenen Rechner.
- **Polymarket** (Gamma API und CLOB API, `gamma-api.polymarket.com`, `clob.polymarket.com`): öffentliche Lese-Endpunkte ohne Konto. Es gelten die [Nutzungsbedingungen von Polymarket](https://polymarket.com/tos). Die Software liest nur Kurse und handelt nicht.
- **U.S. Department of the Treasury** ([Daily Treasury Par Yield Curve Rates](https://home.treasury.gov/resource-center/data-chart-center/interest-rates/TextView?type=daily_treasury_yield_curve)): tägliche Renditen der US-Staatsanleihen von einem Monat bis 30 Jahre, als Werk der US-Regierung gemeinfrei. Grundlage für den Spread der Emittenten.
- **ICE BofA US Corporate und High Yield Option-Adjusted Spreads** über [FRED](https://fred.stlouisfed.org/) der Federal Reserve Bank of St. Louis (zum Beispiel `BAMLH0A0HYM2`, `BAMLC0A4CBBB`). Die Reihen gehören ICE Data Indices, LLC; FRED stellt sie nur zur internen Nutzung bereit, eine Weitergabe ist nicht erlaubt. Die Software lädt sie deshalb nur auf den eigenen Rechner. FRED nimmt nur Abrufe an, deren Kennung eine Kontaktadresse enthält; die Software nennt dafür die Adresse dieses Repositorys.
- **Interactive Brokers** (optional, über die lokale TWS oder das IB Gateway): Anleihelisten und Tageskurse (Geldkurs) aus dem eigenen Konto. Es gelten die Bedingungen von Interactive Brokers und der Börsen für Marktdaten; die Kurse bleiben in der lokalen Datenbank.

## Rahmen

Konvex Compute ist ein Lernprojekt zum Erfahrungsaustausch. Software und Anzeigen sind keine Anlageberatung und keine Empfehlung zum Kauf oder Verkauf. Konvex Investment bietet keinen Einzel-Support und keine Einzelbetreuung für eigene oder fremde Codebasen an.

## Lizenz

Code unter [MIT-Lizenz](LICENSE). Die gebündelte Schriftart Inter Tight steht unter der SIL Open Font License ([`ui/assets/Inter-Tight-LICENSE.txt`](ui/assets/Inter-Tight-LICENSE.txt)).
