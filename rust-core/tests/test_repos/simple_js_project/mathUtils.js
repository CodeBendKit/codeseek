class MathUtils {
    calculateSum(numbers) {
        if (!Array.isArray(numbers) || numbers.length === 0) {
            return 0;
        }
        
        let total = 0;
        for (const num of numbers) {
            total = this._addToTotal(total, num);
        }
        return total;
    }
    
    _addToTotal(currentTotal, newNumber) {
        return currentTotal + newNumber;
    }
    
    calculateAverage(numbers) {
        if (!Array.isArray(numbers) || numbers.length === 0) {
            return 0;
        }
        
        const total = this.calculateSum(numbers);
        const count = numbers.length;
        return this._divideNumbers(total, count);
    }
    
    _divideNumbers(numerator, denominator) {
        if (denominator === 0) {
            throw new Error('Cannot divide by zero');
        }
        return numerator / denominator;
    }
    
    factorial(n) {
        if (n < 0) {
            throw new Error('Factorial not defined for negative numbers');
        }
        if (n <= 1) {
            return 1;
        }
        return n * this.factorial(n - 1);
    }
    
    fibonacci(n) {
        if (n <= 0) {
            return 0;
        }
        if (n === 1) {
            return 1;
        }
        return this.fibonacci(n - 1) + this.fibonacci(n - 2);
    }
    
    power(base, exponent) {
        if (exponent === 0) {
            return 1;
        }
        if (exponent < 0) {
            return 1 / this.power(base, -exponent);
        }
        
        let result = 1;
        for (let i = 0; i < exponent; i++) {
            result = this._multiplyNumbers(result, base);
        }
        return result;
    }
    
    _multiplyNumbers(a, b) {
        return a * b;
    }
    
    isPrime(n) {
        if (n < 2) {
            return false;
        }
        if (n === 2) {
            return true;
        }
        if (n % 2 === 0) {
            return false;
        }
        
        for (let i = 3; i <= Math.sqrt(n); i += 2) {
            if (n % i === 0) {
                return false;
            }
        }
        return true;
    }
}

module.exports = MathUtils; 