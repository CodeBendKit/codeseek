class DataProcessor {
    constructor() {
        this.cache = new Map();
    }
    
    processData(inputData) {
        // Check cache first
        if (this.cache.has(inputData)) {
            return this.cache.get(inputData);
        }
        
        // Process data through multiple steps
        const cleanedData = this._cleanInput(inputData);
        const validatedData = this._validateData(cleanedData);
        const transformedData = this._transformData(validatedData);
        const finalResult = this._finalizeResult(transformedData);
        
        // Cache the result
        this.cache.set(inputData, finalResult);
        return finalResult;
    }
    
    _cleanInput(data) {
        if (typeof data === 'string') {
            return data.trim().toLowerCase();
        }
        return String(data);
    }
    
    _validateData(data) {
        if (!data) {
            throw new Error('Data cannot be empty');
        }
        return data;
    }
    
    _transformData(data) {
        // Apply multiple transformations
        const step1 = this._step1Transform(data);
        const step2 = this._step2Transform(step1);
        return step2;
    }
    
    _step1Transform(data) {
        return data.toUpperCase();
    }
    
    _step2Transform(data) {
        return `PROCESSED_${data}`;
    }
    
    _finalizeResult(data) {
        return `FINAL: ${data}`;
    }
    
    getCacheStats() {
        return {
            size: this.cache.size,
            keys: Array.from(this.cache.keys())
        };
    }
    
    clearCache() {
        this.cache.clear();
    }
}

module.exports = DataProcessor; 