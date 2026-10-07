/*
    Get started with concurrency. Try implementing something which has two threads that play ping
    pong and a main thread which waits for them to finish.
*/

use std::sync::mpsc;
use std::sync::mpsc::{Receiver, Sender};
use std::thread;

fn run() {
    let (tx1, rx1): (Sender<Message>, Receiver<Message>) = mpsc::channel();
    let (tx2, rx2): (Sender<Message>, Receiver<Message>) = mpsc::channel();

    let handle1 = thread::spawn(move || {
        let first_message = Message {
            message: String::from("ping"),
            id: 0,
        };
        println!("Thread 1 sending first {first_message:#?}");
        tx1.send(first_message).unwrap();

        loop {
            match rx2.recv() {
                Ok(mut message) => {
                    println!("Thread 1 received: {message:#?}");

                    if message.id >= 3 {
                        println!("Thread 1 has received message id >= 3. Breaking out of the loop");
                        break;
                    }

                    message.id = message.id + 1;
                    message.message = String::from("ping");
                    tx1.send(message).unwrap();
                }
                Err(_) => {
                    println!("Thread 1 received error on recv.")
                }
            };
        }

        println!("Thread 1 is finishing")
    });

    let handle2 = thread::spawn(move || {
        loop {
            match rx1.recv() {
                Ok(mut message) => {
                    println!("Thread 2 received {message:#?}");

                    message.id = message.id + 1;
                    message.message = String::from("pong");

                    println!("Thread 2 sending {message:#?}");
                    tx2.send(message).unwrap();
                }
                Err(_) => {
                    println!("Thread 2 received error on recv.");
                    break;
                }
            }
        }

        println!("Thread 2 is finishing")
    });

    println!("Main thread has initialized 2 threads. Waiting on join now. ");
    handle1.join().unwrap();
    handle2.join().unwrap();
    print!("Both threads have finished. Program is Done.");
}

#[derive(Debug)]
struct Message {
    message: String,
    id: i32,
}

#[cfg(test)]
mod tests {
    use crate::experiments::concurrent_ping_pong::run;

    #[test]
    fn test_run() {
        run();
    }
}
