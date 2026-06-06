pub fn apply_transform(value: i32) -> i32 {
    let transformed = value + 10;
    validate_result(transformed)
}

fn validate_result(value: i32) -> i32 {
    if value < 0 {
        0
    } else {
        value
    }
}

pub fn format_output(value: i32) -> String {
    let prefix = get_prefix();
    format!("{}: {}", prefix, value)
}

fn get_prefix() -> String {
    "Result".to_string()
}

pub fn process_list(items: &[i32]) -> Vec<i32> {
    let mut result = Vec::new();
    for &item in items {
        let processed = process_item(item);
        result.push(processed);
    }
    result
}

fn process_item(item: i32) -> i32 {
    if item > 0 {
        item * 2
    } else {
        item.abs()
    }
} 