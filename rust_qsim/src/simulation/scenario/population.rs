use crate::generated::population::leg::Route;
use crate::generated::population::{Activity, GenericRoute, Leg, Person, Plan, PtRouteDescription};
use crate::simulation::InternalAttributes;
use crate::simulation::agents::agent::SimulationAgent;
use crate::simulation::id::Id;
use crate::simulation::io::proto::proto_population::{load_from_proto, write_to_proto};
use crate::simulation::io::xml::population::{
    IOActivity, IOLeg, IOPTRouteDescription, IOPerson, IOPlan, IOPlanElement, IORoute,
};
use crate::simulation::scenario::Coordinate;
use crate::simulation::scenario::network::{Link, Network};
use crate::simulation::scenario::vehicles::Garage;
use crate::simulation::scenario::vehicles::InternalVehicle;
use crate::simulation::time::SimTime;
use itertools::{EitherOrBoth, Itertools};
use nohash_hasher::IntMap;
use serde_json::{Error, Value};
use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

pub const PREPLANNING_HORIZON: &str = "preplanningHorizon";
pub const SUBPOPULATION: &str = "subpopulation";
pub const DEFAULT_SUBPOPULATION: &str = "person";

trait FromIOPerson<T> {
    fn from_io(io: T, id: Id<InternalPerson>) -> Self;
}

pub fn from_file<F: Fn(&InternalPerson) -> bool>(
    path: impl AsRef<Path>,
    garage: &mut Garage,
    filter: F,
) -> Population {
    if path.as_ref().extension().unwrap().eq("binpb") {
        load_from_proto(path, filter)
    } else if path.as_ref().extension().unwrap().eq("xml")
        || path.as_ref().extension().unwrap().eq("gz")
        || path.as_ref().extension().unwrap().eq("zst")
    {
        let persons = crate::simulation::io::xml::population::load_from_xml(path, garage)
            .into_iter()
            .filter(|(_id, p)| filter(p))
            .collect();
        Population { persons }
    } else {
        panic!(
            "Tried to load {:?}. File format not supported. Either use `.xml`, `.xml.gz`, `.xml.zst`, or `.binpb` as extension",
            path.as_ref()
        );
    }
}

#[derive(Debug, Default, PartialEq)]
pub struct Population {
    pub persons: IntMap<Id<InternalPerson>, InternalPerson>,
}

impl Population {
    pub fn new() -> Self {
        Population {
            persons: Default::default(),
        }
    }

    pub fn from_file(file_path: impl AsRef<Path>, garage: &mut Garage) -> Self {
        from_file(file_path, garage, |_p| true)
    }

    pub fn from_file_filtered<F>(file_path: &Path, garage: &mut Garage, filter: F) -> Self
    where
        F: Fn(&InternalPerson) -> bool,
    {
        from_file(file_path, garage, filter)
    }

    pub fn from_file_filtered_part(
        file_path: &Path,
        net: &Network,
        garage: &mut Garage,
        part: u32,
    ) -> Self {
        from_file(file_path, garage, |p| {
            let act = p.plan_element_at(0).and_then(|p| p.as_activity()).unwrap();
            let partition = net.get_link(&act.link_id).partition;
            partition == part
        })
    }

    pub fn from_persons(persons: Vec<InternalPerson>) -> Self {
        let persons = persons
            .into_iter()
            .map(|p| (p.id().clone(), p))
            .collect::<IntMap<_, _>>();
        Population { persons }
    }

    pub fn from_agents(agents: Vec<SimulationAgent>) -> Self {
        let persons = agents.into_iter().filter_map(|a| a.into_person()).collect();
        Self::from_persons(persons)
    }

    pub fn split_by_start_link_partition(self, net: &Network, num_parts: u32) -> Vec<Self> {
        let mut parts: Vec<_> = (0..num_parts).map(|_| Population::new()).collect();

        for (id, person) in self.persons {
            // Replanning is assumed to preserve this first activity location. If that changes,
            // partition assignment needs an explicit migration/repartitioning policy.
            let act = person
                .plan_element_at(0)
                .and_then(|p| p.as_activity())
                .unwrap_or_else(|| {
                    panic!(
                        "Person {} does not have an initial activity for partition assignment.",
                        id.external()
                    )
                });
            let partition = net.get_link(&act.link_id).partition;
            assert!(
                partition < num_parts,
                "Person {} starts on link {} with partition {}, but only {} partitions exist.",
                id.external(),
                act.link_id.external(),
                partition,
                num_parts
            );
            parts[partition as usize].persons.insert(id, person);
        }

        parts
    }

    pub fn take_from_filter(&mut self, filter: impl Fn(&InternalPerson) -> bool) -> Self {
        let mut persons = IntMap::default();
        let to_move: Vec<_> = self
            .persons
            .iter()
            .filter(|(_, v)| filter(v))
            .map(|(k, _)| k.clone())
            .collect();

        for key in to_move {
            if let Some(value) = self.persons.remove(&key) {
                persons.insert(key, value);
            }
        }
        Population { persons }
    }

    pub fn take_from_filtered_part(&mut self, net: &Network, part: u32) -> Self {
        let x = Self::f(net, part);
        self.take_from_filter(x)
    }

    fn f(net: &Network, part: u32) -> impl Fn(&InternalPerson) -> bool + use<'_> {
        move |p: &InternalPerson| {
            let act = p.plan_element_at(0).and_then(|p| p.as_activity()).unwrap();
            let partition = net.get_link(&act.link_id).partition;
            partition == part
        }
    }

    pub fn to_file(&self, file_path: &Path) {
        if file_path.extension().unwrap().eq("binpb") {
            write_to_proto(self, file_path);
        } else if file_path.extension().unwrap().eq("xml")
            || file_path.extension().unwrap().eq("gz")
            || file_path.extension().unwrap().eq("zst")
        {
            crate::simulation::io::xml::population::write_to_xml(self, file_path);
        } else {
            panic!(
                "file format not supported. Either use `.xml`, `.xml.gz`, `.xml.zst`, or `.binpb` as extension"
            );
        }
    }
}

#[derive(Debug, PartialEq, Clone)]
pub struct InternalActivity {
    pub act_type: Id<String>,
    pub link_id: Id<Link>,
    pub coord: Option<Coordinate>,
    pub start_time: Option<SimTime>,
    pub end_time: Option<SimTime>,
    pub max_dur: Option<Duration>,
    pub attributes: InternalAttributes,
}

#[derive(Debug, PartialEq, Clone)]
pub struct InternalLeg {
    pub mode: Id<String>,
    pub routing_mode: Option<Id<String>>,
    pub dep_time: Option<SimTime>,
    pub trav_time: Option<Duration>,
    pub route: Option<InternalRoute>,
    pub attributes: InternalAttributes,
}

#[derive(Debug, PartialEq, Clone)]
pub enum InternalRoute {
    Generic(InternalGenericRoute),
    Network(InternalNetworkRoute),
    Pt(InternalPtRoute),
}

#[derive(Debug, PartialEq, Clone)]
pub struct InternalGenericRoute {
    start_link: Id<Link>,
    end_link: Id<Link>,
    trav_time: Option<Duration>,
    distance: Option<f64>,
    vehicle: Option<Id<InternalVehicle>>,
}

#[derive(Debug, PartialEq, Clone)]
pub struct InternalNetworkRoute {
    generic_delegate: InternalGenericRoute,
    route: Vec<Id<Link>>,
}

#[derive(Debug, PartialEq, Clone)]
pub struct InternalPtRoute {
    pub generic_delegate: InternalGenericRoute,
    pub description: InternalPtRouteDescription,
}

#[derive(Debug, PartialEq, Clone)]
pub struct InternalPtRouteDescription {
    pub transit_route_id: String,
    pub boarding_time: Option<SimTime>,
    pub transit_line_id: String,
    pub access_facility_id: String,
    pub egress_facility_id: String,
}

#[derive(Debug, PartialEq, Clone)]
pub enum InternalPlanElement {
    Activity(InternalActivity),
    Leg(InternalLeg),
}

#[derive(Debug, PartialEq, Clone)]
pub struct InternalPlan {
    pub attributes: InternalAttributes,
    pub score: Option<f64>,
    pub selected: bool,
    pub elements: Vec<InternalPlanElement>,
}

#[derive(Debug, PartialEq, Clone)]
pub struct InternalPerson {
    id: Id<InternalPerson>,
    plans: Vec<InternalPlan>,
    subpopulation: Id<String>,
    attributes: InternalAttributes,
}

#[derive(Debug, PartialEq)]
pub struct InternalPopulation {
    pub persons: Vec<InternalPerson>,
}

impl InternalPerson {
    pub fn new(id: Id<InternalPerson>, plan: InternalPlan) -> Self {
        InternalPerson {
            id,
            plans: vec![plan],
            subpopulation: Id::create(DEFAULT_SUBPOPULATION),
            attributes: InternalAttributes::default(),
        }
    }

    pub fn id(&self) -> &Id<InternalPerson> {
        &self.id
    }

    pub fn plans(&self) -> &Vec<InternalPlan> {
        &self.plans
    }

    pub fn plans_mut(&mut self) -> &mut Vec<InternalPlan> {
        &mut self.plans
    }

    pub fn subpopulation(&self) -> &Id<String> {
        &self.subpopulation
    }

    pub fn plan_element_at(&self, index: usize) -> Option<&InternalPlanElement> {
        self.selected_plan().unwrap().elements.get(index)
    }

    pub fn total_elements(&self) -> usize {
        self.selected_plan().unwrap().elements.len()
    }

    pub fn selected_plan(&self) -> Option<&InternalPlan> {
        self.plans.iter().find(|&plan| plan.selected)
    }

    pub fn attributes(&self) -> &InternalAttributes {
        &self.attributes
    }

    pub fn attributes_mut(&mut self) -> &mut InternalAttributes {
        &mut self.attributes
    }

    pub fn selected_plan_mut(&mut self) -> &mut InternalPlan {
        self.plans
            .iter_mut()
            .find(|plan| plan.selected)
            .expect("No selected plan found")
    }

    pub fn add_new_plan_as_selected(&mut self, plan: InternalPlan) -> usize {
        // Deselect all existing plans
        for p in &mut self.plans {
            p.selected = false;
        }
        // Add the new plan and mark it as selected
        let mut new_plan = plan;
        new_plan.selected = true;
        self.plans.push(new_plan);
        self.plans().len() - 1
    }

    pub fn mark_plan_as_selected(&mut self, plan_index: usize) {
        if plan_index >= self.plans.len() {
            panic!(
                "Plan index {} is out of bounds for person {}",
                plan_index,
                self.id.external()
            );
        }
        for (i, p) in self.plans.iter_mut().enumerate() {
            p.selected = i == plan_index;
        }
    }
}

impl Default for InternalPlan {
    fn default() -> Self {
        Self {
            attributes: InternalAttributes::default(),
            score: None,
            selected: true,
            elements: Vec::new(),
        }
    }
}

impl InternalPlan {
    pub fn add_leg(&mut self, leg: InternalLeg) {
        self.elements.push(InternalPlanElement::Leg(leg));
    }

    pub fn add_act(&mut self, activity: InternalActivity) {
        self.elements.push(InternalPlanElement::Activity(activity));
    }

    pub fn legs(&self) -> Vec<&InternalLeg> {
        self.elements
            .iter()
            .filter_map(|e| match e {
                InternalPlanElement::Leg(leg) => Some(leg),
                _ => None,
            })
            .collect()
    }

    pub fn acts(&self) -> Vec<&InternalActivity> {
        self.elements
            .iter()
            .filter_map(|e| match e {
                InternalPlanElement::Activity(act) => Some(act),
                _ => None,
            })
            .collect()
    }

    pub fn acts_mut(&mut self) -> Vec<&mut InternalActivity> {
        self.elements
            .iter_mut()
            .filter_map(|e| match e {
                InternalPlanElement::Activity(act) => Some(act),
                _ => None,
            })
            .collect()
    }

    pub fn legs_mut(&mut self) -> Vec<&mut InternalLeg> {
        self.elements
            .iter_mut()
            .filter_map(|e| match e {
                InternalPlanElement::Leg(leg) => Some(leg),
                _ => None,
            })
            .collect()
    }
}

impl InternalActivity {
    pub fn new(
        coord: Option<Coordinate>,
        act_type: &str,
        link_id: Id<Link>,
        start_time: Option<SimTime>,
        end_time: Option<SimTime>,
        max_dur: Option<Duration>,
    ) -> Self {
        InternalActivity {
            coord,
            act_type: Id::create(act_type),
            link_id,
            start_time,
            end_time,
            max_dur,
            attributes: InternalAttributes::default(),
        }
    }

    // i think this should go into the utils module rather than being here. paul, mar'26
    pub fn cmp_end_time(&self, begin: SimTime) -> SimTime {
        if let Some(end_time) = self.end_time {
            end_time
        } else if let Some(max_dur) = self.max_dur {
            begin.saturating_add(max_dur)
        } else {
            // Bounded replacement for OptionalTime.undefined(): even the maximum representable
            SimTime::from_nanos(u64::MAX)
        }
    }

    // i think this should go into the utils module rather than being here. paul, mar'26
    pub fn is_interaction(&self) -> bool {
        self.act_type.external().contains("interaction")
    }
}

impl FromStr for InternalPtRouteDescription {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let desc: Value = serde_json::from_str(s)?;

        Ok(InternalPtRouteDescription {
            transit_route_id: trim_quotes(&desc["transitRouteId"]),
            boarding_time: desc["boardingTime"].as_str().and_then(parse_time),
            transit_line_id: trim_quotes(&desc["transitLineId"]),
            access_facility_id: trim_quotes(&desc["accessFacilityId"]),
            egress_facility_id: trim_quotes(&desc["egressFacilityId"]),
        })
    }
}

impl InternalRoute {
    pub fn as_generic(&self) -> &InternalGenericRoute {
        match self {
            InternalRoute::Generic(g) => g,
            InternalRoute::Network(n) => &n.generic_delegate,
            InternalRoute::Pt(p) => &p.generic_delegate,
        }
    }

    pub fn as_generic_mut(&mut self) -> &mut InternalGenericRoute {
        match self {
            InternalRoute::Generic(g) => g,
            InternalRoute::Network(n) => &mut n.generic_delegate,
            InternalRoute::Pt(p) => &mut p.generic_delegate,
        }
    }

    pub fn as_network(&self) -> Option<&InternalNetworkRoute> {
        match self {
            InternalRoute::Network(n) => Some(n),
            _ => None,
        }
    }

    pub fn as_pt(&self) -> Option<&InternalPtRoute> {
        match self {
            InternalRoute::Pt(p) => Some(p),
            _ => None,
        }
    }

    pub fn start_link(&self) -> &Id<Link> {
        &self.as_generic().start_link
    }

    pub fn end_link(&self) -> &Id<Link> {
        &self.as_generic().end_link
    }

    fn from_io(io: IORoute, id: Id<InternalPerson>, mode: Id<String>) -> Self {
        let external = match io.vehicle {
            Some(v) if v != "null" && !v.is_empty() => v,
            _ => format!("{}_{}", id.external(), mode.external()),
        };
        let generic = InternalGenericRoute::new(
            Id::create(io.start_link.expect("Route must have start link").as_str()),
            Id::create(io.end_link.expect("Route must have end link").as_str()),
            parse_duration_opt(&io.trav_time),
            Option::from(io.distance),
            Some(Id::create(&external)),
        );

        match io.r#type.expect("Route must have type").as_str() {
            "generic" => InternalRoute::Generic(generic),
            "default_pt" => {
                let description = io
                    .route
                    .and_then(|s| InternalPtRouteDescription::from_str(&s).ok())
                    .expect("Failed to parse PT route description");
                InternalRoute::Pt(InternalPtRoute {
                    generic_delegate: generic,
                    description,
                })
            }
            "links" => {
                let route = io
                    .route
                    .unwrap_or_default()
                    .split_whitespace()
                    .map(Id::create)
                    .collect();
                InternalRoute::Network(InternalNetworkRoute {
                    generic_delegate: generic,
                    route,
                })
            }
            other_unknown_str => panic!("Unknown route type: {}", other_unknown_str),
        }
    }

    pub(crate) fn get_route_description(self) -> Option<String> {
        match self {
            InternalRoute::Generic(_) => None,
            InternalRoute::Network(nr) => {
                // if route starts and ends with same link, and has length 2 (only start and end
                // node contained => not a true round trip), remove the end link from the route
                // before writing
                // Note: When such routes have been read from file, the InternalRoute will likely
                // already only contain one link in its "route" vector, since this is how the route
                // is written in the files. But since we only remove the last link before writing in
                // the case length == 2 (not length < 2), this should not cause any issues here.
                let route = if nr.route().first() == nr.route().last() && nr.route().len() == 2 {
                    let mut route = nr.route().clone();
                    route.pop();
                    route
                } else {
                    nr.route().clone()
                };

                Some(
                    route
                        .iter()
                        .map(|id| id.external().to_string())
                        .collect::<Vec<String>>()
                        .join(" "),
                )
            }
            InternalRoute::Pt(ptr) => Some(
                serde_json::to_string(&IOPTRouteDescription {
                    transit_route_id: ptr.description.transit_route_id,
                    boarding_time: ptr
                        .description
                        .boarding_time
                        .map(|t| t.format_hh_mm_ss_trimmed())
                        .unwrap_or_else(|| "undefined".to_string()),
                    transit_line_id: ptr.description.transit_line_id,
                    access_facility_id: ptr.description.access_facility_id,
                    egress_facility_id: ptr.description.egress_facility_id,
                })
                .unwrap(),
            ),
        }
    }
}

impl From<Route> for InternalRoute {
    fn from(route: Route) -> Self {
        match route {
            Route::GenericRoute(g) => InternalRoute::Generic(g.into()),
            Route::NetworkRoute(n) => InternalRoute::Network(InternalNetworkRoute {
                generic_delegate: n.delegate.unwrap().into(),
                route: n
                    .route
                    .into_iter()
                    .map(|id| Id::get_from_ext(&id))
                    .collect(),
            }),
            Route::PtRoute(p) => InternalRoute::Pt(InternalPtRoute {
                generic_delegate: p.delegate.unwrap().into(),
                description: p.information.unwrap().into(),
            }),
        }
    }
}

impl From<GenericRoute> for InternalGenericRoute {
    fn from(g: GenericRoute) -> Self {
        InternalGenericRoute {
            start_link: Id::get_from_ext(&g.start_link),
            end_link: Id::get_from_ext(&g.end_link),
            trav_time: g.trav_time_ns.map(Duration::from_nanos),
            distance: g.distance,
            vehicle: g.veh_id.map(|s| Id::get_from_ext(&s)),
        }
    }
}

impl From<PtRouteDescription> for InternalPtRouteDescription {
    fn from(value: PtRouteDescription) -> Self {
        InternalPtRouteDescription {
            transit_route_id: value.transit_route_id,
            boarding_time: value.boarding_time_ns.map(SimTime::from_nanos),
            transit_line_id: value.transit_line_id,
            access_facility_id: value.access_facility_id,
            egress_facility_id: value.egress_facility_id,
        }
    }
}

impl InternalGenericRoute {
    pub fn new(
        start_link: Id<Link>,
        end_link: Id<Link>,
        trav_time: Option<Duration>,
        distance: Option<f64>,
        vehicle: Option<Id<InternalVehicle>>,
    ) -> Self {
        Self {
            start_link,
            end_link,
            trav_time,
            distance,
            vehicle,
        }
    }

    pub fn end_link(&self) -> &Id<Link> {
        &self.end_link
    }

    pub fn start_link(&self) -> &Id<Link> {
        &self.start_link
    }

    pub fn vehicle(&self) -> &Option<Id<InternalVehicle>> {
        &self.vehicle
    }

    pub fn trav_time(&self) -> Option<Duration> {
        self.trav_time
    }

    pub fn set_trav_time(&mut self, trav_time: Option<Duration>) {
        self.trav_time = trav_time;
    }

    pub fn distance(&self) -> Option<f64> {
        self.distance
    }

    pub fn set_distance(&mut self, distance: Option<f64>) {
        self.distance = distance;
    }
}

impl InternalNetworkRoute {
    pub fn route_element_at(&self, index: usize) -> Option<&Id<Link>> {
        self.route.get(index)
    }

    pub fn new(generic_delegate: InternalGenericRoute, route: Vec<Id<Link>>) -> Self {
        Self {
            generic_delegate,
            route,
        }
    }

    pub fn generic_delegate(&self) -> &InternalGenericRoute {
        &self.generic_delegate
    }

    pub fn route(&self) -> &Vec<Id<Link>> {
        &self.route
    }
}

impl InternalPtRoute {
    pub fn generic_delegate(&self) -> &InternalGenericRoute {
        &self.generic_delegate
    }

    pub fn description(&self) -> &InternalPtRouteDescription {
        &self.description
    }

    pub fn start_link(&self) -> &Id<Link> {
        &self.generic_delegate.start_link
    }

    pub fn end_link(&self) -> &Id<Link> {
        &self.generic_delegate.end_link
    }
}

impl InternalLeg {
    pub fn new(
        route: InternalRoute,
        mode: &str,
        routing_mode: &str,
        trav_time: Duration,
        dep_time: Option<SimTime>,
    ) -> Self {
        Self {
            route: Some(route),
            mode: Id::create(mode),
            routing_mode: Some(Id::create(routing_mode)),
            trav_time: Some(trav_time),
            dep_time,
            attributes: InternalAttributes::default(),
        }
    }

    pub fn travel_time(&self) -> Duration {
        if let Some(leg_trav_time) = self.trav_time {
            leg_trav_time
        } else {
            self.route
                .as_ref()
                .unwrap()
                .as_generic()
                .trav_time
                .unwrap_or_else(|| {
                    panic!("Neither leg nor route travel time is set for this leg. The mode of this leg is: {}. \
                    If this is a teleported mode, the travel time must be present. Leg: {:?}", self.mode, self);
                })
        }
    }
}

impl FromIOPerson<IOLeg> for InternalLeg {
    fn from_io(io: IOLeg, id: Id<InternalPerson>) -> Self {
        let routing_mode = io
            .attributes
            .as_ref()
            .and_then(|a| a.find("routingMode"))
            .map(Id::<String>::create);

        let mode = Id::create(&io.mode);
        InternalLeg {
            mode: mode.clone(),
            routing_mode,
            dep_time: parse_time_opt(&io.dep_time),
            trav_time: parse_trav_time(
                &io.trav_time,
                &io.route.as_ref().and_then(|r| r.trav_time.clone()),
            ),
            route: io.route.map(|r| InternalRoute::from_io(r, id, mode)),
            attributes: io
                .attributes
                .map(InternalAttributes::from)
                .unwrap_or_default(),
        }
    }
}

impl From<Leg> for InternalLeg {
    fn from(io: Leg) -> Self {
        InternalLeg {
            mode: Id::get_from_ext(&io.mode),
            routing_mode: io.routing_mode.map(|s| Id::get_from_ext(&s)),
            dep_time: io.dep_time_ns.map(SimTime::from_nanos),
            trav_time: io.trav_time_ns.map(Duration::from_nanos),
            route: io.route.map(InternalRoute::from),
            attributes: InternalAttributes::from(&io.attributes),
        }
    }
}

impl From<IOActivity> for InternalActivity {
    fn from(io: IOActivity) -> Self {
        InternalActivity {
            act_type: Id::create(&io.r#type),
            link_id: Id::create(&io.link.expect("Activity must have a link id")),
            coord: io.x.map(|x| {
                Coordinate::new_2d(
                    x,
                    io.y.expect("y coordinate should be given when x coord is given"),
                )
            }),
            start_time: parse_time_opt(&io.start_time),
            end_time: parse_time_opt(&io.end_time),
            max_dur: parse_duration_opt(&io.max_dur),
            attributes: io
                .attributes
                .map(InternalAttributes::from)
                .unwrap_or_default(),
        }
    }
}

impl From<Activity> for InternalActivity {
    fn from(value: Activity) -> Self {
        InternalActivity {
            act_type: Id::get_from_ext(&value.act_type),
            link_id: Id::get_from_ext(&value.link_id),
            coord: Some(Coordinate::new_3d(
                value.coordinate.as_ref().unwrap().x,
                value.coordinate.as_ref().unwrap().y,
                value.coordinate.as_ref().unwrap().z,
            )),
            start_time: value.start_time_ns.map(SimTime::from_nanos),
            end_time: value.end_time_ns.map(SimTime::from_nanos),
            max_dur: value.max_dur_ns.map(Duration::from_nanos),
            attributes: InternalAttributes::from(&value.attributes),
        }
    }
}

fn trim_quotes(s: &Value) -> String {
    s.to_string().trim_matches('"').to_string()
}

fn parse_trav_time(
    leg_trav_time: &Option<String>,
    route_trav_time: &Option<String>,
) -> Option<Duration> {
    if let Some(trav_time) = parse_duration_opt(leg_trav_time) {
        Some(trav_time)
    } else {
        parse_duration_opt(route_trav_time)
    }
}

fn parse_time_opt(value: &Option<String>) -> Option<SimTime> {
    if let Some(time) = value.as_ref() {
        parse_time(time)
    } else {
        None
    }
}

fn parse_duration_opt(value: &Option<String>) -> Option<Duration> {
    if let Some(time) = value.as_ref() {
        parse_duration(time)
    } else {
        None
    }
}

fn parse_time(value: &str) -> Option<SimTime> {
    if value == "undefined" {
        None
    } else {
        SimTime::parse(value).ok()
    }
}

fn parse_duration(value: &str) -> Option<Duration> {
    parse_time(value).map(SimTime::as_duration)
}

impl From<IOPerson> for InternalPerson {
    fn from(io: IOPerson) -> Self {
        let id = Id::create(&io.id);
        let attributes = io
            .attributes
            .map(InternalAttributes::from)
            .unwrap_or_default();
        let subpopulation = attributes
            .get::<String>(SUBPOPULATION)
            .unwrap_or_else(|| DEFAULT_SUBPOPULATION.to_string());
        InternalPerson {
            id: id.clone(),
            plans: io
                .plans
                .into_iter()
                .map(|p| InternalPlan::from_io(p, id.clone()))
                .collect(),
            subpopulation: Id::create(&subpopulation),
            attributes,
        }
    }
}

impl From<Person> for InternalPerson {
    fn from(value: Person) -> Self {
        let id: Id<InternalPerson> = Id::get_from_ext(&value.id);
        let subpopulation = value
            .subpopulation
            .unwrap_or_else(|| DEFAULT_SUBPOPULATION.to_string());
        InternalPerson {
            id: id.clone(),
            plans: value.plan.into_iter().map(InternalPlan::from).collect(),
            subpopulation: Id::create(&subpopulation),
            attributes: InternalAttributes::from(&value.attributes),
        }
    }
}

impl FromIOPerson<IOPlanElement> for InternalPlanElement {
    fn from_io(io: IOPlanElement, id: Id<InternalPerson>) -> Self {
        match io {
            IOPlanElement::Activity(act) => {
                InternalPlanElement::Activity(InternalActivity::from(act))
            }
            IOPlanElement::Leg(leg) => InternalPlanElement::Leg(InternalLeg::from_io(leg, id)),
        }
    }
}

impl InternalPlanElement {
    pub fn as_activity(&self) -> Option<&InternalActivity> {
        if let InternalPlanElement::Activity(act) = self {
            Some(act)
        } else {
            None
        }
    }

    pub fn as_activity_mut(&mut self) -> Option<&mut InternalActivity> {
        if let InternalPlanElement::Activity(act) = self {
            Some(act)
        } else {
            None
        }
    }

    pub fn as_leg(&self) -> Option<&InternalLeg> {
        if let InternalPlanElement::Leg(leg) = self {
            Some(leg)
        } else {
            None
        }
    }

    pub fn as_leg_mut(&mut self) -> Option<&mut InternalLeg> {
        if let InternalPlanElement::Leg(leg) = self {
            Some(leg)
        } else {
            None
        }
    }
}

impl FromIOPerson<IOPlan> for InternalPlan {
    fn from_io(io: IOPlan, id: Id<InternalPerson>) -> Self {
        InternalPlan {
            attributes: io
                .attributes
                .map(InternalAttributes::from)
                .unwrap_or_default(),
            score: io.score,
            selected: io.selected,
            elements: io
                .elements
                .into_iter()
                .map(|p| InternalPlanElement::from_io(p, id.clone()))
                .collect(),
        }
    }
}

impl From<Plan> for InternalPlan {
    fn from(io: Plan) -> Self {
        let acts = io
            .acts
            .into_iter()
            .map(InternalActivity::from)
            .collect::<Vec<_>>();
        let legs = io
            .legs
            .into_iter()
            .map(InternalLeg::from)
            .collect::<Vec<_>>();

        let mut elements = Vec::new();
        for pair in acts.into_iter().zip_longest(legs.into_iter()) {
            match pair {
                EitherOrBoth::Both(a, l) => {
                    elements.push(InternalPlanElement::Activity(a));
                    elements.push(InternalPlanElement::Leg(l));
                }
                EitherOrBoth::Left(a) => {
                    elements.push(InternalPlanElement::Activity(a));
                }
                EitherOrBoth::Right(l) => {
                    panic!("Plan ends with a leg {:?}. That is not allowed.", l);
                }
            }
        }

        InternalPlan {
            attributes: InternalAttributes::from(&io.attributes),
            score: io.score,
            selected: io.selected,
            elements,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::simulation::config::{MetisOptions, PartitionMethod};
    use crate::simulation::id::Id;
    use crate::simulation::io::xml::attributes::{IOAttribute, IOAttributes};
    use crate::simulation::io::xml::population::{IOLeg, IOPerson, IOPlan, IORoute};
    use crate::simulation::scenario::Coordinate;
    use crate::simulation::scenario::network::{Link, Network};
    use crate::simulation::scenario::population::{
        FromIOPerson, InternalActivity, InternalLeg, InternalPerson, InternalRoute, Population,
    };
    use crate::simulation::scenario::vehicles::Garage;
    use crate::simulation::scenario::vehicles::InternalVehicle;
    use crate::simulation::time::SimTime;
    use macros::deterministic_id_test;
    use std::collections::HashSet;
    use std::path::PathBuf;
    use std::time::Duration;

    #[deterministic_id_test]
    fn cmp_end_time_uses_bounded_open_ended_sentinel() {
        let activity = InternalActivity::new(
            Some(Coordinate::new_2d(0.0, 0.0)),
            "home",
            Id::create("1"),
            None,
            None,
            None,
        );

        assert_eq!(
            SimTime::from_nanos(u64::MAX),
            activity.cmp_end_time(SimTime::default())
        );
    }

    #[deterministic_id_test]
    fn person_from_xml_uses_subpopulation_attribute() {
        let person = InternalPerson::from(IOPerson {
            attributes: Some(IOAttributes {
                attributes: vec![IOAttribute::new_with_class(
                    "subpopulation".to_string(),
                    "java.lang.String".to_string(),
                    "freight".to_string(),
                )],
            }),
            id: "1".to_string(),
            plans: vec![IOPlan {
                attributes: None,
                selected: true,
                score: None,
                elements: Vec::new(),
            }],
        });

        assert_eq!("freight", person.subpopulation().external());
    }

    #[deterministic_id_test]
    fn person_from_xml_defaults_subpopulation_to_person() {
        let person = InternalPerson::from(IOPerson {
            attributes: None,
            id: "1".to_string(),
            plans: vec![IOPlan {
                attributes: None,
                selected: true,
                score: None,
                elements: Vec::new(),
            }],
        });

        assert_eq!("person", person.subpopulation().external());
    }

    #[deterministic_id_test]
    fn network_route_ignores_xml_text_whitespace() {
        let route = InternalRoute::from_io(
            IORoute {
                r#type: Some("links".to_string()),
                start_link: Some("1".to_string()),
                end_link: Some("20".to_string()),
                trav_time: None,
                distance: None,
                vehicle: Some("1_car".to_string()),
                route: Some("1 6 15 20\n                ".to_string()),
            },
            Id::create("1"),
            Id::create("car"),
        );

        assert_eq!(
            vec![
                Id::<Link>::get_from_ext("1"),
                Id::<Link>::get_from_ext("6"),
                Id::<Link>::get_from_ext("15"),
                Id::<Link>::get_from_ext("20"),
            ],
            route.as_network().unwrap().route
        );
    }

    #[deterministic_id_test]
    fn from_io_1_plan() {
        let _net = Network::from_file_as_is(&PathBuf::from("./assets/equil/equil-network.xml"));
        let mut garage = Garage::from_file(&PathBuf::from("./assets/equil/equil-vehicles.xml"));
        let pop = Population::from_file(
            &PathBuf::from("./assets/equil/equil-1-plan.xml"),
            &mut garage,
        );

        assert_eq!(1, pop.persons.len());

        let agent = pop.persons.get(&Id::get_from_ext("1")).unwrap();
        assert!(agent.selected_plan().is_some());

        let plan = agent.selected_plan().unwrap();
        assert_eq!(4, plan.acts().len());
        assert_eq!(3, plan.legs().len());

        let binding = plan.acts();
        let home_act = binding.first().unwrap();
        assert_eq!("h", home_act.act_type.external());
        assert_eq!(Id::<Link>::get_from_ext("1"), home_act.link_id);
        assert_eq!(-25000., home_act.coord.as_ref().unwrap().x);
        assert_eq!(0., home_act.coord.as_ref().unwrap().y);
        assert_eq!(Some(SimTime::from_secs(6 * 3600)), home_act.end_time);
        assert_eq!(None, home_act.start_time);
        assert_eq!(None, home_act.max_dur);

        let binding = plan.legs();
        let leg = binding.first().unwrap();
        assert_eq!(None, leg.dep_time);
        assert!(leg.route.is_some());
        let net_route = leg.route.as_ref().unwrap().as_network().unwrap();
        assert_eq!(
            Some(Id::<InternalVehicle>::get_from_ext("1_car")),
            net_route.generic_delegate.vehicle
        );
        assert_eq!(
            vec![
                Id::<Link>::get_from_ext("1"),
                Id::<Link>::get_from_ext("6"),
                Id::<Link>::get_from_ext("15"),
                Id::<Link>::get_from_ext("20"),
            ],
            net_route.route
        );
    }

    #[deterministic_id_test]
    fn from_io_multi_mode() {
        let _net = Network::from_file_as_is(&PathBuf::from("./assets/3-links/3-links-network.xml"));
        let mut garage = Garage::from_file(&PathBuf::from("./assets/3-links/vehicles.xml"));
        let pop =
            Population::from_file(&PathBuf::from("./assets/3-links/3-agent.xml"), &mut garage);

        // check that we have all three vehicle types
        let expected_veh_types = HashSet::from(["car", "bike", "walk"]);
        assert_eq!(3, garage.vehicle_types.len());
        assert!(
            garage
                .vehicle_types
                .keys()
                .all(|type_id| expected_veh_types.contains(type_id.external()))
        );

        // check that we have a vehicle for each mode and for each person
        assert_eq!(9, garage.vehicles.len());

        // check population
        // activity types should be done as id. If id is not present this will crash
        assert_eq!("home", Id::<String>::get_from_ext("home").external());
        assert_eq!("errands", Id::<String>::get_from_ext("errands").external());

        // each of the network mode should also have an interaction activity type
        assert_eq!(
            "car interaction",
            Id::<String>::get_from_ext("car interaction").external()
        );
        assert_eq!(
            "bike interaction",
            Id::<String>::get_from_ext("bike interaction").external()
        );

        // agents should also have ids
        assert_eq!("100", Id::<InternalPerson>::get_from_ext("100").external());
        assert_eq!("200", Id::<InternalPerson>::get_from_ext("200").external());
        assert_eq!("300", Id::<InternalPerson>::get_from_ext("300").external());

        // we expect three agents overall
        assert_eq!(3, pop.persons.len());

        // todo test bookkeeping of garage person_2_vehicle
    }

    #[deterministic_id_test]
    fn from_io() {
        let net = Network::from_file(
            "./assets/equil/equil-network.xml",
            2,
            &PartitionMethod::Metis(MetisOptions::default()),
        );
        let mut garage = Garage::from_file(&PathBuf::from("./assets/equil/equil-vehicles.xml"));
        let pop1 = Population::from_file_filtered_part(
            &PathBuf::from("./assets/equil/equil-plans.xml.gz"),
            &net,
            &mut garage,
            0,
        );
        let pop2 = Population::from_file_filtered_part(
            &PathBuf::from("./assets/equil/equil-plans.xml.gz"),
            &net,
            &mut garage,
            1,
        );

        // metis produces unstable results on small networks so, make sure that one of the populations
        // has all the agents and the other doesn't
        assert!(pop1.persons.len() == 100 || pop2.persons.len() == 100);
        assert!(pop1.persons.is_empty() || pop2.persons.is_empty());
    }

    #[deterministic_id_test]
    fn test_from_xml_to_binpb_same() {
        let net = Network::from_file(
            "./assets/equil/equil-network.xml",
            1,
            &PartitionMethod::Metis(MetisOptions::default()),
        );
        let mut garage = Garage::from_file(&PathBuf::from("./assets/equil/equil-vehicles.xml"));
        let population = Population::from_file(
            &PathBuf::from("./assets/equil/equil-plans.xml.gz"),
            &mut garage,
        );

        let temp_file = PathBuf::from(
            "test_output/simulation/population/population/test_from_xml_to_binpb_same/plans.binpb",
        );
        population.to_file(&temp_file);
        let population2 = Population::from_file_filtered_part(&temp_file, &net, &mut garage, 0);
        assert_eq!(population, population2);
    }

    #[deterministic_id_test]
    fn test_from_io_generic_route() {
        Id::<Link>::create("1");
        Id::<Link>::create("2");
        Id::<InternalVehicle>::create("person_car");

        let io_leg = IOLeg {
            mode: "car".to_string(),
            dep_time: Some("12:00:00".to_string()),
            trav_time: Some("00:30:00".to_string()),
            route: Some(IORoute {
                r#type: Some("generic".to_string()),
                start_link: Some("1".to_string()),
                end_link: Some("2".to_string()),
                trav_time: Some("00:20:00".to_string()),
                distance: Some(42.0),
                vehicle: None,
                route: None,
            }),
            attributes: None,
        };

        let leg = InternalLeg::from_io(io_leg, Id::create("person"));

        assert_eq!(leg.mode.external(), "car");
        assert_eq!(leg.trav_time, Some(Duration::from_secs(1800)));
        assert_eq!(leg.dep_time, Some(SimTime::from_secs(43200)));
        assert_eq!(leg.routing_mode, None);
        let route = leg.route.as_ref().unwrap();
        assert!(matches!(route, InternalRoute::Generic(_)));
        assert_eq!(route.as_generic().start_link.external(), "1");
        assert_eq!(route.as_generic().end_link.external(), "2");
        assert_eq!(
            route.as_generic().trav_time,
            Some(Duration::from_secs(1200))
        );
        assert_eq!(route.as_generic().distance, Some(42.0));
        assert_eq!(
            route.as_generic().vehicle.as_ref().unwrap().external(),
            "person_car"
        );
    }

    #[deterministic_id_test]
    fn test_from_io_pt_route() {
        Id::<Link>::create("1");
        Id::<Link>::create("2");
        Id::<String>::create("pt");
        Id::<InternalVehicle>::create("person_pt");

        let io_leg = IOLeg {
            mode: "pt".to_string(),
            dep_time: None,
            trav_time: Some("00:30:00".to_string()),
            route: Some(IORoute {
                r#type: Some("default_pt".to_string()),
                start_link: Some("1".to_string()),
                end_link: Some("2".to_string()),
                trav_time: Some("00:20:00".to_string()),
                distance: Some(f64::NAN),
                vehicle: None,
                route: Some(String::from(
                    "{\"transitRouteId\":\"3to1\",\"boardingTime\":\"undefined\",\"transitLineId\":\"Blue Line\",\"accessFacilityId\":\"3\",\"egressFacilityId\":\"1\"}",
                )),
            }),
            attributes: None,
        };

        let leg = InternalLeg::from_io(io_leg, Id::create("person"));

        print!("{:?}", leg);
    }
}
