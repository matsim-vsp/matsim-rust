# Plan: Fahrplanbasierte ÖV-Mobsim (Stufe 1, Variante 2b ohne Kapazität)

## Kontext

Bisher werden ÖV-Legs teleportiert. Der Fahrplan wird zwar geladen (`ScenarioCore::transit_schedule`), in der Mobsim aber nicht genutzt; `simulation/pt/mod.rs` ist leer. Ziel dieser Stufe: Fahrzeuge fahren nach Fahrplan von Haltestelle zu Haltestelle (ohne Netz-Queues), Agenten warten an Haltestellen, steigen ein und aus. Es gibt keinen Router, kein Störungsmodell, keine Kapazität, keine Zeit für Ein- und Aussteigen und keinen Informationskanal. Wer eine Abfahrt verpasst, wartet wie in MATSim auf das nächste passende Fahrzeug. Spätere Stufen (Kapazität, Störungen, Rust-RAPTOR, asynchrones Umplanen) bauen darauf auf. Leitlinie: Performance und Erweiterbarkeit ausbalancieren, nichts abstrahieren, wofür es noch keine zweite Implementierung gibt.

### Vorbild: SBB-Engine (geprüft im Quellcode `contribs/sbb-extensions`)

`SBBTransitQSimEngine` bewegt Fahrzeuge der `deterministicServiceModes` ohne Netz über eine eigene Ereigniswarteschlange.
- **Ankunft:** an der nächsten Haltestelle zu `Abfahrtszeit + arrivalOffset` (ersatzweise `departureOffset`), nie früher als geplant.
- **Warten:** Das Fahrzeug wartet immer bis zur planmäßigen Abfahrt (unabhängig von `awaitDeparture`).
- **Fahrer:** Jede Abfahrt hat einen Fahrer `pt_<line>_<route>_<departure>`. Seine Events: `TransitDriverStarts`, `departure`, `PersonEntersVehicle`, `VehicleArrivesAtFacility` und `VehicleDepartsAtFacility` (beide mit `delay` 0), am Ende `PersonLeavesVehicle` und `arrival`.
- **Einsteigeregel:** MATSims `TransitAgentImpl` mit `checkLineAndStop`: richtige **Linie** (nicht Route), und die geplante Ausstiegshaltestelle kommt noch. `boardingTime` spielt keine Rolle.

## Architekturentscheidungen (mit dir abgestimmt)

1. **Fahrzeuge wie bei Autos.**
   - Als Daten sind es `InternalVehicle` und `InternalVehicleType` aus der Datei der ÖV-Fahrzeuge. Sie liegen in einer **eigenen Garage** `transit_garage: Arc<Garage>` im `ScenarioCore`, geladen mit dem vorhandenen `Garage::from_file`.
   - Zur Laufzeit sind es `SimulationVehicle` mit `driver: None` und `passengers`. Es gibt keinen eigenen Fahrzeugtyp.
2. **Die Engine fährt, es gibt keinen Fahrer-Agenten. Der Plan liegt pro Route, nicht pro Fahrzeug.**
   - Die `TransitEngine` folgt einem einmal vorberechneten, dichten `TransitSimSchedule` (als `Arc` im `ScenarioCore`, wenige MB):
     - pro Route ein zusammenhängendes Array der Halte mit Facility, Stop-Link, zuständiger Partition sowie Ankunfts- und Abfahrts-Offset;
     - pro Abfahrt Routenindex, Abfahrtszeit, Fahrzeug-ID und Fahrer-ID.
   - Der Fahrer existiert nur als `Id<InternalPerson>` für die MATSim-Events.
   - Die laufende Fahrt (`TransitRun`) trägt `SimulationVehicle`, Routenindex, Halteindex und Abfahrtszeit inline. Über Partitionen wandern nur diese Daten; der Fahrplan ist überall derselbe `Arc`.
   - Eine Fahrerlogik kommt erst, wenn Fahrer entscheiden sollen (Busse im Netz, DRT, Anschlüsse abwarten).
3. **Die Fahrgäste entscheiden über einen eigenen Trait** `TransitPassengerLogic` in `pt/passenger.rs`:
   - `wants_to_board(line, route, upcoming_stops) -> bool`: steigt der Agent hier ein?
   - `wants_to_alight(stop) -> bool`: steigt er hier aus?
   - `waiting_for_line() -> Option<Id<TransitLine>>`: ein Hinweis für den Index. Bei `Some` wird der Agent nur zu Fahrzeugen dieser Linie gefragt, bei `None` zu allen.
   - Erreichbar ist der Trait über eine einzige Methode `SimulationAgentLogic::as_transit_passenger() -> Option<&dyn TransitPassengerLogic>`, standardmäßig `None`. So wächst `SimulationAgentLogic` um nur eine Methode (vgl. Issue #203).
   - `PlanBasedSimulationLogic` implementiert die MATSim-Regel aus der ÖV-Route des aktuellen Legs; `AdaptivePlanBasedSimulationLogic` delegiert und kann später überschreiben.
4. **Physik in der Engine, Entscheidungen beim Agenten.** Das ist dieselbe Aufteilung wie bei der Netz-Engine.

## Performance

- **ÖV aus (Standard):** praktisch keine Kosten, nur eine Abfrage beim Zuordnen zur Engine und ein leerer `Vec` in `InternalSyncMessage`.
- **ÖV an:** Kosten fallen nur bei Ereignissen an, nie pro Tick. Fällige Ankünfte und Abfahrten liegen in einer `TimeQueue`. Haltende Fahrzeuge werden nicht jede Sekunde geprüft: Eingestiegen wird, wenn ein Fahrzeug oder ein Agent an der Haltestelle ankommt.
- **Haltestellenzustand** liegt dicht, indiziert über die interne `Id<TransitStopFacility>`. Wartende sind dort nach Linie gruppiert, haltende Fahrzeuge in einer kleinen Liste.
- **ID-Auflösung:** Die String-IDs der `InternalPtRouteDescription` werden einmal pro ÖV-Leg aufgelöst, beim Erreichen der Haltestelle. Das Populationsmodell bleibt unverändert; typisierte IDs kommen nur, wenn das Profiling sie verlangt.
- **Keine Hash-Lookups im heißen Pfad:** Ein Halt-Ereignis liest das Fahrzeug, das ohnehin aus der Warteschlange kommt, und einen Eintrag im Halte-Array der Route. Die Arrays dicht befahrener Linien teilen sich viele Fahrzeuge, sie bleiben warm im Cache. Die Maps des `TransitSchedule` werden nur beim Aufbau gelesen.
- **Fahrplanzeiten** werden in **einer** Funktion berechnet. Dort wird später ein Störungsmodell eingehängt.
- **Messen:** Sobald Berlin fahrplanbasiert läuft, mit `hotpath` eine Baseline gegenüber dem Teleportieren aufnehmen und erst danach weiter optimieren (etwa den Index der Wartenden).

## Ablauf im Tick T

In `LegEngine::do_step`, nach den Netzbewegungen und vor `send_recv` (so werden Fahrzeuge zu anderen Partitionen noch im selben Schritt gesendet, auch im letzten; empfangene Fahrzeuge kommen frühestens im nächsten Tick an):

1. **Fahrten starten**, deren erster Halt lokal liegt: `TransitDriverStarts`, `departure` und `PersonEntersVehicle` (Fahrer), danach die Ankunft am ersten Halt.
2. **Fällige Ankünfte:**
   - `VehicleArrivesAtFacility`;
   - Fahrgäste mit `wants_to_alight` steigen aus (`PersonLeavesVehicle`; die Agenten gehen an die `LegEngine`, die wie bisher `arrival` meldet);
   - Wartende mit `wants_to_board` steigen ein (`PersonEntersVehicle`).
3. **Neu angekommene Wartende** steigen sofort in passende, dort haltende Fahrzeuge. Das entspricht der sekundenweisen Prüfung der SBB-Engine.
4. **Abfahrt** bei planmäßiger Abfahrt ≤ T: `VehicleDepartsAtFacility`. Die nächste Ankunft wird lokal eingereiht oder als Nachricht an die Partition des nächsten Stop-Links geschickt. Am letzten Halt melden die Events das Ende des Fahrers, und die Fahrt verschwindet.

Weitere Regeln:
- **Bewusste Abweichung:** zwischen Abfahrt und nächster Ankunft mindestens ein Tick (die SBB-Engine erlaubt 0 s). Nur so ist das Ergebnis unabhängig davon, ob der nächste Halt in einer anderen Partition liegt; das entspricht der bestehenden Link-Regel.
- **Reihenfolge:** Gleichzeitige Ereignisse werden nach Abfahrtsindex geordnet (sortiert nach externen IDs von Linie, Route und Abfahrt), nie nach `IntMap`-Iteration.
- **Simulationsende:** Wartende und Fahrgäste gehen über `drain()` zurück und bekommen wie bisher Stuck-Events; Fahrer bekommen keine.

## Schritte

0 und 2 hängen von nichts ab; 5 braucht 1 bis 4.

0. **Java-Referenz** in `parallel-qsim-berlin`: ein Runner mit `SBBTransitModule` für `pt_tutorial`.
   - Einstellungen: `usingTransitInMobsim`, `transitModes={pt}`, `deterministicServiceModes={train}`, Zeiten zum Ein- und Aussteigen auf 0, `lastIteration=0`.
   - Ziel: `matsim_rust/tests/resources/pt_tutorial/expected_events_sbb/`.
1. **Konfiguration und Daten:**
   - `config::Transit` erhält eine Simulationsart (Teleportieren oder fahrplanbasiert, Standard Teleportieren), `passenger_modes` (Standard `["pt"]`) und `vehicles_path`.
   - `transit_garage` kommt in `Scenario` und `ScenarioCore`.
   - Der dichte `TransitSimSchedule` (siehe Architekturentscheidung 2) wird einmal in `From<Scenario> for ControllerScenario` gebaut. Routen und Abfahrten sind nach externen IDs sortiert; der Abfahrtsindex dient zugleich als stabile Reihenfolge.
   - Die Fahrer-IDs entstehen dort **nach** der Population, damit sich interne Personen-IDs nicht verschieben und XML und Protobuf gleich bleiben.
   - Validierung: Stop-Links existieren, Offsets steigen monoton, jedes Fahrzeug existiert in der `transit_garage` und fährt keine zwei Abfahrten gleichzeitig.
   - Dateien: `simulation/config.rs`, `simulation/scenario/mod.rs`, neu `simulation/pt/schedule.rs`.
2. **Events:**
   - Neu: `TransitDriverStartsEvent`, `VehicleArrivesAtFacilityEvent`, `VehicleDepartsAtFacilityEvent`, `AgentWaitingForPtEvent`, mit XML- und Proto-Anbindung nach dem Muster von `PtTeleportationArrivalEvent` (`events/mod.rs`, `io/proto/proto_events.rs`, `io/xml/events.rs`).
   - `PersonDepartureEvent::routing_mode` wird optional, weil Fahrer keinen Routing-Modus haben.
3. **Fahrgast-Trait** (`pt/passenger.rs`): `TransitPassengerLogic`, die Methode `as_transit_passenger` und die Implementierungen für beide Plan-Logiken. Unit-Tests der MATSim-Regel.
4. **Partitionswechsel:**
   - Eine eigene Liste `transit_vehicles` in `InternalSyncMessage` (inklusive `is_empty` und `take_*`), dazu `NetMessageBroker::add_transit_vehicle`.
   - Die neue Variante `PartitionChangeEntity::TransitVehicle`: Die Backpacks der Fahrgäste wandern mit, der Reisezeit-Sammler ignoriert diese Fahrzeuge.
   - Eine Assertion für verspätet empfangene Ankünfte, wie in `TeleportationEngine::receive_remote_agent`.
5. **`TransitEngine`** (`pt/transit_engine.rs`) und Einbindung:
   - `LegEngine::find_responsible_engine` leitet Passagiermodi an die neue Engine; dazu `drain` und die Rückgabe der Ausgestiegenen.
   - Eine neue `AgentEvent`-Variante beim Aussteigen setzt den Routenzeiger aufs Ende, damit `curr_link_id` den Stop-Link liefert.
   - Unit-Tests:
     - Ein- und Aussteigen;
     - verpasste Abfahrt, dann das nächste Fahrzeug derselben Linie;
     - früh angekommener Agent;
     - zwei Fahrzeuge gleichzeitig an einer Haltestelle;
     - letzter Halt;
     - Stuck-Events.
6. **Planvorbereitung** (`scenario/prepare/prepare_for_mobsim.rs`): Bei fahrplanbasierter Simulation brauchen ÖV-Legs keine Reisezeit. Dazu kommt eine rein lesende Prüfung der ÖV-Route: Linie und Haltestellen existieren, Start- und End-Link stimmen mit den Stop-Links überein, und die Ausstiegs- liegt nach der Einstiegshaltestelle.
7. **Erlebte Pläne** (`scoring/backpacking/`, `scoring/partial_plans.rs`), wie MATSims `EventsToLegs` (geprüft):
   - Fahrer ignorieren.
   - Je Fahrzeug Linie und Route aus `TransitDriverStarts` und die letzte Haltestelle aus `VehicleArrivesAtFacility` halten und beim Partitionswechsel mitgeben.
   - Daraus die `InternalPtRoute` bauen, mit der Distanz wie `RouteUtils.calcDistance(passengerRoute, schedule, network)`.
   - Bis dieser Schritt fertig ist, laufen die Integrationstests mit `scoring.mode: Disabled`.
8. **Integrationstests** (`tests/simulation/pt.rs`):
   - `pt_tutorial` fahrplanbasiert mit einer und mit zwei Partitionen;
   - Abgleich mit der Java-Referenz (der Vergleicher ignoriert die Reihenfolge innerhalb einer Zeit);
   - zwei Läufe ergeben dieselben Events;
   - der teleportierte Test bleibt unverändert.
9. **Dokumentation:** In `docs/architecture.md` einen Abschnitt zur ÖV-Simulation mit Architektur und Abweichungen von der SBB-Engine ergänzen (mindestens ein Tick pro Abschnitt, keine Zeiten zum Ein- und Aussteigen, keine Kapazität, keine `chainedDepartures`, keine Link-Events); dazu die Konfiguration.

## Bewusst nicht in dieser Stufe

Kapazität und abgewiesenes Einsteigen, Störungsmodell, `chainedDepartures`, Link-Events, Fahrerlogik, Router, Informationskanal, Umplanen innerhalb des Tages.

## Verifikation

- `cargo test -p matsim-rust --lib simulation::pt -- --test-threads=1`
- `cargo test -p matsim-rust --test simulation pt:: -- --test-threads=1`
- `cargo test --workspace -- --test-threads=1 --skip berlin::`
- `cargo fmt --all -- --check` und `git diff --check`
- Optional, nur `--release`: Berlin 0,1 % fahrplanbasiert als Lauf, der nicht abstürzen darf, plus Laufzeitvergleich mit dem Teleportieren.

## Verfeinerung nach Schritt 1

Die folgenden Abschnitte konkretisieren die Schritte 2–9 anhand des Codes. Wo sie vom obigen Text abweichen, gelten sie.

Reihenfolge: 2 → 3 → 4 → 5 → 6 → 8a (Integrationstest ohne Scoring) → 7 → 8b → 9.
Pro Schritt ein Commit (nach Rückfrage), jeweils mit grünen Unit-Tests.

---

### Schritt 2: Events

Dateien: `simulation/events/mod.rs`, `simulation/io/xml/events.rs`, `simulation/io/proto/proto_events.rs`,
Muster: `PtTeleportationArrivalEvent` (Struct mit `#[event_struct]`, `TYPE`, `try_from_proto_event`,
`From<&E> for GenericEvent`, Writer-Zweig in `xml/events.rs:179`, Parser in `handle()` (`xml/events.rs:361`)
und `try_event_from_proto` (`proto_events.rs:612`)). Proto braucht **keine** `.proto`-Änderung, Events
laufen über die Attribut-Map von `GenericEvent`.

Neue Events (Typnamen und Attribute wie MATSim, mit der Java-Referenz aus Schritt 0 gegenprüfen):
- `TransitDriverStartsEvent` – `"TransitDriverStarts"`: `driverId`, `vehicleId`, `transitLineId`, `transitRouteId`, `departureId`
- `VehicleArrivesAtFacilityEvent` – `"VehicleArrivesAtFacility"`: `vehicle`, `facility`, `delay`
- `VehicleDepartsAtFacilityEvent` – `"VehicleDepartsAtFacility"`: `vehicle`, `facility`, `delay`
- `AgentWaitingForPtEvent` – `"waitingForPt"`: `agent`, `atStop`, `destinationStop`

`PersonDepartureEvent::routing_mode` wird `Option<Id<String>>` (XML: Attribut weglassen; Proto: Attribut
optional, wie `PersonStuckEvent::leg_mode`). Aufrufer anpassen: `leg_engine.rs:269` (`Some(..)`),
`scoring/partial_plans.rs:299`, Tests in `events/utils.rs`, `backpacking_engine.rs`.

Tests: Roundtrip XML und Proto je Event; Departure ohne `routing_mode` in beiden Formaten.

### Schritt 3: Fahrgast-Trait (`simulation/pt/passenger.rs`)

```rust
pub trait TransitPassengerLogic {
    fn access_stop(&self) -> Id<TransitStopFacility>;   // wo der Agent wartet
    fn egress_stop(&self) -> Id<TransitStopFacility>;   // für AgentWaitingForPt
    fn waiting_for_line(&self) -> Option<Id<TransitLine>>;
    fn wants_to_board(&self, line: &Id<TransitLine>, route: &Id<TransitRoute>, upcoming: &[SimRouteStop]) -> bool;
    fn wants_to_alight(&self, stop: &Id<TransitStopFacility>) -> bool;
}
```
**Befund 1:** Die Engine muss wissen, an welcher Haltestelle der Agent wartet; dafür kommen
`access_stop` und `egress_stop` zu den drei Methoden aus dem Plan hinzu.

- `SimulationAgentLogic::as_transit_passenger(&self) -> Option<&dyn TransitPassengerLogic>` mit Default `None`
  (`agents/mod.rs`), Weiterleitung in `SimulationAgent` (`agents/agent.rs`) und `AdaptivePlanBasedSimulationLogic`.
- `PlanBasedSimulationLogic` implementiert MATSims `checkLineAndStop` aus `InternalPtRouteDescription` des
  aktuellen Legs: richtige Linie, Ausstiegshaltestelle kommt in `upcoming` noch vor. Die String-IDs werden
  hier per `Id::try_get_from_ext` aufgelöst; die Engine ruft `access_stop`/`egress_stop`/`waiting_for_line`
  nur einmal pro Leg (beim Erreichen der Haltestelle) und speichert das Ergebnis. `wants_to_alight` vergleicht
  per `external()` (String-Vergleich, kein Hash).
- Neue `AgentEvent`-Variante `TransitVehicleLeft()`: `PlanBasedSimulationLogic` setzt den Routenzeiger aufs
  Ende (`set_curr_route_element_to_last`, `agent_logic.rs:76`), sodass `curr_link_id` den End-Link liefert.

Unit-Tests: richtige/falsche Linie, andere Route derselben Linie steigt ein, Ausstieg schon passiert
(nicht in `upcoming`), Nicht-ÖV-Leg liefert `None`.

### Schritt 4: Partitionswechsel

- `pt/transit_run.rs` (oder in der Engine): `TransitRun { departure: usize, stop: usize, vehicle: SimulationVehicle, last_departure: SimTime }`
  `SimulationVehicle` bekommt `new_transit(InternalVehicle)` (ohne Fahrer), `add_passenger`, `take_passengers_where`.
  **Befund 2:** `SimulationVehicle::driver()` und `curr_link_id()` paniken ohne Fahrer, ebenso
  `emit_partition_*_events_for_vehicle` (`engines/mod.rs`) und Backpacking (`person_ids`). Transit-Fahrzeuge
  gehen deshalb nie durch diese Funktionen.
- `messages.rs`: `TransitVehicleMessage { run, arrival: SimTime, attachments }`, `InternalSyncMessage::transit_vehicles`
  inkl. `is_empty`, `take_transit_vehicles`; `NetMessageBroker::add_transit_vehicle(msg, to, now)` – die
  Zielpartition kommt direkt aus `SimRouteStop::partition`, kein Link-Lookup.
- `PartitionChangeEntity::TransitVehicle(&SimulationVehicle)`: Backpacking nimmt nur die Fahrgäste mit
  (`backpacking_data_collector.rs:212`). Partition-Events nur `AgentLeaves/EntersPartition` der Fahrgäste,
  kein `VehicleEntersPartition` (der Reisezeit-Rechner `travel_time_calculator.rs` sieht ÖV-Fahrzeuge damit nicht).
- Assertion für verspätete Ankünfte wie `TeleportationEngine::receive_remote_agent` (`teleportation_engine.rs:124`).
- `describe_unprocessed_messages` (`controller/mod.rs:390`) zählt auch Fahrgäste in ÖV-Fahrzeugen.

**Befund 3:** Partitionen lesen verlässlich nur Nachrichten von Nachbarn (gemeinsamer Link). Liegt der
nächste Halt in einer Partition ohne gemeinsamen Link (Route läuft durch eine dritte Partition), gilt dasselbe
Risiko wie bei der Teleportation: Abhilfe `global_sync`, sonst greift die Assertion. Für Stufe 1 so lassen
und in `docs/architecture.md` dokumentieren.

Tests: Nachricht mit Fahrzeug + 2 Fahrgästen inkl. Attachments (Muster `messages.rs:298`), Backpacking-Send/Receive.

### Schritt 5: `TransitEngine` (`simulation/pt/transit_engine.rs`) und Einbindung

Zustand pro Partition (alles dicht, keine Hash-Lookups im heißen Pfad):
- `schedule: Arc<TransitSimSchedule>`.
- `starts: Vec<usize>` – lokale Abfahrten (erster Halt lokal), sortiert nach (Zeit, Abfahrtsindex), plus Cursor.
- `runs: Vec<Option<TransitRun>>` + Free-List (Slab nur für aktive Fahrten; ein Slab über alle Abfahrten wäre
  bei Berlin pro Partition zu groß).
- Zwei `TimeQueue`s mit `add_with_order(order = Abfahrtsindex)`: Ankünfte und Abfahrten, Einträge sind Slab-Indizes.
- `stops: Vec<StopState>` indiziert über `Id<TransitStopFacility>::internal()`, nur bei `ScheduleBased` allokiert.
  `StopState { waiting: Vec<Waiting { line: Option<Id<TransitLine>>, agent }>, halting: Vec<usize> }`.
  Wartende in Ankunftsreihenfolge (FIFO wie MATSim); Filter über `line` vor `wants_to_board`. Gruppierung nach
  Linie erst, wenn das Profiling es verlangt.
- Fahrplanzeiten in **einer** Funktion: Ankunft = max(Abfahrt + arrival_offset, vorige Abfahrt + 1 Tick),
  Abfahrt = max(Ankunft, Abfahrt + departure_offset) – hier hängt später das Störungsmodell.

Ablauf in `LegEngine::do_step` vor `send_recv` (umgesetzt; ursprünglich danach geplant, siehe oben). Empfangene Fahrzeuge reiht die `send_recv`-Schleife ein:
1. Empfangene `TransitVehicleMessage`s in die Ankunfts-Queue (nach Attachments/Partition-Events).
2. Fahrten starten: `TransitDriverStarts`, `departure` (Fahrer, Modus der Route, ohne Routing-Modus),
   `PersonEntersVehicle` (Fahrer), dann Ankunft am ersten Halt einreihen.
3. Fällige Ankünfte: `VehicleArrivesAtFacility` (delay 0) → Aussteigen (`PersonLeavesVehicle`,
   `TransitVehicleLeft`, Agent zurück an `LegEngine` → `arrival` wie bisher) → Einsteigen → Abfahrt einreihen,
   Fahrzeug in `halting`. Letzter Halt: `PersonLeavesVehicle` + `arrival` des Fahrers, Fahrt aus dem Slab.
   (Ob SBB am letzten Halt noch `VehicleDepartsAtFacility` schreibt, an der Java-Referenz prüfen.)
4. Neu angekommene Wartende (aus `receive_agent` dieses Ticks gepuffert): `waitingForPt`, sofort in haltende
   passende Fahrzeuge, sonst in `waiting`.
5. Fällige Abfahrten: `VehicleDepartsAtFacility`, aus `halting` entfernen, nächste Ankunft lokal einreihen oder senden.

Einbindung:
- `ResponsibleEngine::Transit` in `find_responsible_engine` (`leg_engine.rs:285`): `ScheduleBased`, Modus in
  `passenger_modes`, Route `Pt`. Modi als `IntSet` einmal beim Bau aufgelöst.
- `LegEngine::new` bekommt `Arc<TransitSimSchedule>` und Transit-Garage (`simulation.rs:202`).
- `drain()` gibt Wartende und Fahrgäste zurück (Stuck-Events wie bisher, `simulation.rs:90`); Fahrer existieren
  nicht als Agenten und bekommen keine.

Unit-Tests (Engine mit Default-`comp_env` und Event-Sammler): Ein-/Aussteigen, verpasste Abfahrt → nächstes
Fahrzeug derselben Linie, früh angekommener Agent, zwei Fahrzeuge gleichzeitig an einer Haltestelle
(Reihenfolge nach Abfahrtsindex), letzter Halt, Stuck beim Warten und im Fahrzeug, +1-Tick-Regel bei Offset 0.

### Schritt 6: Planvorbereitung (`scenario/prepare/prepare_for_mobsim.rs`)

- `PrepareForMobsimContext` bekommt `transit_schedule` und `transit_sim_schedule`.
- In `trip_is_valid` (`:274`): für Passagiermodi bei `ScheduleBased` keine Reisezeitpflicht (Ausnahme analog
  zu `is_network_mode`, `:300`).
- Rein lesende Prüfung der Pt-Route: Linie, Route, Zugangs- und Ausstiegshaltestelle existieren, beide liegen auf
  der Route, Ausstieg nach Zugang, `start_link`/`end_link` gleich den Stop-Links. Ungültig → wie bisher
  `NeedsRouting` (ohne ÖV-Router ergibt das einen `PrepareError`).
- Tests: gültige Route, falsche Reihenfolge, falscher Start-Link, unbekannte Linie; teleportiert unverändert.

### Schritt 7: Erlebte Pläne (`scoring/backpacking/`, `scoring/partial_plans.rs`)

- Fahrer ignorieren. **Befund 4:** Der Collector panikt für Personen ohne Backpack (`backpacking_data_collector.rs:138`),
  und Fahrer-IDs liegen nicht sicher hinter allen Personen-IDs (vorgeladener ID-Store). Daher
  `TransitSimSchedule::is_driver(&Id<InternalPerson>)` über einen sortierten `Vec<u64>` der internen Fahrer-IDs (Binärsuche).
- Je Fahrzeug Linie/Route aus `TransitDriverStarts` und letzte Haltestelle aus `VehicleArrivesAtFacility` halten;
  beim Partitionswechsel im `BackpackingAttachment` mitgeben.
- `PersonEntersVehicle` eines ÖV-Fahrzeugs → Route-Typ `Pt` (statt `Network`), Zugang + `boarding_time`;
  `PersonLeavesVehicle` → Ausstieg.
- **Befund 5:** Distanz wie `RouteUtils.calcDistance` ohne Netz zur Scoring-Zeit: `SimRouteStop` bekommt
  `distance_from_start` (kumulierte Linklängen der Netzroute bis zum Stop-Link, in Schritt 1 nachgerüstet);
  Distanz = Differenz zwischen Ausstieg und Zugang. Mit Java gegenprüfen (Linklänge des Start-Links zählt nicht mit).
- Tests: Fahrgast über zwei Halte, Partitionswechsel dazwischen, Fahrer-Events erzeugen keinen Plan.

### Schritt 8: Integrationstests (`tests/simulation/pt.rs`)

- 8a (nach Schritt 6, `scoring.mode: Disabled`): `pt_tutorial` fahrplanbasiert mit 1 und 2 Partitionen;
  neue Config `tests/resources/pt_tutorial/pt_tutorial_schedule_based_config.yml` (mit `vehicles_path`).
  Zwei Läufe ergeben dieselben Events; 1 und 2 Partitionen ergeben dieselben Events (Vergleicher ignoriert
  Reihenfolge innerhalb einer Zeit, `events/comparison.rs`).
- 8b (nach Schritt 7): Scoring an; Abgleich mit der Java-Referenz `expected_events_sbb/`, sobald du sie aus
  Schritt 0 lieferst (Hinweis: `deterministicServiceModes={train,pt}`, da `3to1` Modus `pt` hat).
- Bestehender teleportierter Test bleibt unverändert.

### Schritt 9: Dokumentation

`docs/architecture.md`: Abschnitt ÖV-Simulation (Datenfluss Scenario → `TransitSimSchedule` → `TransitEngine`,
Partitionswechsel, Befund 3) und Abweichungen von SBB (≥1 Tick je Abschnitt, keine Ein-/Aussteigezeiten, keine
Kapazität, keine `chainedDepartures`, keine Link-Events, delay immer 0). `README.md`/Config-Doku:
`transit.simulation`, `transit.vehicles_path`, `transit.passenger_modes`.

---

### Verifikation

- Pro Schritt: `cargo test -p matsim-rust --lib simulation::pt -- --test-threads=1` plus betroffene Module
  (`events`, `io`, `messaging`, `scoring`, `scenario::prepare`), `RUSTFLAGS="-D warnings" cargo check --tests`.
- Ab 8a: `cargo test -p matsim-rust --test simulation pt:: -- --test-threads=1`.
- Vor jedem Push: `cargo test --workspace -- --test-threads=1 --skip berlin::`, `cargo fmt --all -- --check`,
  `git diff --check`.
- Optional am Ende, nur `--release`: Berlin 0,1 % fahrplanbasiert (muss durchlaufen) + Laufzeit gegenüber Teleportieren.
