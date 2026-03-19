#[derive(Debug, Clone)]
pub enum Credentials {
    Token(String),
    Basic { username: String, password: String },
}
