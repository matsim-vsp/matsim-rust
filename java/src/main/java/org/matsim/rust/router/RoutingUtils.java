package org.matsim.rust.router;

/**
 * Constants and time conversions of the router.
 * <p>
 * Times are nanoseconds on the wire ({@code proto/routing.proto}) and seconds in MATSim.
 */
public final class RoutingUtils {

	/** The routing mode the router serves and that its legs carry (the raptor does not set it). */
	public static final String ROUTING_MODE = "pt";

	/** Port the server listens on unless {@code --port} says otherwise. */
	public static final int DEFAULT_PORT = 50051;
	/** Number of routing threads unless {@code --threads} says otherwise. */
	public static final int DEFAULT_THREADS = 1;
	/** Output directory unless {@code --output} says otherwise. */
	public static final String DEFAULT_OUTPUT = "output/router";

	/** How long running requests may take to finish when the server stops, and the worker threads to end. */
	public static final long STOP_TIMEOUT_SECONDS = 10;

	public static final long NANOS_PER_SECOND = 1_000_000_000L;
	public static final int SECONDS_PER_HOUR = 3600;

	private RoutingUtils() {
	}

	/** Nanoseconds on the wire to whole seconds. */
	public static int nsToS(long ns) {
		return (int) (ns / NANOS_PER_SECOND);
	}

	/** Seconds in MATSim to nanoseconds on the wire; the fraction of a second is dropped. */
	public static long sToNs(double s) {
		return (long) s * NANOS_PER_SECOND;
	}
}
