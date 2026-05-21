---
name: test-scaffold
description: Scaffold unit and integration tests for your project. Analyzes source code and generates test files with proper setup, assertions, and edge case coverage.
---

# Test Scaffold

## Overview

Analyze source code and generate comprehensive test suites. Creates unit tests, integration tests, and test utilities with proper assertions, mocking, and edge case coverage.

## When to Use

- User asks to "write tests", "add tests", or "scaffold tests"
- Creating test coverage for existing code
- Setting up test infrastructure for a new project
- Generating edge case tests

## Workflow

1. **Read source files** to understand functions, types, and behavior
2. **Identify testable units** — public functions, API endpoints, data transformations
3. **Generate test files** with proper framework setup
4. **Include** happy path, edge cases, error cases, and boundary conditions
5. **Write** test files using `write_file`

## Test Patterns by Language

### Python (pytest)
```python
import pytest
from module import function_under_test

class TestFunctionName:
    def test_happy_path(self):
        result = function_under_test(valid_input)
        assert result == expected_output

    def test_edge_case_empty(self):
        result = function_under_test("")
        assert result == default_value

    def test_error_case(self):
        with pytest.raises(ValueError):
            function_under_test(invalid_input)

    @pytest.fixture
    def sample_data(self):
        return {"key": "value"}
```

### Rust
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_happy_path() {
        let result = function_under_test(valid_input);
        assert_eq!(result, expected);
    }

    #[test]
    #[should_panic(expected = "error message")]
    fn test_error_case() {
        function_under_test(invalid_input);
    }

    #[test]
    fn test_edge_case() {
        assert!(function_under_test("").is_empty());
    }
}
```

### JavaScript/TypeScript (Jest/Vitest)
```typescript
describe('functionName', () => {
  it('should handle valid input', () => {
    expect(functionName(validInput)).toBe(expected);
  });

  it('should throw on invalid input', () => {
    expect(() => functionName(null)).toThrow();
  });

  it('should handle empty input', () => {
    expect(functionName('')).toEqual(defaultValue);
  });
});
```

## Test Categories

### Unit Tests
- Test individual functions in isolation
- Mock external dependencies
- Cover all code branches

### Integration Tests
- Test modules working together
- Use real (test) databases/services where possible
- Test API request/response cycles

### Edge Cases to Always Include
- Empty/null/undefined inputs
- Boundary values (0, -1, MAX_INT)
- Very large inputs
- Unicode and special characters
- Concurrent access (if applicable)
- Timeout and error recovery

## Best Practices

- Name tests descriptively: `test_returns_empty_list_when_no_items_match`
- One assertion per test when possible
- Use fixtures/setup for shared test data
- Test behavior, not implementation details
- Place test files adjacent to source or in a `tests/` directory per project convention
- Include both positive and negative test cases
