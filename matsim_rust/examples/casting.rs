use std::any::Any;

trait Parent: Any {
    fn parent_method(&self);

    fn into_any(self: Box<Self>) -> Box<dyn Any>;
}

trait Child: Parent {
    fn child_method(&self);
}

struct MyType;

impl Parent for MyType {
    fn parent_method(&self) {
        println!("parent_method");
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

impl Child for MyType {
    fn child_method(&self) {
        println!("child_method");
    }
}

fn main() {
    // Child -> Parent: geht
    let child: Box<dyn Child> = Box::new(MyType);
    child.child_method();

    let parent: Box<dyn Parent> = child;
    parent.parent_method();

    // Parent -> Child: geht NICHT direkt
    //
    // let child_again: Box<dyn Child> = parent;
    //
    // Stattdessen nur über Downcast auf den konkreten Typ:

    let any = parent.into_any();

    match any.downcast::<MyType>() {
        Ok(my_type) => {
            let child_again: Box<dyn Child> = my_type;
            child_again.child_method();
        }
        Err(_) => {
            println!("Not a MyType");
        }
    }
}
