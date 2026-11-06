//! A tiny crate with the usual Rust syntax.
use std::collections::HashMap;
use std::fmt::{self, Display};

/// A point in the plane.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point<T> {
    pub x: T,
    pub y: T,
}

impl<T: Display> Display for Point<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({}, {})", self.x, self.y)
    }
}

enum Shape { Circle(f64), Rect { w: f64, h: f64 }, Empty }

const LIMIT: usize = 1_000;
static GREETING: &str = "hello\n\"world\"";

fn area(s: &Shape) -> f64 {
    match s {
        Shape::Circle(r) => std::f64::consts::PI * r * r,
        Shape::Rect { w, h } => w * h,
        Shape::Empty => 0.0,
    }
}

fn main() {
    let mut counts: HashMap<&str, i32> = HashMap::new();
    for word in "a b a".split_whitespace() {
        *counts.entry(word).or_insert(0) += 1;
    }
    let raw = r#"raw "string" here"#;
    let ch = 'x';
    let lifetime: &'static str = "s";
    /* block
       comment */
    let closure = |a: i32, b: i32| -> i32 { a + b };
    println!("{} {:?} {}", closure(1, 2), counts, raw);
    if let Some(v) = counts.get("a") { assert!(*v > 0, "positive"); }
    unsafe { let p = &0 as *const i32; let _ = *p; }
}
