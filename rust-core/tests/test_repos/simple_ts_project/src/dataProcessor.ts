interface CacheEntry {
  value: string;
  timestamp: number;
}

export class DataProcessor {
  private cache: Map<string, CacheEntry>;
  private readonly cacheTimeout: number;

  constructor(cacheTimeout: number = 300000) { // 5 minutes default
    this.cache = new Map();
    this.cacheTimeout = cacheTimeout;
  }
  
  processData(inputData: string): string {
    // Check cache first
    const cached = this.getFromCache(inputData);
    if (cached) {
      return cached;
    }
    
    // Process data through multiple steps
    const cleanedData = this.cleanInput(inputData);
    const validatedData = this.validateData(cleanedData);
    const transformedData = this.transformData(validatedData);
    const finalResult = this.finalizeResult(transformedData);
    
    // Cache the result
    this.addToCache(inputData, finalResult);
    return finalResult;
  }
  
  private cleanInput(data: string): string {
    if (typeof data === 'string') {
      return data.trim().toLowerCase();
    }
    return String(data);
  }
  
  private validateData(data: string): string {
    if (!data) {
      throw new Error('Data cannot be empty');
    }
    return data;
  }
  
  private transformData(data: string): string {
    // Apply multiple transformations
    const step1 = this.step1Transform(data);
    const step2 = this.step2Transform(step1);
    return step2;
  }
  
  private step1Transform(data: string): string {
    return data.toUpperCase();
  }
  
  private step2Transform(data: string): string {
    return `PROCESSED_${data}`;
  }
  
  private finalizeResult(data: string): string {
    return `FINAL: ${data}`;
  }

  private getFromCache(key: string): string | null {
    const entry = this.cache.get(key);
    if (!entry) {
      return null;
    }
    
    // Check if cache entry is expired
    if (Date.now() - entry.timestamp > this.cacheTimeout) {
      this.cache.delete(key);
      return null;
    }
    
    return entry.value;
  }

  private addToCache(key: string, value: string): void {
    this.cache.set(key, {
      value,
      timestamp: Date.now()
    });
  }
  
  getCacheStats(): { size: number; keys: string[] } {
    return {
      size: this.cache.size,
      keys: Array.from(this.cache.keys())
    };
  }
  
  clearCache(): void {
    this.cache.clear();
  }

  // Additional utility methods
  processBatch(inputs: string[]): string[] {
    return inputs.map(input => this.processData(input));
  }

  async processDataAsync(inputData: string): Promise<string> {
    // Simulate async processing
    return new Promise((resolve) => {
      setTimeout(() => {
        resolve(this.processData(inputData));
      }, 10);
    });
  }
} 