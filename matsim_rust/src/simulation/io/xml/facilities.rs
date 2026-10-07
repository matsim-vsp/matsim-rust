use std::path::Path;

use serde::{Deserialize, Serialize};
use tracing::info;

use crate::simulation::io::xml;
use crate::simulation::io::xml::attributes::IOAttributes;
use crate::simulation::scenario::facilities::{
    ActivityFacilities, ActivityFacility, ActivityOption, OpenDay, OpeningTime,
};

pub(crate) fn load_from_xml(path: &Path) -> IOFacilities {
    let io_facilities = IOFacilities::from_file(path.to_str().unwrap());

    info!(
        "Finished reading facilities. It contains {} facilities.",
        io_facilities.facilities.len()
    );

    io_facilities
}

pub(crate) fn write_to_xml(facilities: &ActivityFacilities, path: &Path) {
    info!("Writing facilities to XML at {path:?}");
    xml::write_to_file(
        &IOFacilities::from(facilities),
        path,
        "<!DOCTYPE facilities SYSTEM \"http://www.matsim.org/files/dtd/facilities_v2.dtd\">",
    );
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Clone)]
#[serde(rename = "facilities")]
pub struct IOFacilities {
    #[serde(rename = "@name", skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(rename = "@aggregation_layer", skip_serializing_if = "Option::is_none")]
    pub aggregation_layer: Option<String>,
    #[serde(
        rename = "@xml:lang",
        alias = "@lang",
        skip_serializing_if = "Option::is_none"
    )]
    pub lang: Option<String>,
    #[serde(rename = "attributes", skip_serializing_if = "Option::is_none")]
    pub attributes: Option<IOAttributes>,
    #[serde(rename = "facility", default)]
    pub facilities: Vec<IOFacility>,
}

impl IOFacilities {
    pub fn from_file(file_path: &str) -> Self {
        xml::read_from_file(file_path)
    }
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Clone)]
pub struct IOFacility {
    #[serde(rename = "@id")]
    pub id: String,
    #[serde(rename = "@x")]
    pub x: Option<f64>,
    #[serde(rename = "@y")]
    pub y: Option<f64>,
    #[serde(rename = "@z", skip_serializing_if = "Option::is_none")]
    pub z: Option<f64>,
    #[serde(rename = "@linkId", skip_serializing_if = "Option::is_none")]
    pub link_id: Option<String>,
    #[serde(rename = "@desc", skip_serializing_if = "Option::is_none")]
    pub desc: Option<String>,
    #[serde(rename = "activity", default)]
    pub activities: Vec<IOFacilityActivity>,
    #[serde(rename = "attributes", skip_serializing_if = "Option::is_none")]
    pub attributes: Option<IOAttributes>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Clone)]
pub struct IOFacilityActivity {
    #[serde(rename = "@type")]
    pub activity_type: String,
    #[serde(rename = "capacity", skip_serializing_if = "Option::is_none")]
    pub capacity: Option<IOCapacity>,
    #[serde(rename = "opentime", default)]
    pub open_times: Vec<IOOpenTime>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Clone, Copy)]
pub struct IOCapacity {
    #[serde(rename = "@value")]
    pub value: f64,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Clone)]
pub struct IOOpenTime {
    #[serde(rename = "@day", default)]
    pub day: IOOpenDay,
    #[serde(rename = "@start_time")]
    pub start_time: String,
    #[serde(rename = "@end_time")]
    pub end_time: String,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq, Clone, Copy, Default)]
#[serde(rename_all = "lowercase")]
pub enum IOOpenDay {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
    Wkday,
    Wkend,
    #[default]
    Wk,
}

impl From<&ActivityFacilities> for IOFacilities {
    fn from(facilities: &ActivityFacilities) -> Self {
        IOFacilities {
            name: facilities.name.clone(),
            aggregation_layer: facilities.aggregation_layer.clone(),
            lang: facilities.lang.clone(),
            attributes: IOAttributes::from_internal_none_if_empty(&facilities.attributes),
            facilities: facilities
                .sorted_facilities()
                .into_iter()
                .map(IOFacility::from)
                .collect(),
        }
    }
}

impl From<&ActivityFacility> for IOFacility {
    fn from(facility: &ActivityFacility) -> Self {
        IOFacility {
            id: facility.id.external().to_string(),
            x: Some(facility.coord.x),
            y: Some(facility.coord.y),
            z: Some(facility.coord.z),
            link_id: facility
                .base_link
                .as_ref()
                .map(|id| id.external().to_string()),
            desc: facility.desc.clone(),
            activities: facility
                .activities
                .iter()
                .map(IOFacilityActivity::from)
                .collect(),
            attributes: IOAttributes::from_internal_none_if_empty(&facility.attributes),
        }
    }
}

impl From<&ActivityOption> for IOFacilityActivity {
    fn from(option: &ActivityOption) -> Self {
        IOFacilityActivity {
            activity_type: option.activity_type.external().to_string(),
            capacity: option.capacity.map(|value| IOCapacity { value }),
            open_times: option.open_times.iter().map(IOOpenTime::from).collect(),
        }
    }
}

impl From<&OpeningTime> for IOOpenTime {
    fn from(open_time: &OpeningTime) -> Self {
        IOOpenTime {
            day: IOOpenDay::from(open_time.day),
            start_time: open_time.start_time.format_hh_mm_ss_trimmed(),
            end_time: open_time.end_time.format_hh_mm_ss_trimmed(),
        }
    }
}

impl From<OpenDay> for IOOpenDay {
    fn from(day: OpenDay) -> Self {
        match day {
            OpenDay::Mon => IOOpenDay::Mon,
            OpenDay::Tue => IOOpenDay::Tue,
            OpenDay::Wed => IOOpenDay::Wed,
            OpenDay::Thu => IOOpenDay::Thu,
            OpenDay::Fri => IOOpenDay::Fri,
            OpenDay::Sat => IOOpenDay::Sat,
            OpenDay::Sun => IOOpenDay::Sun,
            OpenDay::Wkday => IOOpenDay::Wkday,
            OpenDay::Wkend => IOOpenDay::Wkend,
            OpenDay::Wk => IOOpenDay::Wk,
        }
    }
}

#[cfg(test)]
mod tests {
    use quick_xml::de::from_str;

    use crate::simulation::io::xml::facilities::{IOFacilities, IOOpenDay};

    #[test]
    fn parse_empty_facilities() {
        let xml = "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
                <!DOCTYPE facilities SYSTEM \"https://www.matsim.org/files/dtd/facilities_v2.dtd\">
                <facilities />";

        let facilities: IOFacilities = from_str(xml).unwrap();

        assert_eq!(None, facilities.name);
        assert_eq!(0, facilities.facilities.len());
    }

    #[test]
    fn parse_full_facility() {
        let xml = "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
                <!DOCTYPE facilities SYSTEM \"https://www.matsim.org/files/dtd/facilities_v2.dtd\">
                <facilities name=\"test facilities\" aggregation_layer=\"parcel\" xml:lang=\"en-US\">
                    <attributes>
                        <attribute name=\"source\" class=\"java.lang.String\">fixture</attribute>
                    </attributes>
                    <facility id=\"f1\" x=\"1.5\" y=\"2.5\" z=\"3.5\" linkId=\"l1\" desc=\"facility one\">
                        <activity type=\"work\">
                            <capacity value=\"42.5\" />
                            <opentime day=\"mon\" start_time=\"08:00:00\" end_time=\"12:00:00\" />
                            <opentime day=\"wkday\" start_time=\"13:00:00\" end_time=\"18:00:00\" />
                        </activity>
                        <attributes>
                            <attribute name=\"priority\" class=\"java.lang.Integer\">7</attribute>
                        </attributes>
                    </facility>
                </facilities>";

        let facilities: IOFacilities = from_str(xml).unwrap();

        assert_eq!("test facilities", facilities.name.as_deref().unwrap());
        assert_eq!("parcel", facilities.aggregation_layer.as_deref().unwrap());
        assert_eq!("en-US", facilities.lang.as_deref().unwrap());
        assert_eq!(1, facilities.attributes.unwrap().attributes.len());
        assert_eq!(1, facilities.facilities.len());

        let facility = facilities.facilities.first().unwrap();
        assert_eq!("f1", facility.id);
        assert_eq!(Some(1.5), facility.x);
        assert_eq!(Some(2.5), facility.y);
        assert_eq!(Some(3.5), facility.z);
        assert_eq!("l1", facility.link_id.as_deref().unwrap());
        assert_eq!("facility one", facility.desc.as_deref().unwrap());
        assert_eq!(1, facility.activities.len());
        assert_eq!(1, facility.attributes.as_ref().unwrap().attributes.len());

        let activity = facility.activities.first().unwrap();
        assert_eq!("work", activity.activity_type);
        assert_eq!(42.5, activity.capacity.unwrap().value);
        assert_eq!(2, activity.open_times.len());
        assert_eq!(IOOpenDay::Mon, activity.open_times[0].day);
        assert_eq!(IOOpenDay::Wkday, activity.open_times[1].day);
    }

    #[test]
    fn parse_open_time_defaults_to_week() {
        let xml = "<facilities>
                    <facility id=\"f1\">
                        <activity type=\"shop\">
                            <opentime start_time=\"00:00:00\" end_time=\"24:00:00\" />
                        </activity>
                    </facility>
                </facilities>";

        let facilities: IOFacilities = from_str(xml).unwrap();
        let open_time = &facilities.facilities[0].activities[0].open_times[0];

        assert_eq!(IOOpenDay::Wk, open_time.day);
    }
}
