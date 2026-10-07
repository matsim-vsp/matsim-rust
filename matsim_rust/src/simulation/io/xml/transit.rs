use std::path::Path;

use serde::{Deserialize, Serialize};
use tracing::info;

use crate::simulation::io::xml;
use crate::simulation::io::xml::attributes::IOAttributes;
use crate::simulation::scenario::transit::TransitSchedule;

pub fn load_from_xml(path: &Path) -> IOTransitSchedule {
    let io_schedule = IOTransitSchedule::from_file(path.to_str().unwrap());

    let routes = io_schedule
        .transit_lines
        .iter()
        .flat_map(|line| line.transit_routes.iter())
        .count();

    info!(
        "Finished reading transit schedule. It contains {} stops, {} lines and {} routes.",
        io_schedule.transit_stops.stop_facilities.len(),
        io_schedule.transit_lines.len(),
        routes
    );

    io_schedule
}

pub fn write_to_xml(schedule: &TransitSchedule, path: &Path) {
    info!("Writing transit schedule to XML at {path:?}");
    xml::write_to_file(
        &IOTransitSchedule::from(schedule),
        path,
        "<!DOCTYPE transitSchedule SYSTEM \"http://www.matsim.org/files/dtd/transitSchedule_v2.dtd\">",
    );
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Clone)]
#[serde(rename = "transitSchedule")]
pub struct IOTransitSchedule {
    #[serde(rename = "attributes", skip_serializing_if = "Option::is_none")]
    pub attributes: Option<IOAttributes>,
    #[serde(rename = "transitStops")]
    pub transit_stops: IOTransitStops,
    #[serde(
        rename = "minimalTransferTimes",
        skip_serializing_if = "Option::is_none"
    )]
    pub minimal_transfer_times: Option<IOMinimalTransferTimes>,
    #[serde(rename = "transitLine", default)]
    pub transit_lines: Vec<IOTransitLine>,
}

impl IOTransitSchedule {
    pub fn from_file(file_path: &str) -> Self {
        xml::read_from_file(file_path)
    }
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Clone)]
pub struct IOTransitStops {
    #[serde(rename = "stopFacility", default)]
    pub stop_facilities: Vec<IOStopFacility>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Clone)]
pub struct IOStopFacility {
    #[serde(rename = "@id")]
    pub id: String,
    #[serde(rename = "@x")]
    pub x: f64,
    #[serde(rename = "@y")]
    pub y: f64,
    #[serde(rename = "@z", skip_serializing_if = "Option::is_none")]
    pub z: Option<f64>,
    #[serde(rename = "@linkRefId", skip_serializing_if = "Option::is_none")]
    pub link_ref_id: Option<String>,
    #[serde(rename = "@name", skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(rename = "@stopAreaId", skip_serializing_if = "Option::is_none")]
    pub stop_area_id: Option<String>,
    #[serde(rename = "@isBlocking", skip_serializing_if = "Option::is_none")]
    pub is_blocking: Option<bool>,
    #[serde(rename = "attributes", skip_serializing_if = "Option::is_none")]
    pub attributes: Option<IOAttributes>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Clone)]
pub struct IOMinimalTransferTimes {
    #[serde(rename = "relation", default)]
    pub relations: Vec<IOMinimalTransferRelation>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Clone)]
pub struct IOMinimalTransferRelation {
    #[serde(rename = "@fromStop")]
    pub from_stop: String,
    #[serde(rename = "@toStop")]
    pub to_stop: String,
    #[serde(rename = "@transferTime")]
    pub transfer_time: f64,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Clone)]
pub struct IOTransitLine {
    #[serde(rename = "@id")]
    pub id: String,
    #[serde(rename = "@name", skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(rename = "attributes", skip_serializing_if = "Option::is_none")]
    pub attributes: Option<IOAttributes>,
    #[serde(rename = "transitRoute", default)]
    pub transit_routes: Vec<IOTransitRoute>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Clone)]
pub struct IOTransitRoute {
    #[serde(rename = "@id")]
    pub id: String,
    #[serde(rename = "description", skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "transportMode")]
    pub transport_mode: String,
    #[serde(rename = "routeProfile")]
    pub route_profile: IORouteProfile,
    #[serde(rename = "route")]
    pub route: IONetworkRoute,
    #[serde(rename = "departures")]
    pub departures: IODepartures,
    #[serde(rename = "attributes", skip_serializing_if = "Option::is_none")]
    pub attributes: Option<IOAttributes>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Clone)]
pub struct IORouteProfile {
    #[serde(rename = "stop", default)]
    pub stops: Vec<IORouteStop>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Clone)]
pub struct IORouteStop {
    #[serde(rename = "@refId")]
    pub ref_id: String,
    #[serde(rename = "@arrivalOffset", skip_serializing_if = "Option::is_none")]
    pub arrival_offset: Option<String>,
    #[serde(rename = "@departureOffset", skip_serializing_if = "Option::is_none")]
    pub departure_offset: Option<String>,
    #[serde(
        rename = "@awaitDepartureTime",
        alias = "@awaitDeparture",
        skip_serializing_if = "Option::is_none"
    )]
    pub await_departure: Option<bool>,
    #[serde(rename = "attributes", skip_serializing_if = "Option::is_none")]
    pub attributes: Option<IOAttributes>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Clone)]
pub struct IONetworkRoute {
    #[serde(rename = "link", default)]
    pub links: Vec<IORouteLink>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Clone)]
pub struct IORouteLink {
    #[serde(rename = "@refId")]
    pub ref_id: String,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Clone)]
pub struct IODepartures {
    #[serde(rename = "departure", default)]
    pub departures: Vec<IODeparture>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Clone)]
pub struct IODeparture {
    #[serde(rename = "@id")]
    pub id: String,
    #[serde(rename = "@departureTime")]
    pub departure_time: String,
    #[serde(rename = "@vehicleRefId", skip_serializing_if = "Option::is_none")]
    pub vehicle_ref_id: Option<String>,
    #[serde(rename = "attributes", skip_serializing_if = "Option::is_none")]
    pub attributes: Option<IOAttributes>,
}

#[cfg(test)]
mod tests {
    use quick_xml::de::from_str;

    use super::IOTransitSchedule;

    #[test]
    fn parse_minimal_v2_schedule() {
        let xml = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
                    <!DOCTYPE transitSchedule SYSTEM \"http://www.matsim.org/files/dtd/transitSchedule_v2.dtd\">\
                    <transitSchedule>\
                        <transitStops>\
                            <stopFacility id=\"s1\" x=\"1.0\" y=\"2.0\"/>\
                        </transitStops>\
                        <transitLine id=\"l1\">\
                            <transitRoute id=\"r1\">\
                                <transportMode>pt</transportMode>\
                                <routeProfile>\
                                    <stop refId=\"s1\" departureOffset=\"00:00:00\"/>\
                                </routeProfile>\
                                <route>\
                                    <link refId=\"link-1\"/>\
                                </route>\
                                <departures>\
                                    <departure id=\"d1\" departureTime=\"06:00:00\"/>\
                                </departures>\
                            </transitRoute>\
                        </transitLine>\
                    </transitSchedule>";

        let schedule: IOTransitSchedule = from_str(xml).unwrap();

        assert_eq!(1, schedule.transit_stops.stop_facilities.len());
        assert_eq!("s1", schedule.transit_stops.stop_facilities[0].id);
        assert_eq!(1, schedule.transit_lines.len());
        assert_eq!("l1", schedule.transit_lines[0].id);
        assert_eq!(1, schedule.transit_lines[0].transit_routes.len());
        assert_eq!(
            "pt",
            schedule.transit_lines[0].transit_routes[0].transport_mode
        );
    }

    #[test]
    fn parse_route_stop_await_departure_alias() {
        let xml = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
                    <!DOCTYPE transitSchedule SYSTEM \"http://www.matsim.org/files/dtd/transitSchedule_v2.dtd\">\
                    <transitSchedule>\
                        <transitStops>\
                            <stopFacility id=\"s1\" x=\"1.0\" y=\"2.0\"/>\
                        </transitStops>\
                        <transitLine id=\"l1\">\
                            <transitRoute id=\"r1\">\
                                <transportMode>pt</transportMode>\
                                <routeProfile>\
                                    <stop refId=\"s1\" departureOffset=\"00:00:00\" awaitDeparture=\"true\"/>\
                                </routeProfile>\
                                <route>\
                                    <link refId=\"link-1\"/>\
                                </route>\
                                <departures>\
                                    <departure id=\"d1\" departureTime=\"06:00:00\"/>\
                                </departures>\
                            </transitRoute>\
                        </transitLine>\
                    </transitSchedule>";

        let schedule: IOTransitSchedule = from_str(xml).unwrap();
        assert_eq!(
            Some(true),
            schedule.transit_lines[0].transit_routes[0]
                .route_profile
                .stops[0]
                .await_departure
        );
    }

    #[test]
    fn parse_fuller_v2_schedule_with_optional_fields() {
        let xml = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
                    <!DOCTYPE transitSchedule SYSTEM \"http://www.matsim.org/files/dtd/transitSchedule_v2.dtd\">\
                    <transitSchedule>\
                        <attributes>\
                            <attribute name=\"startDate\" class=\"java.lang.String\">2023-01-11</attribute>\
                        </attributes>\
                        <transitStops>\
                            <stopFacility id=\"s1\" x=\"1.0\" y=\"2.0\" z=\"3.0\" linkRefId=\"l-1\" name=\"Stop 1\" stopAreaId=\"area-1\" isBlocking=\"false\">\
                                <attributes>\
                                    <attribute name=\"stopFilter\" class=\"java.lang.String\">station</attribute>\
                                </attributes>\
                            </stopFacility>\
                        </transitStops>\
                        <minimalTransferTimes>\
                            <relation fromStop=\"s1\" toStop=\"s2\" transferTime=\"120\"/>\
                        </minimalTransferTimes>\
                        <transitLine id=\"line-1\" name=\"Blue\">\
                            <attributes>\
                                <attribute name=\"operator\" class=\"java.lang.String\">DB</attribute>\
                            </attributes>\
                            <transitRoute id=\"route-1\">\
                                <description>test route</description>\
                                <transportMode>train</transportMode>\
                                <routeProfile>\
                                    <stop refId=\"s1\" departureOffset=\"00:00:00\" awaitDepartureTime=\"true\"/>\
                                    <stop refId=\"s2\" arrivalOffset=\"00:05:00\" departureOffset=\"00:05:30\"/>\
                                </routeProfile>\
                                <route>\
                                    <link refId=\"l-1\"/>\
                                    <link refId=\"l-2\"/>\
                                </route>\
                                <departures>\
                                    <departure id=\"dep-1\" departureTime=\"06:00:00\" vehicleRefId=\"veh-1\">\
                                        <attributes>\
                                            <attribute name=\"run\" class=\"java.lang.String\">A</attribute>\
                                        </attributes>\
                                    </departure>\
                                </departures>\
                                <attributes>\
                                    <attribute name=\"routeAttr\" class=\"java.lang.String\">x</attribute>\
                                </attributes>\
                            </transitRoute>\
                        </transitLine>\
                    </transitSchedule>";

        let schedule: IOTransitSchedule = from_str(xml).unwrap();
        let stop = &schedule.transit_stops.stop_facilities[0];
        let relation = &schedule.minimal_transfer_times.as_ref().unwrap().relations[0];
        let line = &schedule.transit_lines[0];
        let route = &line.transit_routes[0];
        let departure = &route.departures.departures[0];

        assert_eq!(
            Some("2023-01-11"),
            schedule.attributes.as_ref().unwrap().find("startDate")
        );
        assert_eq!(Some(3.0), stop.z);
        assert_eq!(Some("l-1"), stop.link_ref_id.as_deref());
        assert_eq!(Some("Stop 1"), stop.name.as_deref());
        assert_eq!(Some("area-1"), stop.stop_area_id.as_deref());
        assert_eq!(Some(false), stop.is_blocking);
        assert_eq!(
            Some("station"),
            stop.attributes.as_ref().unwrap().find("stopFilter")
        );
        assert_eq!("s1", relation.from_stop);
        assert_eq!("s2", relation.to_stop);
        assert_eq!(120.0, relation.transfer_time);
        assert_eq!(Some("Blue"), line.name.as_deref());
        assert_eq!(Some("test route"), route.description.as_deref());
        assert_eq!("train", route.transport_mode);
        assert_eq!(Some(true), route.route_profile.stops[0].await_departure);
        assert_eq!(2, route.route.links.len());
        assert_eq!("veh-1", departure.vehicle_ref_id.as_deref().unwrap());
        assert_eq!(
            Some("A"),
            departure.attributes.as_ref().unwrap().find("run")
        );
    }

    #[test]
    fn parse_dresden_v2_schedule_fixture() {
        let schedule =
            IOTransitSchedule::from_file("./assets/dresden/dresden-v1.0-transitSchedule.xml.gz");

        let root_attributes = schedule.attributes.as_ref().unwrap();
        assert_eq!(Some("2023-01-11"), root_attributes.find("startDate"));
        assert_eq!(Some("2023-01-11"), root_attributes.find("endDate"));

        assert_eq!(9150, schedule.transit_stops.stop_facilities.len());
        assert_eq!(376, schedule.transit_lines.len());
        assert_eq!(
            4037,
            schedule
                .transit_lines
                .iter()
                .map(|line| line.transit_routes.len())
                .sum::<usize>()
        );

        let stop_with_attributes = schedule
            .transit_stops
            .stop_facilities
            .iter()
            .find(|stop| {
                stop.name.is_some()
                    && stop.stop_area_id.is_some()
                    && stop.is_blocking.is_some()
                    && stop.attributes.is_some()
            })
            .unwrap();
        assert!(
            stop_with_attributes
                .attributes
                .as_ref()
                .unwrap()
                .find("stopFilter")
                .is_some()
        );

        let sample_stop = schedule
            .transit_stops
            .stop_facilities
            .iter()
            .find(|stop| stop.id == "long_1")
            .unwrap();
        assert_eq!(733340.71, sample_stop.x);
        assert_eq!(5304341.79, sample_stop.y);
        assert_eq!(Some("pt_99"), sample_stop.link_ref_id.as_deref());
        assert_eq!(Some("Rosenheim"), sample_stop.name.as_deref());
        assert_eq!(Some("long_1"), sample_stop.stop_area_id.as_deref());
        assert_eq!(Some(false), sample_stop.is_blocking);
        assert_eq!(
            Some("station_S/U/RE/RB"),
            sample_stop.attributes.as_ref().unwrap().find("stopFilter")
        );

        let sample_line = schedule
            .transit_lines
            .iter()
            .find(|line| line.id == "long_EC 12---27")
            .unwrap();
        assert_eq!(Some("EC 12"), sample_line.name.as_deref());
        assert_eq!(
            Some("10"),
            sample_line
                .attributes
                .as_ref()
                .unwrap()
                .find("gtfs_agency_id")
        );
        assert_eq!(
            Some("EC 12"),
            sample_line
                .attributes
                .as_ref()
                .unwrap()
                .find("gtfs_route_short_name")
        );
        assert_eq!(
            Some("2"),
            sample_line
                .attributes
                .as_ref()
                .unwrap()
                .find("gtfs_route_type")
        );
        assert!(!sample_line.transit_routes.is_empty());

        let route = sample_line
            .transit_routes
            .iter()
            .find(|route| route.id == "long_EC 12---27_0")
            .unwrap();

        assert_eq!("rail", route.transport_mode);
        assert_eq!(
            Some("rail"),
            route.attributes.as_ref().unwrap().find("simple_route_type")
        );
        assert_eq!(15, route.route_profile.stops.len());
        assert_eq!("long_630", route.route_profile.stops[0].ref_id);
        assert_eq!(
            Some("00:00:00"),
            route.route_profile.stops[0].arrival_offset.as_deref()
        );
        assert_eq!(
            Some("00:00:00"),
            route.route_profile.stops[0].departure_offset.as_deref()
        );
        assert_eq!(Some(true), route.route_profile.stops[0].await_departure);
        assert_eq!(15, route.route.links.len());
        assert_eq!("pt_0", route.route.links[0].ref_id);
        assert_eq!(1, route.departures.departures.len());
        assert_eq!("long_1882_0", route.departures.departures[0].id);
        assert_eq!(
            "20:10:00",
            route.departures.departures[0].departure_time.as_str()
        );
        assert_eq!(
            Some("pt_long_EC 12---27_0_0"),
            route.departures.departures[0].vehicle_ref_id.as_deref()
        );
    }
}
