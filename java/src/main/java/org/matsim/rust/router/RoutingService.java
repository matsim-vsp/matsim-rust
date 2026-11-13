package org.matsim.rust.router;

import com.google.inject.Injector;
import com.google.inject.Key;
import com.google.inject.name.Names;
import com.google.protobuf.Empty;
import io.grpc.stub.StreamObserver;
import org.apache.logging.log4j.LogManager;
import org.apache.logging.log4j.Logger;
import org.matsim.api.core.v01.Coord;
import org.matsim.api.core.v01.Id;
import org.matsim.api.core.v01.Scenario;
import org.matsim.api.core.v01.network.Link;
import org.matsim.api.core.v01.population.Activity;
import org.matsim.api.core.v01.population.Leg;
import org.matsim.api.core.v01.population.Person;
import org.matsim.api.core.v01.population.PlanElement;
import org.matsim.core.config.Config;
import org.matsim.core.controler.ControllerUtils;
import org.matsim.core.population.routes.NetworkRoute;
import org.matsim.core.router.RoutingModule;
import org.matsim.core.router.RoutingRequest;
import org.matsim.core.scenario.ScenarioUtils;
import org.matsim.facilities.ActivityFacilitiesFactoryImpl;
import org.matsim.facilities.ActivityFacility;
import org.matsim.facilities.Facility;
import org.matsim.pt.routes.DefaultTransitPassengerRoute;
import org.matsim.schemas.io.types.AttributeValue;
import org.matsim.schemas.io.types.Coordinate;
import org.matsim.schemas.io.types.GenericRoute;
import org.matsim.schemas.io.types.PtRoute;
import org.matsim.schemas.io.types.PtRouteDescription;
import org.matsim.utils.objectattributes.attributable.Attributes;
import routing.Routing;
import routing.RoutingServiceGrpc;

import java.util.List;
import java.util.Map;
import java.util.Optional;
import java.util.concurrent.atomic.AtomicInteger;

/**
 * Answers the routing requests of the Rust simulation with MATSim's pt router (SwissRailRaptor). Each
 * worker thread has its own router, created by {@link #init()}.
 * <p>
 * Requests and responses are the messages of {@code proto/routing.proto}; legs and activities are the
 * shared types of matsim-schemas. Times are nanoseconds on the wire and seconds in MATSim.
 */
final class RoutingService extends RoutingServiceGrpc.RoutingServiceImplBase {
	private static final Logger log = LogManager.getLogger(RoutingService.class);
	private static final int NANOS_PER_SECOND = 1_000_000_000;
	/** The routing mode the responses carry; the raptor does not set it. */
	private static final String ROUTING_MODE = "pt";

	private final ThreadLocal<RoutingModule> router;
	private final Runnable onShutdown;
	private final AtomicInteger lastLoggedHour = new AtomicInteger(-1);

	private RoutingService(ThreadLocal<RoutingModule> router, Runnable onShutdown) {
		this.router = router;
		this.onShutdown = onShutdown;
	}

	/**
	 * Loads the scenario of {@code config} (network, transit schedule, ...) and prepares one pt router per
	 * thread. {@code onShutdown} runs, in a new thread, when a client calls {@code Shutdown}.
	 */
	static RoutingService create(Config config, Runnable onShutdown) {
		Scenario scenario = ScenarioUtils.loadScenario(config);
		Injector injector = ControllerUtils.createAdhocInjector(scenario);
		ThreadLocal<RoutingModule> router = ThreadLocal.withInitial(
			() -> injector.getInstance(Key.get(RoutingModule.class, Names.named(ROUTING_MODE))));
		return new RoutingService(router, onShutdown);
	}

	/** Creates the router of the calling thread, so that the first request does not have to. */
	void init() {
		router.get();
	}

	@Override
	public void shutdown(Empty request, StreamObserver<Empty> responseObserver) {
		log.info("Received shutdown request");
		responseObserver.onNext(Empty.getDefaultInstance());
		responseObserver.onCompleted();
		new Thread(onShutdown, "router-shutdown").start();
	}

	@Override
	public void getRoute(Routing.Request request, StreamObserver<Routing.Response> responseObserver) {
		logSimulationHour(nsToS(request.getNowNs()));

		List<? extends PlanElement> planElements = router.get().calcRoute(toRoutingRequest(request));

		responseObserver.onNext(toResponse(planElements, request));
		responseObserver.onCompleted();
	}

	private void logSimulationHour(int now) {
		int hour = now / 3600;
		int last = lastLoggedHour.get();
		if (hour > last && lastLoggedHour.compareAndSet(last, hour)) {
			log.info("Received route request for simulation hour {}:00", String.format("%02d", hour));
		}
	}

	private static RoutingRequest toRoutingRequest(Routing.Request request) {
		Id<Link> fromLink = Id.createLinkId(request.getFromLinkId());
		Id<Link> toLink = Id.createLinkId(request.getToLinkId());

		return new RoutingRequest() {
			@Override
			public Facility getFromFacility() {
				return facility("fromFacility", request.getFrom(), fromLink);
			}

			@Override
			public Facility getToFacility() {
				return facility("toFacility", request.getTo(), toLink);
			}

			@Override
			public double getDepartureTime() {
				return nsToS(request.getDepartureTimeNs());
			}

			@Override
			public Person getPerson() {
				return null;
			}

			@Override
			public Attributes getAttributes() {
				return null;
			}
		};
	}

	private static Facility facility(String id, Coordinate coordinate, Id<Link> link) {
		return new ActivityFacilitiesFactoryImpl().createActivityFacility(
			Id.create(id, ActivityFacility.class), new Coord(coordinate.getX(), coordinate.getY()), link);
	}

	private static Routing.Response toResponse(List<? extends PlanElement> planElements, Routing.Request request) {
		Routing.Response.Builder response = Routing.Response.newBuilder();
		for (PlanElement element : planElements) {
			if (element instanceof Activity activity) {
				response.addActivities(toProto(activity));
			} else if (element instanceof Leg leg) {
				leg.setRoutingMode(ROUTING_MODE);
				response.addLegs(toProto(leg));
			} else {
				throw new IllegalArgumentException("Unsupported PlanElement type: " + element.getClass().getName());
			}
		}
		return response.setRequestId(request.getRequestId()).build();
	}

	private static org.matsim.schemas.io.types.Leg toProto(Leg leg) {
		org.matsim.schemas.io.types.Leg.Builder builder = org.matsim.schemas.io.types.Leg.newBuilder()
			.setMode(leg.getMode())
			.setTravTimeNs(sToNs(leg.getTravelTime().orElseThrow(() -> new IllegalArgumentException("Leg must have travel time"))));
		leg.getDepartureTime().ifDefined(d -> builder.setDepTimeNs(sToNs(d)));
		Optional.ofNullable(leg.getRoutingMode()).ifPresent(builder::setRoutingMode);

		for (Map.Entry<String, Object> attribute : leg.getAttributes().getAsMap().entrySet()) {
			builder.putAttributes(attribute.getKey(), toProto(attribute.getValue()));
		}

		GenericRoute.Builder genericRoute = GenericRoute.newBuilder()
			.setStartLink(leg.getRoute().getStartLinkId().toString())
			.setEndLink(leg.getRoute().getEndLinkId().toString())
			.setDistance(leg.getRoute().getDistance());
		leg.getRoute().getTravelTime().ifDefined(d -> genericRoute.setTravTimeNs(sToNs(d)));

		if (leg.getRoute() instanceof DefaultTransitPassengerRoute ptRoute) {
			PtRouteDescription description = PtRouteDescription.newBuilder()
				.setAccessFacilityId(ptRoute.getAccessStopId().toString())
				.setEgressFacilityId(ptRoute.getEgressStopId().toString())
				.setBoardingTimeNs(sToNs(ptRoute.getBoardingTime().orElseThrow(() -> new IllegalArgumentException("PT route must have boarding time"))))
				.setTransitRouteId(ptRoute.getRouteId().toString())
				.setTransitLineId(ptRoute.getLineId().toString())
				.build();
			builder.setPtRoute(PtRoute.newBuilder().setDelegate(genericRoute).setInformation(description));
		} else if (leg.getRoute() instanceof NetworkRoute networkRoute) {
			org.matsim.schemas.io.types.NetworkRoute.Builder route = org.matsim.schemas.io.types.NetworkRoute.newBuilder()
				.setDelegate(genericRoute);
			for (Id<Link> linkId : networkRoute.getLinkIds()) {
				route.addRoute(linkId.toString());
			}
			builder.setNetworkRoute(route);
		} else {
			builder.setGenericRoute(genericRoute);
		}

		return builder.build();
	}

	private static AttributeValue toProto(Object value) {
		return switch (value) {
			case String s -> AttributeValue.newBuilder().setStringValue(s).build();
			case Double d -> AttributeValue.newBuilder().setDoubleValue(d).build();
			case Integer i -> AttributeValue.newBuilder().setIntValue(i).build();
			case Boolean b -> AttributeValue.newBuilder().setBoolValue(b).build();
			default -> throw new IllegalArgumentException("Unsupported attribute type: " + value.getClass().getName());
		};
	}

	private static org.matsim.schemas.io.types.Activity toProto(Activity activity) {
		org.matsim.schemas.io.types.Activity.Builder builder = org.matsim.schemas.io.types.Activity.newBuilder()
			.setActType(activity.getType())
			.setLinkId(activity.getLinkId().toString())
			.setCoordinate(Coordinate.newBuilder().setX(activity.getCoord().getX()).setY(activity.getCoord().getY()));
		activity.getStartTime().ifDefined(t -> builder.setStartTimeNs(sToNs(t)));
		activity.getEndTime().ifDefined(t -> builder.setEndTimeNs(sToNs(t)));
		activity.getMaximumDuration().ifDefined(d -> builder.setMaxDurNs(sToNs(d)));
		return builder.build();
	}

	static int nsToS(long ns) {
		return (int) (ns / NANOS_PER_SECOND);
	}

	static long sToNs(double s) {
		return (long) s * NANOS_PER_SECOND;
	}
}
