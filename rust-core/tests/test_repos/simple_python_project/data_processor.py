class DataProcessor:
    """Class for processing data with multiple method calls"""
    
    def __init__(self):
        self.cache = {}
    
    def process_data(self, input_data):
        """Main processing method"""
        if input_data in self.cache:
            return self.cache[input_data]
        
        # Process the data through multiple steps
        cleaned_data = self._clean_input(input_data)
        validated_data = self._validate_data(cleaned_data)
        transformed_data = self._transform_data(validated_data)
        final_result = self._finalize_result(transformed_data)
        
        # Cache the result
        self.cache[input_data] = final_result
        return final_result
    
    def _clean_input(self, data):
        """Clean the input data"""
        if isinstance(data, str):
            return data.strip().lower()
        return str(data)
    
    def _validate_data(self, data):
        """Validate the cleaned data"""
        if not data:
            raise ValueError("Data cannot be empty")
        return data
    
    def _transform_data(self, data):
        """Transform the validated data"""
        # Apply multiple transformations
        step1 = self._step1_transform(data)
        step2 = self._step2_transform(step1)
        return step2
    
    def _step1_transform(self, data):
        """First transformation step"""
        return data.upper()
    
    def _step2_transform(self, data):
        """Second transformation step"""
        return f"PROCESSED_{data}"
    
    def _finalize_result(self, data):
        """Finalize the result"""
        return f"FINAL: {data}"
    
    def get_cache_stats(self):
        """Get cache statistics"""
        return {
            'size': len(self.cache),
            'keys': list(self.cache.keys())
        } 