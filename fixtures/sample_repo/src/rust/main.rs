mod math;

use math::{add, Point};

fn main() {
    let sum = add(1, 2);
    let p = Point { x: 1.0, y: 2.0 };
    let q = Point { x: 0.0, y: 0.0 };
    println!("sum = {}, distance = {}", sum, p.distance(&q));
}
