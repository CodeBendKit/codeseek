pub mod utils;
pub mod math;

pub fn public_function() -> i32 {
    let result = private_helper();
    result * 2
}

fn private_helper() -> i32 {
    21
}

pub fn complex_calculation(a: i32, b: i32) -> i32 {
    let intermediate = math::add(a, b);
    let doubled = math::multiply(intermediate, 2);
    utils::apply_transform(doubled)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_public_function() {
        assert_eq!(public_function(), 42);
    }

    #[test]
    fn test_complex_calculation() {
        assert_eq!(complex_calculation(5, 3), 32);
    }
} 