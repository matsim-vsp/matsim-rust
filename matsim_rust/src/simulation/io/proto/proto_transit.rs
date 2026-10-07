use crate::generated;
use crate::generated::general::Coordinate;
use crate::generated::transit::{
    MinimalTransferTime as WireMinimalTransferTime, TransitDeparture as WireTransitDeparture,
    TransitLine as WireTransitLine, TransitRoute as WireTransitRoute,
    TransitRouteStop as WireTransitRouteStop, TransitSchedule as WireTransitSchedule,
    TransitStopFacility as WireTransitStopFacility,
};
use crate::simulation::scenario::transit::{
    MinimalTransferTime, TransitDeparture, TransitLine, TransitRoute, TransitRouteStop,
    TransitSchedule, TransitStopFacility,
};
use std::path::Path;
use std::time::Duration;
use tracing::info;

pub fn load_from_proto(path: &Path) -> TransitSchedule {
    info!("Start reading proto transit schedule from path: {path:?}");
    let wire_schedule: WireTransitSchedule = generated::read_from_file(path);
    let schedule = TransitSchedule::from(wire_schedule);
    info!("Finished reading proto transit schedule from path: {path:?}");
    schedule
}

pub fn write_to_proto(schedule: &TransitSchedule, path: &Path) {
    info!("Start writing proto transit schedule to path: {path:?}");
    generated::write_to_file(WireTransitSchedule::from(schedule), path);
    info!("Finished writing proto transit schedule to path: {path:?}");
}

fn duration_to_nanos(duration: Duration) -> u64 {
    duration
        .as_nanos()
        .try_into()
        .expect("transit duration exceeds u64::MAX nanoseconds")
}

impl From<&TransitSchedule> for WireTransitSchedule {
    fn from(schedule: &TransitSchedule) -> Self {
        Self {
            lines: schedule
                .lines()
                .values()
                .map(WireTransitLine::from)
                .collect(),
            facilities: schedule
                .facilities()
                .values()
                .map(WireTransitStopFacility::from)
                .collect(),
            minimal_transfer_times: schedule
                .minimal_transfer_times()
                .iter()
                .map(WireMinimalTransferTime::from)
                .collect(),
            attributes: schedule.attributes().as_cloned_map(),
        }
    }
}

impl From<&TransitLine> for WireTransitLine {
    fn from(line: &TransitLine) -> Self {
        Self {
            id: line.id.internal(),
            name: line.name.clone(),
            routes: line.routes.values().map(WireTransitRoute::from).collect(),
            attributes: line.attributes.as_cloned_map(),
        }
    }
}

impl From<&TransitRoute> for WireTransitRoute {
    fn from(route: &TransitRoute) -> Self {
        Self {
            id: route.id.internal(),
            description: route.description.clone(),
            transport_mode: route.transport_mode.internal(),
            stops: route.stops.iter().map(WireTransitRouteStop::from).collect(),
            network_route: route.network_route.iter().map(|id| id.internal()).collect(),
            departures: route
                .departures
                .iter()
                .map(WireTransitDeparture::from)
                .collect(),
            attributes: route.attributes.as_cloned_map(),
        }
    }
}

impl From<&TransitRouteStop> for WireTransitRouteStop {
    fn from(stop: &TransitRouteStop) -> Self {
        Self {
            facility_id: stop.facility_id.internal(),
            arrival_offset_ns: stop.arrival_offset.map(duration_to_nanos),
            departure_offset_ns: stop.departure_offset.map(duration_to_nanos),
            await_departure: stop.await_departure,
            allow_boarding: stop.allow_boarding,
            allow_alighting: stop.allow_alighting,
            minimum_stop_duration_ns: duration_to_nanos(stop.minimum_stop_duration),
        }
    }
}

impl From<&TransitDeparture> for WireTransitDeparture {
    fn from(departure: &TransitDeparture) -> Self {
        Self {
            id: departure.id.internal(),
            departure_time_ns: departure.departure_time.as_nanos(),
            vehicle_ref_id: departure.vehicle_ref_id.as_ref().map(|id| id.internal()),
            attributes: departure.attributes.as_cloned_map(),
        }
    }
}

impl From<&TransitStopFacility> for WireTransitStopFacility {
    fn from(facility: &TransitStopFacility) -> Self {
        Self {
            id: facility.id.internal(),
            coordinate: Some(Coordinate {
                x: facility.coord.x,
                y: facility.coord.y,
                z: facility.coord.z,
            }),
            link_ref_id: facility.link_ref_id.as_ref().map(|id| id.internal()),
            name: facility.name.clone(),
            stop_area_id: facility.stop_area_id.clone(),
            is_blocking: facility.is_blocking,
            attributes: facility.attributes.as_cloned_map(),
        }
    }
}

impl From<&MinimalTransferTime> for WireMinimalTransferTime {
    fn from(transfer: &MinimalTransferTime) -> Self {
        Self {
            from_stop: transfer.from_stop.internal(),
            to_stop: transfer.to_stop.internal(),
            transfer_time: transfer.transfer_time,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::generated::general::Coordinate;
    use crate::generated::transit::{
        MinimalTransferTime as WireMinimalTransferTime, TransitDeparture as WireTransitDeparture,
        TransitLine as WireTransitLine, TransitRoute as WireTransitRoute,
        TransitRouteStop as WireTransitRouteStop, TransitSchedule as WireTransitSchedule,
        TransitStopFacility as WireTransitStopFacility,
    };
    use crate::simulation::InternalAttributes;
    use crate::simulation::id::Id;
    use crate::simulation::scenario::network::Link;
    use crate::simulation::scenario::transit::{
        TransitDeparture, TransitLine, TransitRoute, TransitSchedule, TransitStopFacility,
    };
    use macros::deterministic_id_test;

    #[deterministic_id_test]
    fn transit_schedule_proto_round_trip_preserves_all_domain_fields() {
        let line_id = Id::<TransitLine>::create("line");
        let route_id = Id::<TransitRoute>::create("route");
        let facility_id = Id::<TransitStopFacility>::create("facility");
        let other_facility_id = Id::<TransitStopFacility>::create("other-facility");
        let departure_id = Id::<TransitDeparture>::create("departure");
        let mode_id = Id::<String>::create("rail");
        let vehicle_id = Id::<String>::create("transit-vehicle");
        let link_id = Id::<Link>::create("link");

        let mut attributes = InternalAttributes::default();
        attributes.insert("string", "value");
        attributes.insert("number", 42.5);
        attributes.insert("flag", true);
        let attributes = attributes.as_cloned_map();

        let wire = WireTransitSchedule {
            lines: vec![WireTransitLine {
                id: line_id.internal(),
                name: "Line name".to_string(),
                routes: vec![WireTransitRoute {
                    id: route_id.internal(),
                    description: Some("Route description".to_string()),
                    transport_mode: mode_id.internal(),
                    stops: vec![WireTransitRouteStop {
                        facility_id: facility_id.internal(),
                        arrival_offset_ns: Some(1_234_567),
                        departure_offset_ns: Some(2_345_678),
                        await_departure: Some(true),
                        allow_boarding: false,
                        allow_alighting: true,
                        minimum_stop_duration_ns: 3_456_789,
                    }],
                    network_route: vec![link_id.internal()],
                    departures: vec![WireTransitDeparture {
                        id: departure_id.internal(),
                        departure_time_ns: 4_567_890,
                        vehicle_ref_id: Some(vehicle_id.internal()),
                        attributes: attributes.clone(),
                    }],
                    attributes: attributes.clone(),
                }],
                attributes: attributes.clone(),
            }],
            facilities: vec![
                WireTransitStopFacility {
                    id: facility_id.internal(),
                    coordinate: Some(Coordinate {
                        x: 1.0,
                        y: 2.0,
                        z: 3.0,
                    }),
                    link_ref_id: Some(link_id.internal()),
                    name: Some("Stop".to_string()),
                    stop_area_id: Some("Area".to_string()),
                    is_blocking: Some(false),
                    attributes: attributes.clone(),
                },
                WireTransitStopFacility {
                    id: other_facility_id.internal(),
                    coordinate: Some(Coordinate {
                        x: 4.0,
                        y: 5.0,
                        z: 0.0,
                    }),
                    link_ref_id: None,
                    name: None,
                    stop_area_id: None,
                    is_blocking: None,
                    attributes: Default::default(),
                },
            ],
            minimal_transfer_times: vec![WireMinimalTransferTime {
                from_stop: facility_id.internal(),
                to_stop: other_facility_id.internal(),
                transfer_time: 12.5,
            }],
            attributes,
        };
        let schedule = TransitSchedule::from(wire);
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("schedule.binpb");

        schedule.to_file(&path);
        let loaded = TransitSchedule::from_file(&path);

        assert_eq!(schedule, loaded);
    }
}
