export interface StringStats {
  length: number;
  wordCount: number;
  characterCount: number;
  lineCount: number;
}

export class StringUtils {
  private readonly defaultPrefix: string = 'Formatted: ';

  formatOutput(text: string): string {
    if (!text) {
      return this.getDefaultText();
    }
    
    // Apply multiple formatting steps
    const cleaned = this.cleanText(text);
    const formatted = this.applyFormatting(cleaned);
    return this.addPrefix(formatted);
  }
  
  private cleanText(text: string): string {
    return text.trim();
  }
  
  private applyFormatting(text: string): string {
    // Apply multiple formatting rules
    const step1 = this.capitalizeFirst(text);
    const step2 = this.addPunctuation(step1);
    return step2;
  }
  
  private capitalizeFirst(text: string): string {
    if (!text) {
      return text;
    }
    return text.charAt(0).toUpperCase() + text.slice(1);
  }
  
  private addPunctuation(text: string): string {
    if (text.endsWith('.') || text.endsWith('!') || text.endsWith('?')) {
      return text;
    }
    return text + '.';
  }
  
  private addPrefix(text: string): string {
    return `${this.defaultPrefix}${text}`;
  }
  
  private getDefaultText(): string {
    return 'No input provided';
  }
  
  reverseString(text: string): string {
    if (!text) {
      return text;
    }
    return text.split('').reverse().join('');
  }
  
  countWords(text: string): number {
    if (!text) {
      return 0;
    }
    const words = text.split(/\s+/);
    return words.length;
  }
  
  findLongestWord(text: string): string {
    if (!text) {
      return '';
    }
    
    const words = text.split(/\s+/);
    if (words.length === 0) {
      return '';
    }
    
    let longest = words[0];
    for (const word of words) {
      if (word.length > longest.length) {
        longest = word;
      }
    }
    return longest;
  }
  
  toCamelCase(text: string): string {
    if (!text) {
      return text;
    }
    
    const words = text.split(/[\s_-]+/);
    if (words.length === 0) {
      return text;
    }
    
    let result = words[0].toLowerCase();
    for (let i = 1; i < words.length; i++) {
      const word = words[i];
      if (word) {
        result += word.charAt(0).toUpperCase() + word.slice(1).toLowerCase();
      }
    }
    return result;
  }

  // Additional string manipulation methods
  toSnakeCase(text: string): string {
    if (!text) {
      return text;
    }
    
    return text
      .replace(/([A-Z])/g, '_$1')
      .toLowerCase()
      .replace(/^_/, '');
  }

  toKebabCase(text: string): string {
    if (!text) {
      return text;
    }
    
    return text
      .replace(/([A-Z])/g, '-$1')
      .toLowerCase()
      .replace(/^-/, '');
  }

  truncate(text: string, maxLength: number, suffix: string = '...'): string {
    if (!text || text.length <= maxLength) {
      return text;
    }
    
    return text.substring(0, maxLength - suffix.length) + suffix;
  }

  getStringStats(text: string): StringStats {
    if (!text) {
      return {
        length: 0,
        wordCount: 0,
        characterCount: 0,
        lineCount: 0
      };
    }
    
    return {
      length: text.length,
      wordCount: this.countWords(text),
      characterCount: text.replace(/\s/g, '').length,
      lineCount: text.split('\n').length
    };
  }

  // Text analysis methods
  isPalindrome(text: string): boolean {
    if (!text) {
      return true;
    }
    
    const cleaned = text.toLowerCase().replace(/[^a-z0-9]/g, '');
    return cleaned === this.reverseString(cleaned);
  }

  countOccurrences(text: string, substring: string): number {
    if (!text || !substring) {
      return 0;
    }
    
    const regex = new RegExp(substring, 'g');
    const matches = text.match(regex);
    return matches ? matches.length : 0;
  }

  extractEmails(text: string): string[] {
    if (!text) {
      return [];
    }
    
    const emailRegex = /\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Z|a-z]{2,}\b/g;
    const matches = text.match(emailRegex);
    return matches || [];
  }

  extractUrls(text: string): string[] {
    if (!text) {
      return [];
    }
    
    const urlRegex = /https?:\/\/[^\s]+/g;
    const matches = text.match(urlRegex);
    return matches || [];
  }
} 