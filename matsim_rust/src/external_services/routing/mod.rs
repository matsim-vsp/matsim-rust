use crate::external_services::{RequestAdapter, RequestAdapterFactory, RequestToAdapter};
use crate::generated::routing::routing_service_client::RoutingServiceClient;
use crate::generated::routing::{Request, Response};
use crate::simulation::config::Config;
use crate::simulation::data_structures::RingIter;
use crate::simulation::scenario::Coordinate;
use crate::simulation::scenario::population::{InternalActivity, InternalLeg, InternalPlanElement};
use crate::simulation::time::SimTime;
use derive_builder::Builder;
use itertools::{EitherOrBoth, Itertools};
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot::Sender;
use tokio::task::JoinHandle;
use tracing::info;
use uuid::Uuid;

pub struct RoutingServiceAdapter {
    clients: RingIter<RoutingServiceClient<tonic::transport::Channel>>,
    shutdown_handles: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
}

#[derive(Debug)]
pub struct InternalRoutingRequest {
    pub payload: InternalRoutingRequestPayload,
    pub response_tx: Sender<InternalRoutingResponse>,
}

impl RequestToAdapter for InternalRoutingRequest {}

#[derive(Debug, PartialEq, Builder)]
pub struct InternalRoutingRequestPayload {
    pub person_id: String,
    pub from_link: String,
    pub from: Coordinate,
    pub to_link: String,
    pub to: Coordinate,
    pub mode: String,
    pub departure_time: SimTime,
    pub now: SimTime,
    #[builder(default = "Uuid::now_v7()")]
    pub uuid: Uuid,
}

impl InternalRoutingRequestPayload {
    pub fn equals_ignoring_uuid(&self, other: &Self) -> bool {
        self.person_id == other.person_id
            && self.from_link == other.from_link
            && self.from == other.from
            && self.to_link == other.to_link
            && self.to == other.to
            && self.mode == other.mode
            && self.departure_time == other.departure_time
            && self.now == other.now
    }
}

#[derive(Debug, Clone, Default)]
pub struct InternalRoutingResponse {
    pub(crate) elements: Vec<InternalPlanElement>,
    pub(crate) request_id: Uuid,
}

impl From<InternalRoutingRequestPayload> for Request {
    fn from(req: InternalRoutingRequestPayload) -> Self {
        Request {
            person_id: req.person_id,
            from_link_id: req.from_link,
            from: Some(req.from.into()),
            to_link_id: req.to_link,
            to: Some(req.to.into()),
            mode: req.mode,
            departure_time_ns: req.departure_time.as_nanos(),
            now_ns: req.now.as_nanos(),
            request_id: req.uuid.as_bytes().to_vec(),
        }
    }
}

impl From<Response> for InternalRoutingResponse {
    fn from(value: Response) -> Self {
        //zip legs and activities
        let legs = value
            .legs
            .into_iter()
            .map(InternalLeg::from)
            .collect::<Vec<_>>();
        let activities = value
            .activities
            .into_iter()
            .map(InternalActivity::from)
            .collect::<Vec<_>>();

        let mut elements = Vec::new();
        for pair in legs.into_iter().zip_longest(activities.into_iter()) {
            match pair {
                EitherOrBoth::Both(l, a) => {
                    elements.push(InternalPlanElement::Leg(l));
                    elements.push(InternalPlanElement::Activity(a));
                }
                EitherOrBoth::Left(l) => {
                    elements.push(InternalPlanElement::Leg(l));
                }
                EitherOrBoth::Right(_) => {
                    panic!("Received routing response ends with an activity, but expected a leg.");
                }
            }
        }

        Self {
            elements,
            request_id: Uuid::from_bytes(value.request_id.try_into().unwrap()),
        }
    }
}

impl From<Coordinate> for crate::generated::general::Coordinate {
    fn from(c: Coordinate) -> Self {
        crate::generated::general::Coordinate {
            x: c.x,
            y: c.y,
            z: c.z,
        }
    }
}

impl From<crate::generated::general::Coordinate> for Coordinate {
    fn from(c: crate::generated::general::Coordinate) -> Self {
        Coordinate {
            x: c.x,
            y: c.y,
            z: c.z,
        }
    }
}

/// Factory for creating routing service adapters. Connects to the routing service at the given IP address.
pub struct RoutingServiceAdapterFactory {
    ip: Vec<String>,
    config: Arc<Config>,
    shutdown_handles: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl RoutingServiceAdapterFactory {
    pub fn new(
        ip: Vec<impl Into<String>>,
        config: Arc<Config>,
        shutdown_handles: Arc<Mutex<Vec<JoinHandle<()>>>>,
    ) -> Self {
        Self {
            ip: ip.into_iter().map(|s| s.into()).collect(),
            config,
            shutdown_handles,
        }
    }
}

impl RequestAdapterFactory<InternalRoutingRequest> for RoutingServiceAdapterFactory {
    async fn build(self) -> impl RequestAdapter<InternalRoutingRequest> {
        let mut res = Vec::new();
        for ip in self.ip {
            info!("Connecting to routing service at {}", ip);
            let start = std::time::Instant::now();
            let client;
            loop {
                match RoutingServiceClient::connect(ip.clone()).await {
                    Ok(c) => {
                        client = c;
                        break;
                    }
                    Err(e) => {
                        if start.elapsed().as_secs()
                            >= self.config.computational_setup().retry_time_seconds
                        {
                            panic!(
                                "Failed to connect to routing service at {} after configured retry maximum: {}",
                                ip, e
                            );
                        }
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    }
                }
            }
            res.push(client);
        }
        RoutingServiceAdapter::new(res, self.shutdown_handles)
    }
}

impl RequestAdapter<InternalRoutingRequest> for RoutingServiceAdapter {
    fn on_request(&mut self, internal_req: InternalRoutingRequest) {
        let mut client = self.clients.next_cloned();

        tokio::spawn(async move {
            let request = Request::from(internal_req.payload);

            let response = client
                .get_route(request)
                .await
                .expect("Error while calling routing service");

            let internal_res = InternalRoutingResponse::from(response.into_inner());

            let _ = internal_req.response_tx.send(internal_res);
        });
    }

    fn on_shutdown(&mut self) {
        for client in &mut self.clients {
            let mut c = client.clone();
            let handle = tokio::spawn(async move {
                c.shutdown(())
                    .await
                    .expect("Error while shutting down routing service");
            });
            self.shutdown_handles.lock().unwrap().push(handle);
        }
    }
}

impl RoutingServiceAdapter {
    fn new(
        clients: Vec<RoutingServiceClient<tonic::transport::Channel>>,
        shutdown_handles: Arc<Mutex<Vec<JoinHandle<()>>>>,
    ) -> Self {
        Self {
            clients: RingIter::new(clients),
            shutdown_handles,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::InternalRoutingRequestPayloadBuilder;
    use crate::generated::routing::Request;
    use crate::simulation::scenario::Coordinate;
    use crate::simulation::time::SimTime;

    #[test]
    fn request_conversion_uses_nanoseconds() {
        let request = Request::from(
            InternalRoutingRequestPayloadBuilder::default()
                .person_id("person-1".to_string())
                .from_link("from-link".to_string())
                .from(Coordinate::new_2d(1.0, 2.0))
                .to_link("to-link".to_string())
                .to(Coordinate::new_2d(3.0, 4.0))
                .mode("car".to_string())
                .departure_time(SimTime::from_nanos(1_500_000))
                .now(SimTime::from_nanos(2_750_000))
                .build()
                .unwrap(),
        );

        assert_eq!(1_500_000, request.departure_time_ns);
        assert_eq!(2_750_000, request.now_ns);
    }
}
