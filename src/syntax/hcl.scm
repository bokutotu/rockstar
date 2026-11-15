; HCL / Terraform. Keep template expressions separate from their string text.
(comment) @comment
(numeric_lit) @number
(bool_lit) @boolean
(null_lit) @constant
(string_lit) @string
(template_literal) @string
(quoted_template_start) @string
(quoted_template_end) @string
(heredoc_identifier) @string.special

(attribute (identifier) @property)
(get_attr (identifier) @property)
(object_elem key: (expression (variable_expr (identifier) @property)))
(block . (identifier) @keyword)
(function_call (identifier) @function)
(variable_expr (identifier) @variable)

["for" "in" "if" "else" "endif" "endfor"] @keyword
["=" "!" "!=" "==" "+" "-" "*" "/" "%" "<" "<=" ">" ">=" "&&" "||" "?" ":" "=>"] @operator
["(" ")" "[" "]" "{" "}"] @punctuation.bracket
["," "."] @punctuation.delimiter
[
  (template_interpolation_start)
  (template_interpolation_end)
  (template_directive_start)
  (template_directive_end)
  (strip_marker)
] @punctuation.special
