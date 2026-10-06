use crate::simulation::id::Id;
use crate::simulation::io::proto::proto_facilities::{load_from_proto, write_to_proto};
use crate::simulation::io::xml::facilities::{
    IOFacilities, IOFacility, IOFacilityActivity, IOOpenDay, IOOpenTime, load_from_xml,
    write_to_xml,
};
use crate::simulation::scenario::Coordinate;
use crate::simulation::scenario::network::Link;
use crate::simulation::time::SimTime;
use crate::simulation::{Attributable, Identifiable, InternalAttributes};
use nohash_hasher::IntMap;
use std::path::Path;
use tracing::info;

#[derive(Debug, Clone, PartialEq)]
pub struct ActivityFacilities {
    pub facilities: IntMap<Id<ActivityFacility>, ActivityFacility>,
    pub name: Option<String>,
    pub aggregation_layer: Option<String>,
    pub lang: Option<String>,
    pub attributes: InternalAttributes,
}

impl ActivityFacilities {
    pub fn new(
        name: Option<String>,
        aggregation_layer: Option<String>,
        lang: Option<String>,
        attributes: InternalAttributes,
    ) -> Self {
        Self {
            facilities: IntMap::default(),
            name,
            aggregation_layer,
            lang,
            attributes,
        }
    }

    pub fn add_facility(&mut self, facility: ActivityFacility) {
        let id = facility.id().clone();
        let previous = self.facilities.insert(id.clone(), facility);
        assert!(
            previous.is_none(),
            "Facility with id {} already exists.",
            id
        );
    }

    pub fn get(&self, id: &Id<ActivityFacility>) -> Option<&ActivityFacility> {
        self.facilities.get(id)
    }

    /// Returns all facilities sorted by internal id, e.g. for stable output order.
    pub fn sorted_facilities(&self) -> Vec<&ActivityFacility> {
        let mut facilities: Vec<_> = self.facilities.values().collect();
        facilities.sort_by_key(|facility| facility.id.internal());
        facilities
    }

    pub fn from_file(file_path: &Path) -> Self {
        info!("Reading facilities from {file_path:?}");
        let facilities = if file_path.extension().unwrap().eq("binpb") {
            load_from_proto(file_path)
        } else if file_path.extension().unwrap().eq("xml")
            || file_path.extension().unwrap().eq("gz")
            || file_path.extension().unwrap().eq("zst")
        {
            ActivityFacilities::from(load_from_xml(file_path))
        } else {
            panic!(
                "Tried to load {file_path:?}. File format not supported. Either use `.xml`, `.xml.gz`, `.xml.zst`, or `.binpb` as extension"
            );
        };
        info!(
            "Finished reading facilities. Found {} facilities.",
            facilities.facilities.len()
        );
        facilities
    }

    pub fn to_file(&self, file_path: &Path) {
        if file_path.extension().unwrap().eq("binpb") {
            write_to_proto(self, file_path);
        } else if file_path.extension().unwrap().eq("xml")
            || file_path.extension().unwrap().eq("gz")
            || file_path.extension().unwrap().eq("zst")
        {
            write_to_xml(self, file_path);
        } else {
            panic!(
                "file format not supported. Either use `.xml`, `.xml.gz`, `.xml.zst`, or `.binpb` as extension"
            );
        }
    }
}

impl Default for ActivityFacilities {
    fn default() -> Self {
        Self::new(None, None, None, InternalAttributes::default())
    }
}

impl From<IOFacilities> for ActivityFacilities {
    fn from(io: IOFacilities) -> Self {
        let mut facilities = ActivityFacilities::new(
            io.name,
            io.aggregation_layer,
            io.lang,
            io.attributes.map(Into::into).unwrap_or_default(),
        );

        for io_facility in io.facilities {
            facilities.add_facility(ActivityFacility::from(io_facility));
        }

        facilities
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActivityFacility {
    pub id: Id<ActivityFacility>,
    pub coord: Coordinate,
    /// The link given in the input. If it is missing, `prepare_for_sim` assigns the nearest link of
    /// any mode. Use [`ActivityFacility::base_link`] after `prepare_for_sim`.
    pub base_link: Option<Id<Link>>,
    /// The nearest link per network mode, filled in `prepare_for_sim`. Modes whose nearest link
    /// equals the base link are omitted to save memory;
    /// [`Facility::modal_link`](crate::simulation::replanning::routing::Facility::modal_link)
    /// falls back to the base link for them.
    pub mode_to_link: IntMap<Id<String>, Id<Link>>,
    pub desc: Option<String>,
    pub activities: Vec<ActivityOption>,
    pub attributes: InternalAttributes,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActivityOption {
    pub activity_type: Id<String>,
    pub capacity: Option<f64>,
    pub open_times: Vec<OpeningTime>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OpeningTime {
    pub day: OpenDay,
    pub start_time: SimTime,
    pub end_time: SimTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenDay {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
    Wkday,
    Wkend,
    Wk,
}

impl ActivityFacility {
    /// Returns the base link of the facility. It is always present after `prepare_for_sim`.
    pub fn base_link(&self) -> &Id<Link> {
        self.base_link.as_ref().unwrap_or_else(|| {
            panic!(
                "Facility with id {} has no base link. Base links are assigned in prepare_for_sim.",
                self.id
            )
        })
    }

    pub fn desc(&self) -> Option<&str> {
        self.desc.as_deref()
    }

    pub fn activities(&self) -> &[ActivityOption] {
        &self.activities
    }
}

impl Identifiable<ActivityFacility> for ActivityFacility {
    fn id(&self) -> &Id<ActivityFacility> {
        &self.id
    }
}

impl Attributable for ActivityFacility {
    fn attributes(&self) -> &InternalAttributes {
        &self.attributes
    }

    fn attributes_mut(&mut self) -> &mut InternalAttributes {
        &mut self.attributes
    }
}

impl From<IOFacility> for ActivityFacility {
    fn from(io: IOFacility) -> Self {
        let coord = match (io.x, io.y) {
            (Some(x), Some(y)) => Coordinate::new_3d(x, y, io.z.unwrap_or(0.0)),
            _ => panic!("Facility with id {} must have x and y coordinates.", io.id),
        };
        Id::<String>::create(&io.id);
        ActivityFacility {
            id: Id::create(&io.id),
            coord,
            base_link: io.link_id.as_deref().map(Id::create),
            mode_to_link: IntMap::default(),
            desc: io.desc,
            activities: io.activities.into_iter().map(Into::into).collect(),
            attributes: io.attributes.map(Into::into).unwrap_or_default(),
        }
    }
}

impl From<IOFacilityActivity> for ActivityOption {
    fn from(io: IOFacilityActivity) -> Self {
        ActivityOption {
            activity_type: Id::create(&io.activity_type),
            capacity: io.capacity.map(|capacity| capacity.value),
            open_times: io.open_times.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<IOOpenTime> for OpeningTime {
    fn from(io: IOOpenTime) -> Self {
        OpeningTime {
            day: OpenDay::from(io.day),
            start_time: parse_open_time(&io.start_time),
            end_time: parse_open_time(&io.end_time),
        }
    }
}

impl From<IOOpenDay> for OpenDay {
    fn from(day: IOOpenDay) -> Self {
        match day {
            IOOpenDay::Mon => OpenDay::Mon,
            IOOpenDay::Tue => OpenDay::Tue,
            IOOpenDay::Wed => OpenDay::Wed,
            IOOpenDay::Thu => OpenDay::Thu,
            IOOpenDay::Fri => OpenDay::Fri,
            IOOpenDay::Sat => OpenDay::Sat,
            IOOpenDay::Sun => OpenDay::Sun,
            IOOpenDay::Wkday => OpenDay::Wkday,
            IOOpenDay::Wkend => OpenDay::Wkend,
            IOOpenDay::Wk => OpenDay::Wk,
        }
    }
}

fn parse_open_time(value: &str) -> SimTime {
    SimTime::parse(value)
        .unwrap_or_else(|err| panic!("Invalid facility opentime value {value}: {err}"))
}

#[cfg(test)]
mod tests {
    use crate::simulation::id::Id;
    use crate::simulation::io::xml::facilities::{
        IOCapacity, IOFacilities, IOFacility, IOFacilityActivity, IOOpenDay, IOOpenTime,
    };
    use crate::simulation::scenario::Coordinate;
    use crate::simulation::scenario::facilities::{ActivityFacilities, ActivityFacility, OpenDay};
    use crate::simulation::scenario::network::Link;
    use crate::simulation::time::SimTime;
    use macros::deterministic_id_test;

    #[deterministic_id_test]
    fn conversion_creates_facilities_by_id() {
        let facilities = ActivityFacilities::from(IOFacilities {
            name: Some("test".to_string()),
            aggregation_layer: Some("parcel".to_string()),
            lang: Some("en-US".to_string()),
            attributes: None,
            facilities: vec![IOFacility {
                id: "f1".to_string(),
                x: Some(1.0),
                y: Some(2.0),
                z: None,
                link_id: Some("l1".to_string()),
                desc: Some("facility".to_string()),
                activities: vec![IOFacilityActivity {
                    activity_type: "work".to_string(),
                    capacity: Some(IOCapacity { value: 12.5 }),
                    open_times: vec![IOOpenTime {
                        day: IOOpenDay::Mon,
                        start_time: "08:00:00".to_string(),
                        end_time: "17:30:00".to_string(),
                    }],
                }],
                attributes: None,
            }],
        });

        let facility_id: Id<ActivityFacility> = Id::get_from_ext("f1");
        let facility = facilities.get(&facility_id).unwrap();

        assert_eq!(Coordinate::new_3d(1.0, 2.0, 0.0), facility.coord);
        assert_eq!(Some(Id::<Link>::get_from_ext("l1")), facility.base_link);
        assert!(facility.mode_to_link.is_empty());
        assert_eq!("facility", facility.desc.as_deref().unwrap());
        assert_eq!(1, facility.activities.len());
        assert_eq!(
            Id::<String>::get_from_ext("work"),
            facility.activities[0].activity_type
        );
        assert_eq!(Some(12.5), facility.activities[0].capacity);
        assert_eq!(OpenDay::Mon, facility.activities[0].open_times[0].day);
        assert_eq!(
            SimTime::parse("17:30:00").unwrap(),
            facility.activities[0].open_times[0].end_time
        );
    }

    #[deterministic_id_test]
    #[should_panic(expected = "Facility with id f1 already exists.")]
    fn conversion_panics_on_duplicate_facility_ids() {
        let _ = ActivityFacilities::from(IOFacilities {
            name: None,
            aggregation_layer: None,
            lang: None,
            attributes: None,
            facilities: vec![
                io_facility_with_id_coord_and_link("f1"),
                io_facility_with_id_coord_and_link("f1"),
            ],
        });
    }

    #[deterministic_id_test]
    fn conversion_creates_activity_type_and_link_ids() {
        let facilities = ActivityFacilities::from(IOFacilities {
            name: None,
            aggregation_layer: None,
            lang: None,
            attributes: None,
            facilities: vec![IOFacility {
                id: "f1".to_string(),
                x: Some(1.0),
                y: Some(2.0),
                z: Some(5.0),
                link_id: Some("l1".to_string()),
                desc: None,
                activities: vec![IOFacilityActivity {
                    activity_type: "shop".to_string(),
                    capacity: None,
                    open_times: vec![IOOpenTime {
                        day: IOOpenDay::Wk,
                        start_time: "00:00:00".to_string(),
                        end_time: "24:00:00".to_string(),
                    }],
                }],
                attributes: None,
            }],
        });

        let facility = facilities.get(&Id::get_from_ext("f1")).unwrap();

        assert_eq!(Coordinate::new_3d(1.0, 2.0, 5.0), facility.coord);
        assert_eq!(Some(Id::<Link>::get_from_ext("l1")), facility.base_link);
        assert_eq!(
            Id::<String>::get_from_ext("shop"),
            facility.activities[0].activity_type
        );
        assert!(facility.mode_to_link.is_empty());
    }

    #[deterministic_id_test]
    #[should_panic(expected = "Facility with id f1 must have x and y coordinates.")]
    fn conversion_panics_without_coord() {
        let _ = ActivityFacilities::from(IOFacilities {
            name: None,
            aggregation_layer: None,
            lang: None,
            attributes: None,
            facilities: vec![io_facility_with_id_and_link("f1")],
        });
    }

    #[deterministic_id_test]
    fn conversion_keeps_missing_link_id_for_prepare_for_sim() {
        let facilities = ActivityFacilities::from(IOFacilities {
            name: None,
            aggregation_layer: None,
            lang: None,
            attributes: None,
            facilities: vec![io_facility_with_id_and_coord("f1")],
        });

        let facility = facilities.get(&Id::get_from_ext("f1")).unwrap();
        assert_eq!(None, facility.base_link);
    }

    #[deterministic_id_test]
    #[should_panic(expected = "Base links are assigned in prepare_for_sim")]
    fn base_link_panics_for_unprepared_activity_facility() {
        let facilities = ActivityFacilities::from(IOFacilities {
            name: None,
            aggregation_layer: None,
            lang: None,
            attributes: None,
            facilities: vec![io_facility_with_id_and_coord("f1")],
        });
        let facility = facilities.get(&Id::get_from_ext("f1")).unwrap();

        facility.base_link();
    }

    fn io_facility_with_id_and_link(id: &str) -> IOFacility {
        IOFacility {
            id: id.to_string(),
            x: None,
            y: None,
            z: None,
            link_id: Some("l1".to_string()),
            desc: None,
            activities: Vec::new(),
            attributes: None,
        }
    }

    fn io_facility_with_id_and_coord(id: &str) -> IOFacility {
        IOFacility {
            id: id.to_string(),
            x: Some(1.0),
            y: Some(2.0),
            z: None,
            link_id: None,
            desc: None,
            activities: Vec::new(),
            attributes: None,
        }
    }

    fn io_facility_with_id_coord_and_link(id: &str) -> IOFacility {
        IOFacility {
            id: id.to_string(),
            x: Some(1.0),
            y: Some(2.0),
            z: None,
            link_id: Some("l1".to_string()),
            desc: None,
            activities: Vec::new(),
            attributes: None,
        }
    }
}
