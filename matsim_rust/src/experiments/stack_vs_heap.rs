use std::time::Instant;

#[derive(Debug)]
struct DummyStruct {
    i: usize,
}

impl DummyStruct {
    fn add(&mut self, i: usize) {
        self.i += i;
    }
}

fn stack_allocation(n: usize) -> DummyStruct {
    let mut sum = DummyStruct { i: 0 };
    for i in 0..n {
        sum.add(i)
    }
    sum
}

fn heap_allocation(n: usize) -> Box<DummyStruct> {
    let mut sum = Box::new(DummyStruct { i: 0 });
    for i in 0..n {
        sum.add(i)
    }
    sum
}

fn run() {
    let n = 10_000_000;

    let start = Instant::now();
    let stack_result = stack_allocation(n);
    let stack_duration = start.elapsed();

    let start = Instant::now();
    let heap_result = heap_allocation(n);
    let heap_duration = start.elapsed();

    println!(
        "Stack result: {:?}, Time: {:?}",
        stack_result, stack_duration
    );
    println!("Heap result: {:?}, Time: {:?}", heap_result, heap_duration);

    // divide by at least one
    println!(
        "Ratio Heap/Stack: {:?}",
        heap_duration.as_nanos() / stack_duration.as_nanos().max(1)
    )
}

#[cfg(test)]
mod tests {
    use crate::experiments::stack_vs_heap::run;

    #[test]
    fn test_run() {
        run()
    }
}
