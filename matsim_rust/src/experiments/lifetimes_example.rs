use std::collections::HashMap;
/** Use this part here to figure out things I haven't understood yet.
  At the moment, this is about uderstanding lifetime annotations.
*/
fn run() {
    let strings = vec!["one", "two", "three"];
    let mut mapped: Vec<usize> = vec![];
    let mut mapper = Mapper::new();

    for value in strings {
        //let id = add_key(&mut mapper, value);
        let id = mapper.add_key(value);
        mapped.push(id);
    }

    println!("{mapped:#?}");
}

#[allow(dead_code)]
fn add_key<'a, 'b>(mapper: &'a mut Mapper<'b>, key: &'b str) -> usize {
    let id = mapper.mapping.entry(key).or_insert(mapper.next_id);
    if mapper.next_id == *id {
        mapper.next_id += 1;
    }

    *id
}

struct Mapper<'key> {
    mapping: HashMap<&'key str, usize>,
    next_id: usize,
}

impl<'key> Mapper<'key> {
    fn new() -> Self {
        Mapper {
            mapping: HashMap::new(),
            next_id: 0,
        }
    }

    /**
    This stores a reference to key. and returns a number instead.
    The type of self  is: &'s mut Mapper<'key>. The lifetime of the key variable
    must therefore be of at least 'key, because that is the lifetime of the key
    in the HashMap. The 'self' must have a different lifetime (possibly shorter)
    so that this method can be used in a loop. If 'self' would also have 'key as
    lifetime, the reference to self can't be released until 'key' goes out of scope.
    .... I think....
     */
    fn add_key<'s>(&'s mut self, key: &'key str) -> usize {
        let id = self.mapping.entry(key).or_insert(self.next_id);
        if self.next_id == *id {
            self.next_id += 1;
        }

        *id
    }
}
