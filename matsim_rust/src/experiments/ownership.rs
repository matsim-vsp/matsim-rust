// fn ownership_1() {
//     let mut a: i32 = 1;
//     let b = &a;
//     let c = &mut a;
//
//     println!("{b:#?}, {c:#?}");
// }
//
// fn ownership_2() {
//     let mut vector = vec![1, 2, 3, 4];
//
//     let first = vector.get(0).unwrap();
//
//     vector.push(5);
//
//     println!("{}", first);
// }
//
// struct OwnershipStruct {
//     pub vec1: Vec<i32>,
//     pub vec2: Vec<i32>,
// }
//
// impl OwnershipStruct {
//     fn new() -> OwnershipStruct {
//         OwnershipStruct {
//             vec1: Vec::new(),
//             vec2: Vec::new(),
//         }
//     }
// }
//
// fn ownership_3() {
//     let mut my_struct = OwnershipStruct::new();
//
//     let vec1_ref = &my_struct.vec1;
//     let mut_vec2_ref = &mut my_struct.vec2;
//
//     mut_vec2_ref.push(1);
//
//     println!("{:?}", vec1_ref);
// }
//
