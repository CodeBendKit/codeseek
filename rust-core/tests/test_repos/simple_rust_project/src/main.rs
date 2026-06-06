use std::collections::HashMap;

fn main() {
    let result = process_data();
    println!("Result: {}", result);
    
    let numbers = vec![1, 2, 3, 4, 5];
    let sum = calculate_sum(&numbers);
    println!("Sum: {}", sum);
    
    let filtered = filter_even_numbers(&numbers);
    println!("Filtered: {:?}", filtered);
}

fn process_data() -> i32 {
    let data = fetch_data();
    let processed = transform_data(data);
    validate_data(processed)
}

fn fetch_data() -> i32 {
    let raw_data = get_raw_data();
    parse_raw_data(raw_data)
}

fn get_raw_data() -> String {
    "42".to_string()
}

fn parse_raw_data(raw: String) -> i32 {
    raw.parse().unwrap_or(0)
}

fn transform_data(data: i32) -> i32 {
    let doubled = double_value(data);
    add_offset(doubled)
}

fn double_value(value: i32) -> i32 {
    value * 2
}

fn add_offset(value: i32) -> i32 {
    value + 10
}

fn validate_data(data: i32) -> i32 {
    if data > 0 {
        data
    } else {
        default_value()
    }
}

fn default_value() -> i32 {
    100
}

fn calculate_sum(numbers: &[i32]) -> i32 {
    let mut sum = 0;
    for &num in numbers {
        sum = add_to_sum(sum, num);
    }
    sum
}

fn add_to_sum(current: i32, new_value: i32) -> i32 {
    current + new_value
}

fn filter_even_numbers(numbers: &[i32]) -> Vec<i32> {
    let mut result = Vec::new();
    for &num in numbers {
        if is_even(num) {
            result.push(num);
        }
    }
    result
}

fn is_even(num: i32) -> bool {
    num % 2 == 0
}

pub struct DataProcessor {
    cache: HashMap<String, i32>,
}

impl DataProcessor {
    pub fn new() -> Self {
        Self {
            cache: HashMap::new(),
        }
    }
    
    pub fn process(&mut self, key: &str) -> i32 {
        if let Some(&cached) = self.cache.get(key) {
            return cached;
        }
        
        let result = self.compute_value(key);
        self.cache.insert(key.to_string(), result);
        result
    }
    
    fn compute_value(&self, key: &str) -> i32 {
        let base_value = get_base_value(key);
        apply_multiplier(base_value)
    }
    
    fn get_base_value(&self, key: &str) -> i32 {
        key.len() as i32
    }
    
    fn apply_multiplier(&self, value: i32) -> i32 {
        value * 2
    }
} 