use crate::simulation::config::Config;
use crate::simulation::logging::init_std_out_logging_thread_local;
use derive_builder::Builder;
use futures::future::join_all;
use std::fmt::Debug;
use std::sync::{Arc, Barrier, Mutex};
use std::thread;
use std::thread::JoinHandle;
use tokio::sync::mpsc;
use tokio::sync::mpsc::{Receiver, Sender};
use tracing::{info, warn};

pub mod routing;

/// This trait is a marker trait for requests that can be sent to an adapter.
pub trait RequestToAdapter: Debug + Send {}

/// This struct is a wrapper around the JoinHandle of the adapter thread. Additionally, it holds a shutdown sender for the adapter.
/// The purpose of this struct is to manage the lifecycle of the adapter thread, allowing for sending shutdown signals before waiting for the thread to finish.
#[derive(Debug, Builder)]
#[builder(pattern = "owned")]
pub struct AdapterHandle {
    pub(super) handle: JoinHandle<()>,
    pub(super) shutdown_sender: tokio::sync::watch::Sender<bool>,
}

/// This enum defines the types of external services that can be used in the simulation.
/// It works as a key for different service adapters in the simulation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ExternalServiceType {
    Routing(String),
}

/// This trait defines a factory for creating request adapters.
pub trait RequestAdapterFactory<T: RequestToAdapter>: Send {
    /// This method builds the request adapter. It returns a future that resolves to the adapter instance.
    fn build(self) -> impl std::future::Future<Output = impl RequestAdapter<T>>;

    /// This method creates a channel for sending requests to the adapter.
    fn request_channel(&self, buffer: usize) -> (Sender<T>, Receiver<T>) {
        mpsc::channel(buffer)
    }
}

/// This trait defines the behavior of a request adapter. A request adapter processes incoming requests of type T.
/// One request adapter instance is run in a separate thread with its own tokio runtime. It might use multiple threads internally for the tokio runtime.
///
/// Design thoughts:
/// - This trait does not depend on async/await directly to allow for more flexibility in implementations.
///   I.e. it should allow sync implementations which should not have dependencies on async concepts.
/// - The on_request/on_shutdown method can spawn async tasks internally if needed.
pub trait RequestAdapter<T: RequestToAdapter> {
    fn on_request(&mut self, req: T);

    // This doesn't have the return value Vec<tokio::task::JoinHandle<()>> on purpose. See above design thoughts.
    fn on_shutdown(&mut self) {
        info!("Adapter is shutting down");
    }
}

#[derive(Debug)]
pub struct AsyncExecutor {
    worker_threads: u32,
    barrier: Arc<Barrier>,
    // This field holds shutdown handles and will be filled if shutdown is called.
    shutdown_handles: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
}

impl AsyncExecutor {
    /// Spawns a thread running a routing service adapter.
    pub fn spawn_thread<R: RequestToAdapter + 'static, F: RequestAdapterFactory<R> + 'static>(
        self,
        name: &str,
        request_adapter_factory: F,
    ) -> (JoinHandle<()>, Sender<R>, tokio::sync::watch::Sender<bool>) {
        let (send, recv) = request_adapter_factory.request_channel(10000);
        let (send_sd, recv_sd) = self.shutdown_channel();

        let handle = thread::Builder::new()
            .name(name.into())
            .spawn(move || self.execute_adapter(recv, request_adapter_factory, recv_sd))
            .unwrap();

        (handle, send, send_sd)
    }

    /// This function executes the adapter in a separate thread with its own tokio runtime.
    fn execute_adapter<T: RequestToAdapter>(
        self,
        mut receiver: Receiver<T>,
        req_adapter_factory: impl RequestAdapterFactory<T>,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) {
        let _guard = init_std_out_logging_thread_local();
        info!("Starting adapter");

        let mut builder = if self.worker_threads > 0 {
            let mut b = tokio::runtime::Builder::new_multi_thread();
            b.worker_threads(self.worker_threads as usize);
            b
        } else {
            warn!(
                "Starting adapter with current_thread runtime, this might lead to performance drops. Use carefully."
            );
            tokio::runtime::Builder::new_current_thread()
        };

        let rt = builder.enable_all().build().unwrap();

        rt.block_on(async move {
            let mut req_adapter = req_adapter_factory.build().await;
            self.barrier.wait();

            loop {
                tokio::select! {
                    _ = shutdown.changed() => {
                        if *shutdown.borrow() {
                            info!("Shutdown signal received, exiting adapter.");
                            req_adapter.on_shutdown();
                            break;
                        }
                    }
                    maybe_req = receiver.recv() => {
                        if let Some(req) = maybe_req {
                            req_adapter.on_request(req);
                        }
                    }
                }
            }
            // destroy the mutex lock and join all tasks spawned by the adapter
            // Needing this block to drop the guard explicitly: https://rust-lang.github.io/rust-clippy/master/index.html#await_holding_lock
            let vec;
            {
                let mut guard = self.shutdown_handles.lock().unwrap();
                vec = std::mem::take(&mut *guard);
            }

            // If we don't wait here, the runtime will exit before the shutdown tasks are joined.
            for r in join_all(vec).await {
                if let Err(e) = r {
                    eprintln!("task failed: {e}");
                }
            }

            info!("All shutdown tasks finished, exiting adapter.");
        });
        drop(_guard);
    }

    /// This method creates a shutdown channel for the adapter.
    fn shutdown_channel(
        &self,
    ) -> (
        tokio::sync::watch::Sender<bool>,
        tokio::sync::watch::Receiver<bool>,
    ) {
        tokio::sync::watch::channel(false)
    }

    pub fn new(worker_threads: u32, barrier: Arc<Barrier>) -> Self {
        Self {
            worker_threads,
            barrier,
            shutdown_handles: Arc::new(Mutex::new(vec![])),
        }
    }

    pub fn from_config(config: &Config, barrier: Arc<Barrier>) -> Self {
        Self {
            worker_threads: config.computational_setup().adapter_worker_threads,
            barrier,
            shutdown_handles: Arc::new(Mutex::new(vec![])),
        }
    }

    pub fn shutdown_handles(&self) -> Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>> {
        self.shutdown_handles.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use tokio::sync::mpsc;

    #[test]
    fn test_execute_adapter() {
        let (tx, rx) = mpsc::channel(10);
        let counter = Arc::new(Mutex::new(0));
        let handler = MockRequestAdapterBuilder(counter.clone());
        let (shutdown_send, shutdown_recv) = tokio::sync::watch::channel(false);

        // Spawn the adapter in a separate task
        let handle = thread::spawn(move || {
            AsyncExecutor::new(1, Arc::new(Barrier::new(1))).execute_adapter(
                rx,
                handler,
                shutdown_recv,
            );
        });

        // Send a request
        let (send, recv) = tokio::sync::oneshot::channel();
        tx.blocking_send(MockRequest {
            payload: String::from("Test Payload"),
            response_tx: send,
        })
        .unwrap();

        let string = recv.blocking_recv().unwrap();
        assert_eq!(string, String::from("Ok"));
        assert_eq!(*counter.lock().unwrap(), 1);
        shutdown_send.send(true).unwrap();

        handle.join().unwrap();
    }

    #[derive(Debug)]
    struct MockRequest {
        payload: String,
        response_tx: tokio::sync::oneshot::Sender<String>,
    }

    impl RequestToAdapter for MockRequest {}

    struct MockRequestAdapter(Arc<Mutex<usize>>);

    impl RequestAdapter<MockRequest> for MockRequestAdapter {
        fn on_request(&mut self, req: MockRequest) {
            let arc = self.0.clone();
            tokio::spawn(async move {
                {
                    let mut guard = arc.lock().unwrap();
                    *guard += 1;
                }
                println!("Mock handler received request: {}", req.payload);
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await; // Simulate some processing delay
                req.response_tx.send(String::from("Ok")).unwrap();
            });
        }
    }

    struct MockRequestAdapterBuilder(Arc<Mutex<usize>>);

    impl RequestAdapterFactory<MockRequest> for MockRequestAdapterBuilder {
        async fn build(self) -> impl RequestAdapter<MockRequest> {
            MockRequestAdapter(self.0)
        }
    }
}
