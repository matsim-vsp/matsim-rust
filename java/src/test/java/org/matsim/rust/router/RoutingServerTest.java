package org.matsim.rust.router;

import com.google.protobuf.Empty;
import io.grpc.ManagedChannel;
import io.grpc.ManagedChannelBuilder;
import io.grpc.health.v1.HealthCheckRequest;
import io.grpc.health.v1.HealthCheckResponse.ServingStatus;
import io.grpc.health.v1.HealthGrpc;
import org.junit.jupiter.api.AfterEach;
import org.junit.jupiter.api.BeforeEach;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.io.TempDir;
import org.matsim.core.config.Config;
import org.matsim.core.config.ConfigUtils;
import org.matsim.core.utils.io.IOUtils;
import org.matsim.examples.ExamplesUtils;
import org.matsim.schemas.io.types.Coordinate;
import routing.Routing;
import routing.RoutingServiceGrpc;

import java.nio.file.Path;
import java.time.Duration;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertTimeoutPreemptively;
import static org.junit.jupiter.api.Assertions.assertTrue;

class RoutingServerTest {
	private static final long NANOS_PER_SECOND = 1_000_000_000L;

	@TempDir
	Path output;

	private RoutingServer.Running router;
	private ManagedChannel channel;

	@BeforeEach
	void setUp() throws Exception {
		var scenario = ExamplesUtils.getTestScenarioURL("pt-tutorial");
		Config config = RoutingServer.prepareConfig(ConfigUtils.loadConfig(IOUtils.extendUrl(scenario, "0.config.xml")), output);
		router = RoutingServer.start(config, 0, 2);
		channel = ManagedChannelBuilder.forAddress("localhost", router.port()).usePlaintext().build();
	}

	@AfterEach
	void tearDown() {
		if (channel != null) {
			channel.shutdownNow();
		}
		if (router != null) {
			router.stop();
		}
	}

	@Test
	void routesWithPt() {
		Routing.Request request = Routing.Request.newBuilder()
			.setPersonId("1")
			.setFromLinkId("1112")
			.setFrom(Coordinate.newBuilder().setX(1000).setY(1500))
			.setToLinkId("4142")
			.setTo(Coordinate.newBuilder().setX(4000).setY(1500))
			.setMode("pt")
			.setDepartureTimeNs(27126 * NANOS_PER_SECOND)
			.setNowNs(27000 * NANOS_PER_SECOND)
			.build();

		Routing.Response response = RoutingServiceGrpc.newBlockingStub(channel).getRoute(request);

		assertEquals(3, response.getLegsCount());
		assertEquals("walk", response.getLegs(0).getMode());
		assertEquals("pt", response.getLegs(1).getMode());
		assertEquals("walk", response.getLegs(2).getMode());
		assertTrue(response.getLegs(1).hasPtRoute(), "the pt leg has a pt route");
		response.getLegsList().forEach(leg -> assertEquals("pt", leg.getRoutingMode()));

		assertEquals(2, response.getActivitiesCount());
		assertEquals("pt interaction", response.getActivities(0).getActType());
		assertEquals("pt interaction", response.getActivities(1).getActType());
	}

	@Test
	void stopsOnShutdownCall() {
		RoutingServiceGrpc.newBlockingStub(channel).shutdown(Empty.getDefaultInstance());

		assertTimeoutPreemptively(Duration.ofSeconds(20), router::awaitTermination);
	}

	@Test
	void reportsServingWhenStarted() {
		var health = HealthGrpc.newBlockingStub(channel);

		assertEquals(ServingStatus.SERVING, health.check(HealthCheckRequest.getDefaultInstance()).getStatus());
		assertEquals(ServingStatus.SERVING, health.check(
			HealthCheckRequest.newBuilder().setService(RoutingServiceGrpc.SERVICE_NAME).build()).getStatus());
	}
}
