//! # oxiron
//!
//! A lightweight, zero-dependency utility library for Rust.
//!
//! This initial release ships with a placeholder API; more functionality
//! will land in upcoming versions.
//!
//! # Examples
//!
//! ```
//! assert_eq!(oxiron::add(2, 2), 4);
//! ```

/// Adds two numbers and returns the result.
///
/// # Examples
///
/// ```
/// assert_eq!(oxiron::add(2, 2), 4);
/// ```
pub fn add(left: u64, right: u64) -> u64 {
    left + right
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {
        let result = add(2, 2);
        assert_eq!(result, 4);
    }
}
