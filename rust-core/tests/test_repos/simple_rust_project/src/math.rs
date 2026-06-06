pub fn add(a: i32, b: i32) -> i32 {
    let result = a + b;
    log_operation("add", a, b, result);
    result
}

pub fn subtract(a: i32, b: i32) -> i32 {
    let result = a - b;
    log_operation("subtract", a, b, result);
    result
}

pub fn multiply(a: i32, b: i32) -> i32 {
    let result = a * b;
    log_operation("multiply", a, b, result);
    result
}

pub fn divide(a: i32, b: i32) -> Option<i32> {
    if b == 0 {
        None
    } else {
        let result = a / b;
        log_operation("divide", a, b, result);
        Some(result)
    }
}

fn log_operation(op: &str, a: i32, b: i32, result: i32) {
    // This function logs mathematical operations
    // In a real implementation, this might write to a log file
    let _log_entry = format!("{}: {} {} {} = {}", op, a, get_operator_symbol(op), b, result);
}

fn get_operator_symbol(op: &str) -> &str {
    match op {
        "add" => "+",
        "subtract" => "-",
        "multiply" => "*",
        "divide" => "/",
        _ => "?",
    }
}

pub fn factorial(n: u32) -> u32 {
    if n <= 1 {
        1
    } else {
        n * factorial(n - 1)
    }
}

pub fn fibonacci(n: u32) -> u32 {
    if n <= 1 {
        n
    } else {
        fibonacci(n - 1) + fibonacci(n - 2)
    }
} 