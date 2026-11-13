#[derive(Debug, Default)]
struct User {
    id: i32,
    name: String,
    age: i32,
}

impl User {
    fn new(id: i32, name: String, age: i32) -> Self {
        User { id, name, age }
    }
    pub fn get_id(&self) -> i32 {
        self.id
    }
    pub fn set_id(&mut self, id: i32) {
        self.id = id;
    }
    pub fn get_name(&self) -> &str {
        &self.name
    }
    pub fn set_name(&mut self, name: String) {
        self.name = name;
    }
    pub fn get_age(&self) -> i32 {
        self.age
    }
    pub fn set_age(&mut self, age: i32) {
        self.age = age;
    }
}

#[test]
fn lib_test() {
    let a: User = User::default();
    println!("{:?}", a);
    let mut user: User = User::new(1, "Alice".to_string(), 30);
    user.get_id();
    user.get_name();
    user.get_age();
    println!("{:?}", user);
    let b = &mut user;
    b.set_id(2);
    b.set_name("Bob".to_string());
    b.set_age(25);
    println!("{:?}", b);
}
