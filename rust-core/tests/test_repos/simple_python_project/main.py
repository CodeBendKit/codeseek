#!/usr/bin/env python3

from data_processor import DataProcessor
from math_utils import MathUtils
from string_utils import StringUtils

def main():
    """Main function that demonstrates function call relationships"""
    processor = DataProcessor()
    
    # Process some data
    result = processor.process_data("test_input")
    print(f"Processed result: {result}")
    
    # Use math utilities
    math_utils = MathUtils()
    sum_result = math_utils.calculate_sum([1, 2, 3, 4, 5])
    print(f"Sum: {sum_result}")
    
    # Use string utilities
    string_utils = StringUtils()
    formatted = string_utils.format_output("Hello World")
    print(f"Formatted: {formatted}")

def helper_function():
    """Helper function called by main"""
    return "helper result"

if __name__ == "__main__":
    main() 