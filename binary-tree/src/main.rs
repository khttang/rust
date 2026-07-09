
mod binary_search_tree;
mod hash_map;

use crate::binary_search_tree::BinarySearchTree;
use crate::hash_map::HashMap;

#[derive(Debug, PartialEq, Eq, Hash)]
enum FamilyMember {
    Daddy,
    Mommy,
    FirstChild,
    SecondChild,
}

fn main() {
    let mut tree = BinarySearchTree::<i32>::new();
    
    for value in [9, 5, 3, 7, 1, 4, 6, 8] {
        tree.insert(value);
    }
    
    println!("Contains 4: {}", tree.contains(4));  // true
    println!("Contains 9: {}", tree.contains(9));  // false
    
    let sorted = tree.sorted_values();
    println!("Sorted: {:?}", sorted);  // [1, 3, 4, 5, 6, 7, 8]
    
    tree.delete(3);
    let after_delete = tree.sorted_values();
    println!("After deleting 3: {:?}", after_delete);  // [1, 4, 5, 6, 7, 8]

    let mut tree2 = BinarySearchTree::<String>::new();
    for value in ["banana", "apple", "cherry", "date"] {
        tree2.insert(value.to_string());
    }
    println!("Contains 'apple': {}", tree2.contains("apple".to_string()));  // true
    println!("Contains 'fig': {}", tree2.contains("fig".to_string()));  // false
    let sorted2 = tree2.sorted_values();
    println!("Sorted: {:?}", sorted2);  // ["apple", "banana", "cherry", "date"]

    // HashMap example
    let mut map = HashMap::<String, i32>::new();
    map.insert("apple".to_string(), 1);
    map.insert("banana".to_string(), 2);
    map.insert("cherry".to_string(), 3);
    map.insert("orange".to_string(), 4);
    println!("Value for 'apple': {:?}", map.get("apple".to_string()));  // Some(1)
    println!("Value for 'banana': {:?}", map.get("banana".to_string()));  // Some(2)
    println!("Value for 'cherry': {:?}", map.get("cherry".to_string()));  // Some(3)
    println!("Value for 'orange': {:?}", map.get("orange".to_string()));  // Some(4)
    map.remove(&"banana".to_string());
    println!("Value for 'banana' after removal: {:?}", map.get("banana".to_string()));  // None

    // Additional test cases for HashMap
    let mut map2 = HashMap::<FamilyMember, (String, i32)>::new();
    map2.insert(FamilyMember::Daddy, ("Khiem".to_string(), 1966));
    map2.insert(FamilyMember::Mommy, ("Hanh".to_string(), 1969));
    map2.insert(FamilyMember::FirstChild, ("Thompson".to_string(), 2002));
    map2.insert(FamilyMember::SecondChild, ("Travis".to_string(), 2008));
    println!("Value for Daddy: {:?}", map2.get(FamilyMember::Daddy));  
    println!("Value for Mommy: {:?}", map2.get(FamilyMember::Mommy));  
    println!("Value for FirstChild: {:?}", map2.get(FamilyMember::FirstChild));  
    println!("Value for SecondChild: {:?}", map2.get(FamilyMember::SecondChild)); 
}
