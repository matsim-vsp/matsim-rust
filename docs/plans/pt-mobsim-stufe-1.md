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

In `LegEngine::do_step`, nach `send_recv` und der Teleportation:

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
