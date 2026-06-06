export interface MathResult {
  value: number;
  operation: string;
  timestamp: number;
}

export class MathUtils {
  private results: MathResult[] = [];

  calculateSum(numbers: number[]): number {
    if (!Array.isArray(numbers) || numbers.length === 0) {
      return 0;
    }
    
    let total = 0;
    for (const num of numbers) {
      total = this.addToTotal(total, num);
    }
    
    this.recordResult(total, 'sum');
    return total;
  }
  
  private addToTotal(currentTotal: number, newNumber: number): number {
    return currentTotal + newNumber;
  }
  
  calculateAverage(numbers: number[]): number {
    if (!Array.isArray(numbers) || numbers.length === 0) {
      return 0;
    }
    
    const total = this.calculateSum(numbers);
    const count = numbers.length;
    const average = this.divideNumbers(total, count);
    
    this.recordResult(average, 'average');
    return average;
  }
  
  private divideNumbers(numerator: number, denominator: number): number {
    if (denominator === 0) {
      throw new Error('Cannot divide by zero');
    }
    return numerator / denominator;
  }
  
  factorial(n: number): number {
    if (n < 0) {
      throw new Error('Factorial not defined for negative numbers');
    }
    if (n <= 1) {
      return 1;
    }
    const result = n * this.factorial(n - 1);
    this.recordResult(result, 'factorial');
    return result;
  }
  
  fibonacci(n: number): number {
    if (n <= 0) {
      return 0;
    }
    if (n === 1) {
      return 1;
    }
    const result = this.fibonacci(n - 1) + this.fibonacci(n - 2);
    this.recordResult(result, 'fibonacci');
    return result;
  }
  
  power(base: number, exponent: number): number {
    if (exponent === 0) {
      return 1;
    }
    if (exponent < 0) {
      return 1 / this.power(base, -exponent);
    }
    
    let result = 1;
    for (let i = 0; i < exponent; i++) {
      result = this.multiplyNumbers(result, base);
    }
    
    this.recordResult(result, 'power');
    return result;
  }
  
  private multiplyNumbers(a: number, b: number): number {
    return a * b;
  }
  
  isPrime(n: number): boolean {
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

  // Additional mathematical operations
  gcd(a: number, b: number): number {
    if (b === 0) {
      return a;
    }
    return this.gcd(b, a % b);
  }

  lcm(a: number, b: number): number {
    return (a * b) / this.gcd(a, b);
  }

  private recordResult(value: number, operation: string): void {
    this.results.push({
      value,
      operation,
      timestamp: Date.now()
    });
  }

  getResults(): MathResult[] {
    return [...this.results];
  }

  clearResults(): void {
    this.results = [];
  }

  // Statistical methods
  calculateStandardDeviation(numbers: number[]): number {
    if (numbers.length < 2) {
      return 0;
    }
    
    const mean = this.calculateAverage(numbers);
    const squaredDifferences = numbers.map(num => Math.pow(num - mean, 2));
    const variance = this.calculateAverage(squaredDifferences);
    
    return Math.sqrt(variance);
  }

  calculateMedian(numbers: number[]): number {
    if (numbers.length === 0) {
      return 0;
    }
    
    const sorted = [...numbers].sort((a, b) => a - b);
    const middle = Math.floor(sorted.length / 2);
    
    if (sorted.length % 2 === 0) {
      return (sorted[middle - 1] + sorted[middle]) / 2;
    } else {
      return sorted[middle];
    }
  }
} 