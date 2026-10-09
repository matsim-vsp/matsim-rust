package org.matsimrust.reference;

import ch.sbb.matsim.config.SwissRailRaptorConfigGroup;
import ch.sbb.matsim.routing.pt.raptor.SwissRailRaptorModule;
import ch.sbb.matsim.routing.pt.raptor.RaptorUtils;
import ch.sbb.matsim.routing.pt.raptor.RaptorParameters;
import ch.sbb.matsim.routing.pt.raptor.RaptorStaticConfig;
import ch.sbb.matsim.routing.pt.raptor.SwissRailRaptor;
import ch.sbb.matsim.routing.pt.raptor.SwissRailRaptorData;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.node.ArrayNode;
import com.fasterxml.jackson.databind.node.ObjectNode;
import org.matsim.api.core.v01.Coord;
import org.matsim.api.core.v01.Id;
import org.matsim.api.core.v01.Scenario;
import org.matsim.api.core.v01.network.Link;
import org.matsim.api.core.v01.population.Leg;
import org.matsim.api.core.v01.population.Person;
import org.matsim.api.core.v01.population.PlanElement;
import org.matsim.core.config.Config;
import org.matsim.core.config.ConfigUtils;
import org.matsim.core.controler.Controler;
import org.matsim.facilities.Facility;
import org.matsim.core.router.TripRouter;
import org.matsim.core.scenario.ScenarioUtils;
import org.matsim.core.utils.misc.OptionalTime;
import org.matsim.facilities.ActivityFacility;
import org.matsim.pt.routes.DefaultTransitPassengerRoute;
import org.matsim.pt.transitSchedule.api.TransitStopFacility;
import org.matsim.utils.objectattributes.attributable.AttributesImpl;

import java.io.BufferedReader;
import java.io.IOException;
import java.io.InputStream;
import java.io.InputStreamReader;
import java.io.UncheckedIOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.security.MessageDigest;
import java.util.ArrayList;
import java.util.HexFormat;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.TreeMap;
import java.util.stream.Stream;
import java.util.regex.Pattern;
import java.util.zip.GZIPInputStream;

/**
 * Runs one MATSim scenario and one batch of routing requests, and writes the observable result as
 * the canonical JSON that the Rust differential fixtures compare against.
 *
 * <p>Two boundaries are exercised, mirroring {@code TripRouter} and the simulation integration
 * runner on the Rust side:
 * <ul>
 *   <li>routing requests go through {@link TripRouter#calcRoute}, the same entry point MATSim
 *       replanning uses;</li>
 *   <li>execution is observed through the event file the controler writes.</li>
 * </ul>
 *
 * <p>Usage: {@code ReferenceMain --config <config.xml> --out <reference.json> [--requests <requests.json>]}
 */
public final class ReferenceMain {

    /** Bumped whenever the normalized shape changes; the Rust side asserts the same number. */
    private static final int SCHEMA_VERSION = 3;

    private static final ObjectMapper MAPPER = new ObjectMapper();

    /** The controler writes `<iteration>.events.xml` per iteration, optionally gzipped. */
    private static final Pattern EVENT_FILE = Pattern.compile("(\\d+)\\.events\\.xml(\\.gz)?");

    /**
     * Event types that carry passenger or transit-service meaning, with the attributes kept for
     * each. Everything else is bookkeeping-level; skipped types are reported on stderr so a
     * fixture change cannot silently drop a new observable event.
     *
     * <p>This list is derived from the pinned tree: MATSim 2026.0 emits a teleported pt leg as
     * {@code travelled} with mode {@code pt}, not as a separate event type.
     */
    private static final Map<String, List<String>> EVENT_ATTRIBUTES = new LinkedHashMap<>();
    static {
        EVENT_ATTRIBUTES.put("actstart", List.of("person", "actType", "link", "x", "y"));
        EVENT_ATTRIBUTES.put("actend", List.of("person", "actType", "link", "x", "y"));
        EVENT_ATTRIBUTES.put("departure", List.of("person", "legMode", "computationalRoutingMode", "link"));
        EVENT_ATTRIBUTES.put("arrival", List.of("person", "legMode", "link"));
        EVENT_ATTRIBUTES.put("travelled", List.of("person", "mode", "distance"));
        EVENT_ATTRIBUTES.put("PersonEntersVehicle", List.of("person", "vehicle"));
        EVENT_ATTRIBUTES.put("PersonLeavesVehicle", List.of("person", "vehicle"));
        EVENT_ATTRIBUTES.put("PersonEntersPtVehicle", List.of("person", "vehicle"));
        EVENT_ATTRIBUTES.put("PersonLeavesPtVehicle", List.of("person", "vehicle"));
        EVENT_ATTRIBUTES.put("TransitDriverStarts", List.of(
                "driverId", "vehicleId", "transitLineId", "transitRouteId", "departureId"));
        EVENT_ATTRIBUTES.put("VehicleArrivesAtFacility", List.of("vehicle", "facility", "delay"));
        EVENT_ATTRIBUTES.put("VehicleDepartsAtFacility", List.of("vehicle", "facility", "delay"));
        EVENT_ATTRIBUTES.put("waitingForPt", List.of("person", "atStop", "destinationStop"));
        EVENT_ATTRIBUTES.put("stuckAndAbort", List.of("person", "link", "legMode", "reason"));
    }

    /**
     * Attributes that are numbers, not identifiers. Everything else stays a string so a person or
     * link id is never read back as a number and compared numerically.
     */
    private static final List<String> NUMERIC_ATTRIBUTES = List.of("time", "distance", "x", "y", "delay");

    /**
     * Times are compared at millisecond resolution, which is finer than either simulation clock and
     * therefore cannot hide a clock difference. Distances are recorded at full precision: the
     * reference computes them from link lengths and the two implementations must agree on the value,
     * not on a rounded form of it.
     */
    private static double roundTime(double seconds) {
        return Math.round(seconds * 1_000d) / 1_000d;
    }

    public static void main(String[] args) throws Exception {
        Map<String, String> options = parseOptions(args);
        Path configPath = Path.of(require(options, "--config"));
        Path outputPath = Path.of(require(options, "--out"));

        Config config = ConfigUtils.loadConfig(configPath.toAbsolutePath().toString());
        boolean useSwissRailRaptor = config.getModules().containsKey(SwissRailRaptorConfigGroup.GROUP);
        if (useSwissRailRaptor) {
            config = ConfigUtils.loadConfig(
                    configPath.toAbsolutePath().toString(), new SwissRailRaptorConfigGroup());
        }
        // loadScenario reads the input files named in the config; createScenario alone leaves the
        // scenario empty. `Controler` (one l) is the concrete implementation; `ControlerUtils` is
        // deprecated upstream.
        Scenario scenario = ScenarioUtils.loadScenario(config);
        Controler controler = new Controler(scenario);
        if (useSwissRailRaptor) {
            controler.addOverridingModule(new SwissRailRaptorModule());
        }
        controler.run();

        ObjectNode root = MAPPER.createObjectNode();
        root.put("schema_version", SCHEMA_VERSION);
        root.set("reference", reference(config, configPath, options));
        root.set("itineraries", itineraries(controler, options));
        root.set("trees", trees(controler, options));
        root.set("events", events(Path.of(config.controller().getOutputDirectory())));

        Files.createDirectories(outputPath.toAbsolutePath().getParent());
        MAPPER.writerWithDefaultPrettyPrinter().writeValue(outputPath.toFile(), root);
        System.out.println("wrote " + outputPath.toAbsolutePath());
    }

    /** Everything needed to reproduce the run: version, configuration, seed, clock and inputs. */
    private static ObjectNode reference(Config config, Path configPath, Map<String, String> options) {
        ObjectNode reference = MAPPER.createObjectNode();
        reference.put("implementation", "matsim");
        reference.put("version", options.getOrDefault("--reference-version", "unknown"));
        reference.put("commit", options.getOrDefault("--reference-commit", "unknown"));
        reference.put("config", configPath.toString());
        reference.put("config_sha256", sha256(configPath));
        reference.put("seed", config.global().getRandomSeed());
        reference.put("start_time", 0.0);
        reference.put("end_time", config.qsim().getEndTime().orElse(-1.0));
        reference.put("time_step_size", config.qsim().getTimeStepSize());
        ArrayNode inputs = reference.putArray("inputs");
        for (String[] input : List.of(
                new String[] {"network", config.network().getInputFile()},
                new String[] {"population", config.plans().getInputFile()},
                new String[] {"vehicles", config.vehicles().getVehiclesFile()},
                new String[] {"transit_vehicles", config.transit().getVehiclesFile()},
                new String[] {"transit_schedule", config.transit().getTransitScheduleFile()})) {
            String path = input[1];
            if (path == null || path.isBlank()) {
                continue;
            }
            ObjectNode node = inputs.addObject();
            node.put("role", input[0]);
            node.put("path", path);
            if (Files.isRegularFile(Path.of(path))) {
                node.put("sha256", sha256(Path.of(path)));
            }
        }
        if (options.containsKey("--requests")) {
            String path = options.get("--requests");
            ObjectNode node = inputs.addObject();
            node.put("role", "routing_requests");
            node.put("path", Path.of(path).getFileName().toString());
            if (Files.isRegularFile(Path.of(path))) {
                node.put("sha256", sha256(Path.of(path)));
            }
        }
        return reference;
    }

    /**
     * Runs the recorded routing requests through {@link TripRouter}. A request that MATSim cannot
     * route is recorded as an explicit no-path result rather than omitted, so a Rust side that finds
     * a path is visible.
     */
    private static ArrayNode itineraries(Controler controler, Map<String, String> options) throws IOException {
        ArrayNode itineraries = MAPPER.createArrayNode();
        Path requestsPath = options.containsKey("--requests") ? Path.of(options.get("--requests")) : null;
        if (requestsPath == null) {
            return itineraries;
        }

        Json requests = Json.read(requestsPath);
        TripRouter tripRouter = controler.getTripRouterProvider().get();
        for (Json request : requests.array("requests")) {
            ObjectNode itinerary = MAPPER.createObjectNode();
            itinerary.put("id", request.string("id"));
            itinerary.set("request", request.node());

            Person person = !request.node().hasNonNull("person")
                    ? null
                    : controler.getScenario().getPopulation().getPersons().get(Id.create(request.string("person"), Person.class));
            Facility from = facility(controler.getScenario(), "probe_from_" + request.string("id"), request.field("from"));
            Facility to = facility(controler.getScenario(), "probe_to_" + request.string("id"), request.field("to"));

            List<? extends PlanElement> trip = tripRouter.calcRoute(
                    request.string("mode"),
                    from,
                    to,
                    request.number("departure_time"),
                    person,
                    new AttributesImpl());

            if (trip == null) {
                itinerary.put("result", "no_path");
                itineraries.add(itinerary);
                continue;
            }
            itinerary.put("result", "found");
            ArrayNode legs = itinerary.putArray("legs");
            double arrival = request.number("departure_time");
            for (PlanElement element : trip) {
                if (element instanceof Leg leg) {
                    ObjectNode node = legs.addObject();
                    node.put("mode", leg.getMode());
                    double departure = leg.getDepartureTime().orElse(arrival);
                    double travelTime = leg.getTravelTime().orElse(0.0);
                    node.put("departure_time", round(departure));
                    node.put("arrival_time", round(departure + travelTime));
                    node.put("distance", round(leg.getRoute() == null ? 0.0 : leg.getRoute().getDistance()));
                    Object generalizedCost = leg.getAttributes().getAttribute(RaptorUtils.TOTAL_ROUTE_COST_ATTR_NAME);
                    if (generalizedCost instanceof Number cost && !itinerary.has("generalized_cost")) {
                        itinerary.put("generalized_cost", round(cost.doubleValue()));
                    }
                    ArrayNode rides = node.putArray("rides");
                    DefaultTransitPassengerRoute route =
                            leg.getRoute() instanceof DefaultTransitPassengerRoute transitRoute ? transitRoute : null;
                    for (DefaultTransitPassengerRoute ride = route; ride != null; ride = ride.getChainedRoute()) {
                        ObjectNode rideNode = rides.addObject();
                        rideNode.put("line", ride.transitLine == null ? null : ride.transitLine.toString());
                        rideNode.put("route", ride.transitRoute == null ? null : ride.transitRoute.toString());
                        rideNode.put("boarding_time", round(ride.getBoardingTime().orElse(-1.0)));
                        rideNode.put("access_stop",
                                ride.accessFacility == null ? null : ride.accessFacility.toString());
                        rideNode.put("egress_stop",
                                ride.egressFacility == null ? null : ride.egressFacility.toString());
                    }
                    arrival = departure + travelTime;
                }
            }
            itinerary.put("arrival_time", round(arrival));
            itineraries.add(itinerary);
        }
        return itineraries;
    }

    /** Runs explicit one-to-all trees, recording absent destinations as no-path results. */
    private static ArrayNode trees(Controler controler, Map<String, String> options) throws IOException {
        ArrayNode trees = MAPPER.createArrayNode();
        Path requestsPath = options.containsKey("--requests") ? Path.of(options.get("--requests")) : null;
        if (requestsPath == null) {
            return trees;
        }

        Json requests = Json.read(requestsPath);
        if (!requests.node().has("trees")) {
            return trees;
        }
        RaptorStaticConfig staticConfig = RaptorUtils.createStaticConfig(controler.getConfig());
        staticConfig.setOptimization(RaptorStaticConfig.RaptorOptimization.OneToAllRouting);
        SwissRailRaptorData data = SwissRailRaptorData.create(
                controler.getScenario().getTransitSchedule(), null, staticConfig,
                controler.getScenario().getNetwork(), null);
        SwissRailRaptor raptor = new SwissRailRaptor.Builder(data, controler.getConfig()).build();

        for (Json request : requests.array("trees")) {
            String id = request.string("id");
            TransitStopFacility from = controler.getScenario().getTransitSchedule().getFacilities()
                    .get(Id.create(request.string("from_stop"), TransitStopFacility.class));
            if (from == null) {
                throw new IllegalArgumentException("unknown tree origin stop " + request.string("from_stop"));
            }
            ObjectNode tree = MAPPER.createObjectNode();
            tree.put("id", id);
            tree.put("from_stop", request.string("from_stop"));
            ArrayNode departures = tree.putArray("departures");
            Map<Double, Map<String, TreeArrival>> results = new TreeMap<>();
            RaptorParameters parameters = RaptorUtils.createParameters(controler.getConfig());
            raptor.calcTreesObservable(from, request.number("earliest_departure_time"),
                    request.number("latest_start_time"), parameters, null,
                    new SwissRailRaptor.RaptorObserver() {
                        @Override
                        public void arrivedAtStop(double departureTime, TransitStopFacility stopFacility,
                                double arrivalTime, int transferCount,
                                java.util.function.Supplier<ch.sbb.matsim.routing.pt.raptor.RaptorRoute> route) {
                            results.computeIfAbsent(departureTime, ignored -> new LinkedHashMap<>())
                                    .put(stopFacility.getId().toString(), new TreeArrival(arrivalTime, transferCount));
                        }
                    });
            for (Map.Entry<Double, Map<String, TreeArrival>> result : results.entrySet()) {
                ObjectNode departure = departures.addObject();
                departure.put("departure_time", roundTime(result.getKey()));
                ArrayNode destinations = departure.putArray("destinations");
                for (Json destination : request.array("destinations")) {
                    String stop = destination.string("stop");
                    ObjectNode item = destinations.addObject();
                    item.put("stop", stop);
                    TreeArrival info = result.getValue().get(stop);
                    if (info == null) {
                        item.put("result", "no_path");
                    } else {
                        item.put("result", "found");
                        item.put("arrival_time", roundTime(info.arrivalTime()));
                        item.put("transfer_count", info.transferCount);
                    }
                }
            }
            trees.add(tree);
        }
        return trees;
    }

    private record TreeArrival(double arrivalTime, int transferCount) {}

    private static Facility facility(Scenario scenario, String id, Json end) {
        ActivityFacility facility = scenario.getActivityFacilities().getFactory().createActivityFacility(
                Id.create(id, ActivityFacility.class),
                new Coord(end.number("x"), end.number("y")),
                Id.create(end.string("link"), Link.class));
        scenario.getActivityFacilities().addActivityFacility(facility);
        return facility;
    }

    /**
     * Reads the event file the controler wrote and keeps the passenger-relevant types.
     *
     * <p>File order is kept throughout. The order the simulation emitted events in is the order the
     * journey happened in, and reordering by time or by type would destroy the dependencies that
     * boarding, alighting and service identity rely on.
     */
    private static ArrayNode events(Path outputDirectory) throws IOException {
        ArrayNode events = MAPPER.createArrayNode();
        Path eventFile = eventFile(outputDirectory);
        if (eventFile == null) {
            return events;
        }

        Map<String, Integer> skipped = new TreeMap<>();
        List<Json> records = new ArrayList<>();
        for (Map<String, String> event : readEvents(eventFile)) {
            List<String> kept = EVENT_ATTRIBUTES.get(event.get("type"));
            if (kept == null) {
                skipped.merge(event.get("type"), 1, Integer::sum);
                continue;
            }
            Json record = new Json(MAPPER.createObjectNode());
            record.node().put("time", round(Double.parseDouble(event.getOrDefault("time", "0"))));
            record.node().put("type", event.get("type"));
            for (String attribute : kept) {
                String value = event.get(attribute);
                if (value == null) {
                    continue;
                }
                if (NUMERIC_ATTRIBUTES.contains(attribute)) {
                    double number = Double.parseDouble(value);
                    record.node().put(attribute, attribute.equals("time") ? roundTime(number) : number);
                } else {
                    record.node().put(attribute, value);
                }
            }
            records.add(record);
        }
        if (!skipped.isEmpty()) {
            System.err.println("event types not part of the normalized comparison: " + skipped);
        }

        // File order is kept. Events at the same time on one agent are ordered by the simulation,
        // and that order carries meaning: boarding, alighting and service identity depend on it.
        // Only genuinely independent events (different agents) could be reordered, and reordering
        // them would break the alignment with the Rust event stream.
        for (Json record : records) {
            events.add(record.node());
        }
        return events;
    }

    /** The controler writes `<iteration>.events.xml` per iteration, optionally gzipped. */
    private static Path eventFile(Path outputDirectory) throws IOException {
        if (!Files.isDirectory(outputDirectory)) {
            return null;
        }
        try (Stream<Path> files = Files.walk(outputDirectory)) {
            List<Path> matches = files.filter(Files::isRegularFile)
                    .filter(path -> EVENT_FILE.matcher(path.getFileName().toString()).matches())
                    .toList();
            if (matches.size() > 1) {
                throw new IllegalStateException("the fixture wrote " + matches.size()
                        + " event files; a reference records one iteration, so give it a single-iteration config");
            }
            return matches.isEmpty() ? null : matches.getFirst();
        }
    }

    /** Minimal MATSim event XML reader: one {@code <event .../>} element per line. */
    private static List<Map<String, String>> readEvents(Path eventFile) throws IOException {
        List<Map<String, String>> events = new ArrayList<>();
        try (InputStream in = openEventFile(eventFile);
             BufferedReader reader = new BufferedReader(new InputStreamReader(in, StandardCharsets.UTF_8))) {
            String line;
            while ((line = reader.readLine()) != null) {
                int start = line.indexOf("<event ");
                if (start < 0) {
                    continue;
                }
                Map<String, String> attributes = new LinkedHashMap<>();
                int index = start + "<event ".length();
                while (index < line.length()) {
                    int equals = line.indexOf('=', index);
                    if (equals < 0) {
                        break;
                    }
                    String key = line.substring(index, equals).trim();
                    if (line.charAt(equals + 1) != '"') {
                        index = equals + 1;
                        continue;
                    }
                    int end = line.indexOf('"', equals + 2);
                    if (end < 0) {
                        break;
                    }
                    attributes.put(key, unescape(line.substring(equals + 2, end)));
                    index = end + 1;
                }
                events.add(attributes);
            }
        }
        return events;
    }

    private static InputStream openEventFile(Path eventFile) throws IOException {
        if (eventFile.toString().endsWith(".gz")) {
            return new GZIPInputStream(Files.newInputStream(eventFile));
        }
        return Files.newInputStream(eventFile);
    }

    private static String unescape(String value) {
        return value.replace("&quot;", "\"").replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">");
    }

    private static double round(double value) {
        return roundTime(value);
    }

    private static String sha256(Path path) {
        try {
            MessageDigest digest = MessageDigest.getInstance("SHA-256");
            return HexFormat.of().formatHex(digest.digest(Files.readAllBytes(path)));
        } catch (IOException | java.security.NoSuchAlgorithmException e) {
            throw new UncheckedIOException(new IOException("cannot hash " + path, e));
        }
    }

    private static Map<String, String> parseOptions(String[] args) {
        Map<String, String> options = new LinkedHashMap<>();
        for (int i = 0; i < args.length; i += 2) {
            if (!args[i].startsWith("--") || i + 1 == args.length) {
                throw new IllegalArgumentException("expected --option value pairs, got " + String.join(" ", args));
            }
            options.put(args[i], args[i + 1]);
        }
        return options;
    }

    private static String require(Map<String, String> options, String key) {
        String value = options.get(key);
        if (value == null) {
            throw new IllegalArgumentException("missing " + key);
        }
        return value;
    }

    /** Tiny JSON reader, so the harness needs no JSON parser beyond what MATSim already ships. */
    private record Json(ObjectNode node) {

        static Json read(Path path) throws IOException {
            try {
                return new Json((ObjectNode) MAPPER.readTree(Files.readString(path)));
            } catch (IOException e) {
                throw new IOException("cannot read " + path, e);
            }
        }

        Json field(String name) {
            return node.has(name) && node.get(name).isObject() ? new Json((ObjectNode) node.get(name)) : null;
        }

        List<Json> array(String name) {
            List<Json> elements = new ArrayList<>();
            node.get(name).forEach(element -> elements.add(new Json((ObjectNode) element)));
            return elements;
        }

        String string(String name) {
            return node.get(name).asText();
        }

        double number(String name) {
            return node.get(name).asDouble();
        }
    }
}
