use std::collections::HashMap;
use std::fmt;

pub struct Point {
    pub x: i32,
    pub y: i32,
}

pub enum Shape {
    Circle,
    Square,
}

pub trait Draw {
    fn draw(&self);
}

pub fn origin() -> Point {
    Point { x: 0, y: 0 }
}

const MAX: i32 = 100;

fn _use_them() {
    let _m: HashMap<i32, i32> = HashMap::new();
    let _ = fmt::Error;
    let _ = MAX;
}
