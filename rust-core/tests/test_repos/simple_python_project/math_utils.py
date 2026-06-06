class MathUtils:
    """Class for mathematical operations"""
    
    def calculate_sum(self, numbers):
        """Calculate sum of numbers"""
        if not numbers:
            return 0
        
        total = 0
        for num in numbers:
            total = self._add_to_total(total, num)
        return total
    
    def _add_to_total(self, current_total, new_number):
        """Add a new number to the current total"""
        return current_total + new_number
    
    def calculate_average(self, numbers):
        """Calculate average of numbers"""
        if not numbers:
            return 0
        
        total = self.calculate_sum(numbers)
        count = len(numbers)
        return self._divide_numbers(total, count)
    
    def _divide_numbers(self, numerator, denominator):
        """Divide two numbers"""
        if denominator == 0:
            raise ValueError("Cannot divide by zero")
        return numerator / denominator
    
    def factorial(self, n):
        """Calculate factorial of n"""
        if n < 0:
            raise ValueError("Factorial not defined for negative numbers")
        if n <= 1:
            return 1
        return n * self.factorial(n - 1)
    
    def fibonacci(self, n):
        """Calculate nth Fibonacci number"""
        if n <= 0:
            return 0
        if n == 1:
            return 1
        return self.fibonacci(n - 1) + self.fibonacci(n - 2)
    
    def power(self, base, exponent):
        """Calculate base raised to exponent"""
        if exponent == 0:
            return 1
        if exponent < 0:
            return 1 / self.power(base, -exponent)
        
        result = 1
        for _ in range(exponent):
            result = self._multiply_numbers(result, base)
        return result
    
    def _multiply_numbers(self, a, b):
        """Multiply two numbers"""
        return a * b 