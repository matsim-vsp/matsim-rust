use crate::experiments::run_process_as_service::Message::{Request, Response};
use std::collections::HashMap;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

#[derive(Debug)]
enum Message {
    //number, id, more
    Request(u32, u32, bool),
    Response(u32),
}

fn run() {
    // Create a channel
    let (sender1, receiver_service) = mpsc::channel();
    let (sender_service_to1, receiver1) = mpsc::channel();
    let (sender_service_to2, receiver2) = mpsc::channel();

    let sender2 = sender1.clone();
    // Spawn the first thread
    thread::spawn(move || {
        for i in 1..=5 {
            let request = Request(i, 1, i != 5);
            println!("Thread 1: Sending number {:?}", request);
            sender1.send(request).unwrap();
            // Wait for the response
            let response = receiver1.recv().unwrap();
            println!("Thread 1: Received response {:?}", response);
            thread::sleep(Duration::from_millis(500));
        }
    });

    // Spawn the first thread
    thread::spawn(move || {
        for i in 6..=10 {
            let request = Request(i, 2, i != 10);
            println!("Thread 2: Sending number {:?}", request);
            sender2.send(request).unwrap();
            // Wait for the response
            let response = receiver2.recv().unwrap();
            println!("Thread 2: Received response {:?}", response);
            thread::sleep(Duration::from_millis(500));
        }
    });

    // Spawn the second thread
    thread::spawn(move || {
        let mut expect_more = HashMap::new();
        expect_more.insert(1, true);
        expect_more.insert(2, true);
        while !expect_more.values().all(|v| v == &false) {
            // Receive the number
            let in_message = receiver_service.recv().unwrap();
            // println!("Thread 2: Received number {:?}", in_message);
            // Send back the doubled number
            let res;
            let to_thread_id;
            match in_message {
                Request(i, id, more) => {
                    res = i * 2;
                    expect_more.insert(id, more);
                    to_thread_id = id;
                }
                Response(_) => {
                    panic!("Only expect Requests here.")
                }
            }
            let out_message = Response(res);
            // println!("Thread 2: Sent response {:?}", out_message);
            match to_thread_id {
                1 => {
                    sender_service_to1.send(out_message).unwrap();
                }
                2 => {
                    sender_service_to2.send(out_message).unwrap();
                }
                _ => panic!(),
            }
        }
    })
    .join()
    .unwrap(); // Ensure the second thread completes

    // Give some time for the first thread to complete
    // thread::sleep(Duration::from_secs(3));
}

#[cfg(test)]
mod tests {
    use crate::experiments::run_process_as_service::run;

    #[test]
    fn test_run() {
        run();
    }
}
