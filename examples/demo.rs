fn main() {
    let mut a: A = A::new(1, 2, 3, 4);
    a.set_a(10);
    println!("{:?}", a.get_a());
    println!("{:?}", a.get_b());
    println!("{:?}", a.get_c());
    println!("{:?}", a.get_d());

    const B: i32 = 10;
    println!("{:?}", B);
    let mut b: i32 = 20;
    b += 10;
    {
        let mut b: i32 = 30;
        b += 10;
        println!("{:?}", b + 10);
    }
    println!("{:?}", b + 10);
    let y = {
        let x = 3;
        x + 1
    };

    println!("The value of y is: {y}");

    let x = plus_one(5);

    println!("The value of x is: {x}");
}

fn plus_one(mut x: i32) -> i32 {
    x = x + 1;
    const A: i32 = 1;
    x = x + A;
    x
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct A {
    a: i32,
    b: i32,
    c: i32,
    d: i32,
}

impl A {
    fn new(a: i32, b: i32, c: i32, d: i32) -> Self {
        A { a, b, c, d }
    }
    fn get_a(&self) -> i32 {
        self.a
    }
    fn set_a(&mut self, a: i32) -> i32 {
        self.a = a;
        self.a
    }
    fn get_b(&self) -> i32 {
        self.b
    }
    fn get_c(&self) -> i32 {
        self.c
    }
    fn get_d(&self) -> i32 {
        self.d
    }
    fn add(&self) -> Self  {
        A::new(self.a + self.b + self.c + self.d, 0, 0, 0)
    }
}
