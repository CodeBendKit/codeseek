const DataProcessor = require('./dataProcessor');
const MathUtils = require('./mathUtils');
const StringUtils = require('./stringUtils');

function main() {
    console.log('Starting JavaScript function call graph test...');
    
    // Create instances
    const processor = new DataProcessor();
    const mathUtils = new MathUtils();
    const stringUtils = new StringUtils();
    
    // Process data
    const result = processor.processData('test_input');
    console.log('Processed result:', result);
    
    // Use math utilities
    const sum = mathUtils.calculateSum([1, 2, 3, 4, 5]);
    console.log('Sum:', sum);
    
    // Use string utilities
    const formatted = stringUtils.formatOutput('Hello World');
    console.log('Formatted:', formatted);
    
    // Test helper function
    const helperResult = helperFunction();
    console.log('Helper result:', helperResult);
}

function helperFunction() {
    return 'helper result';
}

// Export for testing
module.exports = {
    main,
    helperFunction
};

// Run if called directly
if (require.main === module) {
    main();
} 