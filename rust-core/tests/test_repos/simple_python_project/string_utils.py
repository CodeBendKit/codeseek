class StringUtils:
    """Class for string manipulation operations"""
    
    def format_output(self, text):
        """Format output text"""
        if not text:
            return self._get_default_text()
        
        # Apply multiple formatting steps
        cleaned = self._clean_text(text)
        formatted = self._apply_formatting(cleaned)
        return self._add_prefix(formatted)
    
    def _clean_text(self, text):
        """Clean the input text"""
        return text.strip()
    
    def _apply_formatting(self, text):
        """Apply formatting to text"""
        # Apply multiple formatting rules
        step1 = self._capitalize_first(text)
        step2 = self._add_punctuation(step1)
        return step2
    
    def _capitalize_first(self, text):
        """Capitalize first letter"""
        if not text:
            return text
        return text[0].upper() + text[1:]
    
    def _add_punctuation(self, text):
        """Add punctuation if missing"""
        if text.endswith(('.', '!', '?')):
            return text
        return text + '.'
    
    def _add_prefix(self, text):
        """Add prefix to formatted text"""
        return f"Formatted: {text}"
    
    def _get_default_text(self):
        """Get default text when input is empty"""
        return "No input provided"
    
    def reverse_string(self, text):
        """Reverse a string"""
        if not text:
            return text
        return text[::-1]
    
    def count_words(self, text):
        """Count words in text"""
        if not text:
            return 0
        words = text.split()
        return len(words)
    
    def find_longest_word(self, text):
        """Find the longest word in text"""
        if not text:
            return ""
        
        words = text.split()
        if not words:
            return ""
        
        longest = words[0]
        for word in words[1:]:
            if len(word) > len(longest):
                longest = word
        return longest 