package org.matsim.rust.router;

import io.grpc.Server;
import io.grpc.ServerBuilder;
import io.grpc.health.v1.HealthCheckResponse.ServingStatus;
import io.grpc.protobuf.services.HealthStatusManager;
import io.grpc.protobuf.services.ProtoReflectionService;
import org.apache.logging.log4j.LogManager;
import org.apache.logging.log4j.Logger;
import org.matsim.application.MATSimAppCommand;
import org.matsim.core.config.Config;
import org.matsim.core.config.ConfigUtils;
import org.matsim.core.controler.OutputDirectoryHierarchy;
import picocli.CommandLine.Command;
import picocli.CommandLine.Option;
import routing.RoutingServiceGrpc;

import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.ExecutionException;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.Future;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.concurrent.atomic.AtomicReference;

/**
 * gRPC routing server for the Rust simulation, see {@code proto/routing.proto}.
 * <p>
 * Loads the scenario of a MATSim config (network, transit schedule, vehicles; no plans), creates one pt
 * router per worker thread and only then opens the port, so a connection means the server is ready. The
 * standard gRPC health service reports {@code SERVING} from then on. The server stops on the
 * {@code Shutdown} call of a client or when the JVM is terminated.
 */
@Command(name = "matsim-rust-router", mixinStandardHelpOptions = true,
	description = "Answers routing requests of matsim-rust with MATSim's pt router.")
public final class RoutingServer implements MATSimAppCommand {
	private static final Logger log = LogManager.getLogger(RoutingServer.class);

	@Option(names = "--config", required = true,
		description = "MATSim config with network, transit schedule and routing parameters. Plans and counts are not read.")
	private String config;

	@Option(names = "--output", defaultValue = "output/router", description = "Output directory (default: ${DEFAULT-VALUE}).")
	private Path output;

	@Option(names = "--threads", defaultValue = "1", description = "Number of routing threads (default: ${DEFAULT-VALUE}).")
	private int threads;

	@Option(names = "--port", defaultValue = "50051", description = "Port to listen on, 0 for any free port (default: ${DEFAULT-VALUE}).")
	private int port;

	public static void main(String[] args) {
		new RoutingServer().execute(args);
	}

	@Override
	public Integer call() throws Exception {
		log.info("Starting router with config {}, output {}, {} threads, port {}", config, output, threads, port);
		Config matsimConfig = prepareConfig(ConfigUtils.loadConfig(config), output);

		Running router = start(matsimConfig, port, threads);
		Runtime.getRuntime().addShutdownHook(new Thread(router::stop, "router-jvm-shutdown"));
		router.awaitTermination();
		log.info("Router stopped");
		return 0;
	}

	/**
	 * Adapts a MATSim config for the router: no plans and counts, single-threaded MATSim internals (the
	 * routers are thread locals), and the given output directory.
	 */
	static Config prepareConfig(Config config, Path output) throws IOException {
		Files.createDirectories(output);
		config.controller().setOutputDirectory(output.toString());
		config.controller().setOverwriteFileSetting(OutputDirectoryHierarchy.OverwriteFileSetting.overwriteExistingFiles);
		config.global().setNumberOfThreads(1);
		config.plans().setInputFile(null);
		config.counts().setInputFile(null);
		return config;
	}

	/**
	 * Loads the scenario, creates the routers of all {@code threads} worker threads and starts the server.
	 * Returns once the server accepts requests.
	 */
	static Running start(Config config, int port, int threads) throws IOException, InterruptedException, ExecutionException {
		AtomicReference<Running> running = new AtomicReference<>();
		RoutingService service = RoutingService.create(config, () -> running.get().stop());
		ExecutorService executor = createExecutor(service, threads);
		HealthStatusManager health = new HealthStatusManager();

		Server server = ServerBuilder.forPort(port)
			.addService(service)
			.addService(health.getHealthService())
			.addService(ProtoReflectionService.newInstance())
			.executor(executor)
			.build()
			.start();
		running.set(new Running(server, executor, health));
		health.setStatus("", ServingStatus.SERVING);
		health.setStatus(RoutingServiceGrpc.SERVICE_NAME, ServingStatus.SERVING);

		log.info("Router listening on port {}", server.getPort());
		return running.get();
	}

	/**
	 * A fixed pool whose threads each create their router before the server starts. A fixed pool starts a
	 * new thread for every task until it has all of its threads, so each thread runs exactly one init.
	 */
	private static ExecutorService createExecutor(RoutingService service, int threads)
		throws InterruptedException, ExecutionException {
		log.info("Initializing {} routing threads", threads);
		AtomicInteger counter = new AtomicInteger();
		ExecutorService executor = Executors.newFixedThreadPool(threads,
			r -> new Thread(r, "router-" + counter.getAndIncrement()));
		List<Future<?>> inits = new ArrayList<>();
		for (int i = 0; i < threads; i++) {
			inits.add(executor.submit(service::init));
		}
		for (Future<?> init : inits) {
			init.get();
		}
		return executor;
	}

	/** A started router. */
	static final class Running {
		private final Server server;
		private final ExecutorService executor;
		private final HealthStatusManager health;

		private Running(Server server, ExecutorService executor, HealthStatusManager health) {
			this.server = server;
			this.executor = executor;
			this.health = health;
		}

		int port() {
			return server.getPort();
		}

		/** Stops accepting requests, lets running ones finish for up to 10 s and stops the worker threads. */
		synchronized void stop() {
			if (server.isShutdown()) {
				return;
			}
			log.info("Stopping router");
			health.enterTerminalState();
			server.shutdown();
			try {
				if (!server.awaitTermination(10, TimeUnit.SECONDS)) {
					server.shutdownNow();
				}
			} catch (InterruptedException e) {
				server.shutdownNow();
				Thread.currentThread().interrupt();
			}
			executor.shutdownNow();
		}

		void awaitTermination() throws InterruptedException {
			server.awaitTermination();
			executor.shutdown();
			executor.awaitTermination(10, TimeUnit.SECONDS);
		}
	}
}
