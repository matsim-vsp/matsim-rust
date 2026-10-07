use crate::generated;
use crate::generated::facilities::{
    ActivityFacilities as WireActivityFacilities, ActivityFacility as WireActivityFacility,
    ActivityOption as WireActivityOption, OpenDay as WireOpenDay, OpeningTime as WireOpeningTime,
};
use crate::generated::general::Coordinate as WireCoordinate;
use crate::simulation::InternalAttributes;
use crate::simulation::id::Id;
use crate::simulation::scenario::Coordinate;
use crate::simulation::scenario::facilities::{
    ActivityFacilities, ActivityFacility, ActivityOption, OpenDay, OpeningTime,
};
use crate::simulation::time::SimTime;
use nohash_hasher::IntMap;
use std::path::Path;
use tracing::info;

pub fn load_from_proto(path: &Path) -> ActivityFacilities {
    info!("Start reading proto facilities from path: {path:?}");
    let wire: WireActivityFacilities = generated::read_from_file(path);
    let facilities = ActivityFacilities::from(wire);
    info!("Finished reading proto facilities from path: {path:?}");
    facilities
}

pub fn write_to_proto(facilities: &ActivityFacilities, path: &Path) {
    info!("Start writing proto facilities to path: {path:?}");
    generated::write_to_file(WireActivityFacilities::from(facilities), path);
    info!("Finished writing proto facilities to path: {path:?}");
}

impl From<&ActivityFacilities> for WireActivityFacilities {
    fn from(facilities: &ActivityFacilities) -> Self {
        Self {
            name: facilities.name.clone(),
            aggregation_layer: facilities.aggregation_layer.clone(),
            lang: facilities.lang.clone(),
            attributes: facilities.attributes.as_cloned_map(),
            facilities: facilities
                .sorted_facilities()
                .into_iter()
                .map(WireActivityFacility::from)
                .collect(),
        }
    }
}

impl From<&ActivityFacility> for WireActivityFacility {
    fn from(facility: &ActivityFacility) -> Self {
        Self {
            id: facility.id.internal(),
            coordinate: Some(WireCoordinate {
                x: facility.coord.x,
                y: facility.coord.y,
                z: facility.coord.z,
            }),
            base_link: facility.base_link.as_ref().map(|id| id.internal()),
            desc: facility.desc.clone(),
            activities: facility
                .activities
                .iter()
                .map(WireActivityOption::from)
                .collect(),
            attributes: facility.attributes.as_cloned_map(),
        }
    }
}

impl From<&ActivityOption> for WireActivityOption {
    fn from(option: &ActivityOption) -> Self {
        Self {
            activity_type: option.activity_type.internal(),
            capacity: option.capacity,
            open_times: option
                .open_times
                .iter()
                .map(WireOpeningTime::from)
                .collect(),
        }
    }
}

impl From<&OpeningTime> for WireOpeningTime {
    fn from(open_time: &OpeningTime) -> Self {
        Self {
            day: WireOpenDay::from(open_time.day) as i32,
            start_time_ns: open_time.start_time.as_nanos(),
            end_time_ns: open_time.end_time.as_nanos(),
        }
    }
}

impl From<OpenDay> for WireOpenDay {
    fn from(day: OpenDay) -> Self {
        match day {
            OpenDay::Mon => WireOpenDay::Mon,
            OpenDay::Tue => WireOpenDay::Tue,
            OpenDay::Wed => WireOpenDay::Wed,
            OpenDay::Thu => WireOpenDay::Thu,
            OpenDay::Fri => WireOpenDay::Fri,
            OpenDay::Sat => WireOpenDay::Sat,
            OpenDay::Sun => WireOpenDay::Sun,
            OpenDay::Wkday => WireOpenDay::Wkday,
            OpenDay::Wkend => WireOpenDay::Wkend,
            OpenDay::Wk => WireOpenDay::Wk,
        }
    }
}

impl From<WireOpenDay> for OpenDay {
    fn from(day: WireOpenDay) -> Self {
        match day {
            WireOpenDay::Mon => OpenDay::Mon,
            WireOpenDay::Tue => OpenDay::Tue,
            WireOpenDay::Wed => OpenDay::Wed,
            WireOpenDay::Thu => OpenDay::Thu,
            WireOpenDay::Fri => OpenDay::Fri,
            WireOpenDay::Sat => OpenDay::Sat,
            WireOpenDay::Sun => OpenDay::Sun,
            WireOpenDay::Wkday => OpenDay::Wkday,
            WireOpenDay::Wkend => OpenDay::Wkend,
            WireOpenDay::Wk => OpenDay::Wk,
        }
    }
}

impl From<WireActivityFacilities> for ActivityFacilities {
    fn from(wire: WireActivityFacilities) -> Self {
        let mut facilities = ActivityFacilities::new(
            wire.name,
            wire.aggregation_layer,
            wire.lang,
            InternalAttributes::from(&wire.attributes),
        );
        for facility in wire.facilities {
            facilities.add_facility(ActivityFacility::from(facility));
        }
        facilities
    }
}

impl From<WireActivityFacility> for ActivityFacility {
    fn from(wire: WireActivityFacility) -> Self {
        let coordinate = wire.coordinate.expect("Facility coordinate is missing");
        Self {
            id: Id::get(wire.id),
            coord: Coordinate::new_3d(coordinate.x, coordinate.y, coordinate.z),
            base_link: wire.base_link.map(Id::get),
            mode_to_link: IntMap::default(),
            desc: wire.desc,
            activities: wire
                .activities
                .into_iter()
                .map(ActivityOption::from)
                .collect(),
            attributes: InternalAttributes::from(&wire.attributes),
        }
    }
}

impl From<WireActivityOption> for ActivityOption {
    fn from(wire: WireActivityOption) -> Self {
        Self {
            activity_type: Id::get(wire.activity_type),
            capacity: wire.capacity,
            open_times: wire.open_times.into_iter().map(OpeningTime::from).collect(),
        }
    }
}

impl From<WireOpeningTime> for OpeningTime {
    fn from(wire: WireOpeningTime) -> Self {
        let day = WireOpenDay::try_from(wire.day)
            .unwrap_or_else(|_| panic!("Invalid facility opening day value {}", wire.day));
        Self {
            day: OpenDay::from(day),
            start_time: SimTime::from_nanos(wire.start_time_ns),
            end_time: SimTime::from_nanos(wire.end_time_ns),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::generated::facilities::ActivityFacilities as WireActivityFacilities;
    use crate::simulation::id::Id;
    use crate::simulation::io::xml::facilities::{
        IOCapacity, IOFacilities, IOFacility, IOFacilityActivity, IOOpenDay, IOOpenTime,
    };
    use crate::simulation::scenario::facilities::{ActivityFacilities, ActivityFacility};
    use macros::deterministic_id_test;
    use std::path::PathBuf;

    fn io_facilities() -> IOFacilities {
        IOFacilities {
            name: Some("test".to_string()),
            aggregation_layer: None,
            lang: Some("en-US".to_string()),
            attributes: None,
            facilities: vec![
                IOFacility {
                    id: "f2".to_string(),
                    x: Some(1.0),
                    y: Some(2.0),
                    z: Some(3.0),
                    link_id: None,
                    desc: Some("second".to_string()),
                    activities: vec![IOFacilityActivity {
                        activity_type: "work".to_string(),
                        capacity: Some(IOCapacity { value: 12.5 }),
                        open_times: vec![
                            IOOpenTime {
                                day: IOOpenDay::Wkday,
                                start_time: "08:00:00".to_string(),
                                end_time: "17:30:00".to_string(),
                            },
                            IOOpenTime {
                                day: IOOpenDay::Wk,
                                start_time: "00:00:00".to_string(),
                                end_time: "24:00:00".to_string(),
                            },
                        ],
                    }],
                    attributes: None,
                },
                IOFacility {
                    id: "f1".to_string(),
                    x: Some(4.0),
                    y: Some(5.0),
                    z: None,
                    link_id: Some("l1".to_string()),
                    desc: None,
                    activities: Vec::new(),
                    attributes: None,
                },
            ],
        }
    }

    #[deterministic_id_test]
    fn facilities_proto_round_trip_preserves_input_fields() {
        let facilities = ActivityFacilities::from(io_facilities());

        let round_trip = ActivityFacilities::from(WireActivityFacilities::from(&facilities));

        assert_eq!(facilities, round_trip);
    }

    #[deterministic_id_test]
    fn facilities_proto_writes_facilities_sorted_by_internal_id() {
        let facilities = ActivityFacilities::from(io_facilities());

        let wire = WireActivityFacilities::from(&facilities);

        let ids: Vec<_> = wire
            .facilities
            .iter()
            .map(|facility| {
                Id::<ActivityFacility>::get(facility.id)
                    .external()
                    .to_string()
            })
            .collect();
        assert_eq!(vec!["f2", "f1"], ids);
    }

    #[deterministic_id_test]
    fn facilities_xml_and_proto_files_load_to_the_same_facilities() {
        let facilities = ActivityFacilities::from(io_facilities());
        let dir = PathBuf::from(
            "test_output/simulation/io/proto/proto_facilities/facilities_xml_and_proto_files_load_to_the_same_facilities",
        );
        let proto_path = dir.join("facilities.binpb");
        let xml_path = dir.join("facilities.xml.gz");

        facilities.to_file(&proto_path);
        facilities.to_file(&xml_path);

        assert_eq!(facilities, ActivityFacilities::from_file(&proto_path));
        assert_eq!(facilities, ActivityFacilities::from_file(&xml_path));
    }
}
