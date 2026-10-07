#[derive(Debug)]
struct RawStruct {
    number: i32,
    reference: *const i32,
}

impl RawStruct {
    fn create_on_stack(i: i32) -> RawStruct {
        let mut res = RawStruct {
            number: i,
            reference: std::ptr::null(),
        };
        res.reference = &res.number;
        unsafe {
            println!(
                "#create_on_stack(): {:?} | number: {:?} | referenced number {:?}",
                res, res.number, &*res.reference
            );
        }
        res
    }

    fn create_on_heap(i: i32) -> Box<RawStruct> {
        let mut res = Box::new(RawStruct {
            number: i,
            reference: std::ptr::null(),
        });
        res.reference = &res.number;
        unsafe {
            println!(
                "#create_on_heap(): {:?} | number: {:?} | referenced number {:?}",
                res, res.number, &*res.reference
            );
        }
        res
    }
}

fn run() {
    let i = 3;

    println!("---- on stack ----");
    let raw_struct = RawStruct::create_on_stack(i);

    println!("{:?}", raw_struct);
    unsafe {
        println!(
            "#Main(): {:?} | number: {:?} | referenced number {:?}",
            raw_struct, raw_struct.number, &*raw_struct.reference
        );
    }

    println!("---- on heap ----");
    let boxed_struct = RawStruct::create_on_heap(i);

    println!("{:?}", boxed_struct);
    unsafe {
        let x = &*boxed_struct.reference;
        println!(
            "#Main(): {:?} | number: {:?} | referenced number {:?}",
            boxed_struct, boxed_struct.number, x
        );

        assert_eq!(x, &boxed_struct.number)
    }
}

#[cfg(test)]
mod test {
    use crate::experiments::pointers::run;

    #[test]
    fn test() {
        run()
    }
}
