Problems to address:
1. The lexer throws comments away (there's literally a test: line_comment_with_keywords_produces_no_tokens). Fine for linting, fatal for formatting. Need it to emit comment/blank-line trivia with spans so the printer can re-attach them — and node positions already exist everywhere, which is exactly what comment re-attachment needs.
2. Typed AST ≠ lossless CST. Need to check what the parser normalizes away (parens, operator layout). The desugar oracle covers us while finding out.
