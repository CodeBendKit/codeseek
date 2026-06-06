class StringUtils {
    formatOutput(text) {
        if (!text) {
            return this._getDefaultText();
        }
        
        // Apply multiple formatting steps
        const cleaned = this._cleanText(text);
        const formatted = this._applyFormatting(cleaned);
        return this._addPrefix(formatted);
    }
    
    _cleanText(text) {
        return text.trim();
    }
    
    _applyFormatting(text) {
        // Apply multiple formatting rules
        const step1 = this._capitalizeFirst(text);
        const step2 = this._addPunctuation(step1);
        return step2;
    }
    
    _capitalizeFirst(text) {
        if (!text) {
            return text;
        }
        return text.charAt(0).toUpperCase() + text.slice(1);
    }
    
    _addPunctuation(text) {
        if (text.endsWith('.') || text.endsWith('!') || text.endsWith('?')) {
            return text;
        }
        return text + '.';
    }
    
    _addPrefix(text) {
        return `Formatted: ${text}`;
    }
    
    _getDefaultText() {
        return 'No input provided';
    }
    
    reverseString(text) {
        if (!text) {
            return text;
        }
        return text.split('').reverse().join('');
    }
    
    countWords(text) {
        if (!text) {
            return 0;
        }
        const words = text.split(/\s+/);
        return words.length;
    }
    
    findLongestWord(text) {
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
    
    toCamelCase(text) {
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
}

module.exports = StringUtils; 