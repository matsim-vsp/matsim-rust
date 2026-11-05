use crate::simulation::InternalAttributes;
use crate::simulation::id::Id;
use crate::simulation::io::proto::proto_transit::{load_from_proto, write_to_proto};
use crate::simulation::io::xml::attributes::IOAttributes;
use crate::simulation::io::xml::transit;
use crate::simulation::io::xml::transit::{
    IODeparture, IODepartures, IOMinimalTransferRelation, IOMinimalTransferTimes, IONetworkRoute,
    IORouteLink, IORouteProfile, IORouteStop, IOStopFacility, IOTransitLine, IOTransitRoute,
    IOTransitSchedule, IOTransitStops,
};
use crate::simulation::scenario::Coordinate;
use crate::simulation::scenario::network::Link;
use crate::simulation::time::SimTime;
use nohash_hasher::IntMap;
use std::ops::Range;
use std::path::Path;
use std::time::Duration;
use tracing::info;

/// Index of a route in [`TransitSchedule::routes`]. Only the schedule creates such indices, so
/// that they cannot be confused with other indices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RouteIndex(u32);

/// Index of a departure in [`TransitSchedule::departures`]. Only the schedule creates such
/// indices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DepartureIndex(u32);

impl RouteIndex {
    fn new(index: usize) -> Self {
        Self(u32::try_from(index).expect("A transit schedule has at most u32::MAX routes."))
    }
}

impl DepartureIndex {
    fn new(index: usize) -> Self {
        Self(u32::try_from(index).expect("A transit schedule has at most u32::MAX departures."))
    }

    /// The position in [`TransitSchedule::departures`], e.g. to index data of the departures.
    pub(crate) fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Debug, PartialEq, Clone)]
pub struct TransitLine {
    pub id: Id<TransitLine>,
    pub name: String,
    pub attributes: InternalAttributes,
    /// The line's routes, which are contiguous in [`TransitSchedule::routes`].
    routes: Range<usize>,
}

#[derive(Debug, PartialEq, Clone)]
pub struct TransitRoute {
    pub id: Id<TransitRoute>,
    pub line: Id<TransitLine>,
    pub description: Option<String>,
    pub transport_mode: Id<String>,
    /// The facilities of the stops, in the order of `stops`.
    pub stop_facilities: Vec<Id<TransitStopFacility>>,
    pub stops: Vec<TransitRouteStop>,
    pub network_route: Vec<Id<Link>>,
    pub attributes: InternalAttributes,
    /// The route's departures, which are contiguous in [`TransitSchedule::departures`].
    departures: Range<usize>,
}

#[derive(Debug, PartialEq, Clone)]
pub struct TransitRouteStop {
    pub arrival_offset: Option<Duration>,
    pub departure_offset: Option<Duration>,
    pub await_departure: Option<bool>,
    pub allow_boarding: bool,
    pub allow_alighting: bool,
    pub minimum_stop_duration: Duration,
    pub attributes: InternalAttributes,
}

impl TransitRouteStop {
    /// The arrival offset. Falls back to the departure offset.
    pub fn arrival(&self) -> Option<Duration> {
        self.arrival_offset.or(self.departure_offset)
    }

    /// The departure offset. Falls back to the arrival offset.
    pub fn departure(&self) -> Option<Duration> {
        self.departure_offset.or(self.arrival_offset)
    }
}

#[derive(Debug, PartialEq, Clone)]
pub struct TransitDeparture {
    pub id: Id<TransitDeparture>,
    pub departure_time: SimTime,
    pub vehicle_ref_id: Option<Id<String>>,
    pub attributes: InternalAttributes,
    // TODO in java there are chainedDepartures. Not sure, if we need this.
}

#[derive(Debug, PartialEq, Clone)]
pub struct MinimalTransferTime {
    pub from_stop: Id<TransitStopFacility>,
    pub to_stop: Id<TransitStopFacility>,
    pub transfer_time: f64,
}

#[derive(Debug, PartialEq, Clone)]
pub struct TransitStopFacility {
    pub id: Id<TransitStopFacility>,
    pub coord: Coordinate,
    pub link_ref_id: Option<Id<Link>>,
    pub name: Option<String>,
    pub stop_area_id: Option<String>,
    pub is_blocking: Option<bool>,
    pub attributes: InternalAttributes,
}

/// The transit schedule with all data of its files.
///
/// Routes and departures are kept in dense vectors. Routes are sorted by the external ids of line
/// and route, so that the layout depends neither on the input format nor on hashing. The routes of
/// a line and the departures of a route are contiguous. Facilities and lines are kept by their
/// ids, in no particular order.
#[derive(Debug, PartialEq, Clone, Default)]
pub struct TransitSchedule {
    facilities: IntMap<Id<TransitStopFacility>, TransitStopFacility>,
    lines: IntMap<Id<TransitLine>, TransitLine>,
    routes: Vec<TransitRoute>,
    departures: Vec<TransitDeparture>,
    /// The route of each departure.
    departure_routes: Vec<RouteIndex>,
    minimal_transfer_times: Vec<MinimalTransferTime>,
    attributes: InternalAttributes,
}

/// A line as it is read from a file. [`TransitSchedule::new`] lays the lines out densely.
pub(crate) struct LoadedLine {
    pub(crate) id: Id<TransitLine>,
    pub(crate) name: String,
    pub(crate) attributes: InternalAttributes,
    pub(crate) routes: Vec<LoadedRoute>,
}

/// A route as it is read from a file, see [`LoadedLine`].
pub(crate) struct LoadedRoute {
    pub(crate) id: Id<TransitRoute>,
    pub(crate) description: Option<String>,
    pub(crate) transport_mode: Id<String>,
    pub(crate) stop_facilities: Vec<Id<TransitStopFacility>>,
    pub(crate) stops: Vec<TransitRouteStop>,
    pub(crate) network_route: Vec<Id<Link>>,
    pub(crate) departures: Vec<TransitDeparture>,
    pub(crate) attributes: InternalAttributes,
}

/// Sorts `items` by `key`. Of items with the same key, the last one is kept, as if they were
/// inserted into a map one after the other.
fn sorted_keeping_last<T>(mut items: Vec<T>, key: impl Fn(&T) -> &str) -> Vec<T> {
    // The sort is stable, so items with the same key stay in the order of the input.
    items.sort_by(|a, b| key(a).cmp(key(b)));
    let mut result: Vec<T> = Vec::with_capacity(items.len());
    for item in items {
        match result.last_mut() {
            Some(last) if key(last) == key(&item) => *last = item,
            _ => result.push(item),
        }
    }
    result
}

impl TransitSchedule {
    /// Lays out the facilities and lines as they were read from a file. A facility, line or route
    /// with the same id as an earlier one replaces it.
    pub(crate) fn new(
        facilities: Vec<TransitStopFacility>,
        lines: Vec<LoadedLine>,
        minimal_transfer_times: Vec<MinimalTransferTime>,
        attributes: InternalAttributes,
    ) -> Self {
        let mut schedule = TransitSchedule {
            // Inserted in the order of the file, so that a later facility replaces an earlier one.
            facilities: facilities
                .into_iter()
                .map(|facility| (facility.id.clone(), facility))
                .collect(),
            minimal_transfer_times,
            attributes,
            ..Default::default()
        };
        // Sorted, so that the routes are laid out in the order of the external ids of their lines.
        for line in sorted_keeping_last(lines, |line| line.id.external()) {
            let first_route = schedule.routes.len();
            for route in sorted_keeping_last(line.routes, |route| route.id.external()) {
                let route_index = RouteIndex::new(schedule.routes.len());
                let first_departure = schedule.departures.len();
                schedule
                    .departure_routes
                    .extend(std::iter::repeat_n(route_index, route.departures.len()));
                schedule.departures.extend(route.departures);
                schedule.routes.push(TransitRoute {
                    id: route.id,
                    line: line.id.clone(),
                    description: route.description,
                    transport_mode: route.transport_mode,
                    stop_facilities: route.stop_facilities,
                    stops: route.stops,
                    network_route: route.network_route,
                    attributes: route.attributes,
                    departures: first_departure..schedule.departures.len(),
                });
            }

            schedule.lines.insert(
                line.id.clone(),
                TransitLine {
                    id: line.id,
                    name: line.name,
                    attributes: line.attributes,
                    routes: first_route..schedule.routes.len(),
                },
            );
        }
        schedule
    }

    pub fn from_file(file_path: &Path) -> Self {
        info!("Reading transit schedule from {file_path:?}");
        let schedule = if file_path.extension().unwrap().eq("binpb") {
            load_from_proto(file_path)
        } else if file_path.extension().unwrap().eq("xml")
            || file_path.extension().unwrap().eq("gz")
            || file_path.extension().unwrap().eq("zst")
        {
            TransitSchedule::from(transit::load_from_xml(file_path))
        } else {
            panic!(
                "Tried to load {file_path:?}. File format not supported. Either use `.xml`, `.xml.gz`, `.xml.zst`, or `.binpb` as extension"
            );
        };
        info!(
            "Finished reading transit schedule. Found {} lines, {} routes and {} facilities.",
            schedule.num_lines(),
            schedule.num_routes(),
            schedule.facilities.len()
        );
        schedule
    }

    pub fn to_file(&self, file_path: &Path) {
        if file_path.extension().unwrap().eq("binpb") {
            write_to_proto(self, file_path);
        } else if file_path.extension().unwrap().eq("xml")
            || file_path.extension().unwrap().eq("gz")
            || file_path.extension().unwrap().eq("zst")
        {
            transit::write_to_xml(self, file_path);
        } else {
            panic!(
                "file format not supported. Either use `.xml`, `.xml.gz`, `.xml.zst`, or `.binpb` as extension"
            );
        }
    }

    /// The facilities, in no particular order.
    pub fn facilities(&self) -> impl Iterator<Item = &TransitStopFacility> {
        self.facilities.values()
    }

    pub fn facility(&self, id: &Id<TransitStopFacility>) -> Option<&TransitStopFacility> {
        self.facilities.get(id)
    }

    /// The link of a stop facility.
    pub fn stop_link(&self, facility: &Id<TransitStopFacility>) -> Option<&Id<Link>> {
        self.facility(facility)?.link_ref_id.as_ref()
    }

    /// The lines, in no particular order.
    pub fn lines(&self) -> impl Iterator<Item = &TransitLine> {
        self.lines.values()
    }

    pub fn line(&self, id: &Id<TransitLine>) -> Option<&TransitLine> {
        self.lines.get(id)
    }

    pub fn num_lines(&self) -> usize {
        self.lines.len()
    }

    pub fn routes(&self) -> &[TransitRoute] {
        &self.routes
    }

    pub fn routes_of(&self, line: &TransitLine) -> &[TransitRoute] {
        &self.routes[line.routes.clone()]
    }

    pub fn route(&self, index: RouteIndex) -> &TransitRoute {
        &self.routes[index.0 as usize]
    }

    /// Finds a route by the ids of its line and route.
    pub fn find_route(
        &self,
        line: &Id<TransitLine>,
        route: &Id<TransitRoute>,
    ) -> Option<RouteIndex> {
        let line = self.line(line)?;
        line.routes
            .clone()
            .find(|&index| self.routes[index].id == *route)
            .map(RouteIndex::new)
    }

    /// The first link of the route's network route, or the link of its first stop if it has no
    /// network route.
    pub fn start_link<'a>(&'a self, route: &'a TransitRoute) -> Option<&'a Id<Link>> {
        route
            .network_route
            .first()
            .or_else(|| self.stop_link(route.stop_facilities.first()?))
    }

    /// The last link of the route's network route, or the link of its last stop if it has no
    /// network route.
    pub fn end_link<'a>(&'a self, route: &'a TransitRoute) -> Option<&'a Id<Link>> {
        route
            .network_route
            .last()
            .or_else(|| self.stop_link(route.stop_facilities.last()?))
    }

    pub fn departures(&self) -> &[TransitDeparture] {
        &self.departures
    }

    pub fn departures_of(&self, route: &TransitRoute) -> &[TransitDeparture] {
        &self.departures[route.departures.clone()]
    }

    pub fn departure(&self, index: DepartureIndex) -> &TransitDeparture {
        &self.departures[index.0 as usize]
    }

    /// The departure, e.g. to give it another vehicle while the scenario is prepared.
    pub(crate) fn departure_mut(&mut self, index: DepartureIndex) -> &mut TransitDeparture {
        &mut self.departures[index.0 as usize]
    }

    /// The route which the departure serves.
    pub fn departure_route(&self, index: DepartureIndex) -> RouteIndex {
        self.departure_routes[index.0 as usize]
    }

    /// All departures with their indices, in the order of the indices.
    pub fn indexed_departures(&self) -> impl Iterator<Item = (DepartureIndex, &TransitDeparture)> {
        self.departures
            .iter()
            .enumerate()
            .map(|(index, departure)| (DepartureIndex::new(index), departure))
    }

    pub fn minimal_transfer_times(&self) -> &Vec<MinimalTransferTime> {
        &self.minimal_transfer_times
    }

    pub fn attributes(&self) -> &InternalAttributes {
        &self.attributes
    }

    pub fn num_routes(&self) -> usize {
        self.routes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

impl From<IOTransitSchedule> for TransitSchedule {
    fn from(io: IOTransitSchedule) -> Self {
        let facilities = io
            .transit_stops
            .stop_facilities
            .into_iter()
            .map(TransitStopFacility::from)
            .collect();

        let lines = io.transit_lines.into_iter().map(LoadedLine::from).collect();

        let minimal_transfer_times = io
            .minimal_transfer_times
            .map(|times| {
                times
                    .relations
                    .into_iter()
                    .map(MinimalTransferTime::from)
                    .collect()
            })
            .unwrap_or_default();

        TransitSchedule::new(
            facilities,
            lines,
            minimal_transfer_times,
            io.attributes.map(Into::into).unwrap_or_default(),
        )
    }
}

impl From<&TransitSchedule> for IOTransitSchedule {
    fn from(schedule: &TransitSchedule) -> Self {
        Self {
            attributes: IOAttributes::from_internal_none_if_empty(schedule.attributes()),
            transit_stops: IOTransitStops {
                stop_facilities: schedule.facilities().map(IOStopFacility::from).collect(),
            },
            minimal_transfer_times: (!schedule.minimal_transfer_times().is_empty()).then(|| {
                IOMinimalTransferTimes {
                    relations: schedule
                        .minimal_transfer_times()
                        .iter()
                        .map(IOMinimalTransferRelation::from)
                        .collect(),
                }
            }),
            transit_lines: schedule
                .lines()
                .map(|line| IOTransitLine {
                    id: line.id.external().to_string(),
                    name: (!line.name.is_empty()).then(|| line.name.clone()),
                    attributes: IOAttributes::from_internal_none_if_empty(&line.attributes),
                    transit_routes: schedule
                        .routes_of(line)
                        .iter()
                        .map(|route| io_route(schedule, route))
                        .collect(),
                })
                .collect(),
        }
    }
}

fn io_route(schedule: &TransitSchedule, route: &TransitRoute) -> IOTransitRoute {
    IOTransitRoute {
        id: route.id.external().to_string(),
        description: route.description.clone(),
        transport_mode: route.transport_mode.external().to_string(),
        route_profile: IORouteProfile {
            stops: route
                .stop_facilities
                .iter()
                .zip(&route.stops)
                .map(|(facility, stop)| io_route_stop(facility, stop))
                .collect(),
        },
        route: IONetworkRoute {
            links: route
                .network_route
                .iter()
                .map(|id| IORouteLink {
                    ref_id: id.external().to_string(),
                })
                .collect(),
        },
        departures: IODepartures {
            departures: schedule
                .departures_of(route)
                .iter()
                .map(IODeparture::from)
                .collect(),
        },
        attributes: IOAttributes::from_internal_none_if_empty(&route.attributes),
    }
}

impl From<&TransitDeparture> for IODeparture {
    fn from(departure: &TransitDeparture) -> Self {
        Self {
            id: departure.id.external().to_string(),
            departure_time: format_time(departure.departure_time),
            vehicle_ref_id: departure
                .vehicle_ref_id
                .as_ref()
                .map(|id| id.external().to_string()),
            attributes: IOAttributes::from_internal_none_if_empty(&departure.attributes),
        }
    }
}

impl From<&MinimalTransferTime> for IOMinimalTransferRelation {
    fn from(transfer: &MinimalTransferTime) -> Self {
        Self {
            from_stop: transfer.from_stop.external().to_string(),
            to_stop: transfer.to_stop.external().to_string(),
            transfer_time: transfer.transfer_time,
        }
    }
}

impl From<&TransitStopFacility> for IOStopFacility {
    fn from(facility: &TransitStopFacility) -> Self {
        Self {
            id: facility.id.external().to_string(),
            x: facility.coord.x,
            y: facility.coord.y,
            z: Some(facility.coord.z),
            link_ref_id: facility
                .link_ref_id
                .as_ref()
                .map(|id| id.external().to_string()),
            name: facility.name.clone(),
            stop_area_id: facility.stop_area_id.clone(),
            is_blocking: facility.is_blocking,
            attributes: IOAttributes::from_internal_none_if_empty(&facility.attributes),
        }
    }
}

impl From<crate::generated::transit::TransitSchedule> for TransitSchedule {
    fn from(wire: crate::generated::transit::TransitSchedule) -> Self {
        let lines = wire.lines.into_iter().map(LoadedLine::from).collect();
        let facilities = wire
            .facilities
            .into_iter()
            .map(TransitStopFacility::from)
            .collect();

        TransitSchedule::new(
            facilities,
            lines,
            wire.minimal_transfer_times
                .into_iter()
                .map(MinimalTransferTime::from)
                .collect(),
            InternalAttributes::from(&wire.attributes),
        )
    }
}

impl From<crate::generated::transit::TransitLine> for LoadedLine {
    fn from(wire: crate::generated::transit::TransitLine) -> Self {
        let routes = wire.routes.into_iter().map(LoadedRoute::from).collect();

        Self {
            id: Id::get(wire.id),
            name: wire.name,
            routes,
            attributes: InternalAttributes::from(&wire.attributes),
        }
    }
}

impl From<crate::generated::transit::TransitRoute> for LoadedRoute {
    fn from(wire: crate::generated::transit::TransitRoute) -> Self {
        let (stop_facilities, stops) = wire
            .stops
            .into_iter()
            .map(|stop| (Id::get(stop.facility_id), TransitRouteStop::from(stop)))
            .unzip();
        Self {
            id: Id::get(wire.id),
            description: wire.description,
            transport_mode: Id::get(wire.transport_mode),
            stop_facilities,
            stops,
            network_route: wire.network_route.into_iter().map(Id::get).collect(),
            departures: wire
                .departures
                .into_iter()
                .map(TransitDeparture::from)
                .collect(),
            attributes: InternalAttributes::from(&wire.attributes),
        }
    }
}

impl From<crate::generated::transit::TransitRouteStop> for TransitRouteStop {
    fn from(wire: crate::generated::transit::TransitRouteStop) -> Self {
        Self {
            arrival_offset: wire.arrival_offset_ns.map(Duration::from_nanos),
            departure_offset: wire.departure_offset_ns.map(Duration::from_nanos),
            await_departure: wire.await_departure,
            allow_boarding: wire.allow_boarding,
            allow_alighting: wire.allow_alighting,
            minimum_stop_duration: Duration::from_nanos(wire.minimum_stop_duration_ns),
            attributes: InternalAttributes::from(&wire.attributes),
        }
    }
}

impl From<crate::generated::transit::TransitDeparture> for TransitDeparture {
    fn from(wire: crate::generated::transit::TransitDeparture) -> Self {
        Self {
            id: Id::get(wire.id),
            departure_time: SimTime::from_nanos(wire.departure_time_ns),
            vehicle_ref_id: wire.vehicle_ref_id.map(Id::get),
            attributes: InternalAttributes::from(&wire.attributes),
        }
    }
}

impl From<crate::generated::transit::TransitStopFacility> for TransitStopFacility {
    fn from(wire: crate::generated::transit::TransitStopFacility) -> Self {
        let coordinate = wire.coordinate.expect("Transit stop coordinate is missing");
        Self {
            id: Id::get(wire.id),
            coord: Coordinate::new_3d(coordinate.x, coordinate.y, coordinate.z),
            link_ref_id: wire.link_ref_id.map(Id::get),
            name: wire.name,
            stop_area_id: wire.stop_area_id,
            is_blocking: wire.is_blocking,
            attributes: InternalAttributes::from(&wire.attributes),
        }
    }
}

impl From<crate::generated::transit::MinimalTransferTime> for MinimalTransferTime {
    fn from(wire: crate::generated::transit::MinimalTransferTime) -> Self {
        Self {
            from_stop: Id::get(wire.from_stop),
            to_stop: Id::get(wire.to_stop),
            transfer_time: wire.transfer_time,
        }
    }
}

impl From<IOTransitLine> for LoadedLine {
    fn from(io: IOTransitLine) -> Self {
        Id::<String>::create(&io.id);
        let routes = io
            .transit_routes
            .into_iter()
            .map(LoadedRoute::from)
            .collect();

        LoadedLine {
            id: Id::create(&io.id),
            name: io.name.unwrap_or_default(),
            routes,
            attributes: io.attributes.map(Into::into).unwrap_or_default(),
        }
    }
}

impl From<IOTransitRoute> for LoadedRoute {
    fn from(io: IOTransitRoute) -> Self {
        Id::<String>::create(&io.id);
        let id = Id::create(&io.id);
        let transport_mode = Id::create(&io.transport_mode);
        let (stop_facilities, stops) = io
            .route_profile
            .stops
            .into_iter()
            .map(route_stop_from_io)
            .unzip();
        LoadedRoute {
            id,
            description: io.description,
            transport_mode,
            stop_facilities,
            stops,
            network_route: io
                .route
                .links
                .into_iter()
                .map(|link| Id::create(&link.ref_id))
                .collect(),
            departures: io
                .departures
                .departures
                .into_iter()
                .map(TransitDeparture::from)
                .collect(),
            attributes: io.attributes.map(Into::into).unwrap_or_default(),
        }
    }
}

/// The flags of a stop, which older versions of this code wrote into the stop's attributes instead
/// of as XML attributes.
const LEGACY_STOP_FLAGS: [&str; 4] = [
    "allowBoarding",
    "allowAlighting",
    "awaitDeparture",
    "minimumStopDuration",
];

fn route_stop_from_io(io: IORouteStop) -> (Id<TransitStopFacility>, TransitRouteStop) {
    // The flags are XML attributes, as in MATSim's DTD. Files that older versions of this code
    // wrote have them in the stop's attributes instead. Missing flags get the DTD's defaults.
    let await_departure = io
        .await_departure
        .or_else(|| find_bool_attr(&io.attributes, "awaitDeparture"));
    let allow_boarding = io
        .allow_boarding
        .or_else(|| find_bool_attr(&io.attributes, "allowBoarding"))
        .unwrap_or(true);
    let allow_alighting = io
        .allow_alighting
        .or_else(|| find_bool_attr(&io.attributes, "allowAlighting"))
        .unwrap_or(true);
    let minimum_stop_duration = match &io.minimum_stop_duration {
        Some(value) => parse_duration_or_secs(value).unwrap_or_else(|| {
            panic!("Invalid transit duration value for minimumStopDuration: {value}")
        }),
        None => find_duration_attr(&io.attributes, "minimumStopDuration").unwrap_or_default(),
    };
    // The legacy flags are fields of the stop, so they are not kept as attributes as well.
    let attributes = io
        .attributes
        .map(|mut attrs| {
            attrs
                .attributes
                .retain(|attr| !LEGACY_STOP_FLAGS.contains(&attr.name.as_str()));
            InternalAttributes::from(attrs)
        })
        .unwrap_or_default();

    let stop = TransitRouteStop {
        arrival_offset: parse_duration_opt(&io.arrival_offset),
        departure_offset: parse_duration_opt(&io.departure_offset),
        await_departure,
        allow_boarding,
        allow_alighting,
        minimum_stop_duration,
        attributes,
    };
    (Id::create(&io.ref_id), stop)
}

fn io_route_stop(facility: &Id<TransitStopFacility>, stop: &TransitRouteStop) -> IORouteStop {
    // As MATSim's TransitScheduleWriterV2, write the flags only if they differ from the DTD's
    // defaults. MATSim always writes awaitDeparture; here it is optional and written if it is set.
    IORouteStop {
        ref_id: facility.external().to_string(),
        arrival_offset: format_duration_opt(stop.arrival_offset),
        departure_offset: format_duration_opt(stop.departure_offset),
        allow_boarding: (!stop.allow_boarding).then_some(false),
        allow_alighting: (!stop.allow_alighting).then_some(false),
        await_departure: stop.await_departure,
        minimum_stop_duration: (!stop.minimum_stop_duration.is_zero())
            .then(|| format_duration(stop.minimum_stop_duration)),
        attributes: IOAttributes::from_internal_none_if_empty(&stop.attributes),
    }
}

impl From<IODeparture> for TransitDeparture {
    fn from(io: IODeparture) -> Self {
        TransitDeparture {
            id: Id::create(&io.id),
            departure_time: parse_time_required(&io.departure_time, "departureTime"),
            vehicle_ref_id: io.vehicle_ref_id.map(|id| Id::create(&id)),
            attributes: io.attributes.map(Into::into).unwrap_or_default(),
        }
    }
}

impl From<IOMinimalTransferRelation> for MinimalTransferTime {
    fn from(io: IOMinimalTransferRelation) -> Self {
        MinimalTransferTime {
            from_stop: Id::create(&io.from_stop),
            to_stop: Id::create(&io.to_stop),
            transfer_time: io.transfer_time,
        }
    }
}

impl From<IOStopFacility> for TransitStopFacility {
    fn from(io: IOStopFacility) -> Self {
        Id::<String>::create(&io.id);
        TransitStopFacility {
            id: Id::create(&io.id),
            coord: Coordinate::new_3d(io.x, io.y, io.z.unwrap_or_default()),
            link_ref_id: io.link_ref_id.map(|id| Id::create(&id)),
            name: io.name,
            stop_area_id: io.stop_area_id,
            is_blocking: io.is_blocking,
            attributes: io.attributes.map(Into::into).unwrap_or_default(),
        }
    }
}

fn parse_duration_opt(value: &Option<String>) -> Option<Duration> {
    value.as_deref().and_then(parse_duration)
}

fn format_duration_opt(value: Option<Duration>) -> Option<String> {
    value.map(format_duration)
}

fn parse_time_required(value: &str, field_name: &str) -> SimTime {
    parse_time(value)
        .unwrap_or_else(|| panic!("Invalid transit time value for {field_name}: {value}"))
}

fn parse_time(value: &str) -> Option<SimTime> {
    SimTime::parse(value).ok()
}

#[allow(dead_code)]
fn format_time(value: SimTime) -> String {
    value.format_hh_mm_ss_trimmed()
}

fn parse_duration(value: &str) -> Option<Duration> {
    parse_time(value).map(SimTime::as_duration)
}

fn format_duration(value: Duration) -> String {
    SimTime::from_duration(value).format_hh_mm_ss_trimmed()
}

fn find_bool_attr(attributes: &Option<IOAttributes>, name: &str) -> Option<bool> {
    attributes
        .as_ref()
        .and_then(|attrs| attrs.find(name))
        .and_then(|value| value.parse::<bool>().ok())
}

fn find_duration_attr(attributes: &Option<IOAttributes>, name: &str) -> Option<Duration> {
    parse_duration_or_secs(attributes.as_ref()?.find(name)?)
}

/// Parses a duration given as time (`hh:mm:ss`) or as seconds. Negative and non-finite seconds
/// are no duration.
fn parse_duration_or_secs(value: &str) -> Option<Duration> {
    parse_duration(value).or_else(|| {
        value
            .parse::<u64>()
            .ok()
            .map(Duration::from_secs)
            .or_else(|| {
                value
                    .parse::<f64>()
                    .ok()
                    .and_then(|v| Duration::try_from_secs_f64(v).ok())
            })
    })
}

#[cfg(test)]
mod tests {
    use crate::simulation::InternalAttributes;
    use crate::simulation::id::Id;
    use crate::simulation::io::xml::transit::IORouteStop;
    use crate::simulation::scenario::network::Link;
    use crate::simulation::scenario::transit::{
        TransitDeparture, TransitLine, TransitRoute, TransitRouteStop, TransitSchedule,
        TransitStopFacility, format_duration, format_time, io_route_stop, parse_duration,
        parse_time, route_stop_from_io,
    };
    use crate::simulation::time::SimTime;
    use macros::deterministic_id_test;
    use std::time::Duration;

    #[deterministic_id_test]
    fn tutorial_schedule_is_loaded_as_domain_model() {
        let schedule =
            TransitSchedule::from_file("./assets/pt_tutorial/transitschedule.xml".as_ref());

        assert_eq!(1, schedule.num_lines());
        assert_eq!(4, schedule.facilities().count());
        assert_eq!(2, schedule.num_routes());

        let line_id: Id<TransitLine> = Id::get_from_ext("Blue Line");
        let line = schedule.line(&line_id).unwrap();
        assert_eq!("", line.name);
        assert_eq!(2, schedule.routes_of(line).len());

        let route_1to3_id: Id<TransitRoute> = Id::get_from_ext("1to3");
        let route_1to3 = schedule.route(schedule.find_route(&line_id, &route_1to3_id).unwrap());
        assert_eq!(
            Id::<String>::get_from_ext("train"),
            route_1to3.transport_mode
        );
        assert_eq!(3, route_1to3.stops.len());
        assert_eq!(4, route_1to3.network_route.len());
        assert_eq!(50, schedule.departures_of(route_1to3).len());
    }

    #[deterministic_id_test]
    fn tutorial_schedule_round_trips_through_xml_to_file() {
        let schedule =
            TransitSchedule::from_file("./assets/pt_tutorial/transitschedule.xml".as_ref());
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("schedule.xml.gz");

        schedule.to_file(&path);
        let loaded = TransitSchedule::from_file(&path);

        assert_eq!(schedule, loaded);
    }

    #[deterministic_id_test]
    fn dresden_schedule_is_loaded_as_domain_model() {
        let schedule = TransitSchedule::from_file(
            "./assets/dresden/dresden-v1.0-transitSchedule.xml.gz".as_ref(),
        );

        assert_eq!(376, schedule.num_lines());
        assert_eq!(9150, schedule.facilities().count());
        assert_eq!(4037, schedule.num_routes());

        let facility_id: Id<TransitStopFacility> = Id::get_from_ext("long_1");
        let facility = schedule.facility(&facility_id).unwrap();
        assert_eq!(733340.71, facility.coord.x);
        assert_eq!(5304341.79, facility.coord.y);
        assert_eq!(0., facility.coord.z);
        assert_eq!(
            Some(Id::<Link>::get_from_ext("pt_99")),
            facility.link_ref_id.clone()
        );
        assert_eq!(Some("Rosenheim"), facility.name.as_deref());
        assert_eq!(Some("long_1"), facility.stop_area_id.as_deref());
        assert_eq!(Some(false), facility.is_blocking);
        assert_eq!(
            Some(String::from("station_S/U/RE/RB")),
            facility.attributes.get("stopFilter")
        );

        let line_id: Id<TransitLine> = Id::get_from_ext("long_EC 12---27");
        let line = schedule.line(&line_id).unwrap();
        assert_eq!("EC 12", line.name);
        assert_eq!(
            Some(String::from("10")),
            line.attributes.get("gtfs_agency_id")
        );
        assert_eq!(
            Some(String::from("EC 12")),
            line.attributes.get("gtfs_route_short_name")
        );
        assert_eq!(
            Some(String::from("2")),
            line.attributes.get("gtfs_route_type")
        );

        let route_id: Id<TransitRoute> = Id::get_from_ext("long_EC 12---27_0");
        let route = schedule.route(schedule.find_route(&line_id, &route_id).unwrap());
        let departures = schedule.departures_of(route);
        assert_eq!(Id::<String>::get_from_ext("rail"), route.transport_mode);
        assert_eq!(
            Some(String::from("rail")),
            route.attributes.get("simple_route_type")
        );
        assert_eq!(15, route.stops.len());
        assert_eq!(15, route.network_route.len());
        assert_eq!(1, departures.len());
        assert_eq!(
            Id::<TransitStopFacility>::get_from_ext("long_630"),
            route.stop_facilities[0]
        );
        assert_eq!(Some(Duration::from_secs(0)), route.stops[0].arrival_offset);
        assert_eq!(
            Some(Duration::from_secs(0)),
            route.stops[0].departure_offset
        );
        assert_eq!(Some(true), route.stops[0].await_departure);
        assert_eq!(Duration::from_secs(0), route.stops[0].minimum_stop_duration);
        assert_eq!(Id::<Link>::get_from_ext("pt_0"), route.network_route[0]);
        assert_eq!(
            Id::<TransitDeparture>::get_from_ext("long_1882_0"),
            departures[0].id
        );
        assert_eq!(
            SimTime::from_secs(20 * 3600 + 10 * 60),
            departures[0].departure_time
        );
        assert_eq!(
            Some(Id::<String>::get_from_ext("pt_long_EC 12---27_0_0")),
            departures[0].vehicle_ref_id.clone()
        );
    }

    #[test]
    fn transit_time_parsing_and_formatting_preserves_subseconds() {
        let parsed = parse_time("20:10:00.123456789").unwrap();
        assert_eq!(SimTime::from_nanos(72_600_123_456_789), parsed);
        assert_eq!("20:10:00.123456789", format_time(parsed));
    }

    #[test]
    fn transit_duration_parsing_and_formatting_preserves_subseconds() {
        let parsed = parse_duration("00:05:00.123456789").unwrap();
        assert_eq!(Duration::from_nanos(300_123_456_789), parsed);
        assert_eq!("00:05:00.123456789", format_duration(parsed));
    }

    #[deterministic_id_test]
    fn route_stop_flags_are_written_as_xml_attributes() {
        let facility = Id::<TransitStopFacility>::create("facility");
        let mut attributes = InternalAttributes::default();
        attributes.insert("platform", "2");
        let stop = TransitRouteStop {
            arrival_offset: Some(Duration::from_secs(60)),
            departure_offset: Some(Duration::from_secs(90)),
            await_departure: Some(true),
            allow_boarding: false,
            allow_alighting: false,
            minimum_stop_duration: Duration::from_secs(30),
            attributes,
        };

        let io = io_route_stop(&facility, &stop);
        assert_eq!(Some(false), io.allow_boarding);
        assert_eq!(Some(false), io.allow_alighting);
        assert_eq!(Some(true), io.await_departure);
        assert_eq!(Some("00:00:30"), io.minimum_stop_duration.as_deref());
        let io_attributes = io.attributes.as_ref().unwrap();
        assert_eq!(1, io_attributes.attributes.len());
        assert_eq!(Some("2"), io_attributes.find("platform"));
        assert_eq!((facility.clone(), stop), route_stop_from_io(io));

        // Flags with the DTD's defaults are not written.
        let default_stop = TransitRouteStop {
            arrival_offset: None,
            departure_offset: Some(Duration::ZERO),
            await_departure: None,
            allow_boarding: true,
            allow_alighting: true,
            minimum_stop_duration: Duration::ZERO,
            attributes: InternalAttributes::default(),
        };
        let io = io_route_stop(&facility, &default_stop);
        assert_eq!(None, io.allow_boarding);
        assert_eq!(None, io.allow_alighting);
        assert_eq!(None, io.await_departure);
        assert_eq!(None, io.minimum_stop_duration);
        assert_eq!(None, io.attributes);
        assert_eq!((facility, default_stop), route_stop_from_io(io));
    }

    #[deterministic_id_test]
    fn route_stop_reads_legacy_flags_from_attributes() {
        let xml = "<stop refId=\"s1\" departureOffset=\"00:00:00\">\
                       <attributes>\
                           <attribute name=\"allowBoarding\" class=\"java.lang.Boolean\">false</attribute>\
                           <attribute name=\"allowAlighting\" class=\"java.lang.Boolean\">true</attribute>\
                           <attribute name=\"awaitDeparture\" class=\"java.lang.Boolean\">true</attribute>\
                           <attribute name=\"minimumStopDuration\" class=\"java.lang.Integer\">30</attribute>\
                           <attribute name=\"platform\" class=\"java.lang.String\">2</attribute>\
                       </attributes>\
                   </stop>";
        let io: IORouteStop = quick_xml::de::from_str(xml).unwrap();

        let (_, stop) = route_stop_from_io(io);
        assert!(!stop.allow_boarding);
        assert!(stop.allow_alighting);
        assert_eq!(Some(true), stop.await_departure);
        assert_eq!(Duration::from_secs(30), stop.minimum_stop_duration);
        // Only the attribute that is not a flag is kept.
        assert_eq!(Some(String::from("2")), stop.attributes.get("platform"));
        assert_eq!(1, stop.attributes.iter().count());
    }
}
