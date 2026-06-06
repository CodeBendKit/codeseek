import { DataProcessor } from './dataProcessor';
import { MathUtils } from './mathUtils';
import { StringUtils } from './stringUtils';

interface Config {
  debug: boolean;
  maxRetries: number;
}

class Application {
  private config: Config;
  private dataProcessor: DataProcessor;
  private mathUtils: MathUtils;
  private stringUtils: StringUtils;

  constructor(config: Config) {
    this.config = config;
    this.dataProcessor = new DataProcessor();
    this.mathUtils = new MathUtils();
    this.stringUtils = new StringUtils();
  }

  async run(): Promise<void> {
    console.log('Starting TypeScript function call graph test...');
    
    try {
      // Process data
      const result = await this.processData('test_input');
      console.log('Processed result:', result);
      
      // Use math utilities
      const sum = this.mathUtils.calculateSum([1, 2, 3, 4, 5]);
      console.log('Sum:', sum);
      
      // Use string utilities
      const formatted = this.stringUtils.formatOutput('Hello World');
      console.log('Formatted:', formatted);
      
      // Test helper function
      const helperResult = this.helperFunction();
      console.log('Helper result:', helperResult);
      
    } catch (error) {
      console.error('Error in application:', error);
    }
  }

  private async processData(input: string): Promise<string> {
    if (this.config.debug) {
      console.log('Processing data:', input);
    }
    
    const result = this.dataProcessor.processData(input);
    return this.validateResult(result);
  }

  private validateResult(result: string): string {
    if (!result || result.length === 0) {
      throw new Error('Invalid result');
    }
    return result;
  }

  private helperFunction(): string {
    return 'helper result';
  }

  public getConfig(): Config {
    return { ...this.config };
  }
}

// Main execution
async function main(): Promise<void> {
  const config: Config = {
    debug: true,
    maxRetries: 3
  };
  
  const app = new Application(config);
  await app.run();
}

// Export for testing
export { Application, Config, main };

// Run if called directly
if (require.main === module) {
  main().catch(console.error);
} 